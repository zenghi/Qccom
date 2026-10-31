//! 元件型号参数：二极管、LED、三极管、MOS、稳压芯片、电池。数值取自常见手册的典型值，够教学用。

use super::solver::{BjtM, DiodeM, MosM};

pub struct DiodeSpec {
    pub m: DiodeM,
    /// 最大平均正向电流
    pub imax: f64,
    /// 最大反向电压（稳压管为 0：本来就工作在击穿区）
    pub vr: f64,
    /// 最大功耗
    pub pmax: f64,
}

pub fn diode(model: &str) -> DiodeSpec {
    let (m, imax, vr, pmax) = match model {
        "1N4148" => (DiodeM::from_vf(0.72, 0.01, 1.9, 0.6), 0.3, 75.0, 0.5),
        "SS34" => (DiodeM::from_vf(0.45, 1.0, 1.15, 0.03), 3.0, 40.0, 2.0),
        "1N5819" => (DiodeM::from_vf(0.45, 1.0, 1.2, 0.05), 1.0, 40.0, 1.0),
        _ => (DiodeM::from_vf(0.93, 1.0, 1.8, 0.03), 1.0, 1000.0, 1.5),
    };
    DiodeSpec { m, imax, vr, pmax }
}

pub fn zener(model: &str) -> DiodeSpec {
    let (bv, pmax) = match model {
        "3V3" => (3.3, 0.5),
        "12V" => (12.0, 0.5),
        "SMBJ5.0A" => (6.4, 600.0 / 1000.0 * 5.0),
        _ => (5.1, 0.5),
    };
    let mut m = DiodeM::from_vf(0.75, 0.01, 1.0, 0.5);
    m.bv = bv;
    DiodeSpec { m, imax: 0.2, vr: 0.0, pmax }
}

/// LED：颜色 → (模型, 发光颜色 RGB)
pub fn led(color: &str) -> (DiodeM, [u8; 3]) {
    let (vf, rs, rgb) = match color {
        "yellow" => (2.1, 2.5, [0xff, 0xd0, 0x20]),
        "green" => (2.2, 3.0, [0x30, 0xf0, 0x60]),
        "blue" => (3.0, 8.0, [0x40, 0x90, 0xff]),
        "white" => (3.1, 8.0, [0xf4, 0xf6, 0xff]),
        _ => (2.0, 2.0, [0xff, 0x30, 0x20]),
    };
    (DiodeM::from_vf(vf, 0.02, 2.0, rs), rgb)
}

/// LED 额定电流（亮度按这个算 100%）和最大电流
pub const LED_INOM: f64 = 0.02;
pub const LED_IMAX: f64 = 0.03;

pub struct BjtSpec {
    pub m: BjtM,
    pub darlington: bool,
    pub icmax: f64,
    pub vceo: f64,
    pub pmax: f64,
}

pub fn bjt(model: &str, pnp: bool) -> BjtSpec {
    let (bf, icmax, vceo, pmax, darl) = match model {
        "2N2222" => (150.0, 0.6, 40.0, 0.625, false),
        "9013" => (150.0, 0.5, 25.0, 0.625, false),
        "TIP120" => (1000.0, 5.0, 60.0, 2.0, true),
        "2N3906" => (150.0, 0.2, 40.0, 0.625, false),
        "9012" => (150.0, 0.5, 25.0, 0.625, false),
        _ => (200.0, 0.5, 25.0, 0.625, false),
    };
    BjtSpec {
        m: BjtM {
            is: 1e-14,
            bf: if darl { 30.0 } else { bf },
            br: 5.0,
            pnp,
        },
        darlington: darl,
        icmax,
        vceo,
        pmax,
    }
}

pub struct MosSpec {
    pub m: MosM,
    pub vds: f64,
    pub vgs: f64,
    pub idmax: f64,
    pub pmax: f64,
    /// 是否逻辑电平（3.3V 能完全打开）
    pub logic: bool,
}

pub fn mos(model: &str) -> MosSpec {
    let (vth, rds, at, vds, vgs, idmax, pmax, pch, logic) = match model {
        "AO3400" => (1.05, 0.028, 4.5, 30.0, 12.0, 5.7, 1.4, false, true),
        "IRF540N" => (3.0, 0.044, 10.0, 100.0, 20.0, 33.0, 2.4, false, false),
        "2N7002" => (2.1, 2.0, 10.0, 60.0, 20.0, 0.115, 0.2, false, false),
        "AO3401" => (0.9, 0.05, 4.5, 30.0, 12.0, 4.0, 1.4, true, true),
        "IRF9540N" => (3.0, 0.117, 10.0, 100.0, 20.0, 23.0, 2.4, true, false),
        _ => (1.5, 0.022, 5.0, 55.0, 16.0, 47.0, 2.4, false, true),
    };
    MosSpec {
        m: MosM::from_rds(vth, rds, at, pch),
        vds,
        vgs,
        idmax,
        pmax,
        logic,
    }
}

pub struct LdoSpec {
    pub vout: f64,
    pub drop: f64,
    pub ilim: f64,
    pub iq: f64,
    pub vin_max: f64,
    /// 结到环境热阻（°C/W）
    pub rth: f64,
}

pub fn ldo(model: &str) -> LdoSpec {
    match model {
        "AMS1117-5.0" => LdoSpec {
            vout: 5.0,
            drop: 1.1,
            ilim: 1.0,
            iq: 5e-3,
            vin_max: 15.0,
            rth: 90.0,
        },
        "ME6211-3.3" => LdoSpec {
            vout: 3.3,
            drop: 0.1,
            ilim: 0.5,
            iq: 40e-6,
            vin_max: 6.5,
            rth: 250.0,
        },
        "HT7333" => LdoSpec {
            vout: 3.3,
            drop: 0.09,
            ilim: 0.25,
            iq: 4e-6,
            vin_max: 12.0,
            rth: 250.0,
        },
        "LM7805" => LdoSpec {
            vout: 5.0,
            drop: 2.0,
            ilim: 1.5,
            iq: 5e-3,
            vin_max: 35.0,
            rth: 50.0,
        },
        _ => LdoSpec {
            vout: 3.3,
            drop: 1.1,
            ilim: 1.0,
            iq: 5e-3,
            vin_max: 15.0,
            rth: 90.0,
        },
    }
}

/// 电池：标称、满电、截止电压，内阻，默认容量
pub struct BatSpec {
    pub vnom: f64,
    pub vfull: f64,
    pub vcut: f64,
    pub r: f64,
    pub mah: f64,
    /// 每月自放电（比例）
    pub self_dis: f64,
}

pub fn battery(kind: &str) -> BatSpec {
    match kind {
        "aa" => BatSpec {
            vnom: 1.5,
            vfull: 1.6,
            vcut: 1.0,
            r: 0.15,
            mah: 2500.0,
            self_dis: 0.003,
        },
        "2aa" => BatSpec {
            vnom: 3.0,
            vfull: 3.2,
            vcut: 2.0,
            r: 0.3,
            mah: 2500.0,
            self_dis: 0.003,
        },
        "3aa" => BatSpec {
            vnom: 4.5,
            vfull: 4.8,
            vcut: 3.0,
            r: 0.45,
            mah: 2500.0,
            self_dis: 0.003,
        },
        "9v" => BatSpec {
            vnom: 9.0,
            vfull: 9.6,
            vcut: 6.0,
            r: 1.5,
            mah: 500.0,
            self_dis: 0.005,
        },
        "cr2032" => BatSpec {
            vnom: 3.0,
            vfull: 3.2,
            vcut: 2.0,
            r: 15.0,
            mah: 220.0,
            self_dis: 0.001,
        },
        "18650x2" => BatSpec {
            vnom: 7.4,
            vfull: 8.4,
            vcut: 6.0,
            r: 0.1,
            mah: 2600.0,
            self_dis: 0.03,
        },
        _ => BatSpec {
            vnom: 3.7,
            vfull: 4.2,
            vcut: 3.0,
            r: 0.08,
            mah: 2000.0,
            self_dis: 0.03,
        },
    }
}

/// 开路电压随剩余电量（0..1）变化：锂电有明显的平台，碱性电池斜一些
pub fn battery_ocv(b: &BatSpec, soc: f64) -> f64 {
    let s = soc.clamp(0.0, 1.0);
    // 归一化曲线：0% → 0，10% → 0.45，20%..90% 平台缓升，100% → 1
    let f = if s < 0.1 {
        0.45 * (s / 0.1).powf(0.6)
    } else if s < 0.9 {
        0.45 + 0.4 * (s - 0.1) / 0.8
    } else {
        0.85 + 0.15 * (s - 0.9) / 0.1
    };
    b.vcut + (b.vfull - b.vcut) * f
}

/// 单片机板子的电气参数
pub struct BoardSpec {
    /// 引脚输出高 / 低时的等效内阻（Ω）
    pub r_hi: f64,
    pub r_lo: f64,
    /// 引脚最大电流（A，超过就有损坏风险）
    pub imax: f64,
    /// 运行电流（A，不含外部负载）
    pub i_run: f64,
    /// 掉电复位门限（V）
    pub bod: f64,
    /// 内部上拉电阻
    pub pullup: f64,
}

pub fn board(key: &str) -> BoardSpec {
    match key {
        "uno" => BoardSpec {
            r_hi: 30.0,
            r_lo: 25.0,
            imax: 0.04,
            i_run: 0.045,
            bod: 2.7,
            pullup: 35e3,
        },
        "nano" => BoardSpec {
            r_hi: 30.0,
            r_lo: 25.0,
            imax: 0.04,
            i_run: 0.02,
            bod: 2.7,
            pullup: 35e3,
        },
        // 51：高电平是弱上拉（准双向口），只能灌电流
        "stc89" => BoardSpec {
            r_hi: 20e3,
            r_lo: 30.0,
            imax: 0.02,
            i_run: 0.015,
            bod: 3.8,
            pullup: 20e3,
        },
        "bluepill" => BoardSpec {
            r_hi: 40.0,
            r_lo: 35.0,
            imax: 0.025,
            i_run: 0.03,
            bod: 2.0,
            pullup: 40e3,
        },
        "pico" => BoardSpec {
            r_hi: 50.0,
            r_lo: 45.0,
            imax: 0.012,
            i_run: 0.025,
            bod: 2.0,
            pullup: 50e3,
        },
        "esp32cam" => BoardSpec {
            r_hi: 30.0,
            r_lo: 25.0,
            imax: 0.04,
            i_run: 0.12,
            bod: 2.45,
            pullup: 45e3,
        },
        "nodemcu" => BoardSpec {
            r_hi: 40.0,
            r_lo: 35.0,
            imax: 0.012,
            i_run: 0.07,
            bod: 2.5,
            pullup: 45e3,
        },
        _ => BoardSpec {
            r_hi: 30.0,
            r_lo: 25.0,
            imax: 0.04,
            i_run: 0.05,
            bod: 2.45,
            pullup: 45e3,
        },
    }
}

/// 模块的供电范围（最小、最大电压）和工作电流（A）。没列出的按 3.3–5.5V、5 mA
pub fn module_power(key: &str) -> (f64, f64, f64) {
    match key {
        "oled" | "oled_spi" => (3.0, 5.5, 0.02),
        "st7735" | "st7789" => (3.0, 5.5, 0.04),
        "ili9341" => (3.0, 5.5, 0.08),
        "epaper" => (2.4, 3.6, 0.005),
        "lcd1602" => (4.5, 5.5, 0.025),
        "tm1637" => (3.0, 5.5, 0.03),
        "mipi_lcd" | "mipi_cam" => (3.0, 3.6, 0.1),
        "ov7670" | "ov2640" | "ov5640" => (2.8, 3.6, 0.06),
        "esp01" => (3.0, 3.6, 0.08),
        "hc05" | "jdy31" => (3.6, 6.0, 0.03),
        "hm10" => (3.3, 6.0, 0.009),
        "cat1" => (3.4, 4.3, 0.02),
        "nbiot" => (3.1, 4.2, 0.005),
        "lora" => (1.8, 3.7, 0.012),
        "w5500" => (3.0, 3.6, 0.13),
        "lan8720" => (3.0, 3.6, 0.05),
        "max485" => (4.5, 5.5, 0.003),
        "tja1050" => (4.75, 5.25, 0.01),
        "gps" => (3.0, 5.5, 0.025),
        "dht11" => (3.0, 5.5, 0.001),
        "sht30" => (2.15, 5.5, 0.001),
        "hcsr04" => (4.5, 5.5, 0.015),
        "relay_mod" => (4.5, 5.5, 0.07),
        "ina219" | "ina226" => (3.0, 5.5, 0.001),
        "encoder" => (3.0, 5.5, 0.0),
        "servo" => (4.0, 6.0, 0.006),
        "tb6612" => (2.7, 5.5, 0.002),
        "l298n" => (5.0, 35.0, 0.036),
        "ne555" => (4.5, 16.0, 0.0),
        "lm358" => (3.0, 32.0, 0.0),
        "hc595" => (2.0, 6.0, 0.0),
        _ => (3.0, 5.5, 0.005),
    }
}
