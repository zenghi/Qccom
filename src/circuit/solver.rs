//! 电路求解（改进节点分析 MNA 的简化版）：所有电源都用诺顿等效（电流源并联内阻），矩阵只有节点电压。
//! 非线性元件（二极管、三极管、MOS、稳压芯片等）用牛顿迭代，雅可比矩阵用数值差分求，模型只需要给出端口电流。
//! 电容、电感用后向欧拉的伴随模型做瞬态。节点 0 是地。

/// 二极管模型
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiodeM {
    /// 饱和电流
    pub is: f64,
    /// 理想因子 × 热电压
    pub nvt: f64,
    /// 串联电阻
    pub rs: f64,
    /// 反向击穿电压（稳压管、TVS；0 = 不击穿）
    pub bv: f64,
}

impl DiodeM {
    /// 按「电流 if_ 时压降 vf」反推饱和电流
    pub fn from_vf(vf: f64, if_: f64, n: f64, rs: f64) -> DiodeM {
        let nvt = n * VT;
        let vj = vf - if_ * rs;
        DiodeM {
            is: if_ / ((vj / nvt).exp() - 1.0),
            nvt,
            rs,
            bv: 0.0,
        }
    }
}

/// 三极管（Ebers-Moll）
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BjtM {
    pub is: f64,
    pub bf: f64,
    pub br: f64,
    pub pnp: bool,
}

/// MOS 管（平方律，带亚阈值平滑过渡）
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosM {
    pub vth: f64,
    /// 跨导系数 K（Id = K·Vov²）
    pub k: f64,
    pub lambda: f64,
    pub pch: bool,
}

impl MosM {
    /// 按手册的「Vgs = vgs 时导通电阻 rds」反推 K
    pub fn from_rds(vth: f64, rds: f64, vgs: f64, pch: bool) -> MosM {
        MosM {
            vth,
            k: 1.0 / (2.0 * rds * (vgs - vth).max(0.1)),
            lambda: 0.01,
            pch,
        }
    }
}

pub const VT: f64 = 0.025852;
/// 每个节点对地的极小电导，悬空节点也能解
const GMIN: f64 = 1e-9;

/// 指数限幅：超过 80 之后按直线延伸，防止溢出
fn limexp(x: f64) -> f64 {
    if x < 80.0 {
        x.exp()
    } else {
        let e = 80f64.exp();
        e * (1.0 + x - 80.0)
    }
}

pub fn diode_i(m: &DiodeM, v: f64) -> f64 {
    let mut i = m.is * (limexp(v / m.nvt) - 1.0);
    if m.bv > 0.0 {
        // 反向击穿：反压到 Vz 时约 5 mA（稳压管手册的测试电流量级），之后指数上升
        i -= 5e-3 * limexp((-v - m.bv) / 0.04);
    }
    i
}

/// 非线性元件：给出端口电压，算出流入元件的端口电流（即从节点流出的电流）
#[derive(Clone, Debug, PartialEq)]
pub enum Nl {
    /// 端口：阳极、阴极
    Diode(DiodeM),
    /// 端口：C、B、E
    Bjt(BjtM),
    /// 端口：D、G、S
    Mos(MosM),
    /// 线性稳压器 / 开关电源的平均模型。端口：输入、输出、地
    /// vset 输出电压，drop 压差，ilim 限流，iq 静态电流，eff 效率（LDO 用 0 表示输入电流 = 输出电流）
    Reg {
        vset: f64,
        drop: f64,
        ilim: f64,
        iq: f64,
        eff: f64,
        rout: f64,
        on: bool,
    },
    /// 运放（单电源、轨到轨近似）。端口：同相、反相、输出、V+、V−
    OpAmp { gain: f64, rout: f64 },
    /// 光耦：端口：LED 阳极、LED 阴极、集电极、发射极
    Opto { led: DiodeM, ctr: f64 },
    /// 受控电流源：从 a 流到 b 的电流 = g·(V(c) − V(d))，限制在 ±imax。端口：a、b、c、d
    Vccs { g: f64, imax: f64 },
    /// 带限流的电源（实验室电源、USB 口）：电压 v、内阻 r、最大电流 ilim。端口：正、负
    Psu { v: f64, r: f64, ilim: f64 },
}

/// 软限幅到 ±1：|x| 小时基本等于 x，大了平滑地停在 ±1
pub fn soft_clip(x: f64) -> f64 {
    x / (1.0 + x.powi(4)).powf(0.25)
}

impl Nl {
    pub fn pins(&self) -> usize {
        match self {
            Nl::Diode(_) => 2,
            Nl::Bjt(_) | Nl::Mos(_) | Nl::Reg { .. } => 3,
            Nl::OpAmp { .. } => 5,
            Nl::Opto { .. } | Nl::Vccs { .. } => 4,
            Nl::Psu { .. } => 2,
        }
    }

    /// v：各端口电压；out：从各端口节点流进元件的电流
    pub fn currents(&self, v: &[f64], out: &mut [f64]) {
        match self {
            Nl::Diode(m) => {
                let i = diode_with_rs(m, v[0] - v[1]);
                out[0] = i;
                out[1] = -i;
            }
            Nl::Bjt(m) => {
                let s = if m.pnp { -1.0 } else { 1.0 };
                let vbe = s * (v[1] - v[2]);
                let vbc = s * (v[1] - v[0]);
                let ef = limexp(vbe / VT) - 1.0;
                let er = limexp(vbc / VT) - 1.0;
                let ic = m.is * (ef - er) - m.is / m.br * er;
                let ib = m.is / m.bf * ef + m.is / m.br * er;
                out[0] = s * ic;
                out[1] = s * ib;
                out[2] = -s * (ic + ib);
            }
            Nl::Mos(m) => {
                let s = if m.pch { -1.0 } else { 1.0 };
                let (mut vd, vg, mut vs) = (s * v[0], s * v[1], s * v[2]);
                // 对称：漏源反过来时交换
                let swap = vd < vs;
                if swap {
                    std::mem::swap(&mut vd, &mut vs);
                }
                let vgs = vg - vs;
                let vds = vd - vs;
                let n = 0.04;
                let vov = n * (1.0 + limexp((vgs - m.vth) / n)).ln();
                let id = if vds < vov { m.k * (2.0 * vov * vds - vds * vds) } else { m.k * vov * vov } * (1.0 + m.lambda * vds);
                let id = if swap { -id } else { id };
                out[0] = s * id;
                out[1] = 0.0;
                out[2] = -s * id;
            }
            Nl::Reg { vset, drop, ilim, iq, eff, rout, on } => {
                let (vin, vout, vg) = (v[0] - v[2], v[1] - v[2], 0.0);
                let _ = vg;
                if !on || vin <= 0.0 {
                    out.fill(0.0);
                    // 关断时输出对地放电很慢，输入几乎不取电
                    return;
                }
                // 目标电压：输入不够时跟着输入掉（软过渡）
                let head = vin - drop;
                let k = 0.05;
                let target = if head > *vset {
                    *vset - k * (1.0 + (-(head - vset) / k).exp()).ln()
                } else {
                    head - k * (1.0 + (-(vset - head) / k).exp()).ln()
                }
                .max(0.0);
                // 稳压器只能往外送电流、不能吸电流（输出比目标高时只剩一点漏电导）；送出的电流软限流
                let x = (target - vout) / rout / ilim;
                let pos = 0.5 * (x + (x * x + 1e-6).sqrt());
                let iout = ilim * soft_clip(pos);
                let iout_pos = iout.max(0.0);
                let iin = if *eff > 0.0 { iout_pos * vout.max(0.0) / (eff * vin.max(0.1)) } else { iout_pos } + iq * (vin / 1.0).tanh();
                out[0] = iin;
                out[1] = -iout;
                out[2] = iout - iin;
            }
            Nl::OpAmp { gain, rout } => {
                let (vp, vn, vo, vcc, vee) = (v[0], v[1], v[2], v[3], v[4]);
                let span = (vcc - vee).max(0.01);
                let mid = (vcc + vee) / 2.0;
                // 输出摆幅比电源轨少一点（LM358 高端约少 1.5V，这里统一 0.05~1.5 之间折中）
                let top = vcc - 0.05 * span.min(1.0) * 20.0_f64.min(1.0);
                // 增益按 2000 封顶，饱和用代数函数（远离线性区斜率按 1/x² 衰减，不像 tanh 那样指数趋零），牛顿迭代才收敛
                let x = gain.min(2000.0) * (vp - vn) / (span / 2.0);
                let target = mid + (top - mid) * x / (1.0 + x.abs());
                let io = (target - vo) / rout;
                // 输入端几乎不取电，输出电流从电源轨来
                out[0] = 0.0;
                out[1] = 0.0;
                out[2] = -io;
                out[3] = io.max(0.0);
                out[4] = (-io).max(0.0) * -1.0;
            }
            Nl::Opto { led, ctr } => {
                let i_led = diode_with_rs(led, v[0] - v[1]);
                let vce = v[2] - v[3];
                let ic = ctr * i_led.max(0.0) * (vce / 0.15).tanh().max(0.0);
                out[0] = i_led;
                out[1] = -i_led;
                out[2] = ic;
                out[3] = -ic;
            }
            Nl::Psu { v: e, r, ilim } => {
                // 电流从正极流出到外部 = 流入元件的电流为负
                // 限流以后再加一点点漏电导，牛顿迭代才有方向
                let d = e - (v[0] - v[1]);
                let i = ilim * soft_clip(d / r / ilim) + 1e-4 * d;
                out[0] = -i;
                out[1] = i;
            }
            Nl::Vccs { g, imax } => {
                let i = imax * (g * (v[2] - v[3]) / imax).tanh();
                out[0] = i;
                out[1] = -i;
                out[2] = 0.0;
                out[3] = 0.0;
            }
        }
    }
}

/// 带串联电阻的二极管：用几步牛顿解出结电压
fn diode_with_rs(m: &DiodeM, v: f64) -> f64 {
    if m.rs <= 0.0 {
        return diode_i(m, v);
    }
    // 解 i = f(v − i·rs)
    let mut vj = v.min(m.nvt * 40.0);
    for _ in 0..40 {
        let i = diode_i(m, vj);
        let h = 1e-6;
        let di = (diode_i(m, vj + h) - diode_i(m, vj - h)) / (2.0 * h);
        let f = vj + i * m.rs - v;
        let df = 1.0 + di * m.rs;
        let step = f / df;
        vj -= step.clamp(-0.2, 0.2);
        if step.abs() < 1e-9 {
            break;
        }
    }
    (v - vj) / m.rs
}

/// 一个元件。节点号 0 是地
#[derive(Clone, Debug, PartialEq)]
pub enum Dev {
    R {
        a: usize,
        b: usize,
        r: f64,
    },
    /// 电容（v0 = 上一步两端电压）
    C {
        a: usize,
        b: usize,
        c: f64,
        v0: f64,
    },
    /// 电感（i0 = 上一步电流，从 a 到 b）
    L {
        a: usize,
        b: usize,
        l: f64,
        r: f64,
        i0: f64,
    },
    /// 电压源：p 比 n 高 v，内阻 r
    V {
        p: usize,
        n: usize,
        v: f64,
        r: f64,
    },
    /// 电流源：从 a 经过元件流到 b
    I {
        a: usize,
        b: usize,
        i: f64,
    },
    /// 开关
    Sw {
        a: usize,
        b: usize,
        on: bool,
        ron: f64,
    },
    N {
        pins: Vec<usize>,
        m: Nl,
    },
}

pub const ROFF: f64 = 1e9;

impl Dev {
    pub fn nodes(&self) -> Vec<usize> {
        match self {
            Dev::R { a, b, .. } | Dev::C { a, b, .. } | Dev::L { a, b, .. } | Dev::I { a, b, .. } | Dev::Sw { a, b, .. } => vec![*a, *b],
            Dev::V { p, n, .. } => vec![*p, *n],
            Dev::N { pins, .. } => pins.clone(),
        }
    }
}

/// 求解模式
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// 直流工作点：电容开路、电感短路
    Dc,
    /// 瞬态一步
    Tran(f64),
}

pub struct Solver {
    pub n: usize,
    g: Vec<f64>,
    b: Vec<f64>,
    /// 上次的解（牛顿迭代起点）
    pub v: Vec<f64>,
    pub iters: usize,
}

impl Solver {
    pub fn new(nodes: usize) -> Solver {
        Solver {
            n: nodes,
            g: vec![0.0; nodes * nodes],
            b: vec![0.0; nodes],
            v: vec![0.0; nodes],
            iters: 0,
        }
    }

    fn add_g(&mut self, a: usize, b: usize, g: f64) {
        let n = self.n;
        if a != 0 {
            self.g[a * n + a] += g;
        }
        if b != 0 {
            self.g[b * n + b] += g;
        }
        if a != 0 && b != 0 {
            self.g[a * n + b] -= g;
            self.g[b * n + a] -= g;
        }
    }

    /// 电流 i 从节点 a 流出、经元件流入节点 b
    fn add_i(&mut self, a: usize, b: usize, i: f64) {
        if a != 0 {
            self.b[a] -= i;
        }
        if b != 0 {
            self.b[b] += i;
        }
    }

    fn stamp_linear(&mut self, d: &Dev, mode: Mode) {
        match *d {
            Dev::R { a, b, r } => self.add_g(a, b, 1.0 / r.max(1e-6)),
            Dev::C { a, b, c, v0 } => {
                if let Mode::Tran(dt) = mode {
                    let g = c / dt;
                    self.add_g(a, b, g);
                    // 伴随电流源：等效于 i = g(v − v0)
                    self.add_i(b, a, g * v0);
                } else {
                    self.add_g(a, b, GMIN);
                }
            }
            Dev::L { a, b, l, r, i0 } => match mode {
                Mode::Tran(dt) => {
                    // v = r·i + L(i − i0)/dt → i = (v + L·i0/dt) / (r + L/dt)
                    let z = r + l / dt;
                    self.add_g(a, b, 1.0 / z);
                    self.add_i(a, b, (l / dt) * i0 / z);
                }
                Mode::Dc => self.add_g(a, b, 1.0 / r.max(1e-3)),
            },
            Dev::V { p, n, v, r } => {
                let r = r.max(1e-3);
                self.add_g(p, n, 1.0 / r);
                self.add_i(n, p, v / r);
            }
            Dev::I { a, b, i } => self.add_i(a, b, i),
            Dev::Sw { a, b, on, ron } => self.add_g(a, b, if on { 1.0 / ron.max(1e-4) } else { 1.0 / ROFF }),
            Dev::N { .. } => {}
        }
    }

    /// 解一次（含牛顿迭代）。返回是否收敛。直接牛顿不收敛时，把电源从 0 慢慢加到 100% 再试（电源步进）
    pub fn solve(&mut self, devs: &[Dev], mode: Mode) -> bool {
        let n = self.n;
        if n <= 1 {
            self.v = vec![0.0; n];
            return true;
        }
        let mut v = self.v.clone();
        v.resize(n, 0.0);
        v[0] = 0.0;
        // 线性部分只跟元件和步长有关，先盖一次
        self.g.iter_mut().for_each(|x| *x = 0.0);
        self.b.iter_mut().for_each(|x| *x = 0.0);
        for k in 1..n {
            self.g[k * n + k] += GMIN;
        }
        for d in devs {
            self.stamp_linear(d, mode);
        }
        let (g0, b0) = (self.g.clone(), self.b.clone());
        if let Some(x) = newton(n, &g0, &b0, 1.0, devs, &v, 120, &mut self.iters) {
            self.v = x;
            return true;
        }
        // 电源步进：先从全 0 开始，按 λ 把独立源逐步加满
        let mut x = vec![0.0; n];
        let mut lam: f64 = 0.0;
        let mut step: f64 = 0.1;
        let mut total = self.iters;
        while lam < 1.0 {
            let next = (lam + step).min(1.0);
            let mut it = 0;
            match newton(n, &g0, &b0, next, devs, &x, 60, &mut it) {
                Some(y) => {
                    x = y;
                    lam = next;
                    step = (step * 1.5).min(0.25);
                }
                None => {
                    step /= 4.0;
                    if step < 1e-4 {
                        self.iters = total + it;
                        return false;
                    }
                }
            }
            total += it;
        }
        self.iters = total;
        self.v = x;
        true
    }

    /// 求解收敛后更新电容、电感的状态
    pub fn commit(&self, devs: &mut [Dev], mode: Mode) {
        for d in devs {
            match d {
                Dev::C { a, b, v0, .. } => *v0 = self.v[*a] - self.v[*b],
                Dev::L { a, b, l, r, i0 } => {
                    let vab = self.v[*a] - self.v[*b];
                    *i0 = match mode {
                        Mode::Tran(dt) => (vab + *l / dt * *i0) / (*r + *l / dt),
                        Mode::Dc => vab / r.max(1e-3),
                    };
                }
                _ => {}
            }
        }
    }

    /// 元件电流（两端元件：从第一个端口流到第二个端口；多端元件：各端口流入的电流）
    pub fn current(&self, d: &Dev, mode: Mode) -> Vec<f64> {
        let v = &self.v;
        match *d {
            Dev::R { a, b, r } => one((v[a] - v[b]) / r.max(1e-6)),
            Dev::C { a, b, c, v0 } => one(match mode {
                Mode::Tran(dt) => c / dt * ((v[a] - v[b]) - v0),
                Mode::Dc => 0.0,
            }),
            Dev::L { a, b, l, r, i0 } => one(match mode {
                Mode::Tran(dt) => ((v[a] - v[b]) + l / dt * i0) / (r + l / dt),
                Mode::Dc => (v[a] - v[b]) / r.max(1e-3),
            }),
            // 电源：从正极流出（对外供电为正）
            Dev::V { p, n, v: e, r } => one((e - (v[p] - v[n])) / r.max(1e-3)),
            Dev::I { i, .. } => one(i),
            Dev::Sw { a, b, on, ron } => one((v[a] - v[b]) / if on { ron.max(1e-4) } else { ROFF }),
            Dev::N { ref pins, ref m } => {
                let pv: Vec<f64> = pins.iter().map(|p| v[*p]).collect();
                let mut out = vec![0.0; pins.len()];
                m.currents(&pv, &mut out);
                out
            }
        }
    }
}

/// 节点残差：从各节点流出的电流之和（线性部分 + 非线性元件）。λ 缩放独立源
fn residual(n: usize, g0: &[f64], b0: &[f64], lam: f64, devs: &[Dev], v: &[f64], out: &mut [f64]) {
    let mut cur = [0.0; 8];
    let mut pv = [0.0; 8];
    for k in 1..n {
        let mut s = -lam * b0[k];
        for j in 1..n {
            s += g0[k * n + j] * v[j];
        }
        out[k] = s;
    }
    for d in devs {
        if let Dev::N { pins, m } = d {
            let np = pins.len();
            for (k, p) in pins.iter().enumerate() {
                pv[k] = v[*p];
            }
            m.currents(&pv[..np], &mut cur[..np]);
            for k in 0..np {
                if pins[k] != 0 {
                    out[pins[k]] += cur[k];
                }
            }
        }
    }
}

fn norm(r: &[f64]) -> f64 {
    r.iter().skip(1).map(|x| x * x).sum::<f64>().sqrt()
}

/// 牛顿迭代 + 残差回溯线搜索。λ 缩放独立源（电源步进用）
#[allow(clippy::too_many_arguments)]
fn newton(n: usize, g0: &[f64], b0: &[f64], lam: f64, devs: &[Dev], start: &[f64], max_it: usize, iters: &mut usize) -> Option<Vec<f64>> {
    let mut v = start.to_vec();
    let mut g = vec![0.0; n * n];
    let mut b = vec![0.0; n];
    let mut r = vec![0.0; n];
    let mut r2 = vec![0.0; n];
    let mut cur = [0.0; 8];
    let mut cur2 = [0.0; 8];
    let mut pv = [0.0; 8];
    let nonlinear = devs.iter().any(|d| matches!(d, Dev::N { .. }));
    residual(n, g0, b0, lam, devs, &v, &mut r);
    let mut f = norm(&r);
    if nonlinear && f < 1e-10 {
        *iters = 0;
        return Some(v);
    }
    for it in 0..max_it {
        *iters = it + 1;
        g.copy_from_slice(g0);
        for k in 0..n {
            b[k] = lam * b0[k];
        }
        for d in devs {
            let Dev::N { pins, m } = d else { continue };
            let np = pins.len();
            for (k, p) in pins.iter().enumerate() {
                pv[k] = v[*p];
            }
            m.currents(&pv[..np], &mut cur[..np]);
            // 数值雅可比：∂I_k/∂V_j，线性化 I_k ≈ I_k0 + Σ g_kj (V_j − V_j0)
            for j in 0..np {
                let h = 1e-6 * (1.0 + pv[j].abs());
                let keep = pv[j];
                pv[j] = keep + h;
                m.currents(&pv[..np], &mut cur2[..np]);
                pv[j] = keep;
                for k in 0..np {
                    let gkj = (cur2[k] - cur[k]) / h;
                    let (pk, pj) = (pins[k], pins[j]);
                    if pk != 0 {
                        if pj != 0 {
                            g[pk * n + pj] += gkj;
                        }
                        b[pk] += gkj * keep;
                    }
                }
            }
            for k in 0..np {
                if pins[k] != 0 {
                    b[pins[k]] -= cur[k];
                }
            }
        }
        let x = lu_solve(n, &b, &g)?;
        if !nonlinear {
            return Some(x);
        }
        let mut maxd: f64 = 0.0;
        for k in 1..n {
            maxd = maxd.max((x[k] - v[k]).abs());
        }
        // 先试最多 2V 的一步，残差没变小就回溯。回溯到很小也不行，或者只能走不到 0.5V 而且残差几乎没变
        // （限流、饱和这类平坦区域），就按 0.5V 硬走一步
        let a_clamp = if maxd > 0.5 { 0.5 / maxd } else { 1.0 };
        let a0 = if maxd > 2.0 { 2.0 / maxd } else { 1.0 };
        let mut alpha = a0;
        let mut trial = v.clone();
        let mut f2;
        let mut failed = false;
        loop {
            for k in 1..n {
                trial[k] = v[k] + (x[k] - v[k]) * alpha;
            }
            residual(n, g0, b0, lam, devs, &trial, &mut r2);
            f2 = norm(&r2);
            if f2.is_finite() && f2 < f * (1.0 - 1e-4 * alpha) {
                break;
            }
            if alpha < a0 * 1e-4 {
                failed = true;
                break;
            }
            alpha *= 0.5;
        }
        if failed || (alpha < a_clamp && f2 > 0.999 * f) {
            alpha = a_clamp;
            for k in 1..n {
                trial[k] = v[k] + (x[k] - v[k]) * alpha;
            }
            residual(n, g0, b0, lam, devs, &trial, &mut r2);
            f2 = norm(&r2);
        }
        if !f2.is_finite() {
            return None;
        }
        // 线搜索失败也接受小步，避免卡死
        std::mem::swap(&mut v, &mut trial);
        f = f2;
        let vmax = v.iter().fold(0.0f64, |a, b| a.max(b.abs()));
        let step = maxd * alpha;
        if step < 1e-6 + 1e-6 * vmax && f < 1e-6 {
            return Some(v);
        }
    }
    None
}

fn one(x: f64) -> Vec<f64> {
    vec![x]
}

/// 解 G·x = b（只解 1..n 号节点，0 号是地）。部分主元高斯消元
fn lu_solve(n: usize, b: &[f64], g: &[f64]) -> Option<Vec<f64>> {
    let m = n - 1;
    let mut a = vec![0.0; m * m];
    let mut y = vec![0.0; m];
    for i in 0..m {
        for j in 0..m {
            a[i * m + j] = g[(i + 1) * n + (j + 1)];
        }
        y[i] = b[i + 1];
    }
    for c in 0..m {
        // 选主元
        let mut p = c;
        let mut best = a[c * m + c].abs();
        for r in c + 1..m {
            let x = a[r * m + c].abs();
            if x > best {
                best = x;
                p = r;
            }
        }
        if best < 1e-30 || !best.is_finite() {
            return None;
        }
        if p != c {
            for j in 0..m {
                a.swap(c * m + j, p * m + j);
            }
            y.swap(c, p);
        }
        let d = a[c * m + c];
        for r in c + 1..m {
            let f = a[r * m + c] / d;
            if f == 0.0 {
                continue;
            }
            for j in c..m {
                a[r * m + j] -= f * a[c * m + j];
            }
            y[r] -= f * y[c];
        }
    }
    let mut x = vec![0.0; n];
    for i in (0..m).rev() {
        let mut s = y[i];
        for j in i + 1..m {
            s -= a[i * m + j] * x[j + 1];
        }
        x[i + 1] = s / a[i * m + i];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn led_red() -> DiodeM {
        DiodeM::from_vf(2.0, 0.01, 2.0, 2.0)
    }

    #[test]
    fn divider() {
        let devs = vec![Dev::V { p: 1, n: 0, v: 5.0, r: 0.0 }, Dev::R { a: 1, b: 2, r: 1000.0 }, Dev::R { a: 2, b: 0, r: 2000.0 }];
        let mut s = Solver::new(3);
        assert!(s.solve(&devs, Mode::Dc));
        assert!((s.v[2] - 3.333).abs() < 0.01, "{:?}", s.v);
    }

    #[test]
    fn led_with_resistor() {
        // 5V − 220Ω − 红 LED：约 (5 − 2)/220 ≈ 13.6 mA
        let devs = vec![
            Dev::V { p: 1, n: 0, v: 5.0, r: 0.0 },
            Dev::R { a: 1, b: 2, r: 220.0 },
            Dev::N {
                pins: vec![2, 0],
                m: Nl::Diode(led_red()),
            },
        ];
        let mut s = Solver::new(3);
        assert!(s.solve(&devs, Mode::Dc));
        let i = (s.v[1] - s.v[2]) / 220.0;
        assert!(i > 0.012 && i < 0.0145, "i = {i}, v = {:?}", s.v);
    }

    #[test]
    fn led_without_resistor() {
        // 直接接 5V：电流大到几百 mA（会「烧」）
        let devs = vec![
            Dev::V { p: 1, n: 0, v: 5.0, r: 0.05 },
            Dev::N {
                pins: vec![1, 0],
                m: Nl::Diode(led_red()),
            },
        ];
        let mut s = Solver::new(2);
        assert!(s.solve(&devs, Mode::Dc));
        let i = s.current(&devs[1], Mode::Dc)[0];
        assert!(i > 0.3, "i = {i}");
    }

    #[test]
    fn bjt_switch() {
        // 3.3V − 1k − 基极，集电极 5V − 330Ω：饱和，Vce < 0.2V
        let q = BjtM {
            is: 1e-14,
            bf: 200.0,
            br: 5.0,
            pnp: false,
        };
        let devs = vec![
            Dev::V { p: 1, n: 0, v: 5.0, r: 0.0 },
            Dev::V { p: 2, n: 0, v: 3.3, r: 0.0 },
            Dev::R { a: 2, b: 3, r: 1000.0 },
            Dev::R { a: 1, b: 4, r: 330.0 },
            Dev::N { pins: vec![4, 3, 0], m: Nl::Bjt(q) },
        ];
        let mut s = Solver::new(5);
        assert!(s.solve(&devs, Mode::Dc));
        assert!(s.v[4] < 0.25, "vce = {}", s.v[4]);
        assert!((s.v[3] - 0.7).abs() < 0.1, "vbe = {}", s.v[3]);
    }

    #[test]
    fn nmos_logic_level() {
        // AO3400：Vth 1.0，2.5V 时 48mΩ。3.3V 驱动，负载 10Ω 接 5V
        let m = MosM::from_rds(1.0, 0.048, 2.5, false);
        let devs = vec![
            Dev::V { p: 1, n: 0, v: 5.0, r: 0.0 },
            Dev::V { p: 2, n: 0, v: 3.3, r: 0.0 },
            Dev::R { a: 1, b: 3, r: 10.0 },
            Dev::N { pins: vec![3, 2, 0], m: Nl::Mos(m) },
        ];
        let mut s = Solver::new(4);
        assert!(s.solve(&devs, Mode::Dc));
        assert!(s.v[3] < 0.05, "vds = {}", s.v[3]);
        // IRF540N：Vth 3.0，10V 时 44mΩ。3.3V 驱动时只是半导通
        let m = MosM::from_rds(3.0, 0.044, 10.0, false);
        let mut devs = devs;
        devs[3] = Dev::N { pins: vec![3, 2, 0], m: Nl::Mos(m) };
        let mut s = Solver::new(4);
        assert!(s.solve(&devs, Mode::Dc));
        assert!(s.v[3] > 0.5, "vds = {}", s.v[3]);
    }

    #[test]
    fn rc_charge() {
        // 5V、10k、100µF：τ = 1 s，1 s 后约 63%
        let mut devs = vec![Dev::V { p: 1, n: 0, v: 5.0, r: 0.0 }, Dev::R { a: 1, b: 2, r: 10_000.0 }, Dev::C { a: 2, b: 0, c: 100e-6, v0: 0.0 }];
        let mut s = Solver::new(3);
        let dt = 1e-3;
        for _ in 0..1000 {
            assert!(s.solve(&devs, Mode::Tran(dt)));
            s.commit(&mut devs, Mode::Tran(dt));
        }
        assert!((s.v[2] - 5.0 * 0.632).abs() < 0.05, "v = {}", s.v[2]);
    }

    #[test]
    fn ldo() {
        // 5V 进，3.3V 出，100Ω 负载 → 33 mA，输入电流也约 33 mA
        let devs = vec![
            Dev::V { p: 1, n: 0, v: 5.0, r: 0.0 },
            Dev::N {
                pins: vec![1, 2, 0],
                m: Nl::Reg {
                    vset: 3.3,
                    drop: 1.1,
                    ilim: 1.0,
                    iq: 5e-3,
                    eff: 0.0,
                    rout: 0.05,
                    on: true,
                },
            },
            Dev::R { a: 2, b: 0, r: 100.0 },
        ];
        let mut s = Solver::new(3);
        assert!(s.solve(&devs, Mode::Dc));
        assert!((s.v[2] - 3.3).abs() < 0.05, "vout = {}", s.v[2]);
        // 输入只有 3.6V：输出跟着输入掉到 2.5V 左右
        let mut devs = devs;
        devs[0] = Dev::V { p: 1, n: 0, v: 3.6, r: 0.0 };
        assert!(s.solve(&devs, Mode::Dc));
        assert!(s.v[2] < 2.7 && s.v[2] > 2.2, "vout = {}", s.v[2]);
    }

    #[test]
    fn zener() {
        // 9V − 1k − 5.1V 稳压管
        let mut z = DiodeM::from_vf(0.7, 0.01, 1.0, 0.5);
        z.bv = 5.1;
        let devs = vec![Dev::V { p: 1, n: 0, v: 9.0, r: 0.0 }, Dev::R { a: 1, b: 2, r: 1000.0 }, Dev::N { pins: vec![0, 2], m: Nl::Diode(z) }];
        let mut s = Solver::new(3);
        assert!(s.solve(&devs, Mode::Dc));
        assert!((s.v[2] - 5.1).abs() < 0.25, "vz = {}", s.v[2]);
    }

    #[test]
    fn psu_current_limit() {
        // 5V 限流 0.1A，接 10Ω：电流被限住，电压掉到 1V 左右
        let devs = vec![
            Dev::N {
                pins: vec![1, 0],
                m: Nl::Psu { v: 5.0, r: 0.01, ilim: 0.1 },
            },
            Dev::R { a: 1, b: 0, r: 10.0 },
        ];
        let mut s = Solver::new(2);
        assert!(s.solve(&devs, Mode::Dc));
        assert!(s.v[1] > 0.95 && s.v[1] < 1.1, "v = {}", s.v[1]);
        // 1kΩ：不限流，5V
        let devs = vec![
            Dev::N {
                pins: vec![1, 0],
                m: Nl::Psu { v: 5.0, r: 0.01, ilim: 0.1 },
            },
            Dev::R { a: 1, b: 0, r: 1000.0 },
        ];
        assert!(s.solve(&devs, Mode::Dc));
        assert!((s.v[1] - 5.0).abs() < 0.01, "v = {}", s.v[1]);
    }

    #[test]
    fn opamp_follower() {
        let devs = vec![
            Dev::V { p: 1, n: 0, v: 5.0, r: 0.0 },
            Dev::V { p: 2, n: 0, v: 2.0, r: 0.0 },
            Dev::N {
                pins: vec![2, 3, 3, 1, 0],
                m: Nl::OpAmp { gain: 1e5, rout: 10.0 },
            },
            Dev::R { a: 3, b: 0, r: 1000.0 },
        ];
        let mut s = Solver::new(4);
        assert!(s.solve(&devs, Mode::Dc));
        assert!((s.v[3] - 2.0).abs() < 0.01, "vo = {}", s.v[3]);
    }
}
