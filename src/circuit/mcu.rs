//! 开发板：引脚设置 + 「如果…就…」规则，不跑真实固件也能让板子动起来。
//!
//! 每个用到的引脚在电路里是 4 个开关（推高、拉低、上拉、下拉）+ 两个钳位二极管；单片机自己的耗电是电源轨到地的电流源。
//! 规则在每步结束后求值，动作改变的引脚在下一步生效。串口打印、显示屏、传感器按协议在 sim 层和别的元件对接。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::lib::{cap, PartDef};
use super::models::{self, BoardSpec};
use super::net::Netlist;
use super::sim::{set_sw, set_sw_r, volt, Build, UartTx, NONE};
use super::solver::{Dev, DiodeM, Nl};
use super::{Circuit, Part};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct McuCfg {
    pub pins: Vec<PinCfg>,
    pub rules: Vec<Rule>,
    pub baud: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PinCfg {
    pub pin: String,
    pub mode: PinMode,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "m")]
pub enum PinMode {
    OutHigh,
    OutLow,
    Blink {
        ms: u32,
    },
    Pwm {
        duty: f32,
        freq: u32,
    },
    Breath {
        ms: u32,
    },
    /// 流水灯：同一组的引脚按 index 轮流点亮
    Seq {
        group: u8,
        index: u8,
        ms: u32,
    },
    Tone {
        freq: u32,
    },
    Servo {
        angle: f32,
    },
    InPullUp,
    InPullDown,
    InFloat,
    Adc,
    UartTx,
    UartRx,
    I2cSda,
    I2cScl,
    SpiSck,
    SpiMosi,
    SpiMiso,
    SpiCs,
}

impl PinMode {
    pub fn is_output(&self) -> bool {
        matches!(
            self,
            PinMode::OutHigh | PinMode::OutLow | PinMode::Blink { .. } | PinMode::Pwm { .. } | PinMode::Breath { .. } | PinMode::Seq { .. } | PinMode::Tone { .. } | PinMode::Servo { .. }
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    pub when: Cond,
    pub then: Vec<Act>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "if")]
pub enum Cond {
    /// 上电（或复位）时执行一次
    Start,
    Every {
        ms: u32,
    },
    /// 引脚变低（按键按下，上拉接法）
    Falling {
        pin: String,
    },
    /// 引脚变高
    Rising {
        pin: String,
    },
    /// 只要引脚是低 / 高就一直执行（每 20 ms）
    WhileLow {
        pin: String,
    },
    WhileHigh {
        pin: String,
    },
    /// ADC 电压超过 / 低于
    Above {
        pin: String,
        v: f32,
    },
    Below {
        pin: String,
        v: f32,
    },
    /// 串口收到一行（等于这个文字，不分大小写）
    Recv {
        text: String,
    },
    TempAbove {
        c: f32,
    },
    TempBelow {
        c: f32,
    },
    DistBelow {
        cm: f32,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "do")]
pub enum Act {
    Set {
        pin: String,
        high: bool,
    },
    Toggle {
        pin: String,
    },
    /// PWM 占空比 = ADC 读数比例
    PwmFromAdc {
        pin: String,
        adc: String,
    },
    PwmSet {
        pin: String,
        duty: f32,
    },
    ServoSet {
        pin: String,
        angle: f32,
    },
    ServoFromAdc {
        pin: String,
        adc: String,
    },
    Print {
        text: String,
    },
    Tone {
        pin: String,
        freq: u32,
        ms: u32,
    },
    /// 74HC595：数据、时钟、锁存引脚，输出值（模板，可以是 {count}）
    Shift {
        data: String,
        clk: String,
        latch: String,
        value: String,
    },
    /// 1 位数码管（直接接引脚，a..g 按顺序列出引脚）显示一个数字
    Seg {
        pins: Vec<String>,
        value: String,
        common_anode: bool,
    },
    /// TM1637 显示
    Tm1637 {
        clk: String,
        dio: String,
        text: String,
    },
    /// I2C / SPI 屏显示文字（自动找到接在 I2C / SPI 上的屏）
    Show {
        text: String,
    },
    ShowImage,
    ReadDht {
        pin: String,
    },
    ReadSht,
    Measure {
        trig: String,
        echo: String,
    },
    Count,
    Mqtt {
        host: String,
        port: u16,
        topic: String,
        payload: String,
    },
    Http {
        url: String,
        body: String,
    },
    Tcp {
        host: String,
        port: u16,
        text: String,
    },
    Photo,
    Sleep {
        ms: u32,
    },
}

/// 规则引擎产生的、要交给 sim 层处理的效果
#[derive(Clone, Debug)]
pub enum Effect {
    /// 屏幕显示文字：(屏, 行)
    Lines(u32, Vec<String>),
    Image(u32, String),
    Digits(u32, String),
    /// 让某个模块的某个开关输出一段脉冲（超声波 ECHO）
    Pulse(u32, f64),
    Net(NetReq),
    Photo(u32),
}

#[derive(Clone, Debug)]
pub enum NetReq {
    Mqtt { host: String, port: u16, topic: String, payload: String },
    Http { url: String, body: String },
    Tcp { host: String, port: u16, text: String },
}

/// 引脚在电路里的器件下标（相对元件的第一个器件）
#[derive(Clone, Copy, Debug, Default)]
struct PinDevs {
    hi: usize,
    lo: usize,
    pu: usize,
    pd: usize,
    /// 钳位二极管：引脚→电源、地→引脚（NONE = 没有，耐 5V 引脚没有上面那个）
    cu: usize,
    cd: usize,
}

pub struct McuRt {
    pub key: String,
    spec: BoardSpec,
    pub cfg: McuCfg,
    cfg_src: String,
    /// 引脚名 → 下标（同名引脚取第一个）
    names: HashMap<String, usize>,
    devs: HashMap<usize, PinDevs>,
    /// 逻辑电源轨、地 的引脚下标
    rail: usize,
    gnd: usize,
    mcu_load: usize,
    /// 运行状态
    pub running: bool,
    pub boot_t: f64,
    pub asleep_until: f64,
    pub resets: u32,
    /// 引脚当前的输出（Some(高/低)），None = 不输出
    out: HashMap<usize, bool>,
    /// 动作设置的 PWM 占空比、舵机角度（覆盖引脚设置）
    pwm_over: HashMap<usize, f32>,
    servo_over: HashMap<usize, f32>,
    tone_until: HashMap<usize, (u32, f64)>,
    /// 定时排好的引脚变化（移位输出用）
    sched: Vec<(f64, usize, bool)>,
    last_in: HashMap<usize, bool>,
    last_every: HashMap<usize, f64>,
    last_while: HashMap<usize, f64>,
    cond_state: HashMap<usize, bool>,
    rx: Vec<u8>,
    pub vars: HashMap<String, String>,
    pub count: i64,
    pub effects: Vec<Effect>,
    pub logic_v: f64,
    /// 板子有没有 USB 串口（Arduino、ESP32 开发板）
    pub usb_serial: bool,
    pub host_out: Vec<u8>,
    pub fault: Option<String>,
    started: bool,
}

/// 常用板子的默认串口波特率
fn default_baud(key: &str) -> u32 {
    match key {
        "uno" | "nano" | "stc89" => 9600,
        _ => 115200,
    }
}

impl McuRt {
    pub fn new(part: &Part, def: &PartDef) -> McuRt {
        let mut names = HashMap::new();
        for (i, p) in def.pins.iter().enumerate() {
            names.entry(p.name.clone()).or_insert(i);
        }
        let logic_v = def.logic_v();
        let rail_name = if def.key == "stc89" {
            "VCC"
        } else if logic_v > 4.0 {
            "5V"
        } else {
            "3V3"
        };
        let mut rt = McuRt {
            key: def.key.to_string(),
            spec: models::board(def.key),
            cfg: McuCfg::default(),
            cfg_src: String::new(),
            rail: names.get(rail_name).copied().unwrap_or(0),
            gnd: names.get("GND").copied().unwrap_or(0),
            names,
            devs: HashMap::new(),
            mcu_load: 0,
            running: false,
            boot_t: 0.0,
            asleep_until: 0.0,
            resets: 0,
            out: HashMap::new(),
            pwm_over: HashMap::new(),
            servo_over: HashMap::new(),
            tone_until: HashMap::new(),
            sched: vec![],
            last_in: HashMap::new(),
            last_every: HashMap::new(),
            last_while: HashMap::new(),
            cond_state: HashMap::new(),
            rx: vec![],
            vars: HashMap::new(),
            count: 0,
            effects: vec![],
            logic_v,
            usb_serial: !matches!(def.key, "bluepill" | "stc89" | "esp32cam"),
            host_out: vec![],
            fault: None,
            started: false,
        };
        rt.reload(part);
        rt
    }

    /// 引脚设置 / 规则改了：重新读
    pub fn reload(&mut self, part: &Part) {
        let src = part.prop("mcu").to_string();
        if src != self.cfg_src {
            self.cfg = serde_json::from_str(&src).unwrap_or_default();
            self.cfg_src = src;
            self.started = false;
        }
        if self.cfg.baud == 0 {
            self.cfg.baud = default_baud(&self.key);
        }
    }

    pub fn pin_idx(&self, name: &str) -> Option<usize> {
        self.names.get(name).copied()
    }

    fn mode_of(&self, pin: usize) -> Option<&PinMode> {
        self.cfg.pins.iter().find(|p| self.names.get(&p.pin) == Some(&pin)).map(|p| &p.mode)
    }

    pub fn compile(&mut self, b: &mut Build, part: &Part, def: &PartDef) {
        self.devs.clear();
        let gnd = b.n(self.gnd);
        let rail = b.n(self.rail);
        let pin = |n: &str| def.pin(n);
        let usb = part.prop("power") == "usb";
        // 板载电源：USB 5V → 稳压 → 3.3V
        let v5 = ["5V", "VBUS", "VCC"].iter().find_map(|n| pin(n));
        if let Some(p5) = v5 {
            let n5 = b.n(p5);
            if usb {
                b.dev(
                    Dev::N {
                        pins: vec![n5, gnd],
                        m: Nl::Psu { v: 5.0, r: 0.15, ilim: 0.5 },
                    },
                    &[Some(p5), Some(self.gnd)],
                );
            }
            if let Some(p3) = pin("3V3") {
                if self.rail != p5 || self.key == "uno" || self.key == "nano" {
                    let n3 = b.n(p3);
                    let (drop, ilim, iq) = if matches!(self.key.as_str(), "uno" | "nano") { (0.3, 0.15, 75e-6) } else { (1.1, 1.0, 5e-3) };
                    b.dev(
                        Dev::N {
                            pins: vec![n5, n3, gnd],
                            m: Nl::Reg {
                                vset: 3.3,
                                drop,
                                ilim,
                                iq,
                                eff: 0.0,
                                rout: 0.05,
                                on: true,
                            },
                        },
                        &[Some(p5), Some(p3), Some(self.gnd)],
                    );
                }
            }
        }
        // VIN → 5V 稳压（Uno、Nano）
        if let (Some(pv), Some(p5)) = (pin("VIN"), v5) {
            if matches!(self.key.as_str(), "uno" | "nano") {
                let (nv, n5) = (b.n(pv), b.n(p5));
                b.dev(
                    Dev::N {
                        pins: vec![nv, n5, gnd],
                        m: Nl::Reg {
                            vset: 5.0,
                            drop: 1.1,
                            ilim: 0.8,
                            iq: 5e-3,
                            eff: 0.0,
                            rout: 0.05,
                            on: true,
                        },
                    },
                    &[Some(pv), Some(p5), Some(self.gnd)],
                );
            }
        }
        // 单片机本身的耗电 + 电源指示灯
        self.mcu_load = b.sim.devs.len() - b.sim.ps[b.pi].d0;
        b.dev(Dev::I { a: rail, b: gnd, i: self.spec.i_run }, &[Some(self.rail), Some(self.gnd)]);
        let pwr_led = b.inner();
        b.r(rail, pwr_led, 1000.0, Some(self.rail), None);
        b.dev(
            Dev::N {
                pins: vec![pwr_led, gnd],
                m: Nl::Diode(models::led("red").0),
            },
            &[None, Some(self.gnd)],
        );
        // 用到的引脚
        let pull = self.spec.pullup;
        for (i, p) in def.pins.iter().enumerate() {
            if p.caps & cap::IO == 0 {
                continue;
            }
            let configured = self.mode_of(i).is_some();
            let onboard_led = p.caps & cap::LED != 0;
            if !b.used(i) && !configured && !onboard_led {
                continue;
            }
            let n = b.n(i);
            let d0 = b.sim.ps[b.pi].d0;
            let rel = |b: &Build| b.sim.devs.len() - d0;
            let hi = rel(b);
            b.sw(rail, n, self.spec.r_hi, Some(self.rail), Some(i));
            let lo = rel(b);
            b.sw(n, gnd, self.spec.r_lo, Some(i), Some(self.gnd));
            let pu = rel(b);
            b.sw(rail, n, pull, Some(self.rail), Some(i));
            let pd = rel(b);
            b.sw(n, gnd, pull, Some(i), Some(self.gnd));
            let clamp = DiodeM::from_vf(0.6, 1e-3, 1.5, 20.0);
            let cu = if p.caps & (cap::FT | cap::OD) == 0 {
                let k = rel(b);
                b.dev(
                    Dev::N {
                        pins: vec![n, rail],
                        m: Nl::Diode(clamp),
                    },
                    &[Some(i), Some(self.rail)],
                );
                k
            } else {
                NONE
            };
            let cd = rel(b);
            b.dev(
                Dev::N {
                    pins: vec![gnd, n],
                    m: Nl::Diode(clamp),
                },
                &[Some(self.gnd), Some(i)],
            );
            if onboard_led {
                let m = b.inner();
                b.r(n, m, 1000.0, Some(i), None);
                let col = if self.key.starts_with("esp") {
                    "blue"
                } else if self.key == "uno" || self.key == "nano" {
                    "yellow"
                } else {
                    "green"
                };
                // 板载 LED：PC13（Blue Pill）、ESP8266 的 D4 是低电平点亮，其它高电平点亮
                if matches!(self.key.as_str(), "bluepill" | "nodemcu") {
                    b.dev(
                        Dev::N {
                            pins: vec![rail, m],
                            m: Nl::Diode(models::led(col).0),
                        },
                        &[Some(self.rail), None],
                    );
                } else {
                    b.dev(
                        Dev::N {
                            pins: vec![m, gnd],
                            m: Nl::Diode(models::led(col).0),
                        },
                        &[None, Some(self.gnd)],
                    );
                }
            }
            self.devs.insert(i, PinDevs { hi, lo, pu, pd, cu, cd });
        }
    }

    /// 上电 / 复位
    fn boot(&mut self, t: f64) {
        self.running = true;
        self.boot_t = t;
        self.out.clear();
        self.pwm_over.clear();
        self.servo_over.clear();
        self.tone_until.clear();
        self.sched.clear();
        self.last_every.clear();
        self.last_while.clear();
        self.cond_state.clear();
        self.started = false;
        for p in &self.cfg.pins {
            if let Some(i) = self.names.get(&p.pin) {
                match p.mode {
                    PinMode::OutHigh => {
                        self.out.insert(*i, true);
                    }
                    PinMode::OutLow => {
                        self.out.insert(*i, false);
                    }
                    _ => {}
                }
            }
        }
    }

    /// 某时刻某引脚的输出电平（None = 不驱动）
    fn level(&self, pin: usize, t: f64) -> Option<bool> {
        if !self.running || t < self.asleep_until {
            return None;
        }
        if let Some((f, until)) = self.tone_until.get(&pin) {
            if t < *until {
                return Some((t * *f as f64).fract() < 0.5);
            }
        }
        if let Some(a) = self.servo_over.get(&pin) {
            return Some(servo_level(*a, t));
        }
        if let Some(d) = self.pwm_over.get(&pin) {
            return Some(pwm_level(*d, self.pwm_freq(), t));
        }
        let lt = t - self.boot_t;
        match self.mode_of(pin) {
            Some(PinMode::Blink { ms }) if !self.out.contains_key(&pin) => Some(((lt * 1000.0 / (*ms).max(1) as f64) as i64) % 2 == 0),
            Some(PinMode::Pwm { duty, freq }) => Some(pwm_level(*duty, *freq, t)),
            Some(PinMode::Breath { ms }) => {
                let x = (lt * 1000.0 / (*ms).max(1) as f64).fract();
                let duty = 50.0 - 50.0 * (std::f64::consts::TAU * x).cos();
                Some(pwm_level(duty as f32, self.pwm_freq(), t))
            }
            Some(PinMode::Seq { group, index, ms }) => {
                let n = self.cfg.pins.iter().filter(|p| matches!(p.mode, PinMode::Seq { group: g, .. } if g == *group)).count().max(1);
                let k = ((lt * 1000.0 / (*ms).max(1) as f64) as usize) % n;
                Some(k == *index as usize)
            }
            Some(PinMode::Tone { freq }) => Some((t * *freq as f64).fract() < 0.5),
            Some(PinMode::Servo { angle }) => Some(servo_level(*angle, t)),
            Some(PinMode::UartTx) => Some(true),
            _ => self.out.get(&pin).copied(),
        }
    }

    fn pwm_freq(&self) -> u32 {
        match self.key.as_str() {
            "uno" | "nano" => 490,
            "stc89" => 100,
            _ => 1000,
        }
    }

    pub fn pre(&mut self, devs: &mut [Dev], v: &[f64], pin_node: &[usize], _part: &Part, t: f64, _pressed: &HashSet<u32>) {
        let rail_v = volt(v, pin_node[self.rail]) - volt(v, pin_node[self.gnd]);
        // 定时的引脚变化
        let mut fired = vec![];
        self.sched.retain(|(at, pin, lv)| {
            if *at <= t + 1e-12 {
                fired.push((*pin, *lv));
                false
            } else {
                true
            }
        });
        for (pin, lv) in fired {
            self.out.insert(pin, lv);
        }
        let asleep = t < self.asleep_until;
        // 单片机电流：睡眠时降到很小
        if let Some(d) = devs.get_mut(self.mcu_load) {
            let i = if !self.running || rail_v < self.spec.bod {
                1e-4
            } else if asleep {
                if self.key.starts_with("esp") {
                    10e-6
                } else {
                    0.1e-3
                }
            } else {
                self.spec.i_run
            };
            if let Dev::I { i: x, .. } = d {
                *x = i;
            }
        }
        let od_pin = |p: usize| -> bool { self.mode_of(p).is_some_and(|m| matches!(m, PinMode::I2cSda | PinMode::I2cScl)) };
        let pins: Vec<(usize, PinDevs)> = self.devs.iter().map(|(k, d)| (*k, *d)).collect();
        for (pin, pd) in pins {
            let lv = self.level(pin, t);
            let mode = self.mode_of(pin).cloned();
            // 51 的 P1~P3：高电平是弱上拉
            let weak_hi = self.key == "stc89";
            let od = od_pin(pin) || (self.key == "stc89" && self.is_p0(pin));
            match lv {
                Some(true) if !od => {
                    set_sw_r(&mut devs[pd.hi], true, self.spec.r_hi);
                    set_sw(&mut devs[pd.lo], false);
                }
                Some(false) => {
                    set_sw(&mut devs[pd.hi], false);
                    set_sw(&mut devs[pd.lo], true);
                }
                _ => {
                    set_sw(&mut devs[pd.hi], false);
                    set_sw(&mut devs[pd.lo], false);
                }
            }
            let running = self.running && !asleep;
            let pu = running
                && (matches!(mode, Some(PinMode::InPullUp))
                    || (weak_hi && !self.is_p0(pin) && !matches!(lv, Some(false)))
                    || (matches!(mode, Some(PinMode::I2cSda | PinMode::I2cScl)) && self.key != "bluepill" && self.key != "stc89"));
            set_sw(&mut devs[pd.pu], pu);
            set_sw(&mut devs[pd.pd], running && matches!(mode, Some(PinMode::InPullDown)));
        }
    }

    fn is_p0(&self, pin: usize) -> bool {
        self.names.iter().any(|(n, i)| *i == pin && n.starts_with("P0."))
    }

    /// 读引脚的逻辑电平
    fn read(&self, v: &[f64], pin_node: &[usize], pin: usize) -> Option<bool> {
        let n = *pin_node.get(pin)?;
        if n == NONE {
            return None;
        }
        let x = volt(v, n) - volt(v, pin_node[self.gnd]);
        if x > self.logic_v * 0.6 {
            Some(true)
        } else if x < self.logic_v * 0.3 {
            Some(false)
        } else {
            None
        }
    }

    fn adc(&self, v: &[f64], pin_node: &[usize], pin: usize) -> f64 {
        let n = pin_node.get(pin).copied().unwrap_or(NONE);
        (volt(v, n) - volt(v, pin_node[self.gnd])).clamp(0.0, self.logic_v)
    }

    pub fn adc_max(&self) -> u32 {
        if matches!(self.key.as_str(), "uno" | "nano" | "nodemcu") {
            1023
        } else {
            4095
        }
    }

    /// 每步结束：检查电源和引脚、跑规则。返回（过载比例和原因, 下一次事件）
    #[allow(clippy::too_many_arguments)]
    pub fn post(&mut self, devs: &[Dev], v: &[f64], pin_node: &[usize], part: &Part, t: f64, dt: f64, ctx: &mut McuCtx) -> (Option<(f64, String)>, Option<f64>) {
        let rail_v = volt(v, pin_node[self.rail]) - volt(v, pin_node[self.gnd]);
        let mut bad: Option<(f64, String)> = None;
        let mut over = |r: f64, w: String| {
            if bad.as_ref().is_none_or(|b| r > b.0) {
                bad = Some((r, w));
            }
        };
        // 电源：掉电复位
        if self.running && rail_v < self.spec.bod {
            self.running = false;
            self.resets += 1;
            ctx.log.push(crate::i18n::tf("电源电压掉到 {}，单片机复位", &[&super::fmt_si(rail_v, "V")]));
        } else if !self.running && rail_v > self.spec.bod + 0.15 {
            self.boot(t);
        }
        // 电源脚电压
        let p5 = self.names.get("5V").or(self.names.get("VBUS")).copied();
        if let Some(p) = p5 {
            let x = volt(v, pin_node[p]) - volt(v, pin_node[self.gnd]);
            over(x / 6.0, crate::i18n::tf("5V 引脚电压 {}，超过 5.5V 会损坏板子", &[&super::fmt_si(x, "V")]));
        }
        if let Some(p) = self.names.get("3V3").copied() {
            let x = volt(v, pin_node[p]) - volt(v, pin_node[self.gnd]);
            over(x / 3.9, crate::i18n::tf("3.3V 引脚接了 {}，会烧坏单片机", &[&super::fmt_si(x, "V")]));
        }
        // 引脚电流和钳位电流
        let mode = super::solver::Mode::Tran(dt);
        let sol_i = |d: &Dev| -> f64 {
            let nodes = d.nodes();
            match d {
                Dev::Sw { on, ron, .. } => {
                    if *on {
                        (volt(v, nodes[0]) - volt(v, nodes[1])) / ron
                    } else {
                        0.0
                    }
                }
                Dev::N { m: Nl::Diode(m), .. } => super::solver::diode_i(m, volt(v, nodes[0]) - volt(v, nodes[1])),
                _ => 0.0,
            }
        };
        let _ = mode;
        for (pin, pd) in &self.devs {
            let name = self.names.iter().find(|(_, i)| *i == pin).map(|(n, _)| n.clone()).unwrap_or_default();
            let io = sol_i(&devs[pd.hi]).abs().max(sol_i(&devs[pd.lo]).abs());
            over(
                io / (self.spec.imax * 1.5),
                crate::i18n::tf("引脚 {} 输出电流 {}，超过最大 {}", &[&name, &super::fmt_si(io, "A"), &super::fmt_si(self.spec.imax, "A")]),
            );
            if pd.cu != NONE {
                let ic = sol_i(&devs[pd.cu]);
                over(
                    ic / 0.01,
                    crate::i18n::tf("引脚 {} 被灌进 {}（外部电压比单片机电源高，可能是 5V 信号接进了 3.3V 引脚）", &[&name, &super::fmt_si(ic, "A")]),
                );
            }
            let icd = sol_i(&devs[pd.cd]);
            over(icd / 0.01, crate::i18n::tf("引脚 {} 电压低于地（负压）", &[&name]));
        }
        if !self.running {
            return (bad, None);
        }
        if t < self.asleep_until {
            return (bad, Some(self.asleep_until));
        }
        if self.asleep_until > 0.0 && t >= self.asleep_until {
            // 醒来：相当于复位
            self.asleep_until = 0.0;
            self.boot(t);
        }
        // 输入
        let mut levels: HashMap<usize, Option<bool>> = HashMap::new();
        for p in &self.cfg.pins {
            if let Some(i) = self.names.get(&p.pin) {
                levels.insert(*i, self.read(v, pin_node, *i));
            }
        }
        // ADC 读数放进变量
        for p in &self.cfg.pins {
            if p.mode == PinMode::Adc {
                if let Some(i) = self.names.get(&p.pin) {
                    let x = self.adc(v, pin_node, *i);
                    self.vars.insert(format!("adc:{}", p.pin), format!("{}", (x / self.logic_v * self.adc_max() as f64).round() as i64));
                    self.vars.insert(format!("v:{}", p.pin), format!("{x:.2}"));
                }
            }
        }
        // 串口收到的行
        let mut lines = vec![];
        while let Some(pos) = self.rx.iter().position(|b| *b == b'\n' || *b == b'\r') {
            let line: Vec<u8> = self.rx.drain(..=pos).collect();
            let s = String::from_utf8_lossy(&line).trim().to_string();
            if !s.is_empty() {
                lines.push(s);
            }
        }
        self.vars.insert("t".into(), format!("{:.1}", t - self.boot_t));
        let mut next: Option<f64> = None;
        let rules = self.cfg.rules.clone();
        let first = !self.started;
        self.started = true;
        for (ri, r) in rules.iter().enumerate() {
            let fire = match &r.when {
                Cond::Start => first,
                Cond::Every { ms } => {
                    let period = (*ms).max(1) as f64 / 1000.0;
                    let last = *self.last_every.entry(ri).or_insert(t);
                    let due = last + period;
                    next = Some(next.map_or(due, |n: f64| n.min(due)));
                    if t + 1e-9 >= due {
                        self.last_every.insert(ri, t);
                        true
                    } else {
                        false
                    }
                }
                Cond::Falling { pin } | Cond::Rising { pin } => {
                    let Some(i) = self.names.get(pin).copied() else { continue };
                    let now = self.read(v, pin_node, i);
                    let prev = self.last_in.get(&i).copied();
                    let hit = match (now, prev, &r.when) {
                        (Some(false), Some(true), Cond::Falling { .. }) => true,
                        (Some(true), Some(false), Cond::Rising { .. }) => true,
                        _ => false,
                    };
                    hit
                }
                Cond::WhileLow { pin } | Cond::WhileHigh { pin } => {
                    let Some(i) = self.names.get(pin).copied() else { continue };
                    let want = matches!(r.when, Cond::WhileHigh { .. });
                    let ok = self.read(v, pin_node, i) == Some(want);
                    let last = self.last_while.get(&ri).copied().unwrap_or(-1.0);
                    if ok && t - last >= 0.02 {
                        self.last_while.insert(ri, t);
                        true
                    } else {
                        false
                    }
                }
                Cond::Above { pin, v: th } | Cond::Below { pin, v: th } => {
                    let Some(i) = self.names.get(pin).copied() else { continue };
                    let x = self.adc(v, pin_node, i);
                    let above = matches!(r.when, Cond::Above { .. });
                    let now = if above { x > *th as f64 } else { x < *th as f64 };
                    // 条件成立的那一刻触发一次；之后要先不成立才会再触发
                    let was = self.cond_state.insert(ri, now).unwrap_or(false);
                    now && !was
                }
                Cond::Recv { text } => lines.iter().any(|l| l.eq_ignore_ascii_case(text.trim())),
                Cond::TempAbove { c } | Cond::TempBelow { c } => {
                    let x: f64 = self.vars.get("temp").and_then(|s| s.parse().ok()).unwrap_or(f64::NAN);
                    let now = if matches!(r.when, Cond::TempAbove { .. }) { x > *c as f64 } else { x < *c as f64 };
                    let was = self.cond_state.insert(ri, now).unwrap_or(false);
                    now && !was
                }
                Cond::DistBelow { cm } => {
                    let x: f64 = self.vars.get("dist").and_then(|s| s.parse().ok()).unwrap_or(f64::NAN);
                    let now = x < *cm as f64;
                    let was = self.cond_state.insert(ri, now).unwrap_or(false);
                    now && !was
                }
            };
            if fire {
                for a in &r.then {
                    self.act(a, v, pin_node, part, t, ctx);
                }
            }
        }
        // 记下输入电平（下一步判断边沿）
        for (i, lv) in levels {
            if let Some(l) = lv {
                self.last_in.insert(i, l);
            }
        }
        // 输出边沿 / 排好的事件
        let mut ev = next;
        for (at, _, _) in &self.sched {
            ev = Some(ev.map_or(*at, |e: f64| e.min(*at)));
        }
        (bad, ev)
    }

    /// 下一次引脚要变化的时刻
    pub fn next_event(&self, t: f64) -> f64 {
        if !self.running {
            return f64::INFINITY;
        }
        if t < self.asleep_until {
            return self.asleep_until;
        }
        let mut e = f64::INFINITY;
        for (at, _, _) in &self.sched {
            e = e.min(*at);
        }
        let lt = t - self.boot_t;
        let mut edge = |period: f64, phase_frac: &[f64], base: f64| {
            if period <= 0.0 {
                return;
            }
            let x = (t - base) / period;
            let k = x.floor();
            for f in phase_frac {
                let at = base + (k + f) * period;
                if at > t + 1e-12 {
                    e = e.min(at);
                }
                let at2 = base + (k + 1.0 + f) * period;
                e = e.min(at2);
            }
        };
        for p in &self.cfg.pins {
            match &p.mode {
                PinMode::Blink { ms } => edge(*ms as f64 / 1000.0, &[0.0], self.boot_t),
                PinMode::Seq { ms, .. } => edge(*ms as f64 / 1000.0, &[0.0], self.boot_t),
                PinMode::Pwm { duty, freq } => edge(1.0 / (*freq).max(1) as f64, &[0.0, *duty as f64 / 100.0], 0.0),
                PinMode::Breath { .. } => edge(1.0 / self.pwm_freq() as f64, &[0.0], 0.0),
                PinMode::Tone { freq } => edge(1.0 / (*freq).max(1) as f64, &[0.0, 0.5], 0.0),
                PinMode::Servo { angle } => edge(0.02, &[0.0, servo_width(*angle) / 0.02], 0.0),
                _ => {}
            }
        }
        for d in self.pwm_over.values() {
            edge(1.0 / self.pwm_freq() as f64, &[0.0, *d as f64 / 100.0], 0.0);
        }
        for a in self.servo_over.values() {
            edge(0.02, &[0.0, servo_width(*a) / 0.02], 0.0);
        }
        let mut ends = vec![];
        for (f, until) in self.tone_until.values() {
            if *until > t {
                edge(1.0 / (*f).max(1) as f64, &[0.0, 0.5], 0.0);
                ends.push(*until);
            }
        }
        for u in ends {
            e = e.min(u);
        }
        // 呼吸灯：PWM 周期内的下降沿近似
        if self.cfg.pins.iter().any(|p| matches!(p.mode, PinMode::Breath { .. })) {
            e = e.min(t + 1.0 / self.pwm_freq() as f64 / 8.0);
        }
        let _ = lt;
        e
    }

    /// 串口接收脚、地脚（找不到返回 None）
    pub fn rx_pin(&self) -> Option<usize> {
        self.cfg_pin(&PinMode::UartRx).or_else(|| self.default_pin(cap::RX))
    }

    pub fn gnd_pin(&self) -> usize {
        self.gnd
    }

    pub fn feed_rx(&mut self, data: &[u8]) {
        self.rx.extend_from_slice(data);
        if self.rx.len() > 4096 {
            let cut = self.rx.len() - 4096;
            self.rx.drain(..cut);
        }
    }

    /// 模板替换：{count} {adc:A0} {v:A0} {temp} {hum} {dist} {t}
    pub fn fill(&self, s: &str) -> String {
        let mut out = String::new();
        let mut rest = s;
        while let Some(a) = rest.find('{') {
            out.push_str(&rest[..a]);
            let after = &rest[a + 1..];
            if let Some(b) = after.find('}') {
                let key = &after[..b];
                match key {
                    "count" => out.push_str(&self.count.to_string()),
                    _ => match self.vars.get(key) {
                        Some(v) => out.push_str(v),
                        None => {
                            out.push('{');
                            out.push_str(key);
                            out.push('}');
                        }
                    },
                }
                rest = &after[b + 1..];
            } else {
                out.push_str(&rest[a..]);
                rest = "";
            }
        }
        out.push_str(rest);
        out
    }

    fn act(&mut self, a: &Act, v: &[f64], pin_node: &[usize], part: &Part, t: f64, ctx: &mut McuCtx) {
        let pin = |s: &McuRt, n: &str| s.names.get(n).copied();
        match a {
            Act::Set { pin: p, high } => {
                if let Some(i) = pin(self, p) {
                    self.pwm_over.remove(&i);
                    self.out.insert(i, *high);
                }
            }
            Act::Toggle { pin: p } => {
                if let Some(i) = pin(self, p) {
                    let cur = self.level(i, t).unwrap_or(false);
                    self.pwm_over.remove(&i);
                    self.out.insert(i, !cur);
                }
            }
            Act::PwmSet { pin: p, duty } => {
                if let Some(i) = pin(self, p) {
                    self.pwm_over.insert(i, duty.clamp(0.0, 100.0));
                }
            }
            Act::PwmFromAdc { pin: p, adc } => {
                if let (Some(i), Some(k)) = (pin(self, p), pin(self, adc)) {
                    let x = self.adc(v, pin_node, k) / self.logic_v;
                    self.pwm_over.insert(i, (x * 100.0) as f32);
                }
            }
            Act::ServoSet { pin: p, angle } => {
                if let Some(i) = pin(self, p) {
                    self.servo_over.insert(i, angle.clamp(0.0, 180.0));
                }
            }
            Act::ServoFromAdc { pin: p, adc } => {
                if let (Some(i), Some(k)) = (pin(self, p), pin(self, adc)) {
                    let x = self.adc(v, pin_node, k) / self.logic_v;
                    self.servo_over.insert(i, (x * 180.0) as f32);
                }
            }
            Act::Tone { pin: p, freq, ms } => {
                if let Some(i) = pin(self, p) {
                    self.tone_until.insert(i, (*freq, t + *ms as f64 / 1000.0));
                }
            }
            Act::Print { text } => {
                let mut s = self.fill(text);
                s.push_str("\r\n");
                self.print(s.as_bytes(), part, pin_node, t, ctx);
            }
            Act::Count => self.count += 1,
            Act::Shift { data, clk, latch, value } => {
                let (Some(d), Some(c), Some(l)) = (pin(self, data), pin(self, clk), pin(self, latch)) else {
                    return;
                };
                let val: u32 = self.fill(value).trim().parse().unwrap_or(0);
                let bits = if val > 0xff { 16 } else { 8 };
                // 每个位 20 µs：数据先准备好，再给一个时钟上升沿；最后锁存
                let half = 10e-6;
                let mut at = t + half;
                self.sched.push((at, l, false));
                for k in (0..bits).rev() {
                    let bit = val >> k & 1 == 1;
                    self.sched.push((at, d, bit));
                    self.sched.push((at, c, false));
                    at += half;
                    self.sched.push((at, c, true));
                    at += half;
                }
                self.sched.push((at, c, false));
                self.sched.push((at, l, true));
                self.sched.push((at + half, l, false));
            }
            Act::Seg { pins, value, common_anode } => {
                let n: usize = self.fill(value).trim().parse().unwrap_or(0) % 10;
                const SEG: [u8; 10] = [0x3f, 0x06, 0x5b, 0x4f, 0x66, 0x6d, 0x7d, 0x07, 0x7f, 0x6f];
                for (k, p) in pins.iter().enumerate().take(8) {
                    if let Some(i) = pin(self, p) {
                        let on = SEG[n] >> k & 1 == 1;
                        self.out.insert(i, on != *common_anode);
                    }
                }
            }
            Act::Tm1637 { clk, dio, text } => {
                let (Some(c), Some(d)) = (pin(self, clk), pin(self, dio)) else { return };
                let txt = self.fill(text);
                let nets = (net_of(ctx.nl, part.id, c), net_of(ctx.nl, part.id, d));
                let found = find_parts(ctx.c, ctx.nl, &["tm1637"], &[("CLK", nets.0), ("DIO", nets.1)]);
                match found.first() {
                    Some(id) => ctx.effects.push(Effect::Digits(*id, txt)),
                    None => ctx.warn(crate::i18n::t("TM1637 没有响应：检查 CLK、DIO 接线")),
                }
            }
            Act::Show { text } => {
                let txt = self.fill(text);
                let lines: Vec<String> = txt.split("\\n").map(|s| s.to_string()).collect();
                match self.find_display(part, pin_node, v, ctx) {
                    Ok(ids) => {
                        for id in ids {
                            ctx.effects.push(Effect::Lines(id, lines.clone()));
                        }
                    }
                    Err(e) => {
                        ctx.warn(&e);
                        let mut s = e;
                        s.push_str("\r\n");
                        self.print(s.as_bytes(), part, pin_node, t, ctx);
                    }
                }
            }
            Act::ShowImage => {
                if let Ok(ids) = self.find_display(part, pin_node, v, ctx) {
                    for id in ids {
                        ctx.effects.push(Effect::Image(id, "sample".into()));
                    }
                }
            }
            Act::ReadDht { pin: p } => {
                let Some(i) = pin(self, p) else { return };
                let net = net_of(ctx.nl, part.id, i);
                let found = find_parts(ctx.c, ctx.nl, &["dht11"], &[("DATA", net)]);
                let ok = found.iter().find(|id| module_ok(ctx, **id, part, self.gnd));
                match ok.and_then(|id| ctx.c.part(*id)) {
                    Some(s) => {
                        self.vars.insert("temp".into(), format!("{:.0}", s.num("temp")));
                        self.vars.insert("hum".into(), format!("{:.0}", s.num("hum")));
                        // 单总线的波形：起始信号 + 40 位
                        ctx.overlay_bits(net, t, &dht_bits(s.num("hum"), s.num("temp")), 80e-6);
                    }
                    None => {
                        self.vars.insert("temp".into(), "NaN".into());
                        self.vars.insert("hum".into(), "NaN".into());
                        ctx.warn(crate::i18n::t("DHT11 读取失败：检查 DATA 接线、供电和上拉"));
                    }
                }
            }
            Act::ReadSht => {
                let (sda, scl) = self.i2c_nets(part, ctx.nl);
                let found = find_parts(ctx.c, ctx.nl, &["sht30"], &[("SDA", sda), ("SCL", scl)]);
                let pulled = self.i2c_pulled(v, pin_node, ctx.nl, part);
                match found.iter().find(|id| module_ok(ctx, **id, part, self.gnd)).and_then(|id| ctx.c.part(*id)) {
                    Some(s) if pulled => {
                        self.vars.insert("temp".into(), format!("{:.1}", s.num("temp")));
                        self.vars.insert("hum".into(), format!("{:.1}", s.num("hum")));
                    }
                    _ => {
                        self.vars.insert("temp".into(), "NaN".into());
                        ctx.warn(crate::i18n::t("SHT30 没有应答：检查 I2C 接线和上拉电阻"));
                    }
                }
            }
            Act::Measure { trig, echo } => {
                let (Some(tr), Some(ec)) = (pin(self, trig), pin(self, echo)) else { return };
                let (nt, ne) = (net_of(ctx.nl, part.id, tr), net_of(ctx.nl, part.id, ec));
                let found = find_parts(ctx.c, ctx.nl, &["hcsr04"], &[("TRIG", nt), ("ECHO", ne)]);
                match found.iter().find(|id| module_ok(ctx, **id, part, self.gnd)).and_then(|id| ctx.c.part(*id)) {
                    Some(s) => {
                        let d = s.num("dist");
                        self.vars.insert("dist".into(), format!("{d:.1}"));
                        ctx.effects.push(Effect::Pulse(s.id, d * 58e-6));
                        // TRIG 的 10 µs 脉冲
                        self.sched.push((t + 1e-6, tr, true));
                        self.sched.push((t + 11e-6, tr, false));
                    }
                    None => {
                        self.vars.insert("dist".into(), "NaN".into());
                        ctx.warn(crate::i18n::t("超声波没有回波：检查 TRIG、ECHO 接线和 5V 供电"));
                    }
                }
            }
            Act::Mqtt { host, port, topic, payload } => {
                if self.has_wifi() {
                    ctx.effects.push(Effect::Net(NetReq::Mqtt {
                        host: host.clone(),
                        port: *port,
                        topic: self.fill(topic),
                        payload: self.fill(payload),
                    }));
                } else {
                    ctx.warn(crate::i18n::t("这块板子没有 WiFi，需要外接 WiFi 或 4G 模块"));
                }
            }
            Act::Http { url, body } => {
                if self.has_wifi() {
                    ctx.effects.push(Effect::Net(NetReq::Http {
                        url: self.fill(url),
                        body: self.fill(body),
                    }));
                } else {
                    ctx.warn(crate::i18n::t("这块板子没有 WiFi，需要外接 WiFi 或 4G 模块"));
                }
            }
            Act::Tcp { host, port, text } => {
                if self.has_wifi() {
                    ctx.effects.push(Effect::Net(NetReq::Tcp {
                        host: host.clone(),
                        port: *port,
                        text: self.fill(text),
                    }));
                } else {
                    ctx.warn(crate::i18n::t("这块板子没有 WiFi，需要外接 WiFi 或 4G 模块"));
                }
            }
            Act::Photo => {
                if self.key == "esp32cam" {
                    ctx.effects.push(Effect::Photo(part.id));
                } else {
                    ctx.warn(crate::i18n::t("拍照需要 ESP32-CAM"));
                }
            }
            Act::Sleep { ms } => {
                self.asleep_until = t + *ms as f64 / 1000.0;
                self.out.clear();
                self.pwm_over.clear();
            }
        }
    }

    fn has_wifi(&self) -> bool {
        self.key.starts_with("esp") || self.key == "nodemcu"
    }

    /// 串口打印：USB 串口（开发板自带的）和 TX 引脚各发一份
    fn print(&mut self, data: &[u8], part: &Part, pin_node: &[usize], t: f64, ctx: &mut McuCtx) {
        if self.usb_serial && part.prop("power") == "usb" {
            self.host_out.extend_from_slice(data);
        }
        let tx = self
            .cfg
            .pins
            .iter()
            .find(|p| p.mode == PinMode::UartTx)
            .and_then(|p| self.names.get(&p.pin).copied())
            .or_else(|| self.default_pin(cap::TX));
        if let Some(i) = tx {
            if pin_node.get(i).is_some_and(|n| *n != NONE) {
                let net = net_of(ctx.nl, part.id, i);
                let gnd = net_of(ctx.nl, part.id, self.gnd);
                ctx.uart.push(UartTx {
                    from: part.id,
                    net,
                    gnd,
                    baud: self.cfg.baud,
                    data: data.to_vec(),
                    t,
                });
                ctx.overlay_uart(net, t, self.cfg.baud, data);
            }
        }
    }

    /// 带某能力的第一个引脚（默认串口、I2C 引脚）
    pub fn default_pin(&self, c: u32) -> Option<usize> {
        let def = super::lib::def(&self.key)?;
        // 串口优先用名字里带 TX / RX 的（ESP32 的 TX/RX、Uno 的 D0/D1）
        def.pins.iter().position(|p| p.caps & c != 0 && p.caps & cap::IO != 0)
    }

    fn cfg_pin(&self, m: &PinMode) -> Option<usize> {
        self.cfg.pins.iter().find(|p| &p.mode == m).and_then(|p| self.names.get(&p.pin).copied())
    }

    fn i2c_pins(&self) -> (Option<usize>, Option<usize>) {
        (
            self.cfg_pin(&PinMode::I2cSda).or_else(|| self.default_pin(cap::SDA)),
            self.cfg_pin(&PinMode::I2cScl).or_else(|| self.default_pin(cap::SCL)),
        )
    }

    fn i2c_nets(&self, part: &Part, nl: &Netlist) -> (usize, usize) {
        let (a, b) = self.i2c_pins();
        (a.map_or(NONE, |p| net_of(nl, part.id, p)), b.map_or(NONE, |p| net_of(nl, part.id, p)))
    }

    /// I2C 线上有没有上拉（空闲时电压够高）
    fn i2c_pulled(&self, v: &[f64], pin_node: &[usize], _nl: &Netlist, _part: &Part) -> bool {
        let (a, b) = self.i2c_pins();
        let hi = |p: Option<usize>| p.is_some_and(|p| self.adc(v, pin_node, p) > self.logic_v * 0.7);
        hi(a) && hi(b)
    }

    /// 找接在本板 I2C / SPI 上的屏幕
    fn find_display(&self, part: &Part, pin_node: &[usize], v: &[f64], ctx: &McuCtx) -> Result<Vec<u32>, String> {
        let (sda, scl) = self.i2c_nets(part, ctx.nl);
        let mut ids = vec![];
        let i2c = find_parts(ctx.c, ctx.nl, &["oled", "lcd1602"], &[("SDA", sda), ("SCL", scl)]);
        if !i2c.is_empty() {
            if !self.i2c_pulled(v, pin_node, ctx.nl, part) {
                return Err(crate::i18n::t("I2C 没有应答（NACK）：SDA、SCL 需要上拉电阻").into());
            }
            // 地址冲突
            let mut seen: HashMap<String, u32> = HashMap::new();
            for id in &i2c {
                let p = ctx.c.part(*id).unwrap();
                let addr = p.prop("addr").to_string();
                if seen.insert(addr.clone(), *id).is_some() {
                    return Err(crate::i18n::tf("两个 I2C 设备地址都是 {}，冲突了", &[&addr]));
                }
            }
            ids.extend(i2c.into_iter().filter(|id| module_ok(ctx, *id, part, self.gnd)));
        }
        // SPI 屏：SCK、MOSI 接在本板引脚上，DC 也接了
        let mine: HashSet<usize> = (0..pin_node.len()).map(|i| net_of(ctx.nl, part.id, i)).filter(|n| *n != NONE).collect();
        for sp in &ctx.c.parts {
            let Some(d) = super::lib::def(&sp.kind) else { continue };
            if !matches!(sp.kind.as_str(), "oled_spi" | "st7735" | "st7789" | "ili9341" | "epaper") {
                continue;
            }
            let on = |names: &[&str]| names.iter().any(|n| d.pin(n).is_some_and(|i| ctx.nl.net_of(sp.id, i).is_some_and(|x| mine.contains(&x))));
            if on(&["SCL", "SCK", "D0"]) && on(&["SDA", "SDI", "D1"]) && on(&["DC"]) && module_ok(ctx, sp.id, part, self.gnd) {
                ids.push(sp.id);
            }
        }
        if ids.is_empty() {
            return Err(crate::i18n::t("找不到屏幕：检查屏幕的供电、地和信号线").into());
        }
        Ok(ids)
    }

    pub fn status(&self) -> String {
        if let Some(f) = &self.fault {
            return f.clone();
        }
        if !self.running {
            return crate::i18n::t("未上电").into();
        }
        if self.asleep_until > 0.0 {
            return crate::i18n::t("睡眠中").into();
        }
        crate::i18n::t("运行中").into()
    }
}

fn pwm_level(duty: f32, freq: u32, t: f64) -> bool {
    let d = duty as f64 / 100.0;
    if d <= 0.0 {
        return false;
    }
    if d >= 1.0 {
        return true;
    }
    (t * freq.max(1) as f64).fract() < d
}

fn servo_width(angle: f32) -> f64 {
    0.5e-3 + angle.clamp(0.0, 180.0) as f64 / 180.0 * 2.0e-3
}

fn servo_level(angle: f32, t: f64) -> bool {
    (t % 0.02) < servo_width(angle)
}

/// DHT11 的 40 位数据：湿度整数、小数、温度整数、小数、校验
fn dht_bits(hum: f64, temp: f64) -> Vec<bool> {
    let b = [hum as u8, 0, temp as u8, 0];
    let sum = b.iter().fold(0u8, |a, x| a.wrapping_add(*x));
    let mut v = vec![];
    for byte in b.iter().chain(std::iter::once(&sum)) {
        for k in (0..8).rev() {
            v.push(byte >> k & 1 == 1);
        }
    }
    v
}

pub fn net_of(nl: &Netlist, id: u32, pin: usize) -> usize {
    nl.net_of(id, pin).unwrap_or(NONE)
}

/// 找某些类型的元件：它们的指定引脚分别接在指定网络上
pub fn find_parts(c: &Circuit, nl: &Netlist, kinds: &[&str], pins: &[(&str, usize)]) -> Vec<u32> {
    let mut out = vec![];
    for p in &c.parts {
        if !kinds.contains(&p.kind.as_str()) {
            continue;
        }
        let Some(d) = super::lib::def(&p.kind) else { continue };
        let ok = pins.iter().all(|(name, net)| *net != NONE && d.pin(name).is_some_and(|i| nl.net_of(p.id, i) == Some(*net)));
        if ok {
            out.push(p.id);
        }
    }
    out
}

/// 模块是否在工作：供电电压够、和单片机共地
fn module_ok(ctx: &McuCtx, id: u32, board: &Part, board_gnd: usize) -> bool {
    let Some(p) = ctx.c.part(id) else { return false };
    let Some(d) = super::lib::def(&p.kind) else { return false };
    let (vcc, gnd) = super::sim::module_pwr_pins(d);
    let (Some(vp), Some(gp)) = (vcc, gnd) else { return true };
    let v = |pin: usize| ctx.nl.net_of(id, pin).and_then(|n| (ctx.net_v)(n)).unwrap_or(0.0);
    let (vmin, _, _) = models::module_power(&p.kind);
    let powered = v(vp) - v(gp) > vmin * 0.9;
    let common = ctx.nl.net_of(id, gp) == ctx.nl.net_of(board.id, board_gnd);
    powered && common && ctx.burnt.get(&id).is_none()
}

/// 规则执行时能看到的电路
pub struct McuCtx<'a> {
    pub c: &'a Circuit,
    pub nl: &'a Netlist,
    pub net_v: &'a dyn Fn(usize) -> Option<f64>,
    pub burnt: &'a HashSet<u32>,
    pub uart: &'a mut Vec<UartTx>,
    pub effects: &'a mut Vec<Effect>,
    pub log: &'a mut Vec<String>,
    pub overlay: &'a mut HashMap<usize, super::sim::Overlay>,
}

impl McuCtx<'_> {
    fn warn(&mut self, s: &str) {
        if self.log.last().map(|x| x.as_str()) != Some(s) {
            self.log.push(s.to_string());
        }
    }

    /// 串口发送的波形：空闲高，每字节 起始位 0 + 8 位（低位在前）+ 停止位 1
    fn overlay_uart(&mut self, net: usize, t: f64, baud: u32, data: &[u8]) {
        if net == NONE || baud == 0 {
            return;
        }
        let bit = 1.0 / baud as f64;
        let o = self.overlay.entry(net).or_default();
        // 接在上一段后面，别重叠
        let mut at = o.edges.last().map_or(t, |e| e.0.max(t));
        let mut level = true;
        for b in data {
            let bits = std::iter::once(false).chain((0..8).map(|k| b >> k & 1 == 1)).chain(std::iter::once(true));
            for v in bits {
                if v != level {
                    o.edges.push((at, v));
                    level = v;
                }
                at += bit;
            }
        }
        if !level {
            o.edges.push((at, true));
        }
    }

    /// 给示波器补一段数字波形
    fn overlay_bits(&mut self, net: usize, t: f64, bits: &[bool], bit_t: f64) {
        if net == NONE {
            return;
        }
        let o = self.overlay.entry(net).or_default();
        let mut at = t;
        o.edges.push((at, false));
        at += 18e-3_f64.min(bit_t * 50.0);
        o.edges.push((at, true));
        for b in bits {
            at += 50e-6;
            o.edges.push((at, false));
            at += 50e-6;
            o.edges.push((at, true));
            at += if *b { 70e-6 } else { 26e-6 };
        }
        o.edges.push((at, false));
        o.edges.push((at + 50e-6, true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template() {
        let part = Part {
            kind: "uno".into(),
            ..Default::default()
        };
        let mut rt = McuRt::new(&part, super::super::lib::def("uno").unwrap());
        rt.vars.insert("temp".into(), "26".into());
        rt.count = 3;
        assert_eq!(rt.fill("T={temp} n={count} x={nope}"), "T=26 n=3 x={nope}");
        assert!(pwm_level(50.0, 1000, 0.0002));
        assert!(!pwm_level(50.0, 1000, 0.0007));
        assert!(servo_level(90.0, 0.001));
        assert!(!servo_level(90.0, 0.0016));
        assert_eq!(dht_bits(55.0, 26.0).len(), 40);
    }
}
