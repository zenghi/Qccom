//! 把开发板的「引脚设置 + 规则」生成真实代码：Arduino、STM32 HAL、51（Keil C51）、ESP-IDF。
//! 生成的是能直接编译的骨架：GPIO、定时、边沿、ADC、串口打印都是真代码；
//! 舵机、温湿度、屏幕、联网这些要库的，Arduino 版直接用常见库，其它版本给出调用位置和说明。

use std::fmt::Write as _;

use super::mcu::{Act, Cond, McuCfg, PinMode};
use super::Circuit;
use crate::i18n::t;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Arduino,
    Hal,
    C51,
    EspIdf,
}

impl Dialect {
    pub fn name(self) -> &'static str {
        match self {
            Dialect::Arduino => "Arduino",
            Dialect::Hal => "STM32 HAL",
            Dialect::C51 => "51 (Keil C51)",
            Dialect::EspIdf => "ESP-IDF",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Dialect::Arduino => "ino",
            _ => "c",
        }
    }
}

/// 这块板子能生成哪些
pub fn dialects(board: &str) -> Vec<Dialect> {
    match board {
        "stc89" => vec![Dialect::C51],
        "bluepill" => vec![Dialect::Hal, Dialect::Arduino],
        "esp32" | "esp32s3" | "esp32c3" | "esp32cam" => vec![Dialect::EspIdf, Dialect::Arduino],
        _ => vec![Dialect::Arduino],
    }
}

/// 引脚在代码里的写法
fn pin_expr(d: Dialect, board: &str, pin: &str) -> String {
    let num = |s: &str| s.trim_start_matches(|c: char| c.is_ascii_alphabetic()).to_string();
    match d {
        Dialect::Arduino => match board {
            "uno" | "nano" if pin.starts_with('D') => num(pin),
            "uno" | "nano" if pin == "TX" => "1".into(),
            "uno" | "nano" if pin == "RX" => "0".into(),
            "esp32" | "esp32s3" | "esp32c3" | "esp32cam" | "pico" if pin.starts_with("IO") || pin.starts_with("GP") => num(pin),
            "esp32" if pin == "VP" => "36".into(),
            "esp32" if pin == "VN" => "39".into(),
            _ => pin.to_string(),
        },
        Dialect::Hal => {
            // PA5 → GPIOA, GPIO_PIN_5
            let port = pin.chars().nth(1).unwrap_or('A');
            format!("GPIO{port}, GPIO_PIN_{}", &pin[2.min(pin.len())..])
        }
        Dialect::C51 => pin.replace('.', "_"),
        Dialect::EspIdf => {
            let n = match pin {
                "VP" => "36".into(),
                "VN" => "39".into(),
                "TX" => "1".into(),
                "RX" => "3".into(),
                _ => num(pin),
            };
            format!("GPIO_NUM_{n}")
        }
    }
}

/// 变量名：PIN_D13、PIN_PA5、PIN_P1_0
fn ident(pin: &str) -> String {
    format!("PIN_{}", pin.replace(['.', '-', ' '], "_").to_ascii_uppercase())
}

/// Arduino：模板文字拆成一串 obj.print(...)（AVR 的 printf 不支持浮点，这样最稳）
fn arduino_chain(obj: &str, text: &str, newline: bool) -> String {
    let mut out = vec![];
    let (fmt, args) = printf_parts(text, Dialect::Arduino);
    let mut it = args.into_iter();
    // 按格式符切开
    let mut rest = fmt.as_str();
    while let Some(i) = rest.find('%') {
        if rest[i + 1..].starts_with('%') {
            out.push(format!("{obj}.print(\"{}%\");", &rest[..i]));
            rest = &rest[i + 2..];
            continue;
        }
        if i > 0 {
            out.push(format!("{obj}.print(\"{}\");", &rest[..i]));
        }
        let spec_end = rest[i + 1..].find(|c: char| c.is_ascii_alphabetic() && c != 'l').map_or(rest.len(), |k| i + 1 + k + 1);
        if let Some(a) = it.next() {
            out.push(format!("{obj}.print({a});"));
        }
        rest = &rest[spec_end.min(rest.len())..];
    }
    if !rest.is_empty() {
        out.push(format!("{obj}.print(\"{rest}\");"));
    }
    if newline {
        match out.last_mut() {
            Some(l) => *l = l.replacen(".print(", ".println(", 1),
            None => out.push(format!("{obj}.println();")),
        }
    }
    out.join(" ")
}

/// 模板文字 → (printf 格式, 参数)
fn printf_parts(text: &str, d: Dialect) -> (String, Vec<String>) {
    let mut fmt = String::new();
    let mut args = vec![];
    let mut rest = text;
    while let Some(a) = rest.find('{') {
        fmt.push_str(&rest[..a].replace('%', "%%").replace('"', "\\\""));
        let after = &rest[a + 1..];
        let Some(b) = after.find('}') else {
            fmt.push_str(&rest[a..]);
            rest = "";
            break;
        };
        let key = &after[..b];
        let (f, arg) = match key {
            "count" => ("%d", "count".to_string()),
            "temp" => ("%.1f", "temp".into()),
            "hum" => ("%.1f", "hum".into()),
            "dist" => ("%.1f", "dist".into()),
            "t" => ("%lu", if d == Dialect::Arduino { "millis() / 1000".into() } else { "uptime_s()".into() }),
            k if k.starts_with("adc:") => ("%d", format!("adc_read({})", ident(&k[4..]))),
            k if k.starts_with("v:") => ("%.2f", format!("adc_volt({})", ident(&k[2..]))),
            _ => ("%s", format!("\"{{{key}}}\"")),
        };
        fmt.push_str(f);
        args.push(arg);
        rest = &after[b + 1..];
    }
    fmt.push_str(&rest.replace('%', "%%").replace('"', "\\\""));
    (fmt, args)
}

struct W {
    s: String,
    ind: usize,
}

impl W {
    fn l(&mut self, line: impl AsRef<str>) {
        let line = line.as_ref();
        if line.is_empty() {
            self.s.push('\n');
            return;
        }
        for _ in 0..self.ind {
            self.s.push_str("    ");
        }
        self.s.push_str(line);
        self.s.push('\n');
    }
    fn open(&mut self, line: impl AsRef<str>) {
        self.l(line);
        self.ind += 1;
    }
    fn close(&mut self, line: &str) {
        self.ind = self.ind.saturating_sub(1);
        self.l(line);
    }
}

struct Api {
    d: Dialect,
    board: &'static str,
}

impl Api {
    fn write(&self, pin: &str, high: &str) -> String {
        let p = ident(pin);
        match self.d {
            Dialect::Arduino => format!(
                "digitalWrite({p}, {});",
                if high == "1" {
                    "HIGH"
                } else if high == "0" {
                    "LOW"
                } else {
                    high
                }
            ),
            Dialect::Hal => format!(
                "HAL_GPIO_WritePin({p}, {});",
                if high == "1" {
                    "GPIO_PIN_SET"
                } else if high == "0" {
                    "GPIO_PIN_RESET"
                } else {
                    high
                }
            ),
            Dialect::C51 => format!("{p} = {high};"),
            Dialect::EspIdf => format!("gpio_set_level({p}, {high});"),
        }
    }
    fn read(&self, pin: &str) -> String {
        let p = ident(pin);
        match self.d {
            Dialect::Arduino => format!("digitalRead({p})"),
            Dialect::Hal => format!("HAL_GPIO_ReadPin({p})"),
            Dialect::C51 => p,
            Dialect::EspIdf => format!("gpio_get_level({p})"),
        }
    }
    fn toggle(&self, pin: &str) -> String {
        let p = ident(pin);
        match self.d {
            Dialect::Arduino => format!("digitalWrite({p}, !digitalRead({p}));"),
            Dialect::Hal => format!("HAL_GPIO_TogglePin({p});"),
            Dialect::C51 => format!("{p} = !{p};"),
            Dialect::EspIdf => format!("gpio_set_level({p}, !gpio_get_level({p}));"),
        }
    }
    fn now(&self) -> &'static str {
        match self.d {
            Dialect::Arduino => "millis()",
            Dialect::Hal => "HAL_GetTick()",
            Dialect::C51 => "ms_ticks",
            Dialect::EspIdf => "(uint32_t)(esp_timer_get_time() / 1000)",
        }
    }
    fn print(&self, text: &str) -> String {
        let (fmt, args) = printf_parts(text, self.d);
        let a = if args.is_empty() { String::new() } else { format!(", {}", args.join(", ")) };
        match self.d {
            Dialect::Arduino => arduino_chain("Serial", text, true),
            _ => format!("printf(\"{fmt}\\r\\n\"{a});"),
        }
    }
    fn pwm(&self, pin: &str, duty: &str) -> String {
        let p = ident(pin);
        match self.d {
            Dialect::Arduino => format!("analogWrite({p}, (int)({duty} * 255));"),
            Dialect::Hal => format!("pwm_set({p}, {duty}); /* {} */", t("在 CubeMX 里给这个脚开 PWM 定时器通道")),
            Dialect::C51 => format!("pwm_duty_{p} = (unsigned char)({duty} * 100);"),
            Dialect::EspIdf => format!("pwm_set({p}, {duty});"),
        }
    }
    fn delay(&self, ms: u32) -> String {
        match self.d {
            Dialect::Arduino => format!("delay({ms});"),
            Dialect::Hal => format!("HAL_Delay({ms});"),
            Dialect::C51 => format!("delay_ms({ms});"),
            Dialect::EspIdf => format!("vTaskDelay(pdMS_TO_TICKS({ms}));"),
        }
    }
}

pub fn generate(c: &Circuit, board_id: u32, d: Dialect) -> String {
    let Some(p) = c.part(board_id) else { return String::new() };
    let Some(def) = super::lib::def(&p.kind) else { return String::new() };
    let cfg: McuCfg = serde_json::from_str(p.prop("mcu")).unwrap_or_default();
    let board = def.key;
    let api = Api { d, board };
    let baud = if cfg.baud == 0 { 115200 } else { cfg.baud };
    let mut w = W { s: String::new(), ind: 0 };
    let title = if c.title.is_empty() { t("电路") } else { c.title.as_str() };
    w.l(format!("// {} · {}", title, def.zh));
    w.l(format!("// {} QCCOM {}", t("由"), t("电路仿真生成。引脚和规则改了以后重新生成即可。")));
    let uses = |f: &dyn Fn(&Act) -> bool| cfg.rules.iter().any(|r| r.then.iter().any(f));
    let has_servo = cfg.pins.iter().any(|x| matches!(x.mode, PinMode::Servo { .. })) || uses(&|a| matches!(a, Act::ServoSet { .. } | Act::ServoFromAdc { .. }));
    let has_dht = uses(&|a| matches!(a, Act::ReadDht { .. }));
    let has_show = uses(&|a| matches!(a, Act::Show { .. }));
    let has_net = uses(&|a| matches!(a, Act::Mqtt { .. } | Act::Http { .. } | Act::Tcp { .. }));
    let has_adc = cfg.pins.iter().any(|x| matches!(x.mode, PinMode::Adc));
    let has_recv = cfg.rules.iter().any(|r| matches!(r.when, Cond::Recv { .. }));
    // ── 头文件
    match d {
        Dialect::Arduino => {
            if has_servo {
                w.l("#include <Servo.h>");
            }
            if has_dht {
                w.l(format!("#include <DHT.h>          // {}", t("库管理器装 \"DHT sensor library\"")));
            }
            if has_show {
                w.l("#include <Wire.h>");
                w.l(format!("#include <Adafruit_SSD1306.h>  // {}", t("库管理器装 \"Adafruit SSD1306\"")));
            }
            if has_net && board.starts_with("esp32") {
                w.l("#include <WiFi.h>");
                w.l(format!("#include <PubSubClient.h>     // {}", t("MQTT 用")));
            }
        }
        Dialect::Hal => {
            w.l("#include \"main.h\"");
            w.l("#include <stdio.h>");
            w.l(format!("// {}", t("用 CubeMX 生成工程后，把下面的内容放进 main.c 对应位置；printf 已重定向到串口")));
        }
        Dialect::C51 => {
            w.l("#include <reg52.h>");
            w.l("#include <stdio.h>");
        }
        Dialect::EspIdf => {
            w.l("#include <stdio.h>");
            w.l("#include \"freertos/FreeRTOS.h\"");
            w.l("#include \"freertos/task.h\"");
            w.l("#include \"driver/gpio.h\"");
            w.l("#include \"esp_timer.h\"");
            if has_adc {
                w.l("#include \"esp_adc/adc_oneshot.h\"");
            }
        }
    }
    w.l("");
    // ── 引脚定义
    for pc in &cfg.pins {
        let e = pin_expr(d, board, &pc.pin);
        match d {
            Dialect::C51 => w.l(format!("sbit {} = {};", ident(&pc.pin), pc.pin.replace('.', "^"))),
            _ => w.l(format!("#define {:<12} {}", ident(&pc.pin), e)),
        }
    }
    w.l("");
    w.l("int count = 0;");
    if has_dht || cfg.rules.iter().any(|r| matches!(r.when, Cond::TempAbove { .. } | Cond::TempBelow { .. })) {
        w.l("float temp = 0, hum = 0;");
    }
    if uses(&|a| matches!(a, Act::Measure { .. })) || cfg.rules.iter().any(|r| matches!(r.when, Cond::DistBelow { .. })) {
        w.l("float dist = 0;");
    }
    if d == Dialect::C51 {
        w.l("volatile unsigned long ms_ticks = 0;");
        w.l("");
        w.open("void timer0_isr(void) interrupt 1 {");
        w.l(format!("TH0 = 0xFC; TL0 = 0x66;   // 1 ms @ 11.0592 MHz"));
        w.l("ms_ticks++;");
        w.close("}");
        w.open("void delay_ms(unsigned int ms) {");
        w.l("unsigned long s = ms_ticks;");
        w.l("while (ms_ticks - s < ms);");
        w.close("}");
        w.open("char putchar(char c) {");
        w.l("SBUF = c; while (!TI); TI = 0; return c;");
        w.close("}");
    }
    if has_adc && d != Dialect::Arduino {
        w.l(format!("int adc_read(int pin);          // {}", t("按芯片的 ADC 驱动实现，返回原始读数")));
    }
    if has_adc {
        let (maxv, vref) = match board {
            "uno" | "nano" => (1023, 5.0),
            "esp32" | "esp32s3" | "esp32c3" | "esp32cam" => (4095, 3.3),
            "nodemcu" => (1023, 3.3),
            _ => (4095, 3.3),
        };
        if d == Dialect::Arduino {
            w.l("#define adc_read(p) analogRead(p)");
        }
        w.l(format!("#define adc_volt(p) (adc_read(p) * {vref:.1}f / {maxv})"));
    }
    if has_servo && d == Dialect::Arduino {
        for pc in cfg.pins.iter().filter(|x| matches!(x.mode, PinMode::Servo { .. })) {
            w.l(format!("Servo servo_{};", ident(&pc.pin).to_lowercase()));
        }
    }
    if has_dht && d == Dialect::Arduino {
        for r in &cfg.rules {
            for a in &r.then {
                if let Act::ReadDht { pin } = a {
                    w.l(format!("DHT dht({}, DHT11);", ident(pin)));
                }
            }
        }
    }
    if has_show && d == Dialect::Arduino {
        w.l("Adafruit_SSD1306 oled(128, 64, &Wire, -1);");
    }
    // 定时、边沿的状态变量
    for (k, r) in cfg.rules.iter().enumerate() {
        match &r.when {
            Cond::Every { .. } => w.l(format!("unsigned long t_rule{k} = 0;")),
            Cond::Falling { pin } | Cond::Rising { pin } => w.l(format!("int last_rule{k} = -1;   // {}", ident(pin))),
            Cond::WhileLow { .. } | Cond::WhileHigh { .. } => w.l(format!("unsigned long t_rule{k} = 0;")),
            Cond::Above { .. } | Cond::Below { .. } | Cond::TempAbove { .. } | Cond::TempBelow { .. } | Cond::DistBelow { .. } => w.l(format!("int was_rule{k} = 0;")),
            _ => {}
        }
    }
    for (k, pc) in cfg.pins.iter().enumerate() {
        if matches!(pc.mode, PinMode::Blink { .. } | PinMode::Breath { .. } | PinMode::Seq { .. }) {
            w.l(format!("unsigned long t_pin{k} = 0;"));
        }
    }
    if has_recv {
        w.l("char line_buf[64];");
        w.l("int line_len = 0;");
    }
    w.l("");
    // ── 规则动作
    let act_lines = |a: &Act| -> Vec<String> {
        match a {
            Act::Set { pin, high } => vec![api.write(pin, if *high { "1" } else { "0" })],
            Act::Toggle { pin } => vec![api.toggle(pin)],
            Act::PwmSet { pin, duty } => vec![api.pwm(pin, &format!("{duty:.2}f"))],
            Act::PwmFromAdc { pin, adc } => vec![api.pwm(pin, &format!("adc_volt({}) / {:.1}f", ident(adc), if matches!(board, "uno" | "nano") { 5.0 } else { 3.3 }))],
            Act::ServoSet { pin, angle } => match d {
                Dialect::Arduino => vec![format!("servo_{}.write({angle:.0});", ident(pin).to_lowercase())],
                _ => vec![format!("servo_set({}, {angle:.0});  // {}", ident(pin), t("50 Hz PWM，脉宽 0.5–2.5 ms 对应 0–180°"))],
            },
            Act::ServoFromAdc { pin, adc } => match d {
                Dialect::Arduino => vec![format!("servo_{}.write(map(analogRead({}), 0, 1023, 0, 180));", ident(pin).to_lowercase(), ident(adc))],
                _ => vec![format!("servo_set({}, adc_volt({}) / 3.3f * 180);", ident(pin), ident(adc))],
            },
            Act::Print { text } => vec![api.print(text)],
            Act::Tone { pin, freq, ms } => match d {
                Dialect::Arduino => vec![format!("tone({}, {freq}, {ms});", ident(pin))],
                _ => vec![format!("buzzer_tone({}, {freq}, {ms});  // {}", ident(pin), t("用定时器 PWM 输出方波"))],
            },
            Act::Shift { data, clk, latch, value } => {
                let v = if value.contains("{count}") { "count".to_string() } else { value.clone() };
                match d {
                    Dialect::Arduino => vec![api.write(latch, "0"), format!("shiftOut({}, {}, MSBFIRST, {v});", ident(data), ident(clk)), api.write(latch, "1")],
                    _ => vec![
                        api.write(latch, "0"),
                        format!(
                            "for (int b = 7; b >= 0; b--) {{ {} {} {} }}",
                            api.write(data, &format!("(({v}) >> b) & 1")),
                            api.write(clk, "1"),
                            api.write(clk, "0")
                        ),
                        api.write(latch, "1"),
                    ],
                }
            }
            Act::Seg { pins, value, common_anode } => {
                let v = if value.contains("{count}") { "count % 10".to_string() } else { value.clone() };
                let mut out = vec![format!(
                    "{{ static const unsigned char seg[10] = {{0x3F,0x06,0x5B,0x4F,0x66,0x6D,0x7D,0x07,0x7F,0x6F}}; unsigned char m = seg[({v}) % 10];{}",
                    if *common_anode { " m = ~m;" } else { "" }
                )];
                for (i, p) in pins.iter().enumerate() {
                    out.push(format!("  {}", api.write(p, &format!("(m >> {i}) & 1"))));
                }
                out.push("}".into());
                out
            }
            Act::Tm1637 { clk, dio, text } => vec![format!("tm1637_show({}, {}, \"{}\");  // {}", ident(clk), ident(dio), text, t("Arduino 可用 TM1637Display 库"))],
            Act::Show { text } => match d {
                Dialect::Arduino => vec!["oled.clearDisplay(); oled.setCursor(0, 0);".into(), arduino_chain("oled", text, false), "oled.display();".into()],
                _ => vec![format!("oled_show(\"{}\");  // {}", text.replace('"', "\\\""), t("SSD1306 驱动：u8g2 或厂商例程"))],
            },
            Act::ShowImage => vec![format!("// {}", t("显示摄像头画面：需要屏幕驱动"))],
            Act::ReadDht { .. } => match d {
                Dialect::Arduino => vec!["temp = dht.readTemperature(); hum = dht.readHumidity();".into()],
                _ => vec![format!("dht11_read(&temp, &hum);  // {}", t("单总线时序：拉低 18 ms 后读 40 位"))],
            },
            Act::ReadSht => vec![format!("sht30_read(&temp, &hum);  // I2C 0x44")],
            Act::Measure { trig, echo } => match d {
                Dialect::Arduino => vec![
                    api.write(trig, "1"),
                    "delayMicroseconds(10);".into(),
                    api.write(trig, "0"),
                    format!("dist = pulseIn({}, HIGH, 30000) / 58.0;", ident(echo)),
                ],
                _ => vec![format!(
                    "dist = hcsr04_measure({}, {});  // {}",
                    ident(trig),
                    ident(echo),
                    t("10 µs 触发脉冲，回波高电平宽度 / 58 = 厘米")
                )],
            },
            Act::Count => vec!["count++;".into()],
            Act::Mqtt { host, port, topic, payload } => {
                let (fmt, args) = printf_parts(payload, d);
                let a = if args.is_empty() { String::new() } else { format!(", {}", args.join(", ")) };
                vec![format!(
                    "{{ char msg[96]; snprintf(msg, sizeof msg, \"{fmt}\"{a}); mqtt_publish(\"{host}\", {port}, \"{topic}\", msg); }}"
                )]
            }
            Act::Http { url, body } => vec![format!("http_post(\"{url}\", \"{}\");", body.replace('"', "\\\""))],
            Act::Tcp { host, port, text } => vec![format!("tcp_send(\"{host}\", {port}, \"{}\");", text.replace('"', "\\\""))],
            Act::Photo => vec![format!("camera_capture();  // esp_camera_fb_get()")],
            Act::Sleep { ms } => vec![api.delay(*ms)],
        }
    };
    let emit_acts = |w: &mut W, acts: &[Act]| {
        for a in acts {
            for l in act_lines(a) {
                w.l(l);
            }
        }
    };
    // ── setup
    let (setup_sig, loop_sig) = match d {
        Dialect::Arduino => ("void setup() {", "void loop() {"),
        Dialect::Hal => ("void app_setup(void) {", "void app_loop(void) {"),
        Dialect::C51 => ("void setup(void) {", "void loop(void) {"),
        Dialect::EspIdf => ("static void setup(void) {", "static void loop_once(void) {"),
    };
    w.open(setup_sig);
    match d {
        Dialect::Arduino => w.l(format!("Serial.begin({baud});")),
        Dialect::Hal => w.l(format!("// {}", t("时钟、GPIO、USART 由 CubeMX 初始化（MX_GPIO_Init 等）"))),
        Dialect::C51 => {
            w.l("TMOD = 0x21;               // T0: 1 ms tick, T1: baud rate");
            w.l("TH0 = 0xFC; TL0 = 0x66; ET0 = 1; TR0 = 1;");
            w.l(format!("TH1 = TL1 = 256 - 11059200UL / 12 / 32 / {baud}; TR1 = 1;"));
            w.l("SCON = 0x50; TI = 0; EA = 1;");
        }
        Dialect::EspIdf => {}
    }
    for pc in &cfg.pins {
        let p = ident(&pc.pin);
        match d {
            Dialect::Arduino => {
                let m = match &pc.mode {
                    m if m.is_output() && !matches!(m, PinMode::Servo { .. }) => Some("OUTPUT"),
                    PinMode::InPullUp => Some("INPUT_PULLUP"),
                    PinMode::InPullDown if board.starts_with("esp32") => Some("INPUT_PULLDOWN"),
                    PinMode::InPullDown | PinMode::InFloat => Some("INPUT"),
                    _ => None,
                };
                if let Some(m) = m {
                    w.l(format!("pinMode({p}, {m});"));
                }
                if let PinMode::Servo { angle } = pc.mode {
                    w.l(format!("servo_{}.attach({p}); servo_{}.write({angle:.0});", p.to_lowercase(), p.to_lowercase()));
                }
            }
            Dialect::EspIdf => {
                if pc.mode.is_output() {
                    w.l(format!("gpio_reset_pin({p}); gpio_set_direction({p}, GPIO_MODE_OUTPUT);"));
                } else if matches!(pc.mode, PinMode::InPullUp | PinMode::InPullDown | PinMode::InFloat) {
                    w.l(format!("gpio_reset_pin({p}); gpio_set_direction({p}, GPIO_MODE_INPUT);"));
                    match pc.mode {
                        PinMode::InPullUp => w.l(format!("gpio_set_pull_mode({p}, GPIO_PULLUP_ONLY);")),
                        PinMode::InPullDown => w.l(format!("gpio_set_pull_mode({p}, GPIO_PULLDOWN_ONLY);")),
                        _ => {}
                    }
                }
            }
            Dialect::Hal => {
                let m = match pc.mode {
                    ref m if m.is_output() => "GPIO_MODE_OUTPUT_PP",
                    PinMode::InPullUp => "GPIO_MODE_INPUT + GPIO_PULLUP",
                    PinMode::InPullDown => "GPIO_MODE_INPUT + GPIO_PULLDOWN",
                    PinMode::Adc => "GPIO_MODE_ANALOG",
                    _ => "GPIO_MODE_INPUT",
                };
                w.l(format!("// {}: {m}  ({})", pc.pin, t("在 CubeMX 里设置")));
            }
            Dialect::C51 => {
                if matches!(pc.mode, PinMode::InPullUp | PinMode::InFloat) {
                    w.l(format!("{p} = 1;   // {}", t("51 准双向口：写 1 才能读输入")));
                }
            }
        }
        match pc.mode {
            PinMode::OutHigh => w.l(api.write(&pc.pin, "1")),
            PinMode::OutLow => w.l(api.write(&pc.pin, "0")),
            PinMode::Pwm { duty, .. } => w.l(api.pwm(&pc.pin, &format!("{duty:.2}f"))),
            PinMode::Tone { freq } if d == Dialect::Arduino => w.l(format!("tone({p}, {freq});")),
            _ => {}
        }
    }
    if has_dht && d == Dialect::Arduino {
        w.l("dht.begin();");
    }
    if has_show && d == Dialect::Arduino {
        w.l("oled.begin(SSD1306_SWITCHCAPVCC, 0x3C); oled.setTextSize(1); oled.setTextColor(SSD1306_WHITE);");
    }
    for r in cfg.rules.iter().filter(|r| r.when == Cond::Start) {
        emit_acts(&mut w, &r.then);
    }
    w.close("}");
    w.l("");
    // ── loop
    w.open(loop_sig);
    w.l(format!("unsigned long now = {};", api.now()));
    for (k, pc) in cfg.pins.iter().enumerate() {
        match pc.mode {
            PinMode::Blink { ms } => {
                w.open(format!("if (now - t_pin{k} >= {}) {{   // {} {ms} ms", ms.max(1) / 2, t("闪烁周期")));
                w.l(format!("t_pin{k} = now;"));
                w.l(api.toggle(&pc.pin));
                w.close("}");
            }
            PinMode::Breath { ms } => {
                w.l(format!(
                    "{{ unsigned long ph = now % {ms}; float x = ph < {h} ? (float)ph / {h} : (float)({ms} - ph) / {h}; {} }}",
                    api.pwm(&pc.pin, "x * x"),
                    h = ms.max(2) / 2
                ));
            }
            PinMode::Seq { group, index, ms } => {
                let n = cfg.pins.iter().filter(|x| matches!(x.mode, PinMode::Seq { group: g, .. } if g == group)).count().max(1);
                w.l(format!("{}", api.write(&pc.pin, &format!("((now / {ms}) % {n}) == {index}"))));
            }
            _ => {}
        }
    }
    if has_recv {
        match d {
            Dialect::Arduino => {
                w.open("while (Serial.available()) {");
                w.l("char ch = Serial.read();");
            }
            Dialect::C51 => {
                w.open("while (RI) {");
                w.l("char ch = SBUF; RI = 0;");
            }
            _ => {
                w.l("int ch;");
                w.open("while ((ch = getchar()) != EOF) {");
            }
        }
        w.open("if (ch == '\\n' || ch == '\\r') {");
        w.l("line_buf[line_len] = 0;");
        for r in &cfg.rules {
            if let Cond::Recv { text } = &r.when {
                w.open(format!("if (line_len > 0 && strcasecmp(line_buf, \"{}\") == 0) {{", text.replace('"', "\\\"")));
                emit_acts(&mut w, &r.then);
                w.close("}");
            }
        }
        w.l("line_len = 0;");
        w.close("} else if (line_len < 63) line_buf[line_len++] = ch;");
        w.close("}");
    }
    for (k, r) in cfg.rules.iter().enumerate() {
        match &r.when {
            Cond::Start | Cond::Recv { .. } => continue,
            Cond::Every { ms } => {
                w.open(format!("if (now - t_rule{k} >= {ms}) {{"));
                w.l(format!("t_rule{k} = now;"));
            }
            Cond::Falling { pin } | Cond::Rising { pin } => {
                let fall = matches!(r.when, Cond::Falling { .. });
                w.open("{");
                w.l(format!("int v = {};", api.read(pin)));
                w.open(format!("if (last_rule{k} == {} && v == {}) {{", if fall { 1 } else { 0 }, if fall { 0 } else { 1 }));
                emit_acts(&mut w, &r.then);
                w.close("}");
                w.l(format!("last_rule{k} = v;"));
                w.close("}");
                continue;
            }
            Cond::WhileLow { pin } | Cond::WhileHigh { pin } => {
                let lv = if matches!(r.when, Cond::WhileLow { .. }) { 0 } else { 1 };
                w.open(format!("if ({} == {lv} && now - t_rule{k} >= 20) {{", api.read(pin)));
                w.l(format!("t_rule{k} = now;"));
            }
            Cond::Above { pin, v } | Cond::Below { pin, v } => {
                let op = if matches!(r.when, Cond::Above { .. }) { ">" } else { "<" };
                w.open("{");
                w.l(format!("int hit = adc_volt({}) {op} {v:.2}f;", ident(pin)));
                w.open(format!("if (hit && !was_rule{k}) {{"));
                emit_acts(&mut w, &r.then);
                w.close("}");
                w.l(format!("was_rule{k} = hit;"));
                w.close("}");
                continue;
            }
            Cond::TempAbove { c: v } | Cond::TempBelow { c: v } | Cond::DistBelow { cm: v } => {
                let (var, op) = match r.when {
                    Cond::TempAbove { .. } => ("temp", ">"),
                    Cond::TempBelow { .. } => ("temp", "<"),
                    _ => ("dist", "<"),
                };
                w.open("{");
                w.l(format!("int hit = {var} {op} {v:.1}f;"));
                w.open(format!("if (hit && !was_rule{k}) {{"));
                emit_acts(&mut w, &r.then);
                w.close("}");
                w.l(format!("was_rule{k} = hit;"));
                w.close("}");
                continue;
            }
        }
        emit_acts(&mut w, &r.then);
        w.close("}");
    }
    w.close("}");
    // ── main
    match d {
        Dialect::Arduino => {}
        Dialect::Hal => {
            w.l("");
            w.l(format!("// {}", t("main() 里：MX_xxx_Init() 之后调用 app_setup()，while(1) 里调用 app_loop()")));
        }
        Dialect::C51 => {
            w.l("");
            w.open("void main(void) {");
            w.l("setup();");
            w.l("while (1) loop();");
            w.close("}");
        }
        Dialect::EspIdf => {
            w.l("");
            w.open("void app_main(void) {");
            w.l("setup();");
            w.open("while (1) {");
            w.l("loop_once();");
            w.l("vTaskDelay(1);");
            w.close("}");
            w.close("}");
        }
    }
    let _ = write!(w.s, "");
    w.s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuit::mcu::{PinCfg, Rule};

    #[test]
    fn button_toggles_led_in_all_dialects() {
        for (board, led, btn) in [("uno", "D13", "D2"), ("bluepill", "PC13", "PA0"), ("stc89", "P1.0", "P3.2"), ("esp32", "IO2", "IO4")] {
            let mut c = Circuit::default();
            let u = c.add_part(board, 0, 0);
            let cfg = McuCfg {
                pins: vec![
                    PinCfg {
                        pin: led.into(),
                        mode: PinMode::OutLow,
                    },
                    PinCfg {
                        pin: btn.into(),
                        mode: PinMode::InPullUp,
                    },
                ],
                rules: vec![
                    Rule {
                        when: Cond::Falling { pin: btn.into() },
                        then: vec![Act::Toggle { pin: led.into() }, Act::Count, Act::Print { text: "pressed {count}".into() }],
                    },
                    Rule {
                        when: Cond::Every { ms: 1000 },
                        then: vec![Act::Print { text: "alive".into() }],
                    },
                ],
                baud: 9600,
            };
            c.part_mut(u).unwrap().set("mcu", serde_json::to_string(&cfg).unwrap());
            for d in dialects(board) {
                let code = generate(&c, u, d);
                if std::env::var("SHOWGEN").is_ok() {
                    eprintln!("==== {board} {d:?}\n{code}");
                }
                assert!(code.contains("count++"), "{board} {d:?}\n{code}");
                assert!(code.contains("%d") || code.contains("Serial.println(count);"), "{code}");
                // 花括号配对
                assert_eq!(code.matches('{').count(), code.matches('}').count(), "{board} {d:?}\n{code}");
                match d {
                    Dialect::Arduino if board == "uno" => assert!(code.contains("#define PIN_D13      13") && code.contains("INPUT_PULLUP")),
                    Dialect::Hal => assert!(code.contains("GPIOC, GPIO_PIN_13") && code.contains("HAL_GPIO_TogglePin")),
                    Dialect::C51 => assert!(code.contains("sbit PIN_P1_0 = P1^0;") && code.contains("void main")),
                    Dialect::EspIdf => assert!(code.contains("GPIO_NUM_2") && code.contains("app_main")),
                    _ => {}
                }
            }
        }
    }
}
