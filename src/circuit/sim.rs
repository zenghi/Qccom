//! 仿真运行时：把画布上的元件编译成求解器的器件，按时间一步步往前算，再把结果变成「看得见」的东西
//! （LED 亮度、电机转速、读数、烧毁……）。
//!
//! 每帧调用 advance()：按速度倍率算出这帧要走的仿真时间，自适应步长（遇到开关、PWM 边沿等事件时精确停在事件点），
//! 有 CPU 预算，跟不上时实际倍率会变小并显示出来。

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use super::atdev::{self, AtDev, NetCmd, NetEvt};
use super::lib::{self, Look, PartDef};
use super::mcu::{Effect, McuCtx, McuRt};
use super::models;
use super::net::Netlist;
use super::solver::{Dev, DiodeM, Mode, Nl, Solver, ROFF};
use super::{Circuit, Part};

pub const NONE: usize = usize::MAX;

/// 每个元件的显示状态（帧平均）
#[derive(Clone, Debug, Default)]
pub struct Vis {
    /// LED / 灯泡亮度（1 = 额定）
    pub glow: f32,
    pub rgb: [f32; 3],
    /// 数码管各段亮度 a..g, dp
    pub segs: [f32; 8],
    /// 电机轴角度 / 舵机摆臂角度（度）
    pub angle: f32,
    /// 电机转速（转/分）
    pub rpm: f32,
    /// 继电器吸合、蜂鸣器在响、模块已上电
    pub on: bool,
    /// 无源蜂鸣器的频率
    pub freq: f32,
    /// 一行状态说明（三极管工作区、MOS 导通情况……）
    pub status: String,
    /// 仪表读数
    pub reading: String,
    /// 文字屏的内容（OLED、LCD1602、TFT 文字）
    pub lines: Vec<String>,
    /// 屏上显示的图片（内置示例图名）
    pub image: String,
    /// TM1637 / 数码管显示的字符
    pub digits: String,
    /// 屏幕背光 / 是否点亮
    pub lit: bool,
}

/// 元件的运行状态
#[derive(Default)]
pub struct PState {
    pub id: u32,
    /// 器件在 Sim::devs 里的范围
    pub d0: usize,
    pub d1: usize,
    /// 每个引脚的节点号（NONE = 没接任何东西）
    pub pin_node: Vec<usize>,
    pub inner: Vec<usize>,
    /// 过载累积（秒），超过阈值就烧
    pub stress: f64,
    pub burnt: Option<String>,
    /// 温度（°C）
    pub temp: f64,
    /// 帧平均：各引脚流入电流、功率
    pub i: Vec<f64>,
    pub p: f64,
    acc_i: Vec<f64>,
    acc_p: f64,
    acc_t: f64,
    /// 帧内峰值电流（引脚最大绝对值）
    pub ipeak: f64,
    pub vis: Vis,
    pub beh: Beh,
}

/// 元件行为（随时间变化的部分）
#[derive(Default)]
pub enum Beh {
    #[default]
    None,
    Battery {
        soc: f64,
        spec: models::BatSpec,
        used_mah: f64,
    },
    Ac {
        amp: f64,
        freq: f64,
    },
    Square {
        amp: f64,
        freq: f64,
        duty: f64,
    },
    Relay {
        closed: bool,
        inom: f64,
    },
    Motor {
        omega: f64,
        angle: f64,
        kv: f64,
    },
    Servo {
        angle: f64,
        target: f64,
        rise: Option<f64>,
        last_pulse: f64,
    },
    Fuse {
        blown: bool,
    },
    Timer555 {
        q: bool,
    },
    Hc595 {
        sr: u8,
        out: u8,
        clk: bool,
        rclk: bool,
    },
    Buzzer {
        last_cross: f64,
        sign: bool,
        period: f64,
    },
    Encoder {
        last: i64,
    },
    Load {
        /// 模块：突发电流的相位（4G / WiFi 发射）
        phase: f64,
    },
    Mcu(Box<McuRt>),
    UsbTtl {
        /// 主机（QCCOM）发来的待发字节
        tx: Vec<u8>,
        busy_until: f64,
    },
}

/// 仿真里发出的一段串口数据
#[derive(Clone, Debug)]
pub struct UartTx {
    pub from: u32,
    pub net: usize,
    pub gnd: usize,
    pub baud: u32,
    pub data: Vec<u8>,
    pub t: f64,
}

/// 记录给示波器 / 逻辑分析仪的数字边沿（协议级的通信不在电路里真的翻转，在这里补上波形）
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    pub edges: Vec<(f64, bool)>,
}

/// 示波器探针 / 逻辑分析仪一个通道记下的电压。只在电压有变化时记一点（前面补上保持值），平的地方很省
#[derive(Clone, Debug, Default)]
pub struct Trace {
    pub part: u32,
    pub pin: usize,
    pub net: usize,
    pub s: VecDeque<(f64, f32)>,
    /// 没记下的最近一点（变化时先把它补进去，免得插值成斜线）
    held: Option<(f64, f32)>,
}

impl Trace {
    fn push(&mut self, t: f64, v: f32) {
        let last = self.s.back().copied();
        match last {
            Some((lt, lv)) if (v - lv).abs() < 2e-3 && t - lt < 2e-3 => {
                self.held = Some((t, v));
            }
            _ => {
                if let Some(h) = self.held.take() {
                    if last.is_some_and(|l| l.0 < h.0) {
                        self.s.push_back(h);
                    }
                }
                self.s.push_back((t, v));
            }
        }
        // 只留最近 10 秒、最多 40 万点
        while self.s.len() > 400_000 || self.s.front().is_some_and(|f| f.0 < t - 10.0) {
            self.s.pop_front();
        }
    }

    /// 时刻 t 的电压（线性插值）
    pub fn at(&self, t: f64) -> Option<f32> {
        let i = self.s.partition_point(|x| x.0 < t);
        if i == 0 {
            return self.s.front().filter(|f| f.0 <= t + 1e-12).map(|f| f.1);
        }
        let a = self.s[i - 1];
        match self.s.get(i) {
            Some(b) if b.0 > a.0 => Some(a.1 + (b.1 - a.1) * ((t - a.0) / (b.0 - a.0)) as f32),
            _ => Some(self.held.filter(|h| h.0 >= a.0 && t >= a.0).map_or(a.1, |h| h.1)),
        }
    }

    /// [t0, t1] 区间里的最小、最大值（画波形时每个像素列一次）
    pub fn range(&self, t0: f64, t1: f64) -> Option<(f32, f32)> {
        let i0 = self.s.partition_point(|x| x.0 < t0);
        let i1 = self.s.partition_point(|x| x.0 <= t1);
        let mut r: Option<(f32, f32)> = self.at(t0).map(|v| (v, v));
        for k in i0..i1 {
            let v = self.s[k].1;
            r = Some(r.map_or((v, v), |(a, b)| (a.min(v), b.max(v))));
        }
        if let Some(v) = self.at(t1) {
            r = Some(r.map_or((v, v), |(a, b)| (a.min(v), b.max(v))));
        }
        r
    }
}

pub struct Sim {
    pub nl: Netlist,
    pub node_of_net: Vec<usize>,
    pub nodes: usize,
    pub devs: Vec<Dev>,
    /// 器件端子 → 所属元件下标、引脚下标
    owner: Vec<(usize, Vec<Option<usize>>)>,
    pub solver: Solver,
    pub ps: Vec<PState>,
    pub t: f64,
    pub dt: f64,
    /// 最近一次求解是否收敛
    pub ok: bool,
    /// 实际速度倍率（跟不上时小于设定）
    pub real_speed: f64,
    pub log: Vec<(f64, u32, String)>,
    pub rebuild: bool,
    /// 按住的按键
    pub pressed: HashSet<u32>,
    /// 串口数据：本帧发出的
    pub uart: Vec<UartTx>,
    /// 给 QCCOM 的数据（仿真 USB 转 TTL 收到的）：元件 id → 字节
    pub host_rx: HashMap<u32, Vec<u8>>,
    /// QCCOM 那边打开仿真串口时的波特率
    pub host_baud: HashMap<u32, u32>,
    pub overlay: HashMap<usize, Overlay>,
    /// 单片机规则产生的效果（显示、联网……），每步处理
    pub effects: Vec<Effect>,
    /// 交给界面层 / 联网线程的请求
    pub net_out: Vec<super::mcu::NetReq>,
    pub photos: Vec<u32>,
    /// 串口模块（WiFi / 蓝牙 / 4G / GPS）的固件状态
    pub at: HashMap<u32, AtDev>,
    /// 模块要做的联网（界面层去连）
    pub at_net: Vec<(u32, NetCmd)>,
    pub burnt_ids: HashSet<u32>,
    /// 超声波 ECHO 脉冲：元件 → (开始, 结束)
    pub pulses: HashMap<u32, (f64, f64)>,
    /// 事件时间点（下一步不跨过它）
    next_ev: f64,
    /// 帧累计时间（求平均用）
    frame_t: f64,
    /// 最近一步各节点电压变化最大值
    last_dv: f64,
    /// 示波器 / 逻辑分析仪通道
    pub traces: Vec<Trace>,
}

/// 编译时用：给一个元件加器件
pub struct Build<'a> {
    pub sim: &'a mut Sim,
    pub pi: usize,
}

impl Build<'_> {
    /// 引脚的节点（第一次用到时分配）
    pub fn n(&mut self, pin: usize) -> usize {
        let pn = self.sim.ps[self.pi].pin_node[pin];
        if pn != NONE {
            return pn;
        }
        let id = self.sim.ps[self.pi].id;
        let net = self.sim.nl.net_of(id, pin).unwrap_or(NONE);
        let node = if net == NONE {
            self.alloc()
        } else if self.sim.node_of_net[net] == NONE {
            let k = self.alloc();
            self.sim.node_of_net[net] = k;
            k
        } else {
            self.sim.node_of_net[net]
        };
        self.sim.ps[self.pi].pin_node[pin] = node;
        node
    }

    /// 引脚所在网络上除了这个元件还有没有别的引脚
    pub fn used(&self, pin: usize) -> bool {
        let id = self.sim.ps[self.pi].id;
        self.sim
            .nl
            .net_of(id, pin)
            .is_some_and(|n| self.sim.nl.members[n].iter().any(|(p, _)| *p != id) || self.sim.node_of_net[n] == 0)
    }

    fn alloc(&mut self) -> usize {
        self.sim.nodes += 1;
        self.sim.nodes - 1
    }

    pub fn inner(&mut self) -> usize {
        let k = self.alloc();
        self.sim.ps[self.pi].inner.push(k);
        k
    }

    /// 加一个器件。terms：器件各端子对应的引脚（None = 内部节点）
    pub fn dev(&mut self, d: Dev, terms: &[Option<usize>]) -> usize {
        self.sim.devs.push(d);
        self.sim.owner.push((self.pi, terms.to_vec()));
        self.sim.devs.len() - 1
    }

    pub fn r(&mut self, a: usize, b: usize, r: f64, ta: Option<usize>, tb: Option<usize>) -> usize {
        self.dev(Dev::R { a, b, r }, &[ta, tb])
    }

    pub fn sw(&mut self, a: usize, b: usize, ron: f64, ta: Option<usize>, tb: Option<usize>) -> usize {
        self.dev(Dev::Sw { a, b, on: false, ron }, &[ta, tb])
    }
}

/// 读写器件的小工具
pub fn set_sw(d: &mut Dev, on_: bool) {
    if let Dev::Sw { on, .. } = d {
        *on = on_;
    }
}

pub fn set_sw_r(d: &mut Dev, on_: bool, r: f64) {
    if let Dev::Sw { on, ron, .. } = d {
        *on = on_;
        *ron = r;
    }
}

pub fn set_r(d: &mut Dev, v: f64) {
    if let Dev::R { r, .. } = d {
        *r = v;
    }
}

pub fn set_v(d: &mut Dev, val: f64) {
    if let Dev::V { v, .. } = d {
        *v = val;
    }
}

pub fn set_i(d: &mut Dev, val: f64) {
    if let Dev::I { i, .. } = d {
        *i = val;
    }
}

/// 节点电压（NONE → 0）
pub fn volt(v: &[f64], n: usize) -> f64 {
    if n == NONE || n >= v.len() {
        0.0
    } else {
        v[n]
    }
}

impl Sim {
    pub fn new(c: &Circuit) -> Sim {
        let mut s = Sim {
            nl: Netlist::default(),
            node_of_net: vec![],
            nodes: 0,
            devs: vec![],
            owner: vec![],
            solver: Solver::new(1),
            ps: vec![],
            t: 0.0,
            dt: 1e-5,
            ok: true,
            real_speed: 1.0,
            log: vec![],
            rebuild: false,
            pressed: HashSet::new(),
            uart: vec![],
            host_rx: HashMap::new(),
            host_baud: HashMap::new(),
            overlay: HashMap::new(),
            effects: vec![],
            net_out: vec![],
            photos: vec![],
            at: HashMap::new(),
            at_net: vec![],
            burnt_ids: HashSet::new(),
            pulses: HashMap::new(),
            next_ev: f64::INFINITY,
            frame_t: 0.0,
            last_dv: 0.0,
            traces: vec![],
        };
        s.compile(c);
        s
    }

    /// 电路结构变了：重新编译，尽量保留电容电压、电感电流、元件状态
    pub fn recompile(&mut self, c: &Circuit) {
        let mut keep: HashMap<u32, (Vec<Dev>, PState)> = HashMap::new();
        for ps in std::mem::take(&mut self.ps) {
            let devs = self.devs[ps.d0..ps.d1].to_vec();
            keep.insert(ps.id, (devs, ps));
        }
        self.compile_with(c, &mut keep);
    }

    fn compile(&mut self, c: &Circuit) {
        self.compile_with(c, &mut HashMap::new());
    }

    fn compile_with(&mut self, c: &Circuit, keep: &mut HashMap<u32, (Vec<Dev>, PState)>) {
        self.nl = Netlist::build(c);
        self.node_of_net = vec![NONE; self.nl.count];
        self.nodes = 1;
        self.devs.clear();
        self.owner.clear();
        self.ps.clear();
        // 地：「地」符号所在网络；没有的话用第一个电源的负极、开发板 GND、USB 转 TTL 的 GND
        let mut gnd_nets: Vec<usize> = c.parts.iter().filter(|p| p.kind == "gnd").filter_map(|p| self.nl.net_of(p.id, 0)).collect();
        if gnd_nets.is_empty() {
            let prio = [
                "usb5v", "battery", "psu", "siggen", "acsrc", "uno", "nano", "esp32", "bluepill", "stc89", "pico", "esp32s3", "esp32c3", "nodemcu", "esp32cam", "usbttl",
            ];
            'o: for k in prio {
                for p in c.parts.iter().filter(|p| p.kind == k) {
                    let d = lib::def(k).unwrap();
                    let pin = d.pins.iter().position(|x| x.name == "-" || x.name == "GND" || x.name == "~2");
                    if let Some(net) = pin.and_then(|i| self.nl.net_of(p.id, i)) {
                        gnd_nets.push(net);
                        break 'o;
                    }
                }
            }
        }
        for n in gnd_nets {
            self.node_of_net[n] = 0;
        }
        for part in &c.parts {
            let n = lib::def(&part.kind).map_or(0, |d| d.pins.len());
            let mut ps = PState {
                id: part.id,
                pin_node: vec![NONE; n],
                i: vec![0.0; n],
                acc_i: vec![0.0; n],
                temp: 25.0,
                ..Default::default()
            };
            if let Some((_, old)) = keep.get_mut(&part.id) {
                ps.burnt = old.burnt.take();
                ps.temp = old.temp;
                ps.beh = std::mem::take(&mut old.beh);
                ps.vis = std::mem::take(&mut old.vis);
            }
            self.ps.push(ps);
        }
        for (pi, part) in c.parts.iter().enumerate() {
            let Some(def) = lib::def(&part.kind) else { continue };
            self.ps[pi].d0 = self.devs.len();
            if self.ps[pi].burnt.is_none() || def.key == "fuse" {
                let mut b = Build { sim: self, pi };
                compile_part(&mut b, part, def);
            }
            self.ps[pi].d1 = self.devs.len();
            // 电容、电感的状态接着用
            if let Some((old, _)) = keep.get(&part.id) {
                let d0 = self.ps[pi].d0;
                if old.len() == self.ps[pi].d1 - d0 {
                    for (k, od) in old.iter().enumerate() {
                        match (&mut self.devs[d0 + k], od) {
                            (Dev::C { v0, .. }, Dev::C { v0: o, .. }) => *v0 = *o,
                            (Dev::L { i0, .. }, Dev::L { i0: o, .. }) => *i0 = *o,
                            _ => {}
                        }
                    }
                }
            }
        }
        let old_v = std::mem::take(&mut self.solver.v);
        self.solver = Solver::new(self.nodes.max(1));
        if old_v.len() == self.nodes {
            self.solver.v = old_v;
        }
        // 示波器探针、逻辑分析仪的通道（重建时保留已经记下的波形）
        let old = std::mem::take(&mut self.traces);
        for part in &c.parts {
            let pins: Vec<usize> = match part.kind.as_str() {
                "probe" => vec![0],
                "logic" => (0..8).collect(),
                _ => continue,
            };
            for pin in pins {
                let net = self.nl.net_of(part.id, pin).unwrap_or(NONE);
                let mut tr = old.iter().find(|x| x.part == part.id && x.pin == pin).cloned().unwrap_or_default();
                tr.part = part.id;
                tr.pin = pin;
                tr.net = net;
                self.traces.push(tr);
            }
        }
        // 串口模块：留着已有的固件状态（连着的 WiFi、TCP）
        self.at.retain(|id, _| c.part(*id).is_some_and(|p| atdev::is_at(&p.kind)));
        for part in c.parts.iter().filter(|p| atdev::is_at(&p.kind)) {
            self.at.entry(part.id).or_insert_with(|| AtDev::new(part)).reload(part);
        }
        self.rebuild = false;
    }

    /// 这个元件的器件
    pub fn devs_of(&self, pi: usize) -> &[Dev] {
        &self.devs[self.ps[pi].d0..self.ps[pi].d1]
    }

    pub fn index_of(&self, id: u32) -> Option<usize> {
        self.ps.iter().position(|p| p.id == id)
    }

    /// 网络电压（没有参与仿真的网络返回 None）
    pub fn net_v(&self, net: usize) -> Option<f64> {
        let n = *self.node_of_net.get(net)?;
        if n == NONE {
            None
        } else {
            Some(volt(&self.solver.v, n))
        }
    }

    pub fn pin_v(&self, id: u32, pin: usize) -> Option<f64> {
        self.nl.net_of(id, pin).and_then(|n| self.net_v(n))
    }

    /// 往前走 wall 秒 × speed（有 CPU 预算）。返回本帧是否有事件（需要重绘）
    pub fn advance(&mut self, c: &Circuit, wall: f64, speed: f64) {
        if self.rebuild {
            self.recompile(c);
        }
        let target = (wall * speed).clamp(0.0, 0.5);
        let t_end = self.t + target;
        let start = Instant::now();
        let budget = Duration::from_millis(10);
        let mut steps = 0;
        let dt_max = (target / 8.0).clamp(1e-6, 2e-3);
        while self.t < t_end - 1e-12 {
            let mut dt = self.dt.min(dt_max).min(t_end - self.t);
            if self.next_ev.is_finite() && self.next_ev > self.t + 1e-12 {
                dt = dt.min(self.next_ev - self.t);
            }
            dt = dt.max(1e-8);
            let at_event = self.next_ev.is_finite() && self.next_ev - self.t <= dt + 1e-15;
            self.step(c, dt);
            // 刚到事件点（开关、PWM 边沿）：下一步用小步长，才能看到开关瞬间的过程（比如电感的反向高压）
            if at_event {
                self.dt = self.dt.min(2e-6);
            }
            steps += 1;
            if start.elapsed() > budget {
                break;
            }
            if self.rebuild {
                self.recompile(c);
            }
        }
        let done = target - (t_end - self.t).max(0.0);
        self.real_speed = if wall > 0.0 { done / wall } else { speed };
        self.end_frame(c);
        self.at_frame(c);
        self.route_serial(c);
    }

    /// 主机（QCCOM 打开的仿真串口）发来数据：开发板走 USB 直接收；USB 转 TTL 从 TXD 发出去
    pub fn host_send(&mut self, c: &Circuit, id: u32, data: &[u8]) {
        let Some(pi) = self.index_of(id) else { return };
        let part = &c.parts[pi];
        match &mut self.ps[pi].beh {
            Beh::Mcu(rt) => {
                if part.prop("power") == "usb" {
                    rt.feed_rx(data);
                }
            }
            Beh::UsbTtl { .. } => {
                let Some(def) = lib::def(&part.kind) else { return };
                let (txd, g) = (def.pin("TXD").unwrap_or(2), def.pin("GND").unwrap_or(0));
                let net = self.nl.net_of(id, txd).unwrap_or(NONE);
                let gnd = self.nl.net_of(id, g).unwrap_or(NONE);
                let baud = self.host_baud.get(&id).copied().unwrap_or(115200);
                self.uart.push(UartTx {
                    from: id,
                    net,
                    gnd,
                    baud,
                    data: data.to_vec(),
                    t: self.t,
                });
                // 和单片机发出的一样补上波形
                self.overlay_bytes(net, baud, data);
            }
            _ => {
                // 蓝牙模块：QCCOM 这边是手机
                let t = self.t;
                if let Some(d) = self.at.get_mut(&id) {
                    d.phone_in(data, t);
                }
            }
        }
    }

    /// 串口发送的波形：空闲高，每字节 起始位 0 + 8 位（低位在前）+ 停止位 1
    fn overlay_bytes(&mut self, net: usize, baud: u32, data: &[u8]) {
        if net == NONE || baud == 0 {
            return;
        }
        let o = self.overlay.entry(net).or_default();
        let bit = 1.0 / baud as f64;
        let mut at = o.edges.last().map_or(self.t, |e| e.0.max(self.t));
        let mut level = true;
        for b in data {
            for v in std::iter::once(false).chain((0..8).map(|k| b >> k & 1 == 1)).chain(std::iter::once(true)) {
                if v != level {
                    o.edges.push((at, v));
                    level = v;
                }
                at += bit;
            }
        }
    }

    /// 串口模块：上电、EN、手机连接，到点的应答从 TX 发出去
    fn at_frame(&mut self, c: &Circuit) {
        let ids: Vec<u32> = self.at.keys().copied().collect();
        for id in ids {
            let Some(part) = c.part(id) else { continue };
            let Some(def) = lib::def(&part.kind) else { continue };
            let (vp, gp) = module_pwr_pins(def);
            let gv = gp.and_then(|g| self.pin_v(id, g)).unwrap_or(0.0);
            let vcc = vp.and_then(|p| self.pin_v(id, p)).unwrap_or(0.0) - gv;
            let (vmin, _, _) = models::module_power(&part.kind);
            let on = vcc > vmin * 0.9 && !self.burnt_ids.contains(&id);
            let en = def.pin("EN").and_then(|p| self.pin_v(id, p)).is_some_and(|v| v - gv > 2.0);
            let phone = self.host_baud.contains_key(&id);
            let t = self.t;
            let d = self.at.get_mut(&id).unwrap();
            d.set_power(on, en, t);
            if d.is_bt() {
                d.set_phone(phone, t);
            }
            let out = d.poll(t);
            let baud = d.baud;
            let to_phone = std::mem::take(&mut d.phone_out);
            for n in std::mem::take(&mut d.net) {
                self.at_net.push((id, n));
            }
            if !to_phone.is_empty() {
                self.host_rx.entry(id).or_default().extend(to_phone);
            }
            if out.is_empty() {
                continue;
            }
            let tx = def.pins.iter().position(|p| matches!(p.name.as_str(), "TX" | "TXD"));
            let net = tx.and_then(|p| self.nl.net_of(id, p)).unwrap_or(NONE);
            let gnd = gp.and_then(|g| self.nl.net_of(id, g)).unwrap_or(NONE);
            self.overlay_bytes(net, baud, &out);
            self.uart.push(UartTx {
                from: id,
                net,
                gnd,
                baud,
                data: out,
                t,
            });
        }
    }

    /// 联网结果交给模块
    pub fn at_event(&mut self, id: u32, e: NetEvt) {
        let t = self.t;
        if let Some(d) = self.at.get_mut(&id) {
            d.net_evt(e, t);
        }
    }

    /// 把这一帧发出的串口数据送到接收方：接在同一网络上的 USB 转 TTL（给 QCCOM）、单片机的 RX。
    /// 波特率不一致收到的是乱码；地没接通收不到
    fn route_serial(&mut self, c: &Circuit) {
        let garble = |d: &[u8], from: u32, to: u32| -> Vec<u8> {
            if from == to || from == 0 || to == 0 {
                return d.to_vec();
            }
            let n = ((d.len() as f64) * to as f64 / from as f64).round().max(1.0) as usize;
            (0..n).map(|k| d[k * d.len() / n].wrapping_mul(37) ^ 0xA5 | 0x80).collect()
        };
        for tx in std::mem::take(&mut self.uart) {
            if tx.net == NONE {
                continue;
            }
            for (pi, part) in c.parts.iter().enumerate() {
                if part.id == tx.from {
                    continue;
                }
                let st = &self.ps[pi];
                if st.burnt.is_some() {
                    continue;
                }
                if let Some(d) = self.at.get_mut(&part.id) {
                    let Some(def) = lib::def(&part.kind) else { continue };
                    let rx = def.pins.iter().position(|p| matches!(p.name.as_str(), "RX" | "RXD"));
                    if rx.is_none() || rx.and_then(|p| self.nl.net_of(part.id, p)) != Some(tx.net) {
                        continue;
                    }
                    let g = module_pwr_pins(def).1;
                    if tx.gnd != NONE && g.and_then(|g| self.nl.net_of(part.id, g)) != Some(tx.gnd) {
                        continue;
                    }
                    d.feed(&garble(&tx.data, tx.baud, d.baud), tx.t);
                    continue;
                }
                match &st.beh {
                    Beh::UsbTtl { .. } => {
                        let Some(def) = lib::def(&part.kind) else { continue };
                        let (rxd, g) = (def.pin("RXD").unwrap_or(1), def.pin("GND").unwrap_or(0));
                        if self.nl.net_of(part.id, rxd) != Some(tx.net) {
                            continue;
                        }
                        if tx.gnd != NONE && self.nl.net_of(part.id, g) != Some(tx.gnd) {
                            continue;
                        }
                        let hb = self.host_baud.get(&part.id).copied().unwrap_or(tx.baud);
                        let d = garble(&tx.data, tx.baud, hb);
                        self.host_rx.entry(part.id).or_default().extend(d);
                    }
                    Beh::Mcu(rt) => {
                        let Some(rxp) = rt.rx_pin() else { continue };
                        if self.nl.net_of(part.id, rxp) != Some(tx.net) {
                            continue;
                        }
                        if tx.gnd != NONE && self.nl.net_of(part.id, rt.gnd_pin()) != Some(tx.gnd) {
                            continue;
                        }
                        let d = garble(&tx.data, tx.baud, rt.cfg.baud.max(1));
                        if let Beh::Mcu(rt) = &mut self.ps[pi].beh {
                            rt.feed_rx(&d);
                        }
                    }
                    _ => {}
                }
            }
        }
        // 开发板 USB 口打印的内容
        for (pi, part) in c.parts.iter().enumerate() {
            if let Beh::Mcu(rt) = &mut self.ps[pi].beh {
                if !rt.host_out.is_empty() {
                    let d = std::mem::take(&mut rt.host_out);
                    let hb = self.host_baud.get(&part.id).copied().unwrap_or(rt.cfg.baud);
                    // USB 虚拟串口：大多数板子（CH340 / CP2102 桥接）波特率要一致；ESP32-S3/C3、Pico 的原生 USB 不在乎
                    let native = matches!(part.kind.as_str(), "esp32s3" | "esp32c3" | "pico");
                    let d = if native { d } else { garble(&d, rt.cfg.baud.max(1), hb) };
                    self.host_rx.entry(part.id).or_default().extend(d);
                }
            }
        }
    }

    fn step(&mut self, c: &Circuit, dt0: f64) {
        let mut dt = dt0;
        let saved_v = self.solver.v.clone();
        // 事件：各元件告诉下一次状态变化的时间
        let mut ps = std::mem::take(&mut self.ps);
        for (pi, st) in ps.iter_mut().enumerate() {
            let part = &c.parts[pi];
            pre_part(self, st, part, self.t, dt);
        }
        self.ps = ps;
        let mut tries = 0;
        loop {
            let ok = self.solver.solve(&self.devs, Mode::Tran(dt));
            let dv = self.solver.v.iter().zip(&saved_v).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
            // 太大的跳变（不是事件引起的）就把步长减半重来
            if (!ok || (dv > 1.5 && dt > 2e-7 && self.last_dv < 0.5)) && tries < 6 {
                tries += 1;
                dt *= 0.25;
                self.solver.v = saved_v.clone();
                continue;
            }
            self.ok = ok;
            self.last_dv = dv;
            break;
        }
        self.solver.commit(&mut self.devs, Mode::Tran(dt));
        let t1 = self.t + dt;
        for k in 0..self.traces.len() {
            let v = self.net_v(self.traces[k].net).unwrap_or(0.0) as f32;
            self.traces[k].push(t1, v);
        }
        // 测量：各元件引脚电流、功率
        self.measure(dt);
        let mut ps = std::mem::take(&mut self.ps);
        self.next_ev = f64::INFINITY;
        for (pi, st) in ps.iter_mut().enumerate() {
            let part = &c.parts[pi];
            post_part(self, st, c, part, self.t, dt);
        }
        self.ps = ps;
        self.apply_effects();
        self.t += dt;
        self.frame_t += dt;
        // 下一步的步长：变化小就放大，变化大就缩小
        self.dt = if self.last_dv < 0.02 {
            (dt * 2.0).min(2e-3)
        } else if self.last_dv < 0.2 {
            (dt * 1.3).min(2e-3)
        } else {
            (dt * 0.5).max(1e-7)
        };
        // 元件要求的事件点
        for (a, b) in self.pulses.values() {
            for e in [*a, *b] {
                if e > self.t + 1e-12 && e < self.next_ev {
                    self.next_ev = e;
                }
            }
        }
        for (pi, st) in self.ps.iter().enumerate() {
            let e = next_event(st, &c.parts[pi], self.t);
            if e < self.next_ev {
                self.next_ev = e;
            }
        }
    }

    fn measure(&mut self, dt: f64) {
        let mode = Mode::Tran(dt);
        let v = &self.solver.v;
        for (k, d) in self.devs.iter().enumerate() {
            let (pi, terms) = &self.owner[k];
            let cur = term_currents(&self.solver, d, mode);
            let nodes = d.nodes();
            let st = &mut self.ps[*pi];
            for (j, t) in terms.iter().enumerate() {
                if j >= cur.len() {
                    break;
                }
                // 功率：所有端子 V·I 之和（内部节点也算，总和就是这个元件吸收的功率）
                st.acc_p += volt(v, nodes[j]) * cur[j] * dt;
                if let Some(pin) = t {
                    if *pin < st.acc_i.len() {
                        st.acc_i[*pin] += cur[j] * dt;
                    }
                }
            }
        }
        for st in self.ps.iter_mut() {
            st.acc_t += dt;
        }
    }

    /// 帧末：算平均值、更新显示状态
    fn end_frame(&mut self, c: &Circuit) {
        let mut ps = std::mem::take(&mut self.ps);
        for (pi, st) in ps.iter_mut().enumerate() {
            if st.acc_t > 0.0 {
                for k in 0..st.i.len() {
                    st.i[k] = st.acc_i[k] / st.acc_t;
                    st.acc_i[k] = 0.0;
                }
                st.p = st.acc_p / st.acc_t;
                st.acc_p = 0.0;
                st.acc_t = 0.0;
            }
            frame_vis(self, st, &c.parts[pi]);
        }
        self.ps = ps;
        self.frame_t = 0.0;
        self.burnt_ids = self.ps.iter().filter(|p| p.burnt.is_some()).map(|p| p.id).collect();
        // 只保留最近的边沿
        let horizon = self.t - 2.0;
        for o in self.overlay.values_mut() {
            if o.edges.len() > 4000 {
                let cut = o.edges.iter().position(|e| e.0 > horizon).unwrap_or(o.edges.len());
                o.edges.drain(..cut.min(o.edges.len().saturating_sub(4000)).max(cut.min(o.edges.len())));
            }
        }
    }

    fn apply_effects(&mut self) {
        for e in std::mem::take(&mut self.effects) {
            match e {
                Effect::Lines(id, l) => {
                    if let Some(st) = self.ps.iter_mut().find(|p| p.id == id) {
                        st.vis.lines = l;
                        st.vis.image.clear();
                    }
                }
                Effect::Image(id, img) => {
                    if let Some(st) = self.ps.iter_mut().find(|p| p.id == id) {
                        st.vis.image = img;
                    }
                }
                Effect::Digits(id, d) => {
                    if let Some(st) = self.ps.iter_mut().find(|p| p.id == id) {
                        st.vis.digits = d;
                    }
                }
                Effect::Pulse(id, dur) => {
                    // 模块收到触发后约 0.5 ms 开始回波
                    self.pulses.insert(id, (self.t + 0.5e-3, self.t + 0.5e-3 + dur));
                    self.next_ev = self.next_ev.min(self.t + 0.5e-3);
                }
                Effect::Net(r) => self.net_out.push(r),
                Effect::Photo(id) => self.photos.push(id),
            }
        }
    }

    /// 修好烧坏的元件
    pub fn repair(&mut self, id: u32) {
        if let Some(st) = self.ps.iter_mut().find(|p| p.id == id) {
            st.burnt = None;
            st.stress = 0.0;
            st.temp = 25.0;
            if let Beh::Fuse { blown } = &mut st.beh {
                *blown = false;
            }
        }
        self.rebuild = true;
    }

    pub fn burn(&mut self, pi: usize, why: String) {
        burn(&mut self.ps[pi], self.t, &mut self.log, why);
        self.rebuild = true;
    }

    /// 两点之间的电阻（断电量电阻 / 通断档）：所有电源置零，从 a 注入 1 mA 看电压
    pub fn measure_r(c: &Circuit, a: (u32, usize), b: (u32, usize)) -> Option<f64> {
        let mut s = Sim::new(c);
        let (na, nb) = (s.nl.net_of(a.0, a.1)?, s.nl.net_of(b.0, b.1)?);
        if na == nb {
            return Some(0.0);
        }
        let node = |s: &mut Sim, net: usize| {
            if s.node_of_net[net] == NONE {
                s.nodes += 1;
                s.node_of_net[net] = s.nodes - 1;
            }
            s.node_of_net[net]
        };
        let ka = node(&mut s, na);
        let kb = node(&mut s, nb);
        let mut devs: Vec<Dev> = s
            .devs
            .iter()
            .filter_map(|d| match d {
                Dev::V { p, n, r, .. } => Some(Dev::R { a: *p, b: *n, r: r.max(1e-3) }),
                Dev::I { .. } => None,
                Dev::N { m: Nl::Psu { r, .. }, pins } => Some(Dev::R { a: pins[0], b: pins[1], r: *r }),
                Dev::N { m: Nl::Reg { .. }, .. } => None,
                Dev::C { .. } => None,
                other => Some(other.clone()),
            })
            .collect();
        devs.push(Dev::I { a: kb, b: ka, i: 1e-3 });
        // 两点都不接地时把 b 接地
        if kb != 0 {
            devs.push(Dev::R { a: kb, b: 0, r: 1e-3 });
        }
        let mut sol = Solver::new(s.nodes);
        if !sol.solve(&devs, Mode::Dc) {
            return None;
        }
        let r = (sol.v[ka] - sol.v[kb]) / 1e-3;
        Some(if r > 1e8 { f64::INFINITY } else { r.max(0.0) })
    }
}

fn burn(st: &mut PState, t: f64, log: &mut Vec<(f64, u32, String)>, why: String) {
    if st.burnt.is_none() {
        log.push((t, st.id, why.clone()));
        st.burnt = Some(why);
    }
}

/// 器件各端子流入的电流
fn term_currents(s: &Solver, d: &Dev, mode: Mode) -> Vec<f64> {
    let c = s.current(d, mode);
    match d {
        Dev::V { .. } => vec![-c[0], c[0]],
        Dev::N { .. } => c,
        _ => vec![c[0], -c[0]],
    }
}

fn diode_dev(b: &mut Build, a: usize, k: usize, m: DiodeM, ta: Option<usize>, tk: Option<usize>) -> usize {
    b.dev(Dev::N { pins: vec![a, k], m: Nl::Diode(m) }, &[ta, tk])
}

/// 把一个元件编译成器件
fn compile_part(b: &mut Build, part: &Part, def: &PartDef) {
    let k = def.key;
    let pin = |name: &str| def.pin(name).unwrap_or(0);
    match k {
        "bb_half" | "bb_full" | "gnd" | "probe" | "logic" | "antenna" | "simslot" | "mipi_lcd" | "mipi_cam" | "lan8720" | "stlink" => {}
        "usb5v" => {
            let (p, n) = (b.n(0), b.n(1));
            let v = part.num("v");
            let ilim = part.num("imax").max(0.01) * 1.6;
            b.dev(
                Dev::N {
                    pins: vec![p, n],
                    m: Nl::Psu { v, r: 0.05, ilim },
                },
                &[Some(0), Some(1)],
            );
        }
        "psu" => {
            let (p, n) = (b.n(0), b.n(1));
            b.dev(
                Dev::N {
                    pins: vec![p, n],
                    m: Nl::Psu {
                        v: part.num("v"),
                        r: 0.01,
                        ilim: part.num("ilim").max(0.001),
                    },
                },
                &[Some(0), Some(1)],
            );
        }
        "battery" => {
            let (p, n) = (b.n(0), b.n(1));
            let spec = models::battery(part.prop("type"));
            let soc = part.num("soc") / 100.0;
            let v = models::battery_ocv(&spec, soc);
            b.dev(Dev::V { p, n, v, r: spec.r }, &[Some(0), Some(1)]);
            let st = &mut b.sim.ps[b.pi];
            if !matches!(st.beh, Beh::Battery { .. }) {
                st.beh = Beh::Battery { soc, spec, used_mah: 0.0 };
            }
        }
        "acsrc" => {
            let (p, n) = (b.n(0), b.n(1));
            b.dev(Dev::V { p, n, v: 0.0, r: 0.5 }, &[Some(0), Some(1)]);
            b.sim.ps[b.pi].beh = Beh::Ac {
                amp: part.num("amp"),
                freq: part.num("freq").max(0.01),
            };
        }
        "siggen" => {
            let (p, n) = (b.n(0), b.n(1));
            b.dev(Dev::V { p, n, v: 0.0, r: 50.0 }, &[Some(0), Some(1)]);
            b.sim.ps[b.pi].beh = Beh::Square {
                amp: part.num("amp"),
                freq: part.num("freq").max(0.01),
                duty: part.num("duty") / 100.0,
            };
        }
        "ldo" => {
            let s = models::ldo(part.prop("model"));
            let (g, o, i) = (b.n(0), b.n(1), b.n(2));
            b.dev(
                Dev::N {
                    pins: vec![i, o, g],
                    m: Nl::Reg {
                        vset: s.vout,
                        drop: s.drop,
                        ilim: s.ilim,
                        iq: s.iq,
                        eff: 0.0,
                        rout: 0.05,
                        on: true,
                    },
                },
                &[Some(2), Some(1), Some(0)],
            );
        }
        "buck" | "boost" => {
            let (ip, im, om, op) = (b.n(0), b.n(1), b.n(2), b.n(3));
            b.r(im, om, 0.001, Some(1), Some(2));
            let vset = part.num("vout");
            let (drop, eff, iq) = if k == "boost" {
                (-100.0, 0.88, 0.2e-3)
            } else {
                (0.6, 0.9, if part.prop("model") == "MP1584" { 0.1e-3 } else { 1.5e-3 })
            };
            b.dev(
                Dev::N {
                    pins: vec![ip, op, im],
                    m: Nl::Reg {
                        vset,
                        drop,
                        ilim: 2.5,
                        iq,
                        eff,
                        rout: 0.03,
                        on: true,
                    },
                },
                &[Some(0), Some(3), Some(1)],
            );
        }
        "tp4056" => {
            let (ip, im, bp, bm, opp, om) = (b.n(0), b.n(1), b.n(2), b.n(3), b.n(4), b.n(5));
            b.r(im, bm, 0.001, Some(1), Some(3));
            b.r(bm, om, 0.05, Some(3), Some(5));
            b.r(bp, opp, 0.001, Some(2), Some(4));
            let ichg = part.num("ichg") / 1000.0;
            b.dev(
                Dev::N {
                    pins: vec![ip, bp, im],
                    m: Nl::Reg {
                        vset: 4.2,
                        drop: 0.3,
                        ilim: ichg,
                        iq: 0.15e-3,
                        eff: 0.0,
                        rout: 0.02,
                        on: true,
                    },
                },
                &[Some(0), Some(2), Some(1)],
            );
        }
        "bms" => {
            let (bp, bm, pp, pm) = (b.n(0), b.n(1), b.n(2), b.n(3));
            b.r(bp, pp, 0.001, Some(0), Some(2));
            let s = b.sw(bm, pm, 0.05, Some(1), Some(3));
            set_sw(&mut b.sim.devs[s], true);
            b.dev(Dev::I { a: bp, b: bm, i: 3e-6 }, &[Some(0), Some(1)]);
        }
        "res" => {
            let (a, c) = (b.n(0), b.n(1));
            b.r(a, c, part.num("r").max(1e-3), Some(0), Some(1));
        }
        "pot" => {
            let (a, w, c) = (b.n(0), b.n(1), b.n(2));
            let r = part.num("r").max(1.0);
            let x = (part.num("pos") / 100.0).clamp(0.0, 1.0);
            b.r(a, w, (r * x).max(0.5), Some(0), Some(1));
            b.r(w, c, (r * (1.0 - x)).max(0.5), Some(1), Some(2));
        }
        "cap" | "ecap" => {
            let (a, c) = (b.n(0), b.n(1));
            // 加一点 ESR，避免和理想电源直接并联时电流无限大
            let m = b.inner();
            b.r(a, m, if k == "ecap" { 0.1 } else { 0.01 }, Some(0), None);
            b.dev(
                Dev::C {
                    a: m,
                    b: c,
                    c: part.num("c").max(1e-15),
                    v0: 0.0,
                },
                &[None, Some(1)],
            );
            if k == "ecap" {
                // 漏电流
                b.r(a, c, 1e7, Some(0), Some(1));
            }
        }
        "ind" => {
            let (a, c) = (b.n(0), b.n(1));
            b.dev(
                Dev::L {
                    a,
                    b: c,
                    l: part.num("l").max(1e-12),
                    r: part.num("dcr").max(1e-4),
                    i0: 0.0,
                },
                &[Some(0), Some(1)],
            );
        }
        "bead" => {
            let (a, c) = (b.n(0), b.n(1));
            b.dev(Dev::L { a, b: c, l: 1e-6, r: 0.1, i0: 0.0 }, &[Some(0), Some(1)]);
        }
        "fuse" => {
            let (a, c) = (b.n(0), b.n(1));
            let blown = matches!(b.sim.ps[b.pi].beh, Beh::Fuse { blown: true }) || b.sim.ps[b.pi].burnt.is_some();
            b.r(a, c, if blown { ROFF } else { 0.05 }, Some(0), Some(1));
            if !matches!(b.sim.ps[b.pi].beh, Beh::Fuse { .. }) {
                b.sim.ps[b.pi].beh = Beh::Fuse { blown: false };
            }
        }
        "diode" => {
            let (a, c) = (b.n(0), b.n(1));
            diode_dev(b, a, c, models::diode(part.prop("model")).m, Some(0), Some(1));
        }
        "zener" => {
            let (a, c) = (b.n(0), b.n(1));
            diode_dev(b, a, c, models::zener(part.prop("model")).m, Some(0), Some(1));
        }
        "led" => {
            let (a, c) = (b.n(0), b.n(1));
            diode_dev(b, a, c, models::led(part.prop("color")).0, Some(0), Some(1));
        }
        "rgbled" => {
            let kk = b.n(1);
            for (pin, col) in [(0, "red"), (2, "green"), (3, "blue")] {
                let a = b.n(pin);
                diode_dev(b, a, kk, models::led(col).0, Some(pin), Some(1));
            }
        }
        "bridge" => {
            let (p, a1, a2, m) = (b.n(0), b.n(1), b.n(2), b.n(3));
            let d = models::diode("1N4007").m;
            diode_dev(b, a1, p, d, Some(1), Some(0));
            diode_dev(b, a2, p, d, Some(2), Some(0));
            diode_dev(b, m, a1, d, Some(3), Some(1));
            diode_dev(b, m, a2, d, Some(3), Some(2));
        }
        "npn" | "pnp" => {
            let (e, bb, c) = (b.n(0), b.n(1), b.n(2));
            let s = models::bjt(part.prop("model"), k == "pnp");
            if s.darlington {
                // 达林顿：两个管子 + 内部电阻
                let mid = b.inner();
                b.dev(
                    Dev::N {
                        pins: vec![c, bb, mid],
                        m: Nl::Bjt(s.m),
                    },
                    &[Some(2), Some(1), None],
                );
                b.dev(
                    Dev::N {
                        pins: vec![c, mid, e],
                        m: Nl::Bjt(s.m),
                    },
                    &[Some(2), None, Some(0)],
                );
                b.r(bb, mid, 8000.0, Some(1), None);
                b.r(mid, e, 120.0, None, Some(0));
                // 内置续流二极管 E→C
                diode_dev(b, e, c, models::diode("1N4007").m, Some(0), Some(2));
            } else {
                b.dev(
                    Dev::N {
                        pins: vec![c, bb, e],
                        m: Nl::Bjt(s.m),
                    },
                    &[Some(2), Some(1), Some(0)],
                );
            }
        }
        "nmos" | "pmos" => {
            let (g, d, s) = (b.n(0), b.n(1), b.n(2));
            let spec = models::mos(part.prop("model"));
            b.dev(
                Dev::N {
                    pins: vec![d, g, s],
                    m: Nl::Mos(spec.m),
                },
                &[Some(1), Some(0), Some(2)],
            );
            // 体二极管
            if k == "nmos" {
                diode_dev(b, s, d, models::diode("1N4148").m, Some(2), Some(1));
            } else {
                diode_dev(b, d, s, models::diode("1N4148").m, Some(1), Some(2));
            }
            // 栅极泄漏（悬空时的栅极不确定）
            b.r(g, s, 1e9, Some(0), Some(2));
        }
        "button" => {
            let (a, c) = (b.n(0), b.n(2));
            b.sw(a, c, 0.05, Some(0), Some(2));
        }
        "slide" => {
            let (p1, com, p3) = (b.n(0), b.n(1), b.n(2));
            b.sw(com, p1, 0.05, Some(1), Some(0));
            b.sw(com, p3, 0.05, Some(1), Some(2));
        }
        "dipsw" => {
            for i in 0..4 {
                let (a, c) = (b.n(i), b.n(4 + (3 - i)));
                b.sw(a, c, 0.1, Some(i), Some(4 + (3 - i)));
            }
        }
        "ldr" | "ntc" => {
            let (a, c) = (b.n(0), b.n(1));
            b.r(a, c, 10e3, Some(0), Some(1));
        }
        "encoder" => {
            // A、B、SW 对 GND 的开关；模块上有 10k 上拉到 +
            let (g, vp, sw, dt, clk) = (b.n(0), b.n(1), b.n(2), b.n(3), b.n(4));
            for (pin, node) in [(2, sw), (3, dt), (4, clk)] {
                b.sw(node, g, 50.0, Some(pin), Some(0));
                b.r(vp, node, 10e3, Some(1), Some(pin));
            }
            if !matches!(b.sim.ps[b.pi].beh, Beh::Encoder { .. }) {
                b.sim.ps[b.pi].beh = Beh::Encoder { last: part.num("count") as i64 };
            }
        }
        "buzzer" => {
            // 有源蜂鸣器：额定电压下约 30 mA
            let (a, c) = (b.n(0), b.n(1));
            let vr = part.num("v");
            let m = b.inner();
            diode_dev(b, a, m, DiodeM::from_vf(0.7, 0.03, 2.0, 1.0), Some(0), None);
            b.r(m, c, ((vr - 0.7) / 0.03).max(10.0), None, Some(1));
        }
        "pbuzzer" => {
            let (a, c) = (b.n(0), b.n(1));
            b.dev(Dev::L { a, b: c, l: 2e-3, r: 16.0, i0: 0.0 }, &[Some(0), Some(1)]);
            b.sim.ps[b.pi].beh = Beh::Buzzer {
                last_cross: 0.0,
                sign: false,
                period: 0.0,
            };
        }
        "relay" => {
            let (l1, l2, com, no, nc) = (b.n(0), b.n(1), b.n(2), b.n(3), b.n(4));
            let vr = part.num("v");
            let rc = match vr as i32 {
                3 => 25.0,
                12 => 400.0,
                _ => 70.0,
            };
            b.dev(
                Dev::L {
                    a: l1,
                    b: l2,
                    l: 0.15,
                    r: rc,
                    i0: 0.0,
                },
                &[Some(0), Some(1)],
            );
            b.sw(com, no, 0.05, Some(2), Some(3));
            b.sw(com, nc, 0.05, Some(2), Some(4));
            if !matches!(b.sim.ps[b.pi].beh, Beh::Relay { .. }) {
                b.sim.ps[b.pi].beh = Beh::Relay { closed: false, inom: vr / rc };
            }
        }
        "motor" => {
            let (a, c) = (b.n(0), b.n(1));
            let vr = part.num("v");
            let m = b.inner();
            let rm = (vr / 0.6).clamp(2.0, 20.0);
            b.dev(Dev::L { a, b: m, l: 1e-3, r: rm, i0: 0.0 }, &[Some(0), None]);
            b.dev(Dev::V { p: m, n: c, v: 0.0, r: 0.0 }, &[None, Some(1)]);
            if !matches!(b.sim.ps[b.pi].beh, Beh::Motor { .. }) {
                // 空载转速约 额定电压 × 2000 rpm/V
                b.sim.ps[b.pi].beh = Beh::Motor {
                    omega: 0.0,
                    angle: 0.0,
                    kv: 1.0 / (vr * 0.85 / (vr * 2000.0 * std::f64::consts::TAU / 60.0)),
                };
            }
        }
        "servo" => {
            let (g, v, sig) = (b.n(0), b.n(1), b.n(2));
            b.dev(Dev::I { a: v, b: g, i: 0.006 }, &[Some(1), Some(0)]);
            b.r(sig, g, 50e3, Some(2), Some(0));
            if !matches!(b.sim.ps[b.pi].beh, Beh::Servo { .. }) {
                b.sim.ps[b.pi].beh = Beh::Servo {
                    angle: 90.0,
                    target: 90.0,
                    rise: None,
                    last_pulse: -1.0,
                };
            }
        }
        "seg7" => {
            let ca = part.prop("common") == "ca";
            let com = b.n(def.pin("COM").unwrap());
            let (m, _) = models::led("red");
            for name in ["a", "b", "c", "d", "e", "f", "g", "dp"] {
                let pi = pin(name);
                let s = b.n(pi);
                if ca {
                    diode_dev(b, com, s, m, Some(2), Some(pi));
                } else {
                    diode_dev(b, s, com, m, Some(pi), Some(2));
                }
            }
        }
        "lamp" => {
            let (a, c) = (b.n(0), b.n(1));
            let vr = part.num("v");
            b.r(a, c, vr / 0.1, Some(0), Some(1));
        }
        "ne555" => {
            let (gnd, trig, out, rst, ctrl, thr, dis, vcc) = (b.n(0), b.n(1), b.n(2), b.n(3), b.n(4), b.n(5), b.n(6), b.n(7));
            // 内部 3 个 5k 分压
            let lo = b.inner();
            b.r(vcc, ctrl, 5e3, Some(7), Some(4));
            b.r(ctrl, lo, 5e3, Some(4), None);
            b.r(lo, gnd, 5e3, None, Some(0));
            b.sw(vcc, out, 15.0, Some(7), Some(2));
            b.sw(out, gnd, 10.0, Some(2), Some(0));
            b.sw(dis, gnd, 10.0, Some(6), Some(0));
            b.dev(Dev::I { a: vcc, b: gnd, i: 3e-3 }, &[Some(7), Some(0)]);
            b.r(trig, gnd, 1e8, Some(1), Some(0));
            b.r(thr, gnd, 1e8, Some(5), Some(0));
            b.r(rst, vcc, 1e6, Some(3), Some(7));
            if !matches!(b.sim.ps[b.pi].beh, Beh::Timer555 { .. }) {
                b.sim.ps[b.pi].beh = Beh::Timer555 { q: false };
            }
        }
        "pc817" => {
            let (a, kk, e, c) = (b.n(0), b.n(1), b.n(2), b.n(3));
            let (led, _) = models::led("red");
            let led = DiodeM {
                rs: 3.0,
                ..DiodeM::from_vf(1.2, 0.02, 1.9, 3.0)
            };
            let _ = led;
            b.dev(
                Dev::N {
                    pins: vec![a, kk, c, e],
                    m: Nl::Opto {
                        led: DiodeM::from_vf(1.2, 0.02, 1.9, 3.0),
                        ctr: 1.0,
                    },
                },
                &[Some(0), Some(1), Some(3), Some(2)],
            );
        }
        "lm358" => {
            let (o1, n1, p1, g, p2, n2, o2, vcc) = (b.n(0), b.n(1), b.n(2), b.n(3), b.n(4), b.n(5), b.n(6), b.n(7));
            b.dev(
                Dev::N {
                    pins: vec![p1, n1, o1, vcc, g],
                    m: Nl::OpAmp { gain: 1e5, rout: 20.0 },
                },
                &[Some(2), Some(1), Some(0), Some(7), Some(3)],
            );
            b.dev(
                Dev::N {
                    pins: vec![p2, n2, o2, vcc, g],
                    m: Nl::OpAmp { gain: 1e5, rout: 20.0 },
                },
                &[Some(4), Some(5), Some(6), Some(7), Some(3)],
            );
            b.dev(Dev::I { a: vcc, b: g, i: 0.7e-3 }, &[Some(7), Some(3)]);
        }
        "hc595" => {
            let vcc = b.n(pin("VCC"));
            let g = b.n(pin("GND"));
            for name in ["QA", "QB", "QC", "QD", "QE", "QF", "QG", "QH", "QH'"] {
                let pi = pin(name);
                let o = b.n(pi);
                b.sw(vcc, o, 50.0, Some(pin("VCC")), Some(pi));
                b.sw(o, g, 40.0, Some(pi), Some(pin("GND")));
            }
            for name in ["SER", "SRCLK", "RCLK", "SRCLR", "OE"] {
                let pi = pin(name);
                let n = b.n(pi);
                b.r(n, g, 1e8, Some(pi), Some(pin("GND")));
            }
            b.dev(Dev::I { a: vcc, b: g, i: 80e-6 }, &[Some(pin("VCC")), Some(pin("GND"))]);
            if !matches!(b.sim.ps[b.pi].beh, Beh::Hc595 { .. }) {
                b.sim.ps[b.pi].beh = Beh::Hc595 {
                    sr: 0,
                    out: 0,
                    clk: false,
                    rclk: false,
                };
            }
        }
        "uln2003" => {
            let g = b.n(pin("GND"));
            let com = b.n(pin("COM"));
            for ch in 1..=7 {
                let ib = pin(&format!("{ch}B"));
                let ic = pin(&format!("{ch}C"));
                let (bn, cn) = (b.n(ib), b.n(ic));
                let m = b.inner();
                b.r(bn, m, 2700.0, Some(ib), None);
                diode_dev(b, m, g, DiodeM::from_vf(1.4, 1e-3, 3.8, 10.0), None, Some(pin("GND")));
                // 输出：达林顿到地（导通约 0.9V），续流二极管到 COM
                let mid = b.inner();
                diode_dev(b, cn, mid, DiodeM::from_vf(0.8, 0.3, 2.0, 0.5), Some(ic), None);
                b.sw(mid, g, 0.5, None, Some(pin("GND")));
                diode_dev(b, cn, com, models::diode("1N4007").m, Some(ic), Some(pin("COM")));
            }
        }
        "tb6612" | "l298n" => {
            let (vm, g) = if k == "tb6612" { (b.n(pin("VM")), b.n(pin("GND"))) } else { (b.n(pin("12V")), b.n(pin("GND"))) };
            let (vmp, gp) = if k == "tb6612" { (pin("VM"), pin("GND")) } else { (pin("12V"), pin("GND")) };
            let ron = if k == "tb6612" { 0.5 } else { 1.5 };
            let outs: &[&str] = if k == "tb6612" { &["AO1", "AO2", "BO1", "BO2"] } else { &["OUT1", "OUT2", "OUT3", "OUT4"] };
            for name in outs {
                let pi = pin(name);
                let o = b.n(pi);
                b.sw(vm, o, ron, Some(vmp), Some(pi));
                b.sw(o, g, ron, Some(pi), Some(gp));
                diode_dev(b, o, vm, models::diode("1N4148").m, Some(pi), Some(vmp));
                diode_dev(b, g, o, models::diode("1N4148").m, Some(gp), Some(pi));
            }
            let ins: &[&str] = if k == "tb6612" {
                &["PWMA", "AIN1", "AIN2", "STBY", "BIN1", "BIN2", "PWMB"]
            } else {
                &["ENA", "IN1", "IN2", "IN3", "IN4", "ENB"]
            };
            for name in ins {
                let pi = pin(name);
                let n = b.n(pi);
                b.r(n, g, if k == "l298n" && name.starts_with("EN") { 1e8 } else { 200e3 }, Some(pi), Some(gp));
            }
            if k == "l298n" {
                // 板载 78M05：12V 进，5V 出
                let v5 = b.n(pin("5V"));
                b.dev(
                    Dev::N {
                        pins: vec![vm, v5, g],
                        m: Nl::Reg {
                            vset: 5.0,
                            drop: 2.0,
                            ilim: 0.5,
                            iq: 5e-3,
                            eff: 0.0,
                            rout: 0.1,
                            on: true,
                        },
                    },
                    &[Some(vmp), Some(pin("5V")), Some(gp)],
                );
                b.dev(Dev::I { a: v5, b: g, i: 0.036 }, &[Some(pin("5V")), Some(gp)]);
            } else {
                let vcc = b.n(pin("VCC"));
                b.dev(Dev::I { a: vcc, b: g, i: 1e-3 }, &[Some(pin("VCC")), Some(gp)]);
            }
        }
        "meter" => {
            let (com, v) = (b.n(0), b.n(1));
            let r = match part.prop("mode") {
                "a" => 0.05,
                "v" => 10e6,
                _ => 1e12,
            };
            b.r(v, com, r, Some(1), Some(0));
        }
        "ammeter" => {
            let (a, c) = (b.n(0), b.n(1));
            b.r(a, c, 0.01, Some(0), Some(1));
        }
        "pmeter" => {
            let (a, c) = (b.n(0), b.n(1));
            b.r(a, c, 0.01, Some(0), Some(1));
        }
        "usbttl" => {
            let (g, rxd, txd, v33, v5) = (b.n(0), b.n(1), b.n(2), b.n(3), b.n(4));
            b.dev(
                Dev::N {
                    pins: vec![v5, g],
                    m: Nl::Psu { v: 5.0, r: 0.1, ilim: 0.5 },
                },
                &[Some(4), Some(0)],
            );
            b.dev(
                Dev::N {
                    pins: vec![v5, v33, g],
                    m: Nl::Reg {
                        vset: 3.3,
                        drop: 0.3,
                        ilim: 0.15,
                        iq: 50e-6,
                        eff: 0.0,
                        rout: 0.1,
                        on: true,
                    },
                },
                &[Some(4), Some(3), Some(0)],
            );
            // TXD：空闲高电平（按跳线选择的电平），RXD 有 10k 上拉
            let lvl = if part.prop("level") == "5" { v5 } else { v33 };
            let lp = if part.prop("level") == "5" { 4 } else { 3 };
            let s = b.sw(lvl, txd, 100.0, Some(lp), Some(2));
            set_sw(&mut b.sim.devs[s], true);
            b.r(lvl, rxd, 10e3, Some(lp), Some(1));
            // 电源灯
            b.r(v5, g, 1500.0, Some(4), Some(0));
            if !matches!(b.sim.ps[b.pi].beh, Beh::UsbTtl { .. }) {
                b.sim.ps[b.pi].beh = Beh::UsbTtl { tx: vec![], busy_until: 0.0 };
            }
        }
        "espprog" => {
            let (en, vdd, txd, g, rxd, io0) = (b.n(0), b.n(1), b.n(2), b.n(3), b.n(4), b.n(5));
            b.dev(
                Dev::N {
                    pins: vec![vdd, g],
                    m: Nl::Psu { v: 3.3, r: 0.1, ilim: 0.5 },
                },
                &[Some(1), Some(3)],
            );
            let s = b.sw(vdd, txd, 100.0, Some(1), Some(2));
            set_sw(&mut b.sim.devs[s], true);
            b.r(vdd, rxd, 10e3, Some(1), Some(4));
            b.r(vdd, en, 10e3, Some(1), Some(0));
            b.r(vdd, io0, 10e3, Some(1), Some(5));
        }
        "uno" | "nano" | "stc89" | "bluepill" | "esp32" | "esp32s3" | "esp32c3" | "esp32cam" | "nodemcu" | "pico" => {
            let mut rt = match std::mem::take(&mut b.sim.ps[b.pi].beh) {
                Beh::Mcu(m) => m,
                _ => Box::new(McuRt::new(part, def)),
            };
            rt.reload(part);
            rt.compile(b, part, def);
            b.sim.ps[b.pi].beh = Beh::Mcu(rt);
        }
        _ => {
            // 其它模块：VCC → GND 的工作电流；继电器模块、电流检测模块单独处理
            compile_module(b, part, def);
        }
    }
}

/// 模块的电源脚
pub fn module_pwr_pins(def: &PartDef) -> (Option<usize>, Option<usize>) {
    let vcc = def.pins.iter().position(|p| matches!(p.name.as_str(), "VCC" | "3V3" | "VBAT" | "+" | "VDD" | "VM"));
    let gnd = def.pins.iter().position(|p| p.name == "GND" || p.name == "-");
    (vcc, gnd)
}

fn compile_module(b: &mut Build, part: &Part, def: &PartDef) {
    let (vcc, gnd) = module_pwr_pins(def);
    let (Some(vp), Some(gp)) = (vcc, gnd) else { return };
    let (v, g) = (b.n(vp), b.n(gp));
    let (_, _, irun) = models::module_power(def.key);
    // 工作电流随电压：低于最低电压时按电阻性减小
    b.dev(
        Dev::N {
            pins: vec![v, g],
            m: Nl::Vccs { g: irun / 0.5, imax: irun },
        },
        &[Some(vp), Some(gp)],
    );
    // Vccs 的控制端就是电源本身：电流 = g·V，限到 imax（电压够了就是恒流）
    let last = b.sim.devs.len() - 1;
    if let Dev::N { pins, .. } = &mut b.sim.devs[last] {
        *pins = vec![v, g, v, g];
    }
    b.sim.owner.last_mut().unwrap().1 = vec![Some(vp), Some(gp), None, None];
    // 突发电流（发射）
    if matches!(def.key, "cat1" | "esp01" | "nbiot" | "lora") {
        b.dev(Dev::I { a: v, b: g, i: 0.0 }, &[Some(vp), Some(gp)]);
        if !matches!(b.sim.ps[b.pi].beh, Beh::Load { .. }) {
            b.sim.ps[b.pi].beh = Beh::Load { phase: 0.0 };
        }
    }
    match def.key {
        "relay_mod" => {
            // IN 经光耦 / 三极管驱动线圈；这里直接按 IN 电平开关触点，IN 输入电流约 1~2 mA
            let inp = def.pin("IN").unwrap();
            let n = b.n(inp);
            if part.prop("trig") == "low" {
                b.r(v, n, 3000.0, Some(vp), Some(inp));
            } else {
                b.r(n, g, 3000.0, Some(inp), Some(gp));
            }
            let (com, no, nc) = (b.n(def.pin("COM").unwrap()), b.n(def.pin("NO").unwrap()), b.n(def.pin("NC").unwrap()));
            b.sw(com, no, 0.05, def.pin("COM"), def.pin("NO"));
            b.sw(com, nc, 0.05, def.pin("COM"), def.pin("NC"));
            if !matches!(b.sim.ps[b.pi].beh, Beh::Relay { .. }) {
                b.sim.ps[b.pi].beh = Beh::Relay { closed: false, inom: 0.07 };
            }
        }
        "ina219" | "ina226" => {
            let (a, c) = (b.n(def.pin("VIN+").unwrap()), b.n(def.pin("VIN-").unwrap()));
            b.r(a, c, 0.1, def.pin("VIN+"), def.pin("VIN-"));
        }
        "oled" | "lcd1602" | "sht30" => {
            // 板载 I2C 上拉（可在属性里关掉，用来演示忘接上拉）
            if part.prop("pullup") != "false" {
                for name in ["SCL", "SDA"] {
                    let pi = def.pin(name).unwrap();
                    let n = b.n(pi);
                    b.r(v, n, 4700.0, Some(vp), Some(pi));
                }
            }
        }
        "dht11" => {
            let pi = def.pin("DATA").unwrap();
            let n = b.n(pi);
            b.r(v, n, 5000.0, Some(vp), Some(pi));
        }
        "hcsr04" => {
            let pi = def.pin("ECHO").unwrap();
            let n = b.n(pi);
            b.sw(v, n, 100.0, Some(vp), Some(pi));
            b.sw(n, g, 100.0, Some(pi), Some(gp));
            let tp = def.pin("TRIG").unwrap();
            let tn = b.n(tp);
            b.r(tn, g, 10e3, Some(tp), Some(gp));
        }
        "esp01" | "hc05" | "jdy31" | "hm10" | "cat1" | "nbiot" | "gps" | "max485" => {
            // TX 输出：空闲高（3.3V 电平）
            if let Some(tp) = def.pins.iter().position(|p| p.name == "TX" || p.name == "TXD" || p.name == "RO") {
                let tn = b.n(tp);
                let s = b.sw(v, tn, 200.0, Some(vp), Some(tp));
                set_sw(&mut b.sim.devs[s], true);
            }
            if let Some(rp) = def.pins.iter().position(|p| p.name == "RX" || p.name == "RXD" || p.name == "DI") {
                let rn = b.n(rp);
                b.r(rn, g, 1e6, Some(rp), Some(gp));
            }
        }
        _ => {}
    }
}

/// 每步开始前：按时间和交互状态更新器件参数
fn pre_part(sim: &mut Sim, st: &mut PState, part: &Part, t: f64, dt: f64) {
    if st.d1 == st.d0 && !matches!(st.beh, Beh::Mcu(_)) {
        return;
    }
    let (d0, d1) = (st.d0, st.d1);
    let devs = &mut sim.devs[d0..d1];
    let t1 = t + dt;
    match part.kind.as_str() {
        "battery" => {
            if let Beh::Battery { soc, spec, .. } = &st.beh {
                let v = models::battery_ocv(spec, *soc);
                set_v(&mut devs[0], v);
            }
        }
        "psu" => {
            if let Dev::N { m: Nl::Psu { v, .. }, .. } = &mut devs[0] {
                *v = part.num("v");
            }
        }
        "acsrc" => {
            if let Beh::Ac { amp, freq } = st.beh {
                set_v(&mut devs[0], amp * (std::f64::consts::TAU * freq * t1).sin());
            }
        }
        "siggen" => {
            if let Beh::Square { amp, freq, duty } = st.beh {
                let ph = (t1 * freq).fract();
                // 正好落在边沿上时按「之后」算
                set_v(&mut devs[0], if ph < duty - 1e-9 { amp } else { 0.0 });
            }
        }
        "pot" => {
            let r = part.num("r").max(1.0);
            let x = (part.num("pos") / 100.0).clamp(0.0, 1.0);
            set_r(&mut devs[0], (r * x).max(0.5));
            set_r(&mut devs[1], (r * (1.0 - x)).max(0.5));
        }
        "ldr" => {
            let lux = part.num("lux").max(0.01);
            // GL5528：10 lux 时约 10k，越亮越小
            let r = (10e3 * (lux / 10.0).powf(-0.7)).clamp(100.0, 2e6);
            set_r(&mut devs[0], r);
        }
        "ntc" => {
            let tk = part.num("temp") + 273.15;
            let r = part.num("r25") * (3950.0 * (1.0 / tk - 1.0 / 298.15)).exp();
            set_r(&mut devs[0], r);
        }
        "button" => set_sw(&mut devs[0], sim.pressed.contains(&part.id)),
        "slide" => {
            let on = part.prop("on") != "false";
            set_sw(&mut devs[0], on);
            set_sw(&mut devs[1], !on);
        }
        "dipsw" => {
            let bits: Vec<char> = part.prop("bits").chars().collect();
            for i in 0..4 {
                set_sw(&mut devs[i], bits.get(i) == Some(&'1'));
            }
        }
        "encoder" => {
            // 位置 → 格雷码：CLK、DT 两相
            let n = part.num("count") as i64;
            let ph = n.rem_euclid(4);
            let (a, bb) = match ph {
                0 => (false, false),
                1 => (true, false),
                2 => (true, true),
                _ => (false, true),
            };
            set_sw(&mut devs[0], sim.pressed.contains(&part.id));
            set_sw(&mut devs[2], a);
            set_sw(&mut devs[4], bb);
        }
        "relay" | "relay_mod" => {
            if let Beh::Relay { closed, .. } = st.beh {
                let k = if part.kind == "relay" { 1 } else { devs.len() - 2 };
                set_sw(&mut devs[k], closed);
                set_sw(&mut devs[k + 1], !closed);
            }
        }
        "motor" => {
            if let Beh::Motor { omega, kv, .. } = st.beh {
                set_v(&mut devs[1], omega / kv);
            }
        }
        "ne555" => {
            if let Beh::Timer555 { q } = st.beh {
                let v = &sim.solver.v;
                let rst_low = volt(v, st.pin_node[3]) - volt(v, st.pin_node[0]) < 0.7;
                let q = q && !rst_low;
                set_sw(&mut devs[3], q);
                set_sw(&mut devs[4], !q);
                set_sw(&mut devs[5], !q);
            }
        }
        "hc595" => {
            if let Beh::Hc595 { out, sr, .. } = st.beh {
                let v = &sim.solver.v;
                let oe_off = volt(v, st.pin_node[12]) - volt(v, st.pin_node[7]) > 2.0;
                let names = [14usize, 0, 1, 2, 3, 4, 5, 6];
                // 引脚下标：QA=14, QB..QH=0..6, QH'=8；器件顺序按 QA..QH, QH'
                let _ = names;
                for bit in 0..8 {
                    let on = out >> bit & 1 == 1;
                    set_sw(&mut devs[bit * 2], on && !oe_off);
                    set_sw(&mut devs[bit * 2 + 1], !on && !oe_off);
                }
                let qh2 = sr >> 7 & 1 == 1;
                set_sw(&mut devs[16], qh2);
                set_sw(&mut devs[17], !qh2);
            }
        }
        "tb6612" | "l298n" => {
            let v = &sim.solver.v;
            let tb = part.kind == "tb6612";
            let def = lib::def(&part.kind).unwrap();
            let g = volt(v, st.pin_node[def.pin("GND").unwrap()]);
            let hi = |name: &str| st.pin_node.get(def.pin(name).unwrap()).is_some_and(|n| *n != NONE && volt(v, *n) - g > 1.5);
            let vlogic = if tb { volt(v, st.pin_node[def.pin("VCC").unwrap()]) - g } else { 5.0 };
            let en_all = !tb || (hi("STBY") && vlogic > 2.5);
            // 每路：(输出1, 输出2, IN1, IN2, PWM)
            let chans: [(&str, &str, usize, usize); 2] = if tb {
                [("AIN1", "AIN2", 0, 1), ("BIN1", "BIN2", 2, 3)]
            } else {
                [("IN1", "IN2", 0, 1), ("IN3", "IN4", 2, 3)]
            };
            for (ci, (i1, i2, o1, o2)) in chans.iter().enumerate() {
                let pwm = if tb { hi(if ci == 0 { "PWMA" } else { "PWMB" }) } else { hi(if ci == 0 { "ENA" } else { "ENB" }) };
                let (a, bb) = (hi(i1), hi(i2));
                // TB6612：IN1/IN2 = 1/0 正转、0/1 反转、1/1 刹车；PWM 低时短路刹车
                let (h1, l1, h2, l2) = if !en_all {
                    (false, false, false, false)
                } else if tb {
                    match (a, bb, pwm) {
                        (true, false, true) => (true, false, false, true),
                        (false, true, true) => (false, true, true, false),
                        (false, false, _) => (false, false, false, false),
                        _ => (false, true, false, true),
                    }
                } else if !pwm {
                    (false, false, false, false)
                } else {
                    (a, !a, bb, !bb)
                };
                set_sw(&mut devs[o1 * 4], h1);
                set_sw(&mut devs[o1 * 4 + 1], l1);
                set_sw(&mut devs[o2 * 4], h2);
                set_sw(&mut devs[o2 * 4 + 1], l2);
            }
        }
        "uln2003" => {
            let v = &sim.solver.v;
            for ch in 0..7 {
                // 每路器件：R、二极管、输出二极管、开关、续流二极管
                let base = ch * 5;
                let ib = st.pin_node[ch];
                let g = st.pin_node[7];
                let on = ib != NONE && volt(v, ib) - volt(v, g) > 1.6;
                set_sw(&mut devs[base + 3], on);
            }
        }
        "cat1" | "esp01" | "nbiot" | "lora" => {
            if let Beh::Load { phase } = st.beh {
                // 发射时的电流尖峰：4G 每 4.6 ms 有 0.58 ms 的 1.8 A；WiFi 约 300 mA
                let v = &sim.solver.v;
                let (vp, gp) = (st.pin_node[0], st.pin_node[1]);
                let up = volt(v, vp) - volt(v, gp) > 2.8;
                let (peak, period, width) = match part.kind.as_str() {
                    "cat1" => (1.8, 4.615e-3, 0.577e-3),
                    "esp01" => (0.3, 0.1, 0.004),
                    "nbiot" => (0.25, 1.0, 0.05),
                    _ => (0.12, 1.0, 0.03),
                };
                let ph = (t1 - phase).rem_euclid(period);
                let i = if up && ph < width { peak } else { 0.0 };
                let k = devs.len() - 1;
                set_i(&mut devs[k], i);
            }
        }
        "hcsr04" => {
            let on = sim.pulses.get(&part.id).is_some_and(|(a, b)| t1 > *a && t1 <= *b);
            set_sw(&mut devs[1], on);
            set_sw(&mut devs[2], !on);
        }
        _ => {}
    }
    if let Beh::Mcu(rt) = &mut st.beh {
        rt.pre(devs, &sim.solver.v, &st.pin_node, part, t1, &sim.pressed);
    }
}

/// 每步结束后：读结果、改状态、检查有没有烧
fn post_part(sim: &mut Sim, st: &mut PState, c: &Circuit, part: &Part, t: f64, dt: f64) {
    let v = &sim.solver.v;
    let pv = |k: usize| volt(v, *st.pin_node.get(k).unwrap_or(&NONE));
    let def = lib::def(&part.kind);
    let mode = Mode::Tran(dt);
    let dev_i = |k: usize| -> f64 { sim.solver.current(&sim.devs[st.d0 + k], mode).first().copied().unwrap_or(0.0) };
    let mut ratio: f64 = 0.0;
    let mut why = String::new();
    let mut over = |r: f64, w: String| {
        if r > ratio {
            ratio = r;
            why = w;
        }
    };
    let has = st.d1 > st.d0;
    match part.kind.as_str() {
        "battery" => {
            if let Beh::Battery { soc, spec, used_mah } = &mut st.beh {
                let i = sim.solver.current(&sim.devs[st.d0], mode)[0];
                let mah = i * dt / 3.6;
                *used_mah += mah;
                *soc = (*soc - mah / part.num("mah").max(1.0)).clamp(0.0, 1.0);
                if i < -2.0 * spec.mah / 1000.0 && part.prop("type") == "li" {
                    over(2.0, crate::i18n::t("锂电池充电电流太大").into());
                }
            }
        }
        "res" if has => {
            let p = dev_i(0).powi(2) * part.num("r");
            let pmax = part.prop("pmax").parse::<f64>().unwrap_or(0.25);
            over(p / pmax, crate::i18n::tf("功率 {} 超过额定 {}", &[&super::fmt_si(p, "W"), &super::fmt_si(pmax, "W")]));
        }
        "pot" if has => {
            let r = part.num("r");
            let p = dev_i(0).powi(2) * (r * part.num("pos") / 100.0) + dev_i(1).powi(2) * (r * (1.0 - part.num("pos") / 100.0));
            over(p / 0.1, crate::i18n::tf("功率 {} 超过额定 {}", &[&super::fmt_si(p, "W"), &super::fmt_si(0.1, "W")]));
        }
        "led" | "rgbled" if has => {
            let n = st.d1 - st.d0;
            for k in 0..n {
                let i = dev_i(k);
                over(
                    i / models::LED_IMAX,
                    crate::i18n::tf("电流 {}，LED 最大只能 {}", &[&super::fmt_si(i, "A"), &super::fmt_si(models::LED_IMAX, "A")]),
                );
                // 反向电压超过 5V
                let vr = -(pv(if part.kind == "led" { 0 } else { [0, 2, 3][k] }) - pv(1));
                over(vr / 5.0 * 0.9, crate::i18n::tf("反向电压 {}，LED 最多承受 5 V", &[&super::fmt_si(vr, "V")]));
            }
        }
        "seg7" if has => {
            for k in 0..8 {
                let i = dev_i(k);
                over(
                    i / models::LED_IMAX,
                    crate::i18n::tf("电流 {}，LED 最大只能 {}", &[&super::fmt_si(i, "A"), &super::fmt_si(models::LED_IMAX, "A")]),
                );
            }
        }
        "diode" | "zener" if has => {
            let s = if part.kind == "diode" {
                models::diode(part.prop("model"))
            } else {
                models::zener(part.prop("model"))
            };
            let i = dev_i(0);
            let p = i.abs() * (pv(0) - pv(1)).abs();
            over(i.abs() / s.imax, crate::i18n::tf("电流 {} 超过额定 {}", &[&super::fmt_si(i.abs(), "A"), &super::fmt_si(s.imax, "A")]));
            over(p / s.pmax, crate::i18n::tf("功率 {} 超过额定 {}", &[&super::fmt_si(p, "W"), &super::fmt_si(s.pmax, "W")]));
            if s.vr > 0.0 {
                let vr = pv(1) - pv(0);
                over(vr / s.vr, crate::i18n::tf("反向电压 {} 超过额定 {}", &[&super::fmt_si(vr, "V"), &super::fmt_si(s.vr, "V")]));
            }
        }
        "npn" | "pnp" if has => {
            let s = models::bjt(part.prop("model"), part.kind == "pnp");
            let sign = if part.kind == "pnp" { -1.0 } else { 1.0 };
            let ic = sign * sim.solver.current(&sim.devs[st.d0], mode)[0];
            let vce = sign * (pv(2) - pv(0));
            let p = (ic * vce).abs();
            over(
                ic.abs() / s.icmax,
                crate::i18n::tf("集电极电流 {} 超过额定 {}", &[&super::fmt_si(ic.abs(), "A"), &super::fmt_si(s.icmax, "A")]),
            );
            over(p / s.pmax, crate::i18n::tf("发热 {} 超过额定 {}", &[&super::fmt_si(p, "W"), &super::fmt_si(s.pmax, "W")]));
            over(
                vce / s.vceo * 0.95,
                crate::i18n::tf("C、E 之间电压 {} 超过耐压 {}（感性负载没加续流二极管？）", &[&super::fmt_si(vce, "V"), &super::fmt_si(s.vceo, "V")]),
            );
        }
        "nmos" | "pmos" if has => {
            let s = models::mos(part.prop("model"));
            let sign = if part.kind == "pmos" { -1.0 } else { 1.0 };
            let id = sign * sim.solver.current(&sim.devs[st.d0], mode)[0];
            let vds = sign * (pv(1) - pv(2));
            let vgs = (pv(0) - pv(2)).abs();
            let p = (id * vds).abs();
            over(p / s.pmax, crate::i18n::tf("发热 {} 超过额定 {}", &[&super::fmt_si(p, "W"), &super::fmt_si(s.pmax, "W")]));
            over(
                vds / s.vds * 0.95,
                crate::i18n::tf("D、S 之间电压 {} 超过耐压 {}（感性负载没加续流二极管？）", &[&super::fmt_si(vds, "V"), &super::fmt_si(s.vds, "V")]),
            );
            over(vgs / s.vgs * 0.9, crate::i18n::tf("栅极电压 {} 超过最大 {}", &[&super::fmt_si(vgs, "V"), &super::fmt_si(s.vgs, "V")]));
            over(
                id.abs() / s.idmax,
                crate::i18n::tf("电流 {} 超过额定 {}", &[&super::fmt_si(id.abs(), "A"), &super::fmt_si(s.idmax, "A")]),
            );
        }
        "ecap" | "cap" if has => {
            let vc = pv(0) - pv(1);
            let vmax = part.num("vmax").max(1.0);
            over(
                vc.abs() / vmax * 0.9,
                crate::i18n::tf("电压 {} 超过耐压 {}", &[&super::fmt_si(vc.abs(), "V"), &super::fmt_si(vmax, "V")]),
            );
            if part.kind == "ecap" && vc < -1.0 {
                over(-vc * 1.5, crate::i18n::t("电解电容接反了（正极电压比负极低），会鼓包甚至炸开").into());
            }
        }
        "fuse" if has => {
            let i = dev_i(0).abs();
            let rated = part.num("i").max(1e-3);
            if i > rated * 1.5 {
                if let Beh::Fuse { blown } = &mut st.beh {
                    *blown = true;
                }
                sim.log.push((t, part.id, crate::i18n::tf("保险丝熔断：电流 {}", &[&super::fmt_si(i, "A")])));
                sim.rebuild = true;
            }
        }
        "relay" | "relay_mod" if has => {
            if let Beh::Relay { closed, inom } = &mut st.beh {
                let on = if part.kind == "relay" {
                    let i = dev_i(0).abs();
                    if *closed {
                        i > 0.3 * *inom
                    } else {
                        i > 0.7 * *inom
                    }
                } else {
                    let def = def.unwrap();
                    let (vcc, gnd, inp) = (pv(def.pin("VCC").unwrap()), pv(def.pin("GND").unwrap()), pv(def.pin("IN").unwrap()));
                    let powered = vcc - gnd > 3.5;
                    let level = inp - gnd;
                    powered && if part.prop("trig") == "low" { level < (vcc - gnd) * 0.4 } else { level > (vcc - gnd) * 0.6 }
                };
                if on != *closed {
                    *closed = on;
                }
            }
        }
        "motor" if has => {
            if let Beh::Motor { omega, angle, kv } = &mut st.beh {
                let i = dev_i(0);
                let vr = part.num("v");
                let istall = vr / (vr / 0.6).clamp(2.0, 20.0);
                // 力矩 = kt·i，摩擦 + 空载损耗；转动惯量让转速有一个上升过程（约 0.15 s）
                let kt = 1.0 / *kv;
                let j = kt * istall * 0.15 / (vr * *kv);
                let fr = kt * istall * 0.08 * omega.signum() + 1e-4 * *omega;
                let acc = (kt * i - fr) / j.max(1e-9);
                *omega += acc * dt;
                if omega.abs() < 0.5 && (kt * i).abs() < kt * istall * 0.08 {
                    *omega = 0.0;
                }
                *angle = (*angle + *omega * dt).rem_euclid(std::f64::consts::TAU);
                over(i.abs() / (istall * 1.2) * 0.5, String::new());
            }
        }
        "servo" if has => {
            if let Beh::Servo { angle, target, rise, last_pulse } = &mut st.beh {
                let powered = pv(1) - pv(0) > 3.5;
                let high = pv(2) - pv(0) > 1.5;
                match (high, *rise) {
                    (true, None) => *rise = Some(t),
                    (false, Some(r)) => {
                        let w = t - r;
                        if (0.4e-3..2.6e-3).contains(&w) {
                            *target = ((w - 0.5e-3) / 2.0e-3 * 180.0).clamp(0.0, 180.0);
                            *last_pulse = t;
                        }
                        *rise = None;
                    }
                    _ => {}
                }
                if powered && t - *last_pulse < 0.1 {
                    let step = 600.0 * dt;
                    let d = (*target - *angle).clamp(-step, step);
                    *angle += d;
                    // 转动时电流约 150 mA
                    let moving = d.abs() > 1e-6;
                    set_i(&mut sim.devs[st.d0], if moving { 0.15 } else { 0.006 });
                }
            }
        }
        "pbuzzer" if has => {
            if let Beh::Buzzer { last_cross, sign, period } = &mut st.beh {
                let i = dev_i(0);
                let s = i > 1e-3;
                if s && !*sign {
                    if *last_cross > 0.0 {
                        *period = t - *last_cross;
                    }
                    *last_cross = t;
                }
                *sign = s;
                if t - *last_cross > 0.1 {
                    *period = 0.0;
                }
            }
        }
        "ne555" if has => {
            if let Beh::Timer555 { q } = &mut st.beh {
                let gnd = pv(0);
                let ctrl = pv(4) - gnd;
                let (trig, thr) = (pv(1) - gnd, pv(5) - gnd);
                let before = *q;
                if trig < ctrl / 2.0 {
                    *q = true;
                } else if thr > ctrl {
                    *q = false;
                }
                if *q != before {
                    sim.next_ev = sim.next_ev.min(t + dt);
                }
                let vcc = pv(7) - gnd;
                over(vcc / 16.0, crate::i18n::tf("电源电压 {} 超过 555 的最大 16V", &[&super::fmt_si(vcc, "V")]));
            }
        }
        "hc595" if has => {
            if let Beh::Hc595 { sr, out, clk, rclk } = &mut st.beh {
                let gnd = pv(7);
                let vcc = pv(15) - gnd;
                let th = vcc * 0.5;
                let hi = |k: usize| pv(k) - gnd > th;
                let (ser, c, r, clr) = (hi(13), hi(10), hi(11), hi(9));
                if !clr {
                    *sr = 0;
                }
                if c && !*clk {
                    *sr = (*sr << 1) | ser as u8;
                }
                if r && !*rclk {
                    *out = *sr;
                }
                *clk = c;
                *rclk = r;
            }
        }
        "ldo" if has => {
            let s = models::ldo(part.prop("model"));
            let vin = pv(2) - pv(0);
            let iout = -st_pin_i(sim, st, 1, dt);
            let p = (vin - (pv(1) - pv(0))).max(0.0) * iout.max(0.0);
            // 结温：一阶热惯性，时间常数 2 s
            let tj = 25.0 + p * s.rth;
            st.temp += (tj - st.temp) * (dt / 2.0).min(1.0);
            over(vin / s.vin_max, crate::i18n::tf("输入电压 {} 超过最大 {}", &[&super::fmt_si(vin, "V"), &super::fmt_si(s.vin_max, "V")]));
            if vin < -0.5 {
                over(2.0, crate::i18n::t("稳压芯片输入输出接反了").into());
            }
        }
        _ => {}
    }
    // 模块供电电压检查
    if let Some(def) = def {
        if matches!(def.cat, lib::Cat::Display | lib::Cat::Camera | lib::Cat::Iot | lib::Cat::Module | lib::Cat::Chip) && has {
            let (vcc, gnd) = module_pwr_pins(def);
            if let (Some(a), Some(b)) = (vcc, gnd) {
                let vs = pv(a) - pv(b);
                let (_, vmax, _) = models::module_power(def.key);
                over(vs / (vmax * 1.08), crate::i18n::tf("供电 {} 超过模块最大 {}", &[&super::fmt_si(vs, "V"), &super::fmt_si(vmax, "V")]));
                if vs < -1.0 {
                    over(2.0, crate::i18n::t("电源正负接反了").into());
                }
            }
        }
    }
    if let Beh::Mcu(rt) = &mut st.beh {
        let Sim {
            devs,
            solver,
            node_of_net,
            nl,
            uart,
            effects,
            overlay,
            burnt_ids,
            log,
            ..
        } = sim;
        let vv = &solver.v;
        let net_v = |n: usize| -> Option<f64> {
            let k = *node_of_net.get(n)?;
            (k != NONE).then(|| volt(vv, k))
        };
        let mut msgs = vec![];
        let mut ctx = McuCtx {
            c,
            nl,
            net_v: &net_v,
            burnt: burnt_ids,
            uart,
            effects,
            log: &mut msgs,
            overlay,
        };
        let (bad, ev) = rt.post(&devs[st.d0..st.d1], vv, &st.pin_node, part, t, dt, &mut ctx);
        for m in msgs {
            log.push((t, part.id, m));
        }
        if let Some((r, w)) = bad {
            over(r, w);
        }
        if let Some(e) = ev {
            sim.next_ev = sim.next_ev.min(e);
        }
    }
    // 过载累积：超过 1 倍开始累积，5 倍以上几乎立刻烧，20 倍以上（击穿）立刻烧
    if ratio > 1.0 && st.burnt.is_none() {
        st.stress += dt * (ratio - 1.0) * if ratio > 5.0 { 50.0 } else { 1.0 };
        if st.stress > 0.25 || ratio > 20.0 {
            let msg = if why.is_empty() { crate::i18n::t("过载").to_string() } else { why };
            burn(st, t, &mut sim.log, msg);
            sim.rebuild = true;
        }
    } else {
        st.stress = (st.stress - dt * 0.5).max(0.0);
    }
}

/// 一步里某引脚流入的电流（帧平均之外，需要即时值时用）
fn st_pin_i(sim: &Sim, st: &PState, pin: usize, dt: f64) -> f64 {
    let mut s = 0.0;
    for k in st.d0..st.d1 {
        let (_, terms) = &sim.owner[k];
        let cur = term_currents(&sim.solver, &sim.devs[k], Mode::Tran(dt));
        for (j, t) in terms.iter().enumerate() {
            if *t == Some(pin) && j < cur.len() {
                s += cur[j];
            }
        }
    }
    s
}

/// 下一次需要停下来的时刻（PWM、方波边沿、单片机事件）
fn next_event(st: &PState, part: &Part, t: f64) -> f64 {
    match &st.beh {
        Beh::Square { freq, duty, .. } => {
            let period = 1.0 / freq;
            let base = (t / period).floor() * period;
            let e1 = base + duty * period;
            if e1 > t + 1e-12 {
                e1
            } else {
                base + period
            }
        }
        Beh::Ac { freq, .. } => t + 1.0 / freq / 40.0,
        Beh::Mcu(rt) => rt.next_event(t),
        _ if part.kind == "hcsr04" => f64::INFINITY,
        Beh::Load { phase } => {
            let (period, width) = match part.kind.as_str() {
                "cat1" => (4.615e-3, 0.577e-3),
                "esp01" => (0.1, 0.004),
                "nbiot" => (1.0, 0.05),
                _ => (1.0, 0.03),
            };
            let ph = (t - phase).rem_euclid(period);
            if ph < width - 1e-12 {
                t + (width - ph)
            } else {
                t + (period - ph)
            }
        }
        _ => f64::INFINITY,
    }
}

/// 帧末更新显示状态
fn frame_vis(sim: &Sim, st: &mut PState, part: &Part) {
    let v = &sim.solver.v;
    let pv = |k: usize| volt(v, *st.pin_node.get(k).unwrap_or(&NONE));
    st.ipeak = st.i.iter().fold(0.0, |a, b| a.max(b.abs()));
    let vis = &mut st.vis;
    match part.kind.as_str() {
        "led" | "lamp" | "buzzer" => {
            let i = st.i[0].max(0.0);
            vis.glow = if part.kind == "lamp" { (st.p / 0.5).sqrt() as f32 } else { (i / models::LED_INOM) as f32 };
            vis.on = i > 5e-3;
        }
        "rgbled" => {
            for (c, k) in [0, 2, 3].iter().enumerate() {
                vis.rgb[c] = (st.i[*k].max(0.0) / models::LED_INOM) as f32;
            }
        }
        "seg7" => {
            let ca = part.prop("common") == "ca";
            let def = lib::def("seg7").unwrap();
            for (k, name) in ["a", "b", "c", "d", "e", "f", "g", "dp"].iter().enumerate() {
                let i = st.i[def.pin(name).unwrap()];
                vis.segs[k] = ((if ca { -i } else { i }).max(0.0) / models::LED_INOM) as f32;
            }
        }
        "motor" => {
            if let Beh::Motor { omega, angle, .. } = st.beh {
                vis.rpm = (omega * 60.0 / std::f64::consts::TAU) as f32;
                vis.angle = angle.to_degrees() as f32;
            }
        }
        "servo" => {
            if let Beh::Servo { angle, .. } = st.beh {
                vis.angle = angle as f32;
            }
        }
        "relay" | "relay_mod" => {
            if let Beh::Relay { closed, .. } = st.beh {
                vis.on = closed;
            }
        }
        "pbuzzer" => {
            if let Beh::Buzzer { period, .. } = st.beh {
                vis.freq = if period > 0.0 { (1.0 / period) as f32 } else { 0.0 };
                vis.on = vis.freq > 20.0;
            }
        }
        "npn" | "pnp" => {
            let s = if part.kind == "pnp" { -1.0 } else { 1.0 };
            let (ie, ib, ic) = (st.i[0], st.i[1], st.i[2]);
            let _ = ie;
            let (ib, ic) = (s * ib, s * ic);
            let vbe = s * (pv(1) - pv(0));
            let vce = s * (pv(2) - pv(0));
            let bf = models::bjt(part.prop("model"), s < 0.0).m.bf;
            vis.status = if vbe < 0.5 || ib < 1e-6 {
                crate::i18n::t("截止").to_string()
            } else if vce < 0.3 || ic < ib * bf * 0.5 {
                crate::i18n::t("饱和").to_string()
            } else {
                crate::i18n::t("放大").to_string()
            };
            vis.reading = format!("Ib {}  Ic {}  Vce {}", super::fmt_si(ib, "A"), super::fmt_si(ic, "A"), super::fmt_si(vce, "V"));
        }
        "nmos" | "pmos" => {
            let s = if part.kind == "pmos" { -1.0 } else { 1.0 };
            let spec = models::mos(part.prop("model"));
            let vgs = s * (pv(0) - pv(2));
            let vds = s * (pv(1) - pv(2));
            let id = s * st.i[1];
            vis.status = if vgs < spec.m.vth - 0.2 {
                crate::i18n::t("关断").to_string()
            } else if vds < (vgs - spec.m.vth) * 0.5 || vds < 0.1 {
                crate::i18n::t("完全导通").to_string()
            } else {
                crate::i18n::t("半导通（发热）").to_string()
            };
            vis.reading = format!("Vgs {}  Id {}  P {}", super::fmt_si(vgs, "V"), super::fmt_si(id, "A"), super::fmt_si(st.p.abs(), "W"));
        }
        "meter" => {
            let vv = pv(1) - pv(0);
            vis.reading = match part.prop("mode") {
                "a" => super::fmt_si(st.i[1], "A"),
                "v" => format!("{vv:.3} V"),
                _ => String::new(),
            };
        }
        "ammeter" | "pmeter" => {
            vis.reading = super::fmt_si(st.i[0], "A");
        }
        "probe" => vis.reading = super::fmt_si(pv(0), "V"),
        "ldo" => vis.reading = format!("{:.0} °C", st.temp),
        _ => {}
    }
    if let Beh::Mcu(rt) = &st.beh {
        vis.status = rt.status();
    }
    let _ = Look::Resistor;
}
