//! 元件库：每种元件的引脚、外形、参数和分类。仿真模型在 sim.rs，画法在 circuitview/draw.rs，
//! 名字、知识卡在 assets/circuit/content.tsv（键 name.<key>、card.<key>）。

use std::collections::HashMap;
use std::sync::OnceLock;

use super::Pt;

/// 引脚能力（单片机引脚、模块引脚），接错检查和引脚设置用
pub mod cap {
    pub const IO: u32 = 1;
    /// 耐 5V
    pub const FT: u32 = 1 << 1;
    pub const ADC: u32 = 1 << 2;
    pub const PWM: u32 = 1 << 3;
    pub const TX: u32 = 1 << 4;
    pub const RX: u32 = 1 << 5;
    pub const SDA: u32 = 1 << 6;
    pub const SCL: u32 = 1 << 7;
    pub const MOSI: u32 = 1 << 8;
    pub const MISO: u32 = 1 << 9;
    pub const SCK: u32 = 1 << 10;
    pub const CS: u32 = 1 << 11;
    /// 启动相关（ESP32 GPIO0/2/12/15 等），不要乱接
    pub const BOOT: u32 = 1 << 12;
    /// 只能输入
    pub const INONLY: u32 = 1 << 13;
    /// 电源脚（输出或输入）
    pub const PWR: u32 = 1 << 14;
    pub const GND: u32 = 1 << 15;
    pub const DAC: u32 = 1 << 16;
    /// 板载 LED 接在这个脚上
    pub const LED: u32 = 1 << 17;
    /// 开漏（51 的 P0 口）
    pub const OD: u32 = 1 << 18;
    pub const VIN: u32 = 1 << 19;
    pub const RST: u32 = 1 << 20;
    pub const V5: u32 = 1 << 21;
    pub const V33: u32 = 1 << 22;
    /// 模块的信号输入（接错检查：两个输出不能接一起、输入不能悬空）
    pub const SIN: u32 = 1 << 23;
    /// 模块的信号输出
    pub const SOUT: u32 = 1 << 24;
}

#[derive(Clone, Debug)]
pub struct PinDef {
    pub name: String,
    pub at: Pt,
    pub caps: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Cat {
    Board,
    Power,
    PowerIc,
    Passive,
    Diode,
    Bjt,
    Mos,
    Input,
    Output,
    Display,
    Camera,
    Iot,
    Chip,
    Module,
    Mcu,
    Debug,
    Meter,
}

impl Cat {
    pub const ALL: [Cat; 17] = [
        Cat::Board,
        Cat::Power,
        Cat::PowerIc,
        Cat::Passive,
        Cat::Diode,
        Cat::Bjt,
        Cat::Mos,
        Cat::Input,
        Cat::Output,
        Cat::Display,
        Cat::Camera,
        Cat::Iot,
        Cat::Chip,
        Cat::Module,
        Cat::Mcu,
        Cat::Debug,
        Cat::Meter,
    ];

    /// 中文名（界面上经 t() 翻译）
    pub fn zh(self) -> &'static str {
        match self {
            Cat::Board => "面包板",
            Cat::Power => "电源",
            Cat::PowerIc => "电源芯片",
            Cat::Passive => "无源元件",
            Cat::Diode => "二极管",
            Cat::Bjt => "三极管",
            Cat::Mos => "MOS 管",
            Cat::Input => "输入",
            Cat::Output => "输出",
            Cat::Display => "显示屏",
            Cat::Camera => "摄像头",
            Cat::Iot => "物联网通信",
            Cat::Chip => "常用芯片",
            Cat::Module => "模块",
            Cat::Mcu => "开发板",
            Cat::Debug => "调试工具",
            Cat::Meter => "仪器",
        }
    }
}

/// 外形（画法）。具体怎么画在界面层
#[derive(Clone, Debug, PartialEq)]
pub enum Look {
    Breadboard {
        cols: i32,
    },
    Resistor,
    Diode,
    Zener,
    Inductor,
    Bead,
    Fuse,
    Cap,
    ECap,
    Led,
    LedRgb,
    To92,
    To220,
    Sot23,
    /// 双列直插：上排从右往左、下排从左往右（和芯片引脚编号一致）
    Dip {
        label: &'static str,
    },
    Battery,
    UsbPower,
    Psu,
    SigGen,
    Ground,
    Button,
    Slide,
    DipSwitch,
    Pot,
    Ldr,
    Ntc,
    Encoder,
    Buzzer,
    Relay,
    Motor,
    Servo,
    Seg7,
    Lamp,
    Bridge,
    Multimeter,
    Ammeter,
    PowerMeter,
    Probe,
    Logic,
    /// 一块 PCB（模块、开发板）：底色、标题、特征
    Pcb {
        color: [u8; 3],
        title: &'static str,
        feature: Feature,
    },
}

/// 模块板子上的特征图案（画在 PCB 上，便于认出来）
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Feature {
    None,
    /// 屏幕：宽、高（格），像素宽高
    Screen {
        w: f32,
        h: f32,
        px: u16,
        py: u16,
        color: bool,
    },
    Lcd1602,
    Seg4,
    Camera,
    Antenna,
    /// 金属屏蔽罩（WiFi / 4G 模组）
    Shield,
    Usb,
    Chip,
    Sensor,
    Ultrasonic,
    Relay,
    Terminal,
    Rj45,
}

#[derive(Clone, Debug)]
pub enum PropKind {
    /// 数值（带单位）
    Num(&'static str),
    /// 选项（值是代码，显示时经 content 翻译）
    Choice(&'static [&'static str]),
    /// 滑块：最小、最大、单位、是否对数；通电时也能拖
    Slider(f64, f64, &'static str, bool),
    Bool,
    Text,
}

#[derive(Clone, Debug)]
pub struct PropDef {
    /// 中文标签
    pub label: &'static str,
    pub default: &'static str,
    pub kind: PropKind,
}

#[derive(Clone, Debug)]
pub struct PartDef {
    pub key: &'static str,
    /// 中文名（其它语言见 content.tsv 的 name.<key>）
    pub zh: &'static str,
    pub cat: Cat,
    /// 自动编号前缀：R、C、D、LED、Q、U……
    pub prefix: &'static str,
    pub pins: Vec<PinDef>,
    /// 外形范围（局部格坐标）：x0, y0, x1, y1
    pub body: [f32; 4],
    pub look: Look,
    /// 两脚元件：第二脚可以拖动拉长（电阻、二极管、电感……）
    pub stretch: bool,
    pub props: Vec<(&'static str, PropDef)>,
    /// 额外搜索词（英文型号等）
    pub kw: &'static str,
}

fn p(name: &str, x: i32, y: i32, caps: u32) -> PinDef {
    PinDef { name: name.into(), at: (x, y), caps }
}

/// 一排引脚：从 (x0, y) 开始向右
fn row(names: &[&str], x0: i32, y: i32) -> Vec<PinDef> {
    names.iter().enumerate().map(|(i, n)| spec(n, x0 + i as i32, y)).collect()
}

/// 引脚说明：「PA9/io.ft.tx」→ 名字 + 能力
fn spec(s: &str, x: i32, y: i32) -> PinDef {
    let (name, caps) = s.split_once('/').unwrap_or((s, ""));
    let mut c = 0;
    for w in caps.split('.') {
        c |= match w {
            "io" => cap::IO,
            "ft" => cap::FT,
            "adc" => cap::ADC,
            "pwm" => cap::PWM,
            "tx" => cap::TX,
            "rx" => cap::RX,
            "sda" => cap::SDA,
            "scl" => cap::SCL,
            "mosi" => cap::MOSI,
            "miso" => cap::MISO,
            "sck" => cap::SCK,
            "cs" => cap::CS,
            "boot" => cap::BOOT,
            "in" => cap::INONLY,
            "pwr" => cap::PWR,
            "gnd" => cap::GND,
            "dac" => cap::DAC,
            "led" => cap::LED,
            "od" => cap::OD,
            "vin" => cap::VIN | cap::PWR,
            "rst" => cap::RST,
            "5v" => cap::V5 | cap::PWR,
            "3v3" => cap::V33 | cap::PWR,
            "si" => cap::SIN,
            "so" => cap::SOUT,
            _ => 0,
        };
    }
    // 名字本身能看出来的
    let up = name.to_ascii_uppercase();
    if up.starts_with("GND") || up == "VSS" {
        c |= cap::GND;
    }
    PinDef {
        name: name.into(),
        at: (x, y),
        caps: c,
    }
}

fn num(label: &'static str, default: &'static str, unit: &'static str) -> PropDef {
    PropDef {
        label,
        default,
        kind: PropKind::Num(unit),
    }
}

fn choice(label: &'static str, default: &'static str, opts: &'static [&'static str]) -> PropDef {
    PropDef {
        label,
        default,
        kind: PropKind::Choice(opts),
    }
}

fn slider(label: &'static str, default: &'static str, lo: f64, hi: f64, unit: &'static str, log: bool) -> PropDef {
    PropDef {
        label,
        default,
        kind: PropKind::Slider(lo, hi, unit, log),
    }
}

fn boolean(label: &'static str, default: &'static str) -> PropDef {
    PropDef { label, default, kind: PropKind::Bool }
}

fn text(label: &'static str, default: &'static str) -> PropDef {
    PropDef { label, default, kind: PropKind::Text }
}

struct B(PartDef);

impl B {
    fn new(key: &'static str, zh: &'static str, cat: Cat, prefix: &'static str, look: Look) -> B {
        B(PartDef {
            key,
            zh,
            cat,
            prefix,
            pins: vec![],
            body: [-0.5, -0.5, 0.5, 0.5],
            look,
            stretch: false,
            props: vec![],
            kw: "",
        })
    }
    fn pins(mut self, v: Vec<PinDef>) -> B {
        self.0.pins = v;
        self
    }
    fn body(mut self, x0: f32, y0: f32, x1: f32, y1: f32) -> B {
        self.0.body = [x0, y0, x1, y1];
        self
    }
    fn stretch(mut self) -> B {
        self.0.stretch = true;
        self
    }
    fn prop(mut self, k: &'static str, d: PropDef) -> B {
        self.0.props.push((k, d));
        self
    }
    fn kw(mut self, k: &'static str) -> B {
        self.0.kw = k;
        self
    }
}

/// 模块：一排排针在 y=0，板子画在上面（负 y）
fn module(key: &'static str, zh: &'static str, cat: Cat, title: &'static str, color: [u8; 3], pins: &[&str], h: f32, feature: Feature) -> B {
    let n = pins.len() as f32;
    let w = n.max(4.0);
    let x0 = -(w - n) / 2.0 - 0.7;
    B::new(key, zh, cat, "U", Look::Pcb { color, title, feature }).pins(row(pins, 0, 0)).body(x0, -h, x0 + w + 0.4, 0.6)
}

/// 开发板：上下两排引脚，间距 gap 格（能跨在面包板中缝上）；body 包住两排
fn board2(key: &'static str, zh: &'static str, title: &'static str, color: [u8; 3], top: &[&str], bottom: &[&str], gap: i32, feature: Feature) -> B {
    let mut pins = row(top, 0, 0);
    pins.extend(row(bottom, 0, gap));
    let n = top.len().max(bottom.len()) as f32;
    B::new(key, zh, Cat::Mcu, "U", Look::Pcb { color, title, feature })
        .pins(pins)
        .body(-2.4, -1.0, n + 0.4, gap as f32 + 1.0)
}

const PCB_BLUE: [u8; 3] = [0x1d, 0x4e, 0x89];
const PCB_GREEN: [u8; 3] = [0x1e, 0x6b, 0x3a];
const PCB_BLACK: [u8; 3] = [0x26, 0x28, 0x2e];
const PCB_RED: [u8; 3] = [0x9b, 0x23, 0x2a];
const PCB_TEAL: [u8; 3] = [0x00, 0x6d, 0x77];
const PCB_PURPLE: [u8; 3] = [0x4a, 0x2a, 0x7a];

fn build() -> Vec<PartDef> {
    use Cat::*;
    let mut v: Vec<B> = vec![];

    // ── 面包板 ──
    v.push(
        B::new("bb_half", "半尺寸面包板", Board, "BB", Look::Breadboard { cols: 30 })
            .pins(breadboard_pins(30))
            .body(-1.5, -1.5, 30.5, 18.5)
            .kw("breadboard 400"),
    );
    v.push(
        B::new("bb_full", "全尺寸面包板", Board, "BB", Look::Breadboard { cols: 63 })
            .pins(breadboard_pins(63))
            .body(-1.5, -1.5, 63.5, 18.5)
            .kw("breadboard 830"),
    );

    // ── 电源 ──
    v.push(
        B::new("usb5v", "USB 5V 电源", Power, "PWR", Look::UsbPower)
            .pins(vec![p("+", 0, 0, cap::PWR), p("-", 0, 2, cap::GND)])
            .body(-4.0, -1.0, 0.5, 3.0)
            .prop("v", num("电压", "5", "V"))
            .prop("imax", num("最大电流", "0.5", "A"))
            .kw("usb vbus"),
    );
    v.push(
        B::new("battery", "电池", Power, "BT", Look::Battery)
            .pins(vec![p("+", 0, 0, cap::PWR), p("-", 0, 3, cap::GND)])
            .body(-4.5, -0.8, 0.5, 3.8)
            .prop("type", choice("类型", "li", &["aa", "2aa", "3aa", "li", "9v", "cr2032", "18650x2"]))
            .prop("mah", num("容量", "2000", "mAh"))
            .prop("soc", slider("剩余电量", "100", 0.0, 100.0, "%", false))
            .kw("battery lipo 18650 aa cr2032 9v"),
    );
    v.push(
        B::new("psu", "可调直流电源", Power, "PSU", Look::Psu)
            .pins(vec![p("+", 0, 0, cap::PWR), p("-", 0, 2, cap::GND)])
            .body(-7.0, -1.5, 0.5, 3.5)
            .prop("v", slider("电压", "5", 0.0, 30.0, "V", false))
            .prop("ilim", num("限流", "1", "A"))
            .kw("power supply bench"),
    );
    v.push(
        B::new("acsrc", "交流信号源", Power, "AC", Look::SigGen)
            .pins(vec![p("~1", 0, 0, 0), p("~2", 0, 2, 0)])
            .body(-5.0, -1.0, 0.5, 3.0)
            .prop("amp", num("幅值", "12", "V"))
            .prop("freq", num("频率", "50", "Hz"))
            .kw("ac sine transformer"),
    );
    v.push(
        B::new("siggen", "方波信号源", Power, "SG", Look::SigGen)
            .pins(vec![p("OUT", 0, 0, cap::SOUT), p("GND", 0, 2, cap::GND)])
            .body(-5.0, -1.0, 0.5, 3.0)
            .prop("amp", num("幅值", "5", "V"))
            .prop("freq", num("频率", "1k", "Hz"))
            .prop("duty", slider("占空比", "50", 0.0, 100.0, "%", false))
            .kw("square pwm function generator"),
    );
    v.push(
        B::new("gnd", "地", Power, "GND", Look::Ground)
            .pins(vec![p("GND", 0, 0, cap::GND)])
            .body(-0.8, -0.2, 0.8, 1.6)
            .kw("ground"),
    );

    // ── 电源芯片 ──
    v.push(
        B::new("ldo", "LDO 稳压芯片", PowerIc, "U", Look::To220)
            .pins(vec![p("GND", 0, 0, cap::GND), p("OUT", 1, 0, cap::PWR), p("IN", 2, 0, cap::PWR)])
            .body(-0.6, -3.2, 2.6, 0.4)
            .prop("model", choice("型号", "AMS1117-3.3", &["AMS1117-3.3", "AMS1117-5.0", "ME6211-3.3", "HT7333", "LM7805"]))
            .kw("ldo regulator ams1117 me6211 ht7333 7805"),
    );
    v.push(
        module("buck", "DC-DC 降压模块", PowerIc, "MP1584", PCB_BLUE, &["IN+", "IN-", "OUT-", "OUT+"], 5.0, Feature::Chip)
            .prop("model", choice("型号", "MP1584", &["MP1584", "MP2307", "TPS5430", "LM2596"]))
            .prop("vout", slider("输出电压", "3.3", 0.8, 20.0, "V", false))
            .kw("buck dcdc step down mp1584 lm2596"),
    );
    v.push(
        module("boost", "DC-DC 升压模块", PowerIc, "MT3608", PCB_BLUE, &["VIN+", "VIN-", "VOUT-", "VOUT+"], 4.0, Feature::Chip)
            .prop("vout", slider("输出电压", "5", 3.0, 28.0, "V", false))
            .kw("boost step up mt3608"),
    );
    v.push(
        module("tp4056", "TP4056 锂电充电", PowerIc, "TP4056", PCB_BLUE, &["IN+", "IN-", "B+", "B-", "OUT+", "OUT-"], 5.0, Feature::Usb)
            .prop("ichg", choice("充电电流", "1000", &["130", "300", "500", "1000"]))
            .kw("charger lithium tp4056 dw01"),
    );
    v.push(module("bms", "电池保护板", PowerIc, "DW01", PCB_GREEN, &["B+", "B-", "P+", "P-"], 3.0, Feature::Chip).kw("bms protection dw01 8205"));

    // ── 无源元件 ──
    v.push(
        B::new("res", "电阻", Passive, "R", Look::Resistor)
            .pins(vec![p("1", 0, 0, 0), p("2", 4, 0, 0)])
            .stretch()
            .prop("r", num("阻值", "220", "Ω"))
            .prop("pmax", choice("功率", "0.25", &["0.125", "0.25", "0.5", "1", "2"]))
            .kw("resistor"),
    );
    v.push(
        B::new("pot", "电位器", Passive, "RV", Look::Pot)
            .pins(vec![p("1", 0, 0, 0), p("W", 1, 0, 0), p("3", 2, 0, 0)])
            .body(-0.8, -3.0, 2.8, 0.5)
            .prop("r", num("阻值", "10k", "Ω"))
            .prop("pos", slider("旋钮位置", "50", 0.0, 100.0, "%", false))
            .kw("potentiometer trimmer"),
    );
    v.push(
        B::new("cap", "瓷片电容", Passive, "C", Look::Cap)
            .pins(vec![p("1", 0, 0, 0), p("2", 1, 0, 0)])
            .body(-0.5, -2.0, 1.5, 0.4)
            .prop("c", num("容量", "100n", "F"))
            .prop("vmax", num("耐压", "50", "V"))
            .kw("capacitor ceramic mlcc 104"),
    );
    v.push(
        B::new("ecap", "电解电容", Passive, "C", Look::ECap)
            .pins(vec![p("+", 0, 0, 0), p("-", 1, 0, 0)])
            .body(-0.7, -3.4, 1.7, 0.4)
            .prop("c", num("容量", "100u", "F"))
            .prop("vmax", num("耐压", "16", "V"))
            .kw("electrolytic capacitor polarized"),
    );
    v.push(
        B::new("ind", "电感", Passive, "L", Look::Inductor)
            .pins(vec![p("1", 0, 0, 0), p("2", 4, 0, 0)])
            .stretch()
            .prop("l", num("电感量", "10u", "H"))
            .prop("dcr", num("直流电阻", "0.1", "Ω"))
            .prop("isat", num("饱和电流", "2", "A"))
            .kw("inductor coil"),
    );
    v.push(
        B::new("bead", "磁珠", Passive, "FB", Look::Bead)
            .pins(vec![p("1", 0, 0, 0), p("2", 3, 0, 0)])
            .stretch()
            .prop("z", num("100MHz 阻抗", "600", "Ω"))
            .kw("ferrite bead"),
    );
    v.push(
        B::new("fuse", "保险丝", Passive, "F", Look::Fuse)
            .pins(vec![p("1", 0, 0, 0), p("2", 4, 0, 0)])
            .stretch()
            .prop("i", num("熔断电流", "0.5", "A"))
            .kw("fuse ptc polyfuse"),
    );

    // ── 二极管 ──
    v.push(
        B::new("diode", "二极管", Diode, "D", Look::Diode)
            .pins(vec![p("A", 0, 0, 0), p("K", 4, 0, 0)])
            .stretch()
            .prop("model", choice("型号", "1N4007", &["1N4007", "1N4148", "SS34", "1N5819"]))
            .kw("diode rectifier schottky 1n4007 1n4148 ss34"),
    );
    v.push(
        B::new("zener", "稳压管", Diode, "D", Look::Zener)
            .pins(vec![p("A", 0, 0, 0), p("K", 4, 0, 0)])
            .stretch()
            .prop("model", choice("型号", "5V1", &["3V3", "5V1", "12V", "SMBJ5.0A"]))
            .kw("zener tvs"),
    );
    v.push(
        B::new("led", "LED", Diode, "LED", Look::Led)
            .pins(vec![p("A", 0, 0, 0), p("K", 1, 0, 0)])
            .body(-0.6, -2.6, 1.6, 0.4)
            .prop("color", choice("颜色", "red", &["red", "yellow", "green", "blue", "white"]))
            .kw("led light"),
    );
    v.push(
        B::new("rgbled", "RGB LED（共阴）", Diode, "LED", Look::LedRgb)
            .pins(vec![p("R", 0, 0, 0), p("K", 1, 0, 0), p("G", 2, 0, 0), p("B", 3, 0, 0)])
            .body(-0.6, -3.0, 3.6, 0.4)
            .kw("rgb led common cathode"),
    );
    v.push(
        B::new("bridge", "整流桥", Diode, "BR", Look::Bridge)
            .pins(vec![p("+", 0, 0, 0), p("~1", 1, 0, 0), p("~2", 2, 0, 0), p("-", 3, 0, 0)])
            .body(-0.6, -2.6, 3.6, 0.4)
            .kw("bridge rectifier kbp307"),
    );

    // ── 三极管 ──
    v.push(
        B::new("npn", "NPN 三极管", Bjt, "Q", Look::To92)
            .pins(vec![p("E", 0, 0, 0), p("B", 1, 0, 0), p("C", 2, 0, 0)])
            .body(-0.6, -2.2, 2.6, 0.4)
            .prop("model", choice("型号", "S8050", &["S8050", "2N2222", "9013", "TIP120"]))
            .kw("npn transistor bjt s8050 2n2222 9013 tip120 darlington"),
    );
    v.push(
        B::new("pnp", "PNP 三极管", Bjt, "Q", Look::To92)
            .pins(vec![p("E", 0, 0, 0), p("B", 1, 0, 0), p("C", 2, 0, 0)])
            .body(-0.6, -2.2, 2.6, 0.4)
            .prop("model", choice("型号", "S8550", &["S8550", "2N3906", "9012"]))
            .kw("pnp transistor bjt s8550 2n3906 9012"),
    );

    // ── MOS 管 ──
    v.push(
        B::new("nmos", "N 沟道 MOS 管", Mos, "Q", Look::To220)
            .pins(vec![p("G", 0, 0, 0), p("D", 1, 0, 0), p("S", 2, 0, 0)])
            .body(-0.6, -3.2, 2.6, 0.4)
            .prop("model", choice("型号", "IRLZ44N", &["AO3400", "IRLZ44N", "IRF540N", "2N7002"]))
            .kw("nmos mosfet n-channel ao3400 irlz44n irf540n 2n7002"),
    );
    v.push(
        B::new("pmos", "P 沟道 MOS 管", Mos, "Q", Look::To220)
            .pins(vec![p("G", 0, 0, 0), p("D", 1, 0, 0), p("S", 2, 0, 0)])
            .body(-0.6, -3.2, 2.6, 0.4)
            .prop("model", choice("型号", "AO3401", &["AO3401", "IRF9540N"]))
            .kw("pmos mosfet p-channel ao3401 irf9540n"),
    );

    // ── 输入 ──
    v.push(
        B::new("button", "按键", Input, "SW", Look::Button)
            .pins(vec![p("1", 0, 0, 0), p("2", 2, 0, 0), p("3", 0, 3, 0), p("4", 2, 3, 0)])
            .body(-0.4, -0.4, 2.4, 3.4)
            .kw("button tact switch push"),
    );
    v.push(
        B::new("slide", "拨动开关", Input, "SW", Look::Slide)
            .pins(vec![p("1", 0, 0, 0), p("COM", 1, 0, 0), p("3", 2, 0, 0)])
            .body(-0.8, -2.0, 2.8, 0.4)
            .prop("on", boolean("拨到 1", "true"))
            .kw("slide switch spdt toggle"),
    );
    v.push(
        B::new("dipsw", "拨码开关（4 位）", Input, "SW", Look::DipSwitch)
            .pins({
                let mut v = row(&["1", "2", "3", "4"], 0, 3);
                v.extend(row(&["8", "7", "6", "5"], 0, 0));
                v
            })
            .body(-0.6, -0.4, 3.6, 3.4)
            .prop("bits", text("开关状态", "0000"))
            .kw("dip switch"),
    );
    v.push(
        B::new("ldr", "光敏电阻", Input, "R", Look::Ldr)
            .pins(vec![p("1", 0, 0, 0), p("2", 1, 0, 0)])
            .body(-0.6, -2.2, 1.6, 0.4)
            .prop("lux", slider("光照", "300", 0.1, 10000.0, "lx", true))
            .kw("ldr photoresistor gl5528 light sensor"),
    );
    v.push(
        B::new("ntc", "NTC 热敏电阻", Input, "RT", Look::Ntc)
            .pins(vec![p("1", 0, 0, 0), p("2", 1, 0, 0)])
            .body(-0.6, -2.2, 1.6, 0.4)
            .prop("r25", num("25°C 阻值", "10k", "Ω"))
            .prop("temp", slider("温度", "25", -20.0, 120.0, "°C", false))
            .kw("ntc thermistor b3950 temperature"),
    );
    v.push(
        module("encoder", "旋转编码器", Input, "EC11", PCB_BLACK, &["GND", "+", "SW/so", "DT/so", "CLK/so"], 4.0, Feature::None)
            .prop("count", slider("转动位置", "0", -50.0, 50.0, "", false))
            .kw("rotary encoder ec11 ky-040"),
    );

    // ── 输出 ──
    v.push(
        B::new("buzzer", "有源蜂鸣器", Output, "BZ", Look::Buzzer)
            .pins(vec![p("+", 0, 0, 0), p("-", 2, 0, 0)])
            .body(-1.0, -3.0, 3.0, 0.4)
            .prop("v", choice("额定电压", "5", &["3.3", "5", "12"]))
            .kw("buzzer active beeper"),
    );
    v.push(
        B::new("pbuzzer", "无源蜂鸣器", Output, "BZ", Look::Buzzer)
            .pins(vec![p("+", 0, 0, 0), p("-", 2, 0, 0)])
            .body(-1.0, -3.0, 3.0, 0.4)
            .kw("buzzer passive speaker tone"),
    );
    v.push(
        B::new("relay", "继电器", Output, "K", Look::Relay)
            .pins(vec![p("L1", 0, 0, 0), p("L2", 0, 4, 0), p("COM", 4, 0, 0), p("NO", 4, 2, 0), p("NC", 4, 4, 0)])
            .body(-0.6, -0.6, 4.6, 4.6)
            .prop("v", choice("线圈电压", "5", &["3", "5", "12"]))
            .kw("relay songle srd-05vdc"),
    );
    v.push(
        B::new("motor", "直流电机", Output, "M", Look::Motor)
            .pins(vec![p("1", 0, 0, 0), p("2", 2, 0, 0)])
            .body(-1.5, -4.0, 3.5, 0.4)
            .prop("v", choice("额定电压", "5", &["3", "5", "6", "12"]))
            .kw("dc motor 130 tt"),
    );
    v.push(module("servo", "舵机", Output, "SG90", [0x1e, 0x5f, 0xb4], &["GND", "VCC", "SIG/si"], 6.0, Feature::None).kw("servo sg90 mg996r"));
    v.push(
        B::new("seg7", "数码管（1 位）", Output, "DS", Look::Seg7)
            .pins({
                let mut v = row(&["e", "d", "COM", "c", "dp"], 0, 5);
                v.extend(row(&["g", "f", "COM", "a", "b"], 0, 0));
                v
            })
            .body(-0.6, -0.4, 4.6, 5.4)
            .prop("common", choice("类型", "cc", &["cc", "ca"]))
            .kw("7 segment display common cathode anode"),
    );
    v.push(
        B::new("lamp", "小灯泡", Output, "L", Look::Lamp)
            .pins(vec![p("1", 0, 0, 0), p("2", 2, 0, 0)])
            .body(-1.0, -3.0, 3.0, 0.4)
            .prop("v", choice("额定电压", "5", &["3", "5", "12"]))
            .kw("lamp bulb incandescent"),
    );

    // ── 显示屏 ──
    let scr = |w: f32, h: f32, px: u16, py: u16, color: bool| Feature::Screen { w, h, px, py, color };
    v.push(
        module(
            "oled",
            "OLED 0.96 寸（I2C）",
            Display,
            "SSD1306",
            PCB_BLUE,
            &["GND", "VCC", "SCL/scl", "SDA/sda"],
            11.0,
            scr(10.0, 5.0, 128, 64, false),
        )
        .prop("chip", choice("驱动芯片", "SSD1306", &["SSD1306", "SH1106"]))
        .prop("addr", choice("地址", "0x3C", &["0x3C", "0x3D"]))
        .kw("oled ssd1306 sh1106 i2c 0.96 1.3"),
    );
    v.push(
        module(
            "oled_spi",
            "OLED 0.96 寸（SPI）",
            Display,
            "SSD1306",
            PCB_BLUE,
            &["GND", "VCC", "D0/sck", "D1/mosi", "RES/si", "DC/si", "CS/si"],
            11.0,
            scr(10.0, 5.0, 128, 64, false),
        )
        .kw("oled ssd1306 spi"),
    );
    v.push(
        module(
            "st7735",
            "TFT 1.8 寸（ST7735）",
            Display,
            "ST7735",
            PCB_RED,
            &["GND", "VCC", "SCL/sck", "SDA/mosi", "RES/si", "DC/si", "CS/si", "BLK"],
            15.0,
            scr(11.0, 13.5, 128, 160, true),
        )
        .kw("tft st7735 spi 1.8"),
    );
    v.push(
        module(
            "st7789",
            "TFT 1.3/2.0 寸（ST7789）",
            Display,
            "ST7789",
            PCB_BLACK,
            &["GND", "VCC", "SCL/sck", "SDA/mosi", "RES/si", "DC/si", "CS/si", "BLK"],
            12.0,
            scr(10.5, 10.5, 240, 240, true),
        )
        .prop("size", choice("尺寸", "1.3", &["1.3", "2.0"]))
        .kw("tft st7789 ips spi 240x240"),
    );
    v.push(
        module(
            "ili9341",
            "TFT 2.4/2.8 寸（ILI9341 + 触摸）",
            Display,
            "ILI9341",
            PCB_RED,
            &[
                "VCC",
                "GND",
                "CS/si",
                "RESET/si",
                "DC/si",
                "SDI/mosi",
                "SCK/sck",
                "LED",
                "SDO/miso",
                "T_CLK/sck",
                "T_CS/si",
                "T_DIN/mosi",
                "T_DO/miso",
                "T_IRQ/so",
            ],
            22.0,
            scr(13.0, 17.5, 240, 320, true),
        )
        .prop("touch", choice("触摸芯片", "XPT2046", &["XPT2046", "FT6236"]))
        .kw("tft ili9341 2.4 2.8 touch xpt2046 ft6236"),
    );
    v.push(
        module(
            "epaper",
            "墨水屏 2.13 寸（SSD1680）",
            Display,
            "SSD1680",
            PCB_BLACK,
            &["BUSY/so", "RES/si", "DC/si", "CS/si", "SCL/sck", "SDA/mosi", "GND", "VCC"],
            12.0,
            scr(14.0, 7.0, 250, 122, false),
        )
        .kw("epaper eink e-paper ssd1680"),
    );
    v.push(
        module(
            "lcd1602",
            "LCD1602（I2C 转接板）",
            Display,
            "LCD1602",
            PCB_GREEN,
            &["GND", "VCC", "SDA/sda", "SCL/scl"],
            14.0,
            Feature::Lcd1602,
        )
        .prop("addr", choice("地址", "0x27", &["0x27", "0x3F"]))
        .kw("lcd 1602 hd44780 pcf8574 i2c"),
    );
    v.push(module("tm1637", "4 位数码管（TM1637）", Display, "TM1637", PCB_BLUE, &["CLK/si", "DIO/si", "VCC", "GND"], 6.0, Feature::Seg4).kw("tm1637 4 digit 7 segment"));
    v.push(
        module(
            "mipi_lcd",
            "MIPI DSI 屏（只讲接线）",
            Display,
            "MIPI DSI",
            PCB_BLACK,
            &["VCC", "GND", "CLK+", "CLK-", "D0+", "D0-", "D1+", "D1-", "RST", "BL"],
            16.0,
            scr(12.0, 13.0, 480, 800, true),
        )
        .kw("mipi dsi lcd fpc"),
    );

    // ── 摄像头 ──
    v.push(
        module(
            "ov7670",
            "OV7670 摄像头（DVP）",
            Camera,
            "OV7670",
            PCB_BLACK,
            &[
                "3V3", "GND", "SIOC/scl", "SIOD/sda", "VSYNC/so", "HREF/so", "PCLK/so", "XCLK/si", "D7", "D6", "D5", "D4", "D3", "D2", "D1", "D0", "RESET", "PWDN",
            ],
            8.0,
            Feature::Camera,
        )
        .kw("ov7670 camera dvp"),
    );
    v.push(
        module(
            "ov2640",
            "OV2640 摄像头（DVP，带 JPEG）",
            Camera,
            "OV2640",
            PCB_BLACK,
            &[
                "3V3", "GND", "SIOC/scl", "SIOD/sda", "VSYNC", "HREF", "PCLK", "XCLK", "D9", "D8", "D7", "D6", "D5", "D4", "D3", "D2", "RESET", "PWDN",
            ],
            8.0,
            Feature::Camera,
        )
        .kw("ov2640 camera jpeg esp32-cam"),
    );
    v.push(
        module(
            "ov5640",
            "OV5640 摄像头（500 万像素）",
            Camera,
            "OV5640",
            PCB_BLACK,
            &[
                "3V3", "GND", "SIOC/scl", "SIOD/sda", "VSYNC", "HREF", "PCLK", "XCLK", "D9", "D8", "D7", "D6", "D5", "D4", "D3", "D2", "RESET", "PWDN",
            ],
            8.0,
            Feature::Camera,
        )
        .kw("ov5640 camera autofocus"),
    );
    v.push(
        module(
            "mipi_cam",
            "MIPI CSI 摄像头（只讲接线）",
            Camera,
            "MIPI CSI",
            PCB_BLACK,
            &["3V3", "GND", "SCL", "SDA", "CLK+", "CLK-", "D0+", "D0-", "D1+", "D1-"],
            7.0,
            Feature::Camera,
        )
        .kw("mipi csi camera raspberry"),
    );

    // ── 物联网通信 ──
    v.push(
        module(
            "esp01",
            "ESP-01（ESP8266 AT 固件）",
            Iot,
            "ESP-01",
            PCB_BLACK,
            &["GND", "GPIO2", "GPIO0", "RX/rx", "TX/tx", "EN", "RST", "3V3"],
            7.0,
            Feature::Antenna,
        )
        .kw("esp8266 esp-01 wifi at"),
    );
    v.push(
        module(
            "hc05",
            "HC-05 蓝牙串口",
            Iot,
            "HC-05",
            PCB_BLUE,
            &["STATE/so", "RXD/rx", "TXD/tx", "GND", "VCC", "EN"],
            9.0,
            Feature::Antenna,
        )
        .prop("name", text("蓝牙名称", "HC-05"))
        .kw("hc-05 hc05 bluetooth spp"),
    );
    v.push(
        module(
            "jdy31",
            "JDY-31 蓝牙串口",
            Iot,
            "JDY-31",
            PCB_BLUE,
            &["STATE/so", "RXD/rx", "TXD/tx", "GND", "VCC", "EN"],
            9.0,
            Feature::Antenna,
        )
        .kw("jdy-31 bluetooth spp"),
    );
    v.push(
        module(
            "hm10",
            "HM-10 蓝牙 BLE",
            Iot,
            "HM-10",
            PCB_BLUE,
            &["STATE/so", "RXD/rx", "TXD/tx", "GND", "VCC", "BRK"],
            9.0,
            Feature::Antenna,
        )
        .kw("hm-10 ble cc2541 bluetooth"),
    );
    v.push(
        module(
            "cat1",
            "4G Cat1 模块",
            Iot,
            "Air780E",
            PCB_BLACK,
            &["VBAT", "GND", "TXD/tx", "RXD/rx", "PWRKEY/si", "NET/so", "SIM", "ANT"],
            10.0,
            Feature::Shield,
        )
        .prop("model", choice("型号", "Air780E", &["Air780E", "EC800M", "ML307"]))
        .prop("csq", slider("信号强度", "24", 0.0, 31.0, "", false))
        .prop("sim", boolean("插了 SIM 卡", "true"))
        .kw("4g cat1 lte air780e ec800m ml307"),
    );
    v.push(
        module(
            "nbiot",
            "NB-IoT 模块",
            Iot,
            "BC260Y",
            PCB_BLACK,
            &["VBAT", "GND", "TXD/tx", "RXD/rx", "PSM", "ANT"],
            8.0,
            Feature::Shield,
        )
        .kw("nbiot nb-iot bc260y"),
    );
    v.push(
        module(
            "lora",
            "LoRa 模块（SX1278）",
            Iot,
            "Ra-01",
            PCB_BLACK,
            &["3V3", "GND", "NSS/cs", "SCK/sck", "MOSI/mosi", "MISO/miso", "RST", "DIO0/so"],
            8.0,
            Feature::Shield,
        )
        .kw("lora sx1278 ra-01 433"),
    );
    v.push(
        module(
            "w5500",
            "W5500 网口模块",
            Iot,
            "W5500",
            PCB_BLUE,
            &["3V3", "GND", "MISO/miso", "MOSI/mosi", "SCS/cs", "SCLK/sck", "RST/si", "INT/so"],
            12.0,
            Feature::Rj45,
        )
        .prop("ip", text("IP 地址", "192.168.1.177"))
        .kw("w5500 ethernet spi rj45"),
    );
    v.push(
        module(
            "lan8720",
            "LAN8720 网口模块（RMII）",
            Iot,
            "LAN8720",
            PCB_GREEN,
            &["TX1", "TX_EN", "TX0", "RX0", "RX1", "nINT/REFCLK", "CRS_DV", "MDIO", "MDC", "3V3", "GND"],
            12.0,
            Feature::Rj45,
        )
        .kw("lan8720 rmii phy ethernet"),
    );
    v.push(
        module(
            "max485",
            "RS485 模块（MAX485）",
            Iot,
            "MAX485",
            PCB_BLUE,
            &["RO/tx", "RE/si", "DE/si", "DI/rx", "VCC", "B", "A", "GND"],
            6.0,
            Feature::Terminal,
        )
        .kw("rs485 max485 modbus"),
    );
    v.push(
        module(
            "tja1050",
            "CAN 模块（TJA1050）",
            Iot,
            "TJA1050",
            PCB_BLUE,
            &["TX/rx", "RX/tx", "VCC", "GND", "CANH", "CANL"],
            6.0,
            Feature::Terminal,
        )
        .kw("can tja1050 transceiver"),
    );
    v.push(
        module("gps", "GPS 模块", Iot, "ATGM336H", PCB_BLUE, &["VCC", "GND", "TX/tx", "RX/rx", "PPS/so"], 9.0, Feature::Antenna)
            .prop("model", choice("型号", "ATGM336H", &["ATGM336H", "NEO-6M"]))
            .prop("fix", boolean("已定位", "true"))
            .kw("gps gnss atgm336h neo-6m nmea"),
    );
    v.push(module("simslot", "SIM 卡座", Iot, "SIM", PCB_BLACK, &["VCC", "RST", "CLK", "IO", "GND"], 6.0, Feature::None).kw("sim card holder"));
    v.push(
        module("antenna", "天线", Iot, "ANT", PCB_BLACK, &["RF", "GND"], 3.0, Feature::Antenna)
            .prop("type", choice("类型", "ipex", &["pcb", "ceramic", "ipex"]))
            .kw("antenna ipex u.fl"),
    );

    // ── 常用芯片 ──
    v.push(
        B::new("ne555", "555 定时器", Chip, "U", Look::Dip { label: "NE555" })
            .pins(dip(&["GND", "TRIG", "OUT", "RESET", "CTRL", "THR", "DIS", "VCC"]))
            .body(-0.6, 0.2, 3.6, 2.8)
            .kw("555 timer ne555"),
    );
    v.push(
        B::new("pc817", "光耦 PC817", Chip, "U", Look::Dip { label: "PC817" })
            .pins(dip(&["A", "K", "E", "C"]))
            .body(-0.6, 0.2, 1.6, 2.8)
            .kw("optocoupler pc817"),
    );
    v.push(
        B::new("lm358", "运放 LM358", Chip, "U", Look::Dip { label: "LM358" })
            .pins(dip(&["OUT1", "IN1-", "IN1+", "GND", "IN2+", "IN2-", "OUT2", "VCC"]))
            .body(-0.6, 0.2, 3.6, 2.8)
            .kw("op amp opamp lm358 comparator"),
    );
    v.push(
        B::new("hc595", "移位寄存器 74HC595", Chip, "U", Look::Dip { label: "74HC595" })
            .pins(dip(&["QB", "QC", "QD", "QE", "QF", "QG", "QH", "GND", "QH'", "SRCLR", "SRCLK", "RCLK", "OE", "SER", "QA", "VCC"]))
            .body(-0.6, 0.2, 7.6, 2.8)
            .kw("74hc595 shift register"),
    );
    v.push(
        B::new("uln2003", "达林顿阵列 ULN2003", Chip, "U", Look::Dip { label: "ULN2003" })
            .pins(dip(&["1B", "2B", "3B", "4B", "5B", "6B", "7B", "GND", "COM", "7C", "6C", "5C", "4C", "3C", "2C", "1C"]))
            .body(-0.6, 0.2, 7.6, 2.8)
            .kw("uln2003 darlington driver stepper"),
    );
    v.push(
        module(
            "tb6612",
            "TB6612 电机驱动",
            Chip,
            "TB6612",
            PCB_RED,
            &[
                "VM", "VCC", "GND", "AO1", "AO2", "BO2", "BO1", "PWMA/si", "AIN2/si", "AIN1/si", "STBY/si", "BIN1/si", "BIN2/si", "PWMB/si",
            ],
            5.0,
            Feature::Chip,
        )
        .kw("tb6612 tb6612fng h bridge motor driver"),
    );
    v.push(
        module(
            "l298n",
            "L298N 电机驱动",
            Chip,
            "L298N",
            PCB_RED,
            &["12V", "GND", "5V", "ENA/si", "IN1/si", "IN2/si", "IN3/si", "IN4/si", "ENB/si", "OUT1", "OUT2", "OUT3", "OUT4"],
            10.0,
            Feature::Terminal,
        )
        .kw("l298n h bridge motor driver"),
    );

    // ── 模块 ──
    v.push(
        module("dht11", "DHT11 温湿度", Module, "DHT11", [0x1e, 0x7b, 0xc8], &["VCC", "DATA/so", "GND"], 5.0, Feature::Sensor)
            .prop("temp", slider("温度", "26", -10.0, 60.0, "°C", false))
            .prop("hum", slider("湿度", "55", 5.0, 95.0, "%", false))
            .kw("dht11 dht22 humidity temperature"),
    );
    v.push(
        module("sht30", "SHT30 温湿度（I2C）", Module, "SHT30", PCB_BLUE, &["VCC", "GND", "SCL/scl", "SDA/sda"], 4.0, Feature::Sensor)
            .prop("temp", slider("温度", "26", -40.0, 120.0, "°C", false))
            .prop("hum", slider("湿度", "55", 0.0, 100.0, "%", false))
            .kw("sht30 sht31 humidity i2c"),
    );
    v.push(
        module(
            "hcsr04",
            "HC-SR04 超声波测距",
            Module,
            "HC-SR04",
            PCB_BLUE,
            &["VCC", "TRIG/si", "ECHO/so", "GND"],
            8.0,
            Feature::Ultrasonic,
        )
        .prop("dist", slider("距离", "30", 2.0, 400.0, "cm", false))
        .kw("hc-sr04 ultrasonic distance sonar"),
    );
    v.push(
        module("relay_mod", "继电器模块", Module, "RELAY", PCB_BLUE, &["VCC", "GND", "IN/si", "COM", "NO", "NC"], 7.0, Feature::Relay)
            .prop("trig", choice("触发方式", "low", &["low", "high"]))
            .kw("relay module optocoupler"),
    );
    v.push(
        module(
            "ina219",
            "INA219 电流检测",
            Module,
            "INA219",
            PCB_PURPLE,
            &["VCC", "GND", "SCL/scl", "SDA/sda", "VIN+", "VIN-"],
            5.0,
            Feature::Chip,
        )
        .kw("ina219 current sensor shunt"),
    );
    v.push(
        module(
            "ina226",
            "INA226 电流检测",
            Module,
            "INA226",
            PCB_PURPLE,
            &["VCC", "GND", "SCL/scl", "SDA/sda", "ALE", "VIN+", "VIN-"],
            5.0,
            Feature::Chip,
        )
        .kw("ina226 current sensor shunt"),
    );

    // ── 开发板 ──
    v.extend(boards());

    // ── 调试工具 ──
    v.push(
        module("usbttl", "USB 转 TTL", Debug, "CH340", PCB_RED, &["GND", "RXD/rx", "TXD/tx", "3V3/3v3", "5V/5v"], 6.0, Feature::Usb)
            .prop("chip", choice("芯片", "CH340", &["CH340", "CP2102", "FT232"]))
            .prop("level", choice("信号电平", "3.3", &["3.3", "5"]))
            .kw("usb ttl uart serial ch340 cp2102 ft232"),
    );
    v.push(
        module(
            "stlink",
            "ST-Link",
            Debug,
            "ST-LINK V2",
            [0x9e, 0x9e, 0xa6],
            &["RST", "SWCLK", "SWDIO", "GND", "3V3/3v3", "5V/5v"],
            7.0,
            Feature::Usb,
        )
        .kw("st-link stlink swd jlink daplink"),
    );
    v.push(
        module(
            "espprog",
            "ESP-Prog",
            Debug,
            "ESP-PROG",
            PCB_BLACK,
            &["EN", "VDD/pwr", "TXD/tx", "GND", "RXD/rx", "IO0"],
            8.0,
            Feature::Usb,
        )
        .kw("esp-prog esp32 jtag uart"),
    );

    // ── 仪器 ──
    v.push(
        B::new("meter", "万用表", Meter, "XM", Look::Multimeter)
            .pins(vec![p("COM", 0, 0, 0), p("VΩ", 3, 0, 0)])
            .body(-1.5, -9.0, 4.5, 0.6)
            .prop("mode", choice("档位", "v", &["v", "a", "ohm", "beep"]))
            .kw("multimeter dmm voltmeter"),
    );
    v.push(
        B::new("ammeter", "电流表", Meter, "XA", Look::Ammeter)
            .pins(vec![p("+", 0, 0, 0), p("-", 4, 0, 0)])
            .body(-0.5, -3.5, 4.5, 0.6)
            .kw("ammeter current meter"),
    );
    v.push(
        B::new("pmeter", "功耗分析仪", Meter, "XP", Look::PowerMeter)
            .pins(vec![p("IN+", 0, 0, 0), p("OUT+", 6, 0, 0), p("GND", 3, 0, cap::GND)])
            .body(-0.5, -4.5, 6.5, 0.6)
            .kw("power analyzer current ppk"),
    );
    v.push(
        B::new("probe", "示波器探针", Meter, "CH", Look::Probe)
            .pins(vec![p("TIP", 0, 0, 0)])
            .body(-0.6, -3.0, 0.6, 0.3)
            .prop("ch", choice("通道", "1", &["1", "2", "3", "4"]))
            .kw("oscilloscope probe scope"),
    );
    v.push(
        B::new("logic", "逻辑分析仪", Meter, "LA", Look::Logic)
            .pins(row(&["CH0", "CH1", "CH2", "CH3", "CH4", "CH5", "CH6", "CH7", "GND/gnd"], 0, 0))
            .body(-0.6, -4.0, 8.6, 0.4)
            .kw("logic analyzer saleae"),
    );

    v.into_iter().map(|b| b.0).collect()
}

/// 双列直插引脚：编号 1..n/2 在下排从左到右，n/2+1..n 在上排从右到左
fn dip(names: &[&str]) -> Vec<PinDef> {
    let h = names.len() / 2;
    names
        .iter()
        .enumerate()
        .map(|(i, n)| if i < h { spec(n, i as i32, 3) } else { spec(n, (names.len() - 1 - i) as i32, 0) })
        .collect()
}

/// 面包板孔位：上电源轨（- 在 y=0，+ 在 y=1）、a–e（y=3..7）、中缝、f–j（y=10..14）、下电源轨（+ 在 y=16，- 在 y=17）。
/// 电源轨每 5 个孔一组
pub fn breadboard_pins(cols: i32) -> Vec<PinDef> {
    let mut v = vec![];
    for (ri, y) in BB_ROWS.iter().enumerate() {
        for x in 0..cols {
            v.push(PinDef {
                name: format!("{}{}", BB_ROW_NAMES[ri], x + 1),
                at: (x, *y),
                caps: 0,
            });
        }
    }
    for y in BB_RAILS {
        for x in 0..cols {
            if rail_hole(x, cols) {
                v.push(PinDef {
                    name: bb_hole_name((x, y)),
                    at: (x, y),
                    caps: 0,
                });
            }
        }
    }
    v
}

pub const BB_ROWS: [i32; 10] = [3, 4, 5, 6, 7, 10, 11, 12, 13, 14];
pub const BB_ROW_NAMES: [char; 10] = ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j'];
/// 电源轨所在行：上 -、上 +、下 +、下 -
pub const BB_RAILS: [i32; 4] = [0, 1, 16, 17];

/// 电源轨上第 x 列有没有孔（每 5 个一组，组间空一格；两头留边）
pub fn rail_hole(x: i32, cols: i32) -> bool {
    x >= 1 && x < cols - 1 && (x - 1) % 6 != 5
}

/// 面包板上一个孔属于哪个「连通组」（同一组的孔内部接通）
pub fn bb_group((x, y): Pt) -> i64 {
    match y {
        0 => -1,
        1 => -2,
        16 => -3,
        17 => -4,
        3..=7 => x as i64 * 2,
        _ => x as i64 * 2 + 1,
    }
}

/// 面包板某个孔的名字（a12、+上）
pub fn bb_hole_name(at: Pt) -> String {
    let (x, y) = at;
    if let Some(i) = BB_ROWS.iter().position(|r| *r == y) {
        return format!("{}{}", BB_ROW_NAMES[i], x + 1);
    }
    match y {
        0 => "-T".into(),
        1 => "+T".into(),
        16 => "+B".into(),
        17 => "-B".into(),
        _ => String::new(),
    }
}

fn boards() -> Vec<B> {
    let mut v = vec![];
    // Arduino Uno：上边数字口，下边电源和模拟口
    {
        let top = [
            "D0/io.rx",
            "D1/io.tx",
            "D2/io",
            "D3/io.pwm",
            "D4/io",
            "D5/io.pwm",
            "D6/io.pwm",
            "D7/io",
            "",
            "D8/io",
            "D9/io.pwm",
            "D10/io.pwm.cs",
            "D11/io.pwm.mosi",
            "D12/io.miso",
            "D13/io.sck.led",
            "GND",
            "AREF",
            "SDA/io.sda",
            "SCL/io.scl",
        ];
        let bottom = [
            "",
            "IOREF",
            "RESET/rst",
            "3V3/3v3",
            "5V/5v",
            "GND",
            "GND",
            "VIN/vin",
            "",
            "A0/io.adc",
            "A1/io.adc",
            "A2/io.adc",
            "A3/io.adc",
            "A4/io.adc.sda",
            "A5/io.adc.scl",
        ];
        let mut pins: Vec<PinDef> = vec![];
        // Uno 上排从右往左是 D0..D13（USB 口朝左时），这里画成从左往右，便于看
        for (i, n) in top.iter().enumerate() {
            if !n.is_empty() {
                pins.push(spec(n, i as i32 + 2, 0));
            }
        }
        for (i, n) in bottom.iter().enumerate() {
            if !n.is_empty() {
                pins.push(spec(n, i as i32 + 6, 20));
            }
        }
        v.push(
            B::new(
                "uno",
                "Arduino Uno",
                Cat::Mcu,
                "U",
                Look::Pcb {
                    color: [0x00, 0x7a, 0x8a],
                    title: "Arduino UNO",
                    feature: Feature::Usb,
                },
            )
            .pins(pins)
            .body(-4.0, -1.0, 23.0, 21.0)
            .prop("power", choice("供电", "usb", &["usb", "ext"]))
            .kw("arduino uno atmega328p"),
        );
    }
    v.push(
        board2(
            "nano",
            "Arduino Nano",
            "NANO",
            [0x1d, 0x4e, 0x89],
            &[
                "D12/io.miso",
                "D11/io.pwm.mosi",
                "D10/io.pwm.cs",
                "D9/io.pwm",
                "D8/io",
                "D7/io",
                "D6/io.pwm",
                "D5/io.pwm",
                "D4/io",
                "D3/io.pwm",
                "D2/io",
                "GND",
                "RST/rst",
                "RX0/io.rx",
                "TX1/io.tx",
            ],
            &[
                "D13/io.sck.led",
                "3V3/3v3",
                "REF",
                "A0/io.adc",
                "A1/io.adc",
                "A2/io.adc",
                "A3/io.adc",
                "A4/io.adc.sda",
                "A5/io.adc.scl",
                "A6/adc.in",
                "A7/adc.in",
                "5V/5v",
                "RST/rst",
                "GND",
                "VIN/vin",
            ],
            6,
            Feature::Usb,
        )
        .prop("power", choice("供电", "usb", &["usb", "ext"]))
        .kw("arduino nano atmega328p"),
    );
    v.push(
        board2(
            "stc89",
            "51 单片机最小系统（STC89C52）",
            "STC89C52",
            PCB_BLUE,
            &[
                "P1.0/io",
                "P1.1/io",
                "P1.2/io",
                "P1.3/io",
                "P1.4/io",
                "P1.5/io",
                "P1.6/io",
                "P1.7/io",
                "RST/rst",
                "P3.0/io.rx",
                "P3.1/io.tx",
                "P3.2/io",
                "P3.3/io",
                "P3.4/io",
                "P3.5/io",
                "P3.6/io",
                "P3.7/io",
                "GND",
            ],
            &[
                "VCC/5v",
                "P0.0/io.od",
                "P0.1/io.od",
                "P0.2/io.od",
                "P0.3/io.od",
                "P0.4/io.od",
                "P0.5/io.od",
                "P0.6/io.od",
                "P0.7/io.od",
                "EA",
                "ALE",
                "PSEN",
                "P2.7/io",
                "P2.6/io",
                "P2.5/io",
                "P2.4/io",
                "P2.3/io",
                "P2.2/io",
                "P2.1/io",
                "P2.0/io",
            ],
            8,
            Feature::Chip,
        )
        .prop("power", choice("供电", "usb", &["usb", "ext"]))
        .kw("51 8051 stc89c52 at89s52 mcs-51"),
    );
    v.push(
        board2(
            "bluepill",
            "STM32 最小系统板（Blue Pill）",
            "STM32F103C8T6",
            [0x1f, 0x3f, 0x9a],
            &[
                "PB12/io.ft.cs",
                "PB13/io.ft.sck",
                "PB14/io.ft.miso",
                "PB15/io.ft.mosi",
                "PA8/io.ft.pwm",
                "PA9/io.ft.tx",
                "PA10/io.ft.rx",
                "PA11/io.ft",
                "PA12/io.ft",
                "PA15/io.ft",
                "PB3/io.ft",
                "PB4/io.ft",
                "PB5/io",
                "PB6/io.ft.scl.pwm",
                "PB7/io.ft.sda.pwm",
                "PB8/io.ft.pwm",
                "PB9/io.ft.pwm",
                "5V/5v",
                "GND",
                "3V3/3v3",
            ],
            &[
                "VBAT",
                "PC13/io.led",
                "PC14/io",
                "PC15/io",
                "PA0/io.adc.pwm",
                "PA1/io.adc.pwm",
                "PA2/io.adc.tx",
                "PA3/io.adc.rx",
                "PA4/io.adc",
                "PA5/io.adc",
                "PA6/io.adc.pwm",
                "PA7/io.adc.pwm",
                "PB0/io.adc.pwm",
                "PB1/io.adc.pwm",
                "PB10/io.ft.scl",
                "PB11/io.ft.sda",
                "NRST/rst",
                "3V3/3v3",
                "GND",
                "GND",
            ],
            6,
            Feature::Usb,
        )
        .prop("power", choice("供电", "usb", &["usb", "ext"]))
        .kw("stm32 stm32f103 blue pill bluepill"),
    );
    let esp32_l = [
        "3V3/3v3",
        "EN/rst",
        "VP/io.in.adc",
        "VN/io.in.adc",
        "IO34/io.in.adc",
        "IO35/io.in.adc",
        "IO32/io.adc.pwm",
        "IO33/io.adc.pwm",
        "IO25/io.adc.pwm.dac",
        "IO26/io.adc.pwm.dac",
        "IO27/io.adc.pwm",
        "IO14/io.adc.pwm",
        "IO12/io.adc.pwm.boot",
        "GND",
        "IO13/io.adc.pwm",
        "5V/5v",
    ];
    let esp32_r = [
        "GND",
        "IO23/io.pwm.mosi",
        "IO22/io.pwm.scl",
        "TX/io.tx",
        "RX/io.rx",
        "IO21/io.pwm.sda",
        "GND",
        "IO19/io.pwm.miso",
        "IO18/io.pwm.sck",
        "IO5/io.pwm.cs.boot",
        "IO17/io.pwm.tx",
        "IO16/io.pwm.rx",
        "IO4/io.adc.pwm",
        "IO0/io.adc.boot",
        "IO2/io.adc.pwm.boot.led",
        "IO15/io.adc.pwm.boot",
    ];
    v.push(
        board2("esp32", "ESP32 DevKit", "ESP32-DevKitC", PCB_BLACK, &esp32_l, &esp32_r, 10, Feature::Shield)
            .prop("power", choice("供电", "usb", &["usb", "ext"]))
            .kw("esp32 devkit wroom"),
    );
    v.push(
        board2(
            "esp32s3",
            "ESP32-S3 DevKit",
            "ESP32-S3",
            PCB_BLACK,
            &[
                "3V3/3v3",
                "RST/rst",
                "IO4/io.adc.pwm",
                "IO5/io.adc.pwm",
                "IO6/io.adc.pwm",
                "IO7/io.adc.pwm",
                "IO15/io.adc.pwm",
                "IO16/io.adc.pwm",
                "IO17/io.adc.pwm.tx",
                "IO18/io.adc.pwm.rx",
                "IO8/io.adc.pwm.sda",
                "IO3/io.adc.boot",
                "IO46/io.boot",
                "IO9/io.adc.pwm.scl",
                "IO10/io.adc.pwm.cs",
                "IO11/io.adc.pwm.mosi",
                "IO12/io.adc.pwm.sck",
                "IO13/io.adc.pwm.miso",
                "IO14/io.adc.pwm",
                "5V/5v",
                "GND",
            ],
            &[
                "GND",
                "TX/io.tx",
                "RX/io.rx",
                "IO1/io.adc.pwm",
                "IO2/io.adc.pwm",
                "IO42/io.pwm",
                "IO41/io.pwm",
                "IO40/io.pwm",
                "IO39/io.pwm",
                "IO38/io.pwm.led",
                "IO37/io",
                "IO36/io",
                "IO35/io",
                "IO0/io.boot",
                "IO45/io.boot",
                "IO48/io.pwm",
                "IO47/io.pwm",
                "IO21/io.pwm",
                "IO20/io",
                "IO19/io",
                "GND",
            ],
            10,
            Feature::Shield,
        )
        .prop("power", choice("供电", "usb", &["usb", "ext"]))
        .kw("esp32-s3 esp32s3"),
    );
    v.push(
        board2(
            "esp32c3",
            "ESP32-C3 SuperMini",
            "ESP32-C3",
            PCB_BLACK,
            &[
                "5V/5v",
                "GND",
                "3V3/3v3",
                "IO4/io.adc.pwm.sck",
                "IO3/io.adc.pwm",
                "IO2/io.adc.pwm.boot",
                "IO1/io.adc.pwm",
                "IO0/io.adc.pwm",
            ],
            &[
                "IO5/io.adc.pwm.miso",
                "IO6/io.pwm.mosi",
                "IO7/io.pwm.cs",
                "IO8/io.pwm.sda.led.boot",
                "IO9/io.pwm.scl.boot",
                "IO10/io.pwm",
                "RX/io.rx",
                "TX/io.tx",
            ],
            6,
            Feature::Antenna,
        )
        .prop("power", choice("供电", "usb", &["usb", "ext"]))
        .kw("esp32-c3 esp32c3 supermini riscv"),
    );
    v.push(
        board2(
            "esp32cam",
            "ESP32-CAM",
            "ESP32-CAM",
            PCB_BLACK,
            &["5V/5v", "GND", "IO12/io.boot", "IO13/io", "IO15/io.boot", "IO14/io", "IO2/io.boot", "IO4/io.led"],
            &["3V3/3v3", "IO16/io", "IO0/io.boot", "GND", "VCC", "U0R/io.rx", "U0T/io.tx", "GND"],
            8,
            Feature::Camera,
        )
        .prop("power", choice("供电", "ext", &["usb", "ext"]))
        .kw("esp32-cam camera ov2640"),
    );
    v.push(
        board2(
            "nodemcu",
            "ESP8266 NodeMCU",
            "NodeMCU",
            PCB_BLACK,
            &[
                "A0/io.adc.in",
                "RSV",
                "RSV",
                "SD3",
                "SD2",
                "SD1",
                "CMD",
                "SD0",
                "CLK",
                "GND",
                "3V3/3v3",
                "EN/rst",
                "RST/rst",
                "GND",
                "VIN/vin",
            ],
            &[
                "D0/io.led",
                "D1/io.pwm.scl",
                "D2/io.pwm.sda",
                "D3/io.boot",
                "D4/io.boot.led",
                "3V3/3v3",
                "GND",
                "D5/io.pwm.sck",
                "D6/io.pwm.miso",
                "D7/io.pwm.mosi",
                "D8/io.boot.cs",
                "RX/io.rx",
                "TX/io.tx",
                "GND",
                "3V3/3v3",
            ],
            9,
            Feature::Shield,
        )
        .prop("power", choice("供电", "usb", &["usb", "ext"]))
        .kw("esp8266 nodemcu lolin"),
    );
    v.push(
        board2(
            "pico",
            "树莓派 Pico",
            "Raspberry Pi Pico",
            PCB_GREEN,
            &[
                "GP0/io.pwm.tx",
                "GP1/io.pwm.rx",
                "GND",
                "GP2/io.pwm.sck",
                "GP3/io.pwm.mosi",
                "GP4/io.pwm.miso.sda",
                "GP5/io.pwm.cs.scl",
                "GND",
                "GP6/io.pwm",
                "GP7/io.pwm",
                "GP8/io.pwm",
                "GP9/io.pwm",
                "GND",
                "GP10/io.pwm",
                "GP11/io.pwm",
                "GP12/io.pwm",
                "GP13/io.pwm",
                "GND",
                "GP14/io.pwm",
                "GP15/io.pwm",
            ],
            &[
                "VBUS/5v",
                "VSYS/vin",
                "GND",
                "3V3_EN",
                "3V3/3v3",
                "ADC_VREF",
                "GP28/io.adc.pwm",
                "GND",
                "GP27/io.adc.pwm",
                "GP26/io.adc.pwm",
                "RUN/rst",
                "GP22/io.pwm",
                "GND",
                "GP21/io.pwm",
                "GP20/io.pwm",
                "GP19/io.pwm",
                "GP18/io.pwm",
                "GND",
                "GP17/io.pwm",
                "GP16/io.pwm",
            ],
            7,
            Feature::Usb,
        )
        .prop("power", choice("供电", "usb", &["usb", "ext"]))
        .kw("raspberry pi pico rp2040"),
    );
    v
}

static DEFS: OnceLock<(Vec<PartDef>, HashMap<&'static str, usize>)> = OnceLock::new();

fn defs() -> &'static (Vec<PartDef>, HashMap<&'static str, usize>) {
    DEFS.get_or_init(|| {
        let v = build();
        let m = v.iter().enumerate().map(|(i, d)| (d.key, i)).collect();
        (v, m)
    })
}

pub fn all() -> &'static [PartDef] {
    &defs().0
}

pub fn def(key: &str) -> Option<&'static PartDef> {
    let (v, m) = defs();
    m.get(key).map(|i| &v[*i])
}

impl PartDef {
    pub fn pin(&self, name: &str) -> Option<usize> {
        self.pins.iter().position(|p| p.name == name)
    }

    pub fn is_board(&self) -> bool {
        self.cat == Cat::Mcu
    }

    /// 单片机逻辑电压
    pub fn logic_v(&self) -> f64 {
        match self.key {
            "uno" | "nano" | "stc89" => 5.0,
            _ => 3.3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_is_sane() {
        let mut keys = std::collections::HashSet::new();
        for d in all() {
            assert!(keys.insert(d.key), "重复的键 {}", d.key);
            assert!(!d.pins.is_empty(), "{} 没有引脚", d.key);
            // 同一个元件的引脚不能落在同一个格点上（面包板除外）
            let mut at = std::collections::HashSet::new();
            for p in &d.pins {
                assert!(at.insert(p.at), "{} 的引脚 {} 位置重复", d.key, p.name);
            }
        }
        assert!(all().len() > 90);
        let bb = def("bb_half").unwrap();
        assert_eq!(bb.pins.len(), 300 + 4 * 24);
        // 555 第 1 脚在左下，第 8 脚在左上
        let t = def("ne555").unwrap();
        assert_eq!(t.pins[0].at, (0, 3));
        assert_eq!(t.pins[7].at, (0, 0));
        assert_eq!(t.pins[4].at, (3, 0));
    }
}

#[cfg(test)]
#[test]
#[ignore]
fn dump_lib() {
    let mut s = String::new();
    for d in all() {
        let pins: Vec<String> = d.pins.iter().map(|p| format!("{}@{},{}", p.name, p.at.0, p.at.1)).collect();
        let props: Vec<String> = d.props.iter().map(|(k, v)| format!("{k}={}", v.default)).collect();
        s += &format!("{} [{}] body={:?} pins: {} | props: {}\n", d.key, d.zh, d.body, pins.join(" "), props.join(" "));
    }
    std::fs::write(std::env::var("OUT").unwrap(), s).unwrap();
}
