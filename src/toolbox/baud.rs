//! 波特率计算：给定外设时钟和目标波特率，算各家 UART 的分频寄存器值和实际误差。

pub struct Row {
    /// 芯片 / 寄存器
    pub name: &'static str,
    /// 寄存器值（已格式化）
    pub reg: String,
    pub actual: f64,
    /// 误差，百分比
    pub err: f64,
}

/// STM32 USART：BRR = fck / baud（OVER16）；OVER8 时低 3 位右移一位
fn stm32(clk: f64, baud: f64, over8: bool) -> Option<Row> {
    let div = if over8 { 2.0 * clk / baud } else { clk / baud };
    let usartdiv = div.round() as u64;
    if usartdiv < 16 || usartdiv > 0xFFFF {
        return None;
    }
    let brr = if over8 { (usartdiv & !0xF) | ((usartdiv & 0xF) >> 1) } else { usartdiv };
    let actual = if over8 { 2.0 * clk / usartdiv as f64 } else { clk / usartdiv as f64 };
    Some(Row {
        name: if over8 { "STM32 BRR (OVER8)" } else { "STM32 BRR (OVER16)" },
        reg: format!("0x{brr:04X}  ({brr})"),
        actual,
        err: (actual - baud) / baud * 100.0,
    })
}

/// AVR：UBRR = fosc / (16 · baud) − 1，U2X 时用 8
fn avr(clk: f64, baud: f64, u2x: bool) -> Option<Row> {
    let k = if u2x { 8.0 } else { 16.0 };
    let ubrr = (clk / (k * baud) - 1.0).round();
    if !(0.0..=4095.0).contains(&ubrr) {
        return None;
    }
    let actual = clk / (k * (ubrr + 1.0));
    Some(Row {
        name: if u2x { "AVR UBRR (U2X=1)" } else { "AVR UBRR (U2X=0)" },
        reg: format!("{}", ubrr as u32),
        actual,
        err: (actual - baud) / baud * 100.0,
    })
}

/// 16550 / 8051 类：整数分频 clk / (16 · baud)
fn uart16550(clk: f64, baud: f64) -> Option<Row> {
    let d = (clk / (16.0 * baud)).round();
    if !(1.0..=65535.0).contains(&d) {
        return None;
    }
    let actual = clk / (16.0 * d);
    Some(Row {
        name: "16550 DLL/DLM",
        reg: format!("{}", d as u32),
        actual,
        err: (actual - baud) / baud * 100.0,
    })
}

/// ESP32：UART_CLKDIV = clk / baud，整数部分 + 4 位小数
fn esp32(clk: f64, baud: f64) -> Option<Row> {
    let div = clk / baud;
    let int = div.floor();
    let frag = ((div - int) * 16.0).round();
    let (int, frag) = if frag >= 16.0 { (int + 1.0, 0.0) } else { (int, frag) };
    if !(1.0..=1048575.0).contains(&int) {
        return None;
    }
    let actual = clk / (int + frag / 16.0);
    Some(Row {
        name: "ESP32 CLKDIV",
        reg: format!("{} + {}/16", int as u32, frag as u32),
        actual,
        err: (actual - baud) / baud * 100.0,
    })
}

pub fn table(clk_hz: f64, baud: f64) -> Vec<Row> {
    if clk_hz <= 0.0 || baud <= 0.0 {
        return vec![];
    }
    [
        stm32(clk_hz, baud, false),
        stm32(clk_hz, baud, true),
        avr(clk_hz, baud, false),
        avr(clk_hz, baud, true),
        uart16550(clk_hz, baud),
        esp32(clk_hz, baud),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// 常用波特率在这个时钟下（STM32 OVER16）的误差，用来挑一个误差小的
pub fn common_errors(clk_hz: f64) -> Vec<(u32, Option<f64>)> {
    [9600u32, 19200, 38400, 57600, 115200, 230400, 460800, 921600, 1_000_000, 1_500_000, 2_000_000, 3_000_000]
        .iter()
        .map(|&b| (b, stm32(clk_hz, b as f64, false).map(|r| r.err)))
        .collect()
}

/// 「72M」「16 MHz」「8000000」都能认
pub fn parse_hz(s: &str) -> Option<f64> {
    let t = s.trim().to_ascii_lowercase().replace(' ', "").replace("hz", "");
    let (num, mul) = if let Some(n) = t.strip_suffix('m') {
        (n, 1e6)
    } else if let Some(n) = t.strip_suffix('k') {
        (n, 1e3)
    } else {
        (t.as_str(), 1.0)
    };
    num.parse::<f64>().ok().map(|v| v * mul).filter(|v| *v > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        // STM32F1 72 MHz 115200 → BRR 0x271
        let r = stm32(72e6, 115200.0, false).unwrap();
        assert_eq!(r.reg, "0x0271  (625)");
        assert!(r.err.abs() < 0.01);
        // AVR 16 MHz 9600 → UBRR 103，误差 0.16%
        let r = avr(16e6, 9600.0, false).unwrap();
        assert_eq!(r.reg, "103");
        assert!((r.err - 0.16).abs() < 0.01);
        // ESP32 APB 80 MHz 115200 → 694 + 7/16
        assert_eq!(esp32(80e6, 115200.0).unwrap().reg, "694 + 7/16");
        assert_eq!(parse_hz("72M"), Some(72e6));
        assert_eq!(parse_hz("16 MHz"), Some(16e6));
        assert_eq!(parse_hz("32768"), Some(32768.0));
    }
}
