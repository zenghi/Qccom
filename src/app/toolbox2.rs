//! 工具箱 v1.4 新增的工具：Modbus 从站、Syslog、Ping / 端口检测、子网计算、本地 MQTT 服务器、
//! 校验计算（可自定义 CRC）、时间戳、正则测试、字模 / 图片取模、颜色转换、ASCII 码表。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::toolbox::bitmap::{self, ColorFmt, Mono, Scan};
use crate::toolbox::crc::{self, Crc, PRESETS};
use crate::toolbox::{broker, mbslave, netcalc, ping, syslog};

use super::logx::small_chip;
use super::toolbox::{cell, copy_label, dense_rows, header_row, hint_line, out_row, server_button, settings_card, Tool, MONO, ROW_H};
use super::*;

/// 记在设置里的部分
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Cfg2 {
    pub mb_tcp: bool,
    pub mb_port: u16,
    pub mb_serial: String,
    pub mb_baud: u32,
    pub mb_unit: u8,
    /// 0 线圈 1 离散输入 2 保持寄存器 3 输入寄存器
    pub mb_table: u8,
    pub mb_start: u16,
    pub mb_hex: bool,
    pub syslog_port: u16,
    pub ping_host: String,
    pub ping_count: u32,
    pub ping_iv: u32,
    pub scan_host: String,
    pub scan_ports: String,
    pub subnet: String,
    pub subnet_other: String,
    pub broker_port: u16,
    pub broker_user: String,
    pub broker_pass: String,
    pub broker_topic: String,
    pub broker_payload: String,
    pub broker_retain: bool,
    pub crc: Crc,
    pub crc_hex: bool,
    pub crc_input: String,
    pub ts_input: String,
    pub re_pat: String,
    pub re_text: String,
    pub re_repl: String,
    pub re_icase: bool,
    /// 取模：false 文字，true 图片
    pub fm_image: bool,
    pub fm_text: String,
    pub fm_font: String,
    pub fm_size: f32,
    pub fm_w: u16,
    pub fm_h: u16,
    pub fm_dx: i32,
    pub fm_dy: i32,
    pub fm_thr: u8,
    pub fm_scan: Scan,
    pub fm_msb: bool,
    pub fm_invert: bool,
    pub fm_name: String,
    pub img_path: String,
    pub img_w: u16,
    pub img_h: u16,
    pub img_keep: bool,
    pub img_color: bool,
    pub img_dither: bool,
    pub img_fmt: ColorFmt,
    pub color: String,
    pub ascii_ext: bool,
}

impl Default for Cfg2 {
    fn default() -> Self {
        Cfg2 {
            mb_tcp: true,
            mb_port: if cfg!(windows) { 502 } else { 1502 },
            mb_serial: String::new(),
            mb_baud: 9600,
            mb_unit: 1,
            mb_table: 2,
            mb_start: 0,
            mb_hex: false,
            syslog_port: if cfg!(windows) { 514 } else { 5514 },
            ping_host: "192.168.1.1".into(),
            ping_count: 0,
            ping_iv: 1000,
            scan_host: "192.168.1.1".into(),
            scan_ports: "21-23, 80, 443, 502, 1883, 8080, 8883".into(),
            subnet: "192.168.1.10/24".into(),
            subnet_other: String::new(),
            broker_port: 1883,
            broker_user: String::new(),
            broker_pass: String::new(),
            broker_topic: "qccom/test".into(),
            broker_payload: "hello".into(),
            broker_retain: false,
            crc: Crc::default(),
            crc_hex: true,
            crc_input: "01 03 00 00 00 0A".into(),
            ts_input: String::new(),
            re_pat: r"temp=(\d+\.\d+)".into(),
            re_text: "I (1234) sensor: temp=23.50 hum=41\nI (2234) sensor: temp=23.62 hum=40".into(),
            re_repl: "$1".into(),
            re_icase: false,
            fm_image: false,
            fm_text: "温度25℃".into(),
            fm_font: String::new(),
            fm_size: 16.0,
            fm_w: 16,
            fm_h: 16,
            fm_dx: 0,
            fm_dy: 0,
            fm_thr: 110,
            fm_scan: Scan::Rows,
            fm_msb: true,
            fm_invert: false,
            fm_name: "font16".into(),
            img_path: String::new(),
            img_w: 64,
            img_h: 64,
            img_keep: true,
            img_color: false,
            img_dither: false,
            img_fmt: ColorFmt::Rgb565,
            color: "#22D3EE".into(),
            ascii_ext: false,
        }
    }
}

/// 运行时状态
#[derive(Default)]
pub struct Ui2 {
    mb: Option<mbslave::Slave>,
    mb_st: mbslave::Shared,
    syslog: Option<syslog::Server>,
    syslog_filter: String,
    /// 只看这个级别及更严重的（8 = 全部）
    syslog_min: u8,
    ping_stop: Option<Arc<AtomicBool>>,
    ping_st: ping::Shared,
    scan_st: ping::ScanShared,
    broker: Option<broker::Broker>,
    /// 当前字体文件（内存映射）
    font: Option<(String, memmap2::Mmap)>,
    font_err: Option<String>,
    /// 取模结果：每个字符的点阵
    glyphs: Vec<(char, Mono)>,
    glyph_key: String,
    img: Option<(String, image::RgbaImage)>,
    img_err: Option<String>,
    img_out: Option<(image::RgbaImage, Mono)>,
    img_key: String,
    preview: Option<egui::TextureHandle>,
    preview_key: String,
}

pub(super) fn tool_text2(tool: Tool) -> (&'static str, &'static str) {
    match tool {
        Tool::MbSlave => (t("Modbus 从站"), t("模拟一个 Modbus TCP / RTU 从站，测试主站程序或 HMI；寄存器值可以直接改")),
        Tool::Syslog => (t("Syslog 接收"), t("接收路由器、交换机、Linux 开发板通过 syslog 发来的日志")),
        Tool::Ping => (t("Ping / 端口"), t("连续 Ping 看延迟和丢包，或者检测设备开了哪些 TCP 端口")),
        Tool::Subnet => (t("子网计算"), t("算网络地址、广播地址、可用 IP 范围，判断两个地址是否在同一网段")),
        Tool::Broker => (t("MQTT 服务器"), t("在本机开一个 MQTT 服务器，不用另外装 Mosquitto")),
        Tool::Checksum => (t("校验计算"), t("常用 CRC、累加和一次全算出来，也可以自定义 CRC 参数")),
        Tool::Timestamp => (t("时间戳转换"), t("Unix 时间戳和日期时间互相转换，秒和毫秒都认")),
        Tool::Regex => (t("正则测试"), t("写日志过滤规则、提取数据前先在这里试正则")),
        Tool::Font => (t("字模 / 取模"), t("文字和图片转成点阵或 RGB565 数组，给 LCD、OLED 用")),
        Tool::Color => (t("颜色转换"), t("RGB888、RGB565、HSV 互相转换")),
        Tool::Ascii => (t("ASCII 码表"), t("字符、十进制、十六进制、八进制和控制字符的含义")),
        other => super::toolbox3::tool_text3(other),
    }
}

impl App {
    pub(super) fn tool2_running(&self, tool: Tool) -> bool {
        match tool {
            Tool::MbSlave => self.tb.x.mb.is_some(),
            Tool::Syslog => self.tb.x.syslog.is_some(),
            Tool::Ping => self.tb.x.ping_st.lock().unwrap().running || self.tb.x.scan_st.lock().unwrap().running,
            Tool::Broker => self.tb.x.broker.is_some(),
            _ => false,
        }
    }

    pub(super) fn tick_toolbox2(&mut self) {
        if self.tb.x.mb.is_some() {
            let err = self.tb.x.mb_st.lock().unwrap().err.take();
            if let Some(e) = err {
                self.tb.x.mb = None;
                self.toast(tf("Modbus 从站已停止：{}", &[&e]), ERR());
            }
        }
    }

    pub(super) fn tb_more(&mut self, ui: &mut egui::Ui, tool: Tool) {
        match tool {
            Tool::MbSlave => self.tb_mbslave(ui),
            Tool::Syslog => self.tb_syslog(ui),
            Tool::Ping => self.tb_ping(ui),
            Tool::Subnet => self.tb_subnet(ui),
            Tool::Broker => self.tb_broker(ui),
            Tool::Checksum => self.tb_checksum(ui),
            Tool::Timestamp => self.tb_timestamp(ui),
            Tool::Regex => self.tb_regex(ui),
            Tool::Font => self.tb_font(ui),
            Tool::Color => self.tb_color(ui),
            Tool::Ascii => self.tb_ascii(ui),
            other => self.tb_v16(ui, other),
        }
    }

    // ------------------------------------------------------------ Modbus 从站

    fn tb_mbslave(&mut self, ui: &mut egui::Ui) {
        let running = self.tb.x.mb.is_some();
        let ports = self.ports.clone();
        let c = &mut self.s.tb.x;
        if c.mb_serial.is_empty() {
            if let Some(p) = ports.first() {
                c.mb_serial = p.name.clone();
            }
        }
        let mut start = false;
        settings_card(ui, |ui| {
            ui.add_enabled_ui(!running, |ui| {
                form_row(ui, t("方式"), |ui| {
                    let mut tcp = c.mb_tcp;
                    ui.allocate_ui(vec2(260.0, 28.0), |ui| tab_strip(ui, &mut tcp, &[(true, "Modbus TCP"), (false, t("RTU（串口）"))], 26.0));
                    c.mb_tcp = tcp;
                });
                if c.mb_tcp {
                    form_row(ui, t("端口"), |ui| {
                        ui.allocate_ui(vec2(90.0, 26.0), |ui| num_field(ui, "mb_port", &mut c.mb_port));
                    });
                } else {
                    form_row(ui, t("串口"), |ui| {
                        egui::ComboBox::from_id_salt("mb_serial").width(200.0).selected_text(&c.mb_serial).show_ui(ui, |ui| {
                            for p in &ports {
                                ui.selectable_value(&mut c.mb_serial, p.name.clone(), format!("{}  {}", p.name, p.desc));
                            }
                        });
                        egui::ComboBox::from_id_salt("mb_baud").width(90.0).selected_text(c.mb_baud.to_string()).show_ui(ui, |ui| {
                            for &b in COMMON_BAUDS {
                                ui.selectable_value(&mut c.mb_baud, b, b.to_string());
                            }
                        });
                    });
                }
                form_row(ui, t("从站地址"), |ui| {
                    ui.add(egui::DragValue::new(&mut c.mb_unit).range(1..=247));
                });
            });
            ui.add_space(6.0);
            start = server_button(ui, running);
        });
        if start {
            if running {
                self.tb.x.mb = None;
            } else {
                let c = &self.s.tb.x;
                let r = if c.mb_tcp {
                    mbslave::start_tcp(c.mb_port, c.mb_unit, self.tb.x.mb_st.clone(), self.ctx.clone())
                } else if self.connected.as_deref() == Some(c.mb_serial.as_str()) {
                    Err(t("这个串口正在调试页里使用").to_string())
                } else {
                    let pc = crate::serial::PortConfig {
                        baud: c.mb_baud,
                        ..Default::default()
                    };
                    mbslave::start_rtu(&c.mb_serial, pc, c.mb_unit, self.tb.x.mb_st.clone(), self.ctx.clone())
                };
                match r {
                    Ok(s) => self.tb.x.mb = Some(s),
                    Err(e) => self.toast(e, ERR()),
                }
            }
        }
        if running && self.s.tb.x.mb_tcp {
            ui.add_space(8.0);
            let ip = crate::toolbox::local_ipv4().map(|i| i.to_string()).unwrap_or_else(|| "127.0.0.1".into());
            let clients = self.tb.x.mb_st.lock().unwrap().clients;
            ui.horizontal(|ui| {
                hint_line(ui, t("主站连接："), &format!("{ip}:{}", self.s.tb.x.mb_port));
                ui.label(RichText::new(tf("{} 个主站已连接", &[&clients])).small().color(TEXT3()));
            });
        }
        ui.add_space(12.0);

        // 寄存器表
        let c = &mut self.s.tb.x;
        ui.horizontal(|ui| {
            let mut tab = c.mb_table;
            ui.allocate_ui(vec2(460.0, 28.0), |ui| {
                tab_strip(ui, &mut tab, &[(2, t("保持寄存器")), (3, t("输入寄存器")), (0, t("线圈")), (1, t("离散输入"))], 26.0)
            });
            c.mb_table = tab;
            ui.add_space(12.0);
            ui.label(RichText::new(t("起始地址")).color(TEXT2()));
            ui.add(egui::DragValue::new(&mut c.mb_start).range(0..=65535 - 39));
            if c.mb_table >= 2 {
                small_chip(ui, &mut c.mb_hex, "HEX", t("按十六进制显示和输入"));
            }
        });
        ui.add_space(6.0);
        let (table, start_addr, hex) = (c.mb_table, c.mb_start as usize, c.mb_hex);
        {
            let mut guard = self.tb.x.mb_st.lock().unwrap();
            let r = &mut guard.regs;
            egui::Frame::new()
                .fill(BG0())
                .stroke(st(1.0, LINE()))
                .corner_radius(R_CARD)
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::Grid::new("mb_regs").num_columns(8).spacing([10.0, 4.0]).show(ui, |ui| {
                        for row in 0..5 {
                            for col in 0..8 {
                                let a = start_addr + row * 8 + col;
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(format!("{a:>5}")).font(MONO(11.0)).color(TEXT3()));
                                    match table {
                                        0 | 1 => {
                                            let v = if table == 0 { &mut r.coils[a] } else { &mut r.discrete[a] };
                                            let (txt, col) = if *v { ("ON", OK()) } else { ("OFF", TEXT3()) };
                                            if ui.add(egui::Button::new(RichText::new(txt).font(MONO(11.5)).color(col)).min_size(vec2(44.0, 20.0))).clicked() {
                                                *v = !*v;
                                            }
                                        }
                                        _ => {
                                            let v = if table == 2 { &mut r.holding[a] } else { &mut r.input[a] };
                                            let mut dv = egui::DragValue::new(v).speed(1.0);
                                            if hex {
                                                dv = dv.hexadecimal(4, false, true);
                                            }
                                            ui.add_sized(vec2(56.0, 20.0), dv);
                                        }
                                    }
                                });
                            }
                            ui.end_row();
                        }
                    });
                });
        }
        ui.add_space(12.0);
        let (log, total) = {
            let s = self.tb.x.mb_st.lock().unwrap();
            (s.log.iter().rev().take(300).cloned().collect::<Vec<_>>(), s.total)
        };
        eyebrow(ui, t("请求记录"), &total.to_string());
        if log.is_empty() {
            ui.label(RichText::new(t("还没有请求")).color(TEXT3()));
            return;
        }
        header_row(
            ui,
            &[
                (t("时间"), 96.0),
                (t("来源"), 150.0),
                (t("从站"), 40.0),
                (t("功能"), 150.0),
                (t("寄存器"), 60.0),
                (t("数量"), 50.0),
                (t("结果"), 140.0),
            ],
        );
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            dense_rows(ui);
            for q in log {
                ui.horizontal(|ui| {
                    let f = MONO(12.0);
                    cell(ui, 96.0, RichText::new(&q.at).font(f.clone()).color(TEXT3()));
                    cell(ui, 150.0, RichText::new(&q.peer).font(f.clone()).color(TEXT2()));
                    cell(ui, 40.0, RichText::new(q.unit.to_string()).font(f.clone()).color(TEXT2()));
                    cell(ui, 150.0, RichText::new(mbslave::func_name(q.func)).font(f.clone()).color(TEXT()));
                    cell(ui, 60.0, RichText::new(q.addr.to_string()).font(f.clone()).color(TEXT()));
                    cell(ui, 50.0, RichText::new(q.count.to_string()).font(f.clone()).color(TEXT()));
                    match q.exc {
                        None => cell(ui, 140.0, RichText::new("OK").font(f).color(OK())),
                        Some(e) => cell(ui, 140.0, RichText::new(format!("{e:02X} {}", crate::modbus::exception_name(e))).font(f).color(ERR())),
                    }
                });
            }
        });
    }

    // ------------------------------------------------------------ Syslog

    fn tb_syslog(&mut self, ui: &mut egui::Ui) {
        let running = self.tb.x.syslog.is_some();
        let mut start = false;
        settings_card(ui, |ui| {
            ui.add_enabled_ui(!running, |ui| {
                form_row(ui, t("UDP 端口"), |ui| {
                    ui.allocate_ui(vec2(90.0, 26.0), |ui| num_field(ui, "syslog_port", &mut self.s.tb.x.syslog_port));
                    if cfg!(not(windows)) && self.s.tb.x.syslog_port < 1024 {
                        ui.label(RichText::new(t("Linux 上 1024 以下的端口需要 root")).small().color(WARN()));
                    }
                });
            });
            ui.add_space(6.0);
            start = server_button(ui, running);
        });
        if start {
            if running {
                self.tb.x.syslog = None;
            } else {
                match syslog::start(self.s.tb.x.syslog_port, self.ctx.clone()) {
                    Ok(s) => self.tb.x.syslog = Some(s),
                    Err(e) => self.toast(tf("监听端口 {} 失败：{}", &[&self.s.tb.x.syslog_port, &e]), ERR()),
                }
            }
        }
        if running {
            ui.add_space(8.0);
            let ip = crate::toolbox::local_ipv4().map(|i| i.to_string()).unwrap_or_else(|| "127.0.0.1".into());
            hint_line(ui, t("开发板上这样发："), &format!("syslogd -R {ip}:{}", self.s.tb.x.syslog_port));
        }
        ui.add_space(12.0);
        let Some(srv) = &self.tb.x.syslog else {
            ui.label(RichText::new(t("启动后，设备发来的日志按时间显示在这里")).color(TEXT3()));
            return;
        };
        let msgs: Vec<syslog::Msg> = srv.st.lock().unwrap().msgs.iter().cloned().collect();
        let x = &mut self.tb.x;
        if x.syslog_min == 0 {
            x.syslog_min = 8;
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut x.syslog_filter)
                    .hint_text(t("过滤（主机、程序、内容）"))
                    .desired_width(240.0)
                    .margin(vec2(8.0, 4.0)),
            );
            ui.add_space(8.0);
            ui.label(RichText::new(t("级别")).color(TEXT2()));
            let label = if x.syslog_min >= 8 { t("全部") } else { syslog::SEVERITY[x.syslog_min as usize] };
            egui::ComboBox::from_id_salt("sl_min").width(90.0).selected_text(label).show_ui(ui, |ui| {
                ui.selectable_value(&mut x.syslog_min, 8, t("全部"));
                for (i, n) in syslog::SEVERITY.iter().enumerate() {
                    ui.selectable_value(&mut x.syslog_min, i as u8, *n);
                }
            });
            if ui.button(t("清空")).clicked() {
                if let Some(s) = &x.syslog {
                    s.st.lock().unwrap().msgs.clear();
                }
            }
        });
        ui.add_space(6.0);
        let q = x.syslog_filter.to_lowercase();
        let min = x.syslog_min;
        let rows: Vec<&syslog::Msg> = msgs
            .iter()
            .filter(|m| min >= 8 || m.severity.is_none_or(|s| s <= min))
            .filter(|m| q.is_empty() || format!("{} {} {}", m.host, m.app, m.text).to_lowercase().contains(&q))
            .collect();
        header_row(ui, &[(t("时间"), 96.0), (t("级别"), 60.0), (t("主机"), 130.0), (t("程序"), 100.0), (t("内容"), 300.0)]);
        let n = rows.len();
        egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show_rows(ui, ROW_H, n, |ui, range| {
            dense_rows(ui);
            for m in &rows[range] {
                ui.horizontal(|ui| {
                    let f = MONO(12.0);
                    let (sev, col) = match m.severity {
                        Some(s) if s <= 3 => (syslog::SEVERITY[s as usize], ERR()),
                        Some(4) => ("WARN", WARN()),
                        Some(s) if s == 7 => ("DEBUG", DEBUG()),
                        Some(s) => (syslog::SEVERITY[s as usize], TEXT2()),
                        None => ("-", TEXT3()),
                    };
                    cell(ui, 96.0, RichText::new(&m.at[..m.at.len().min(12)]).font(f.clone()).color(TEXT3()));
                    cell(ui, 60.0, RichText::new(sev).font(f.clone()).color(col));
                    ui.allocate_ui_with_layout(vec2(130.0, 18.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.set_min_width(130.0);
                        ui.add(egui::Label::new(RichText::new(&m.host).font(f.clone()).color(TEXT2())).truncate()).on_hover_text(&m.from);
                    });
                    cell(ui, 100.0, RichText::new(&m.app).font(f.clone()).color(VIOLET()));
                    ui.add(egui::Label::new(RichText::new(&m.text).font(f).color(TEXT())).truncate()).on_hover_text(&m.text);
                });
            }
        });
    }

    // ------------------------------------------------------------ Ping / 端口

    fn tb_ping(&mut self, ui: &mut egui::Ui) {
        let (running, replies, err) = {
            let s = self.tb.x.ping_st.lock().unwrap();
            (s.running, s.replies.clone(), s.err.clone())
        };
        let mut go = false;
        settings_card(ui, |ui| {
            let c = &mut self.s.tb.x;
            form_row(ui, t("地址"), |ui| {
                ui.add_enabled_ui(!running, |ui| {
                    ui.add_sized(vec2(200.0, 26.0), egui::TextEdit::singleline(&mut c.ping_host).font(MONO(13.0)).margin(vec2(6.0, 4.0)));
                    ui.label(RichText::new(t("次数")).color(TEXT2()));
                    ui.add(egui::DragValue::new(&mut c.ping_count).range(0..=10000)).on_hover_text(t("0 = 一直 Ping，直到停止"));
                    ui.label(RichText::new(t("间隔")).color(TEXT2()));
                    ui.add(egui::DragValue::new(&mut c.ping_iv).range(200..=60000).suffix(" ms"));
                });
                let label = if running { t("停止") } else { "Ping" };
                let fill = if running { BG3() } else { ACCENT() };
                let tc = if running { ERR() } else { ON_ACCENT() };
                go = ui.add(egui::Button::new(RichText::new(label).color(tc).strong()).fill(fill).min_size(vec2(80.0, 26.0))).clicked();
            });
        });
        if go {
            if running {
                if let Some(s) = self.tb.x.ping_stop.take() {
                    s.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            } else {
                let stop = Arc::new(AtomicBool::new(false));
                self.tb.x.ping_stop = Some(stop.clone());
                let c = &self.s.tb.x;
                ping::start(&self.tb.x.ping_st, stop, c.ping_host.trim().to_string(), c.ping_count, c.ping_iv, self.ctx.clone());
            }
        }
        if let Some(e) = err {
            ui.label(RichText::new(tf("无法运行 ping：{}", &[&e])).color(ERR()));
        }
        if !replies.is_empty() {
            ui.add_space(8.0);
            let st = self.tb.x.ping_st.lock().unwrap().summary();
            let (sent, ok, stats) = st;
            let loss = (sent - ok) as f32 * 100.0 / sent as f32;
            ui.horizontal(|ui| {
                pill(
                    ui,
                    &tf("发送 {}  ·  收到 {}  ·  丢包 {}%", &[&sent, &ok, &format!("{loss:.0}")]),
                    if ok == sent { OK() } else { WARN() },
                );
                if let Some((min, avg, max)) = stats {
                    ui.label(RichText::new(format!("min {min:.1} / avg {avg:.1} / max {max:.1} ms")).font(MONO(12.0)).color(TEXT2()));
                }
            });
            ui.add_space(6.0);
            // 延迟柱状图（最近 120 次）
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 80.0), Sense::hover());
            let p = ui.painter();
            p.rect_filled(rect, R_CARD, BG0());
            let last: Vec<&ping::Reply> = replies.iter().rev().take(120).collect();
            let top = last.iter().filter_map(|r| r.ms).fold(1.0f64, f64::max);
            let bw = rect.width() / 120.0;
            for (i, r) in last.iter().enumerate() {
                let x = rect.right() - (i as f32 + 1.0) * bw;
                match r.ms {
                    Some(ms) => {
                        let h = ((ms / top) as f32 * (rect.height() - 8.0)).max(2.0);
                        p.rect_filled(Rect::from_min_max(pos2(x + 1.0, rect.bottom() - 4.0 - h), pos2(x + bw - 1.0, rect.bottom() - 4.0)), 1.0, ACCENT());
                    }
                    None => {
                        p.rect_filled(
                            Rect::from_min_max(pos2(x + 1.0, rect.top() + 4.0), pos2(x + bw - 1.0, rect.bottom() - 4.0)),
                            1.0,
                            ERR().gamma_multiply(0.35),
                        );
                    }
                }
            }
            ui.add_space(4.0);
            egui::ScrollArea::vertical().id_salt("ping_rows").max_height(140.0).stick_to_bottom(true).show(ui, |ui| {
                for r in replies.iter().rev().take(200).rev() {
                    let (txt, col) = match r.ms {
                        Some(ms) => (tf("{}  #{}  {} ms", &[&r.at, &r.seq, &format!("{ms:.1}")]), TEXT2()),
                        None => (tf("{}  #{}  超时", &[&r.at, &r.seq]), ERR()),
                    };
                    ui.label(RichText::new(txt).font(MONO(12.0)).color(col));
                }
            });
        }

        // 端口检测
        ui.add_space(16.0);
        eyebrow(ui, t("TCP 端口检测"), "");
        let (srun, total, results, serr) = {
            let s = self.tb.x.scan_st.lock().unwrap();
            (s.running, s.total, s.results.clone(), s.err.clone())
        };
        let mut scan = false;
        settings_card(ui, |ui| {
            let c = &mut self.s.tb.x;
            form_row(ui, t("地址"), |ui| {
                ui.add_sized(vec2(200.0, 26.0), egui::TextEdit::singleline(&mut c.scan_host).font(MONO(13.0)).margin(vec2(6.0, 4.0)));
            });
            form_row(ui, t("端口"), |ui| {
                let w = ui.available_width() - 100.0;
                ui.add_sized(
                    vec2(w, 26.0),
                    egui::TextEdit::singleline(&mut c.scan_ports).hint_text("22, 80, 8000-8100").font(MONO(13.0)).margin(vec2(6.0, 4.0)),
                );
                scan = ui
                    .add_enabled(!srun, egui::Button::new(RichText::new(t("检测")).color(ON_ACCENT()).strong()).fill(ACCENT()).min_size(vec2(80.0, 26.0)))
                    .clicked();
            });
        });
        if scan {
            match ping::parse_ports(&self.s.tb.x.scan_ports) {
                Ok(ps) if !ps.is_empty() => ping::scan(&self.tb.x.scan_st, self.s.tb.x.scan_host.trim().to_string(), ps, self.ctx.clone()),
                Ok(_) => {}
                Err(e) => self.toast(tf("端口写法不对：{}", &[&e]), ERR()),
            }
        }
        if let Some(e) = serr {
            ui.label(RichText::new(tf("解析 {} 失败", &[&e])).color(ERR()));
        }
        if srun {
            ui.add(
                egui::ProgressBar::new(results.len() as f32 / total.max(1) as f32)
                    .desired_width(240.0)
                    .desired_height(6.0)
                    .fill(ACCENT()),
            );
        }
        let open: Vec<&ping::PortResult> = results.iter().filter(|r| r.r.is_ok()).collect();
        if !results.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new(tf("{} 个端口开放，{} 个关闭或超时", &[&open.len(), &(results.len() - open.len())])).color(TEXT2()));
            ui.horizontal_wrapped(|ui| {
                for r in open {
                    if let Ok(ms) = r.r {
                        pill(ui, &format!(":{}  {ms:.0} ms", r.port), OK());
                    }
                }
            });
        }
    }

    // ------------------------------------------------------------ 子网计算

    fn tb_subnet(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.x;
        settings_card(ui, |ui| {
            form_row(ui, t("地址"), |ui| {
                ui.add_sized(
                    vec2(ui.available_width(), 26.0),
                    egui::TextEdit::singleline(&mut c.subnet)
                        .hint_text("192.168.1.10/24  或  192.168.1.10 255.255.255.0")
                        .font(MONO(13.0))
                        .margin(vec2(6.0, 4.0)),
                );
            });
            form_row(ui, t("比较"), |ui| {
                ui.add_sized(
                    vec2(ui.available_width(), 26.0),
                    egui::TextEdit::singleline(&mut c.subnet_other)
                        .hint_text(t("另一个 IP：看是否在同一网段"))
                        .font(MONO(13.0))
                        .margin(vec2(6.0, 4.0)),
                );
            });
        });
        ui.add_space(12.0);
        match netcalc::parse(&c.subnet) {
            Err(e) => {
                ui.label(RichText::new(tf("格式不对：{}", &[&e])).color(ERR()));
            }
            Ok(s) => {
                let i = s.info();
                out_row(ui, t("网络地址"), &format!("{}/{}", i.network, s.prefix));
                out_row(ui, t("子网掩码"), &i.mask.to_string());
                out_row(ui, t("广播地址"), &i.broadcast.to_string());
                out_row(ui, t("可用范围"), &format!("{} – {}", i.first, i.last));
                out_row(ui, t("可用主机"), &group_digits(i.hosts));
                out_row(ui, t("反掩码"), &i.wildcard.to_string());
                out_row(ui, t("二进制"), &netcalc::binary(s.ip));
                out_row(ui, t("掩码二进制"), &netcalc::binary(i.mask));
                let kind = if i.private { t("私有地址") } else { t("公网地址") };
                out_row(ui, t("类型"), &tf("{} 类 · {}", &[&i.class, &kind]));
                if !c.subnet_other.trim().is_empty() {
                    ui.add_space(6.0);
                    match c.subnet_other.trim().parse::<std::net::Ipv4Addr>() {
                        Ok(o) if s.contains(o) => pill(ui, &tf("{} 和 {} 在同一网段", &[&o, &s.ip]), OK()),
                        Ok(o) => pill(ui, &tf("{} 不在 {}/{} 网段里，需要经过网关", &[&o, &i.network, &s.prefix]), WARN()),
                        Err(_) => pill(ui, t("比较的 IP 格式不对"), ERR()),
                    }
                }
            }
        }
        ui.add_space(14.0);
        eyebrow(ui, t("常用掩码"), "");
        egui::Grid::new("masks").num_columns(4).spacing([24.0, 3.0]).show(ui, |ui| {
            for p in [8u8, 16, 20, 22, 23, 24, 25, 26, 27, 28, 29, 30] {
                let m: std::net::Ipv4Addr = (if p == 0 { 0 } else { u32::MAX << (32 - p as u32) }).into();
                let hosts = (1u64 << (32 - p as u32)).saturating_sub(2);
                ui.label(RichText::new(format!("/{p}")).font(MONO(12.0)).color(ACCENT()));
                ui.label(RichText::new(m.to_string()).font(MONO(12.0)).color(TEXT()));
                ui.label(RichText::new(tf("{} 个主机", &[&group_digits(hosts)])).small().color(TEXT3()));
                ui.end_row();
            }
        });
    }

    // ------------------------------------------------------------ MQTT 服务器

    fn tb_broker(&mut self, ui: &mut egui::Ui) {
        let running = self.tb.x.broker.is_some();
        let mut start = false;
        settings_card(ui, |ui| {
            let c = &mut self.s.tb.x;
            ui.add_enabled_ui(!running, |ui| {
                form_row(ui, t("端口"), |ui| {
                    ui.allocate_ui(vec2(90.0, 26.0), |ui| num_field(ui, "broker_port", &mut c.broker_port));
                });
                form_row(ui, t("用户名"), |ui| {
                    ui.add_sized(vec2(150.0, 26.0), egui::TextEdit::singleline(&mut c.broker_user).hint_text(t("留空 = 不验证")).margin(vec2(6.0, 4.0)));
                    ui.label(RichText::new(t("密码")).color(TEXT2()));
                    ui.add_sized(vec2(150.0, 26.0), egui::TextEdit::singleline(&mut c.broker_pass).password(true).margin(vec2(6.0, 4.0)));
                });
            });
            ui.add_space(6.0);
            start = server_button(ui, running);
        });
        if start {
            if running {
                self.tb.x.broker = None;
            } else {
                let c = &self.s.tb.x;
                let cfg = broker::Config {
                    port: c.broker_port,
                    user: c.broker_user.trim().to_string(),
                    pass: c.broker_pass.clone(),
                };
                match broker::start(cfg, self.ctx.clone()) {
                    Ok(b) => self.tb.x.broker = Some(b),
                    Err(e) => self.toast(tf("监听端口 {} 失败：{}", &[&self.s.tb.x.broker_port, &e]), ERR()),
                }
            }
        }
        let Some(b) = &self.tb.x.broker else {
            ui.add_space(8.0);
            ui.label(RichText::new(t("启动后设备连本机 IP 即可；QCCOM 自己的 MQTT 工具填 127.0.0.1 也能连上来调试")).color(TEXT3()));
            return;
        };
        ui.add_space(8.0);
        let ip = crate::toolbox::local_ipv4().map(|i| i.to_string()).unwrap_or_else(|| "127.0.0.1".into());
        let mut use_it = false;
        ui.horizontal(|ui| {
            hint_line(ui, t("地址："), &format!("mqtt://{ip}:{}", b.port));
            use_it = ui.button(t("用 MQTT 工具连接")).clicked();
        });
        let (clients, log, events, retained, msgs) = {
            let s = b.st.lock().unwrap();
            (
                s.clients(),
                s.log.iter().rev().take(200).cloned().collect::<Vec<_>>(),
                s.events.iter().rev().take(50).cloned().collect::<Vec<_>>(),
                s.retained.len(),
                s.msgs,
            )
        };
        ui.add_space(10.0);
        // 从这里发布
        let c = &mut self.s.tb.x;
        let mut publish = false;
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut c.broker_topic)
                    .hint_text(t("主题"))
                    .font(MONO(12.5))
                    .desired_width(180.0)
                    .margin(vec2(6.0, 4.0)),
            );
            let w = ui.available_width() - 170.0;
            let r = ui.add(
                egui::TextEdit::singleline(&mut c.broker_payload)
                    .hint_text(t("内容"))
                    .font(MONO(12.5))
                    .desired_width(w)
                    .margin(vec2(6.0, 4.0)),
            );
            toggle_chip(ui, &mut c.broker_retain, t("保留"), t("保留消息：之后订阅的客户端也会收到"));
            publish = ui
                .add(egui::Button::new(RichText::new(t("发布")).color(ON_ACCENT())).fill(ACCENT()).min_size(vec2(60.0, 26.0)))
                .clicked()
                || (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)));
        });
        if publish && !c.broker_topic.trim().is_empty() {
            let n = b.publish(c.broker_topic.trim(), c.broker_payload.as_bytes(), c.broker_retain);
            self.toast(tf("已发布，{} 个订阅者收到", &[&n]), OK());
        }
        ui.add_space(10.0);
        ui.columns(2, |cols| {
            let ui = &mut cols[0];
            eyebrow(ui, &format!("{}  {}", t("客户端"), clients.len()), "");
            if clients.is_empty() {
                ui.label(RichText::new(t("还没有客户端连上来")).color(TEXT3()));
            }
            for c in &clients {
                card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&c.id).font(FontId::new(13.0, bold())).color(TEXT()));
                        ui.label(RichText::new(format!("{}  ·  {}", c.addr, c.since)).font(MONO(11.0)).color(TEXT3()));
                    });
                    if !c.subs.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            for (f, q) in &c.subs {
                                pill(ui, &format!("{f}  q{q}"), VIOLET());
                            }
                        });
                    }
                });
                ui.add_space(4.0);
            }
            ui.add_space(8.0);
            eyebrow(ui, t("事件"), "");
            for e in events.iter().take(12) {
                ui.label(RichText::new(e).font(MONO(11.5)).color(TEXT3()));
            }
            let ui = &mut cols[1];
            eyebrow(ui, t("消息"), &tf("{} 条 · 保留 {}", &[&msgs, &retained]));
            egui::ScrollArea::vertical().id_salt("broker_msgs").auto_shrink([false, false]).show(ui, |ui| {
                for m in &log {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&m.at[..m.at.len().min(8)]).font(MONO(11.5)).color(TEXT3()));
                        ui.label(RichText::new(&m.topic).font(MONO(12.0)).color(ACCENT()));
                        if m.qos > 0 {
                            ui.label(RichText::new(format!("q{}", m.qos)).font(MONO(10.5)).color(TEXT3()));
                        }
                        if m.retain {
                            ui.label(RichText::new("R").font(MONO(10.5)).color(WARN()));
                        }
                        ui.label(RichText::new(format!("→{}", m.delivered)).font(MONO(11.0)).color(TEXT3()));
                    });
                    let text = String::from_utf8_lossy(&m.payload);
                    ui.add(egui::Label::new(RichText::new(truncate(&text, 200)).font(MONO(12.0)).color(TEXT())).wrap());
                    ui.label(RichText::new(tf("来自 {}", &[&m.from])).small().color(TEXT3()));
                    ui.add_space(4.0);
                }
            });
        });
        if use_it {
            self.s.net.mqtt.host = "127.0.0.1".into();
            self.s.net.mqtt.port = self.s.tb.x.broker_port;
            self.s.net.mqtt.tls = false;
            self.s.net.mqtt.user = self.s.tb.x.broker_user.clone();
            self.s.net.mqtt.pass = self.s.tb.x.broker_pass.clone();
            self.open_tool(Tool::Mqtt);
            if self.conn.is_none() {
                self.connect();
            }
        }
    }

    // ------------------------------------------------------------ 校验计算

    fn tb_checksum(&mut self, ui: &mut egui::Ui) {
        let enc = self.s.encoding;
        let c = &mut self.s.tb.x;
        ui.horizontal(|ui| {
            small_chip(ui, &mut c.crc_hex, "HEX", t("按十六进制字节计算"));
            ui.label(RichText::new(if c.crc_hex { t("输入十六进制字节") } else { t("输入文字（按当前编码）") }).small().color(TEXT3()));
        });
        ui.add(
            egui::TextEdit::multiline(&mut c.crc_input)
                .font(MONO(13.0))
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text(if c.crc_hex { "01 03 00 00 00 0A" } else { "123456789" }),
        );
        let data = if c.crc_hex { parse_hex(&c.crc_input) } else { Ok(enc.encode(&c.crc_input)) };
        let data = match data {
            Ok(d) => d,
            Err(e) => {
                ui.label(RichText::new(e).color(ERR()));
                return;
            }
        };
        ui.label(RichText::new(tf("{} 字节", &[&data.len()])).small().color(TEXT3()));
        ui.add_space(10.0);
        let hexs = |v: u32, digits: usize| format!("{v:0digits$X}");
        egui::Frame::new()
            .fill(BG0())
            .stroke(st(1.0, LINE()))
            .corner_radius(R_CARD)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::Grid::new("crc_all").num_columns(4).spacing([18.0, 5.0]).show(ui, |ui| {
                    for h in [t("算法"), t("结果"), t("小端字节"), t("大端字节")] {
                        ui.label(RichText::new(h).small().color(TEXT3()));
                    }
                    ui.end_row();
                    for (name, v, d) in crc::sums(&data) {
                        ui.label(RichText::new(name).font(MONO(12.5)).color(VIOLET()));
                        copy_label(ui, &format!("0x{}", hexs(v, d)));
                        ui.label("");
                        ui.label("");
                        ui.end_row();
                    }
                    for (name, p) in PRESETS {
                        let v = p.compute(&data);
                        let fmt = |b: Vec<u8>| b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ");
                        ui.label(RichText::new(*name).font(MONO(12.5)).color(VIOLET()));
                        copy_label(ui, &format!("0x{}", hexs(v, p.hex_digits())));
                        copy_label(ui, &fmt(p.bytes(v, true)));
                        copy_label(ui, &fmt(p.bytes(v, false)));
                        ui.end_row();
                    }
                });
            });
        ui.add_space(14.0);
        eyebrow(ui, t("自定义 CRC"), c.crc.preset_name().unwrap_or(""));
        let cr = &mut c.crc;
        settings_card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("预设")).color(TEXT2()));
                egui::ComboBox::from_id_salt("crc_preset")
                    .width(180.0)
                    .selected_text(cr.preset_name().unwrap_or(t("自定义")))
                    .show_ui(ui, |ui| {
                        for (n, p) in PRESETS {
                            if ui.selectable_label(*cr == *p, *n).clicked() {
                                *cr = *p;
                            }
                        }
                    });
                ui.add_space(10.0);
                ui.label(RichText::new(t("宽度")).color(TEXT2()));
                ui.add(egui::DragValue::new(&mut cr.width).range(3..=32));
            });
            ui.horizontal(|ui| {
                let digits = cr.hex_digits();
                for (label, v) in [(t("多项式"), &mut cr.poly), (t("初值"), &mut cr.init), (t("结果异或"), &mut cr.xorout)] {
                    ui.label(RichText::new(label).color(TEXT2()));
                    ui.add(egui::DragValue::new(v).hexadecimal(digits, false, true).range(0..=u32::MAX));
                    ui.add_space(8.0);
                }
            });
            ui.horizontal(|ui| {
                toggle_chip(ui, &mut cr.refin, t("输入反转"), "RefIn");
                toggle_chip(ui, &mut cr.refout, t("输出反转"), "RefOut");
            });
            let mask = cr.mask();
            cr.poly &= mask;
            cr.init &= mask;
            cr.xorout &= mask;
            let v = cr.compute(&data);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("结果")).color(TEXT2()));
                copy_label(ui, &format!("0x{}", hexs(v, cr.hex_digits())));
                ui.label(
                    RichText::new(tf("校验值（\"123456789\"）：0x{}", &[&hexs(cr.compute(b"123456789"), cr.hex_digits())]))
                        .small()
                        .color(TEXT3()),
                );
            });
        });
    }

    // ------------------------------------------------------------ 时间戳

    fn tb_timestamp(&mut self, ui: &mut egui::Ui) {
        use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
        let now = Local::now();
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("现在")).color(TEXT2()));
                ui.add_space(8.0);
                ui.label(RichText::new(now.format("%Y-%m-%d %H:%M:%S").to_string()).font(MONO(15.0)).color(TEXT()));
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("秒")).color(TEXT2()));
                copy_label(ui, &now.timestamp().to_string());
                ui.add_space(14.0);
                ui.label(RichText::new(t("毫秒")).color(TEXT2()));
                copy_label(ui, &now.timestamp_millis().to_string());
                ui.add_space(14.0);
                ui.label(RichText::new("HEX").color(TEXT2()));
                copy_label(ui, &format!("0x{:08X}", now.timestamp() as u32));
            });
        });
        ui.add_space(12.0);
        let c = &mut self.s.tb.x;
        ui.add(
            egui::TextEdit::singleline(&mut c.ts_input)
                .hint_text(t("输入时间戳（秒 / 毫秒 / 0x 十六进制）或日期时间 2026-10-28 14:30:00"))
                .font(MONO(14.0))
                .desired_width(f32::INFINITY)
                .margin(vec2(8.0, 6.0)),
        );
        ui.add_space(10.0);
        let s = c.ts_input.trim();
        if s.is_empty() {
            return;
        }
        let num = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            i64::from_str_radix(h, 16).ok()
        } else {
            s.parse::<i64>().ok()
        };
        let show = |ui: &mut egui::Ui, utc: DateTime<Utc>| {
            let local = utc.with_timezone(&Local);
            out_row(ui, t("本地时间"), &local.format("%Y-%m-%d %H:%M:%S%.3f").to_string());
            out_row(ui, "UTC", &utc.format("%Y-%m-%d %H:%M:%S%.3f").to_string());
            out_row(ui, "ISO 8601", &local.to_rfc3339());
            out_row(ui, t("秒"), &utc.timestamp().to_string());
            out_row(ui, t("毫秒"), &utc.timestamp_millis().to_string());
            let diff = utc.timestamp() - Utc::now().timestamp();
            let rel = if diff.abs() < 60 {
                t("就是现在").to_string()
            } else {
                let (n, unit) = match diff.abs() {
                    d if d < 3600 => (d / 60, t("分钟")),
                    d if d < 86400 => (d / 3600, t("小时")),
                    d => (d / 86400, t("天")),
                };
                if diff < 0 {
                    tf("{} {}前", &[&n, &unit])
                } else {
                    tf("{} {}后", &[&n, &unit])
                }
            };
            out_row(ui, t("相对现在"), &rel);
        };
        if let Some(n) = num {
            // 13 位左右按毫秒，10 位左右按秒；更大的按微秒
            let (secs, nanos, unit) = if n.abs() >= 100_000_000_000_000 {
                (n / 1_000_000, (n % 1_000_000) * 1000, t("微秒"))
            } else if n.abs() >= 100_000_000_000 {
                (n / 1000, (n % 1000) * 1_000_000, t("毫秒"))
            } else {
                (n, 0, t("秒"))
            };
            ui.label(RichText::new(tf("按{}解析", &[&unit])).small().color(TEXT3()));
            match Utc.timestamp_opt(secs, nanos.max(0) as u32).single() {
                Some(u) => show(ui, u),
                None => {
                    ui.label(RichText::new(t("超出范围")).color(ERR()));
                }
            }
            return;
        }
        let fmts = [
            "%Y-%m-%d %H:%M:%S%.f",
            "%Y-%m-%d %H:%M:%S",
            "%Y-%m-%d %H:%M",
            "%Y/%m/%d %H:%M:%S",
            "%Y-%m-%dT%H:%M:%S%.f",
            "%Y%m%d%H%M%S",
        ];
        if let Ok(d) = DateTime::parse_from_rfc3339(s) {
            show(ui, d.with_timezone(&Utc));
        } else if let Some(nd) = fmts
            .iter()
            .find_map(|f| NaiveDateTime::parse_from_str(s, f).ok())
            .or_else(|| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(0, 0, 0)))
        {
            match Local.from_local_datetime(&nd).single() {
                Some(l) => show(ui, l.with_timezone(&Utc)),
                None => {
                    ui.label(RichText::new(t("这个本地时间不存在（夏令时切换）")).color(ERR()));
                }
            }
        } else {
            ui.label(RichText::new(t("认不出来：输入数字时间戳，或者 2026-10-28 14:30:00 这样的日期时间")).color(ERR()));
        }
    }

    // ------------------------------------------------------------ 正则

    fn tb_regex(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.x;
        ui.horizontal(|ui| {
            let w = ui.available_width() - 70.0;
            ui.add(
                egui::TextEdit::singleline(&mut c.re_pat)
                    .hint_text(t("正则表达式"))
                    .font(MONO(14.0))
                    .desired_width(w)
                    .margin(vec2(8.0, 6.0)),
            );
            small_chip(ui, &mut c.re_icase, "Aa", t("忽略大小写"));
        });
        let re = regex_lite::RegexBuilder::new(&c.re_pat).case_insensitive(c.re_icase).build();
        if let Err(e) = &re {
            ui.label(RichText::new(e.to_string()).font(MONO(12.0)).color(ERR()));
        }
        ui.add_space(8.0);
        ui.label(RichText::new(t("测试文本")).small().color(TEXT3()));
        // 匹配的部分直接在输入框里高亮
        let re_ok = re.as_ref().ok().filter(|_| !c.re_pat.is_empty()).cloned();
        let re_l = re_ok.clone();
        let mut layouter = move |ui: &egui::Ui, text: &str, wrap: f32| {
            let mut job = LayoutJob::default();
            let f = FontId::new(13.0, FontFamily::Monospace);
            let mut last = 0;
            if let Some(re) = &re_l {
                for (i, m) in re.find_iter(text).enumerate() {
                    if m.start() < last || m.is_empty() {
                        continue;
                    }
                    job.append(&text[last..m.start()], 0.0, TextFormat::simple(f.clone(), TEXT()));
                    let bg = if i % 2 == 0 { ACCENT().gamma_multiply(0.28) } else { VIOLET().gamma_multiply(0.28) };
                    job.append(
                        &text[m.start()..m.end()],
                        0.0,
                        TextFormat {
                            font_id: f.clone(),
                            color: TEXT(),
                            background: bg,
                            ..Default::default()
                        },
                    );
                    last = m.end();
                }
            }
            job.append(&text[last..], 0.0, TextFormat::simple(f, TEXT()));
            job.wrap.max_width = wrap;
            ui.fonts(|f| f.layout_job(job))
        };
        ui.add(egui::TextEdit::multiline(&mut c.re_text).desired_rows(6).desired_width(f32::INFINITY).layouter(&mut layouter));
        let Some(re) = re_ok else { return };
        ui.add_space(10.0);
        let caps: Vec<regex_lite::Captures> = re.captures_iter(&c.re_text).take(200).collect();
        eyebrow(ui, t("匹配"), &caps.len().to_string());
        let names: Vec<Option<&str>> = re.capture_names().collect();
        egui::Frame::new()
            .fill(BG0())
            .stroke(st(1.0, LINE()))
            .corner_radius(R_CARD)
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().id_salt("re_caps").max_height(200.0).show(ui, |ui| {
                    for (i, cap) in caps.iter().enumerate() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(format!("#{}", i + 1)).font(MONO(11.5)).color(TEXT3()));
                            for (gi, g) in cap.iter().enumerate() {
                                let Some(g) = g else { continue };
                                let label = match names.get(gi).copied().flatten() {
                                    Some(n) => n.to_string(),
                                    None => format!("${gi}"),
                                };
                                ui.label(RichText::new(label).font(MONO(11.5)).color(if gi == 0 { TEXT3() } else { VIOLET() }));
                                copy_label(ui, g.as_str());
                                ui.add_space(8.0);
                            }
                        });
                    }
                    if caps.is_empty() {
                        ui.label(RichText::new(t("没有匹配")).color(TEXT3()));
                    }
                });
            });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("替换为")).color(TEXT2()));
            ui.add(egui::TextEdit::singleline(&mut c.re_repl).font(MONO(13.0)).desired_width(f32::INFINITY).margin(vec2(6.0, 4.0)));
        });
        let out = re.replace_all(&c.re_text, c.re_repl.as_str()).to_string();
        ui.add_space(4.0);
        egui::Frame::new().fill(BG2()).corner_radius(R_CARD).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(&out).font(MONO(12.5)).color(TEXT())).wrap());
        });
    }

    // ------------------------------------------------------------ 取模

    fn tb_font(&mut self, ui: &mut egui::Ui) {
        let mut mode = self.s.tb.x.fm_image;
        ui.allocate_ui(vec2(300.0, 30.0), |ui| tab_strip(ui, &mut mode, &[(false, t("文字取模")), (true, t("图片取模"))], 28.0));
        self.s.tb.x.fm_image = mode;
        ui.add_space(10.0);
        if mode {
            self.tb_image(ui);
        } else {
            self.tb_text_font(ui);
        }
    }

    /// 取模方式、位序、阴阳码这几项文字和单色图片共用
    fn scan_options(ui: &mut egui::Ui, c: &mut Cfg2) {
        form_row(ui, t("取模方式"), |ui| {
            egui::ComboBox::from_id_salt("fm_scan").width(220.0).selected_text(scan_label(c.fm_scan)).show_ui(ui, |ui| {
                for s in Scan::ALL {
                    ui.selectable_value(&mut c.fm_scan, s, scan_label(s));
                }
            });
            let mut msb = c.fm_msb;
            ui.allocate_ui(vec2(200.0, 26.0), |ui| tab_strip(ui, &mut msb, &[(true, t("高位在前")), (false, t("低位在前"))], 24.0));
            c.fm_msb = msb;
            toggle_chip(ui, &mut c.fm_invert, t("阴码"), t("点亮的像素输出 0（默认阳码：点亮 = 1）"));
        });
    }

    fn load_font(&mut self) {
        let want = self.s.tb.x.fm_font.clone();
        if self.tb.x.font.as_ref().is_some_and(|f| f.0 == want) || want.is_empty() {
            return;
        }
        self.tb.x.font = None;
        self.tb.x.font_err = None;
        match std::fs::File::open(&want).and_then(|f| unsafe { memmap2::Mmap::map(&f) }) {
            Ok(m) => self.tb.x.font = Some((want, m)),
            Err(e) => self.tb.x.font_err = Some(e.to_string()),
        }
    }

    fn tb_text_font(&mut self, ui: &mut egui::Ui) {
        if self.s.tb.x.fm_font.is_empty() {
            self.s.tb.x.fm_font = font_candidates().first().map(|(_, p)| p.clone()).unwrap_or_default();
        }
        self.load_font();
        let c = &mut self.s.tb.x;
        settings_card(ui, |ui| {
            form_row(ui, t("文字"), |ui| {
                ui.add_sized(
                    vec2(ui.available_width(), 26.0),
                    egui::TextEdit::singleline(&mut c.fm_text).font(FontId::proportional(14.0)).margin(vec2(6.0, 4.0)),
                );
            });
            form_row(ui, t("字体"), |ui| {
                let cur = font_candidates().into_iter().find(|(_, p)| *p == c.fm_font).map(|(n, _)| n).unwrap_or_else(|| file_name(&c.fm_font));
                egui::ComboBox::from_id_salt("fm_font").width(220.0).selected_text(cur).show_ui(ui, |ui| {
                    for (n, p) in font_candidates() {
                        ui.selectable_value(&mut c.fm_font, p, n);
                    }
                });
                if ui.button(t("选择字体文件…")).clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter(t("字体"), &["ttf", "ttc", "otf"]).pick_file() {
                        c.fm_font = p.to_string_lossy().into_owned();
                    }
                }
            });
            form_row(ui, t("尺寸"), |ui| {
                ui.label(RichText::new(t("字号")).color(TEXT2()));
                ui.add(egui::DragValue::new(&mut c.fm_size).range(6.0..=128.0).speed(0.5).suffix(" px"));
                ui.add_space(8.0);
                ui.label(RichText::new(t("宽 × 高")).color(TEXT2()));
                ui.add(egui::DragValue::new(&mut c.fm_w).range(4..=128));
                ui.label("×");
                ui.add(egui::DragValue::new(&mut c.fm_h).range(4..=128));
                for (w, h) in [(8u16, 16u16), (12, 12), (16, 16), (24, 24), (32, 32)] {
                    if ui.small_button(format!("{w}×{h}")).clicked() {
                        c.fm_w = w;
                        c.fm_h = h;
                        c.fm_size = h as f32;
                        c.fm_name = format!("font{h}");
                    }
                }
            });
            form_row(ui, t("微调"), |ui| {
                ui.label(RichText::new(t("横向")).color(TEXT2()));
                ui.add(egui::DragValue::new(&mut c.fm_dx).range(-32..=32));
                ui.label(RichText::new(t("纵向")).color(TEXT2()));
                ui.add(egui::DragValue::new(&mut c.fm_dy).range(-32..=32));
                ui.label(RichText::new(t("粗细")).color(TEXT2()));
                ui.add(egui::Slider::new(&mut c.fm_thr, 20..=240).show_value(false)).on_hover_text(t("阈值越低笔画越粗"));
            });
            Self::scan_options(ui, c);
            form_row(ui, t("数组名"), |ui| {
                ui.add_sized(vec2(160.0, 26.0), egui::TextEdit::singleline(&mut c.fm_name).font(MONO(12.5)).margin(vec2(6.0, 4.0)));
            });
        });
        if let Some(e) = &self.tb.x.font_err {
            ui.label(RichText::new(tf("字体打不开：{}", &[e])).color(ERR()));
        }
        let c = &self.s.tb.x;
        let key = format!("{}|{}|{}|{}|{}|{}|{}|{}", c.fm_text, c.fm_font, c.fm_size, c.fm_w, c.fm_h, c.fm_dx, c.fm_dy, c.fm_thr);
        if key != self.tb.x.glyph_key {
            self.tb.x.glyph_key = key;
            self.tb.x.glyphs.clear();
            let data: &[u8] = match &self.tb.x.font {
                Some((_, m)) => &m[..],
                None => include_bytes!("../../assets/JetBrainsMono-Regular.ttf"),
            };
            match ab_glyph::FontRef::try_from_slice_and_index(data, 0) {
                Ok(f) => {
                    for ch in c.fm_text.chars().filter(|c| !c.is_control()).take(256) {
                        let m = bitmap::glyph(&f, ch, c.fm_size, c.fm_w as usize, c.fm_h as usize, c.fm_dx, c.fm_dy, c.fm_thr);
                        self.tb.x.glyphs.push((ch, m));
                    }
                }
                Err(e) => self.tb.x.font_err = Some(e.to_string()),
            }
        }
        ui.add_space(10.0);
        // 预览：每个字一个格子
        let cell_px = (160.0 / c.fm_h.max(c.fm_w) as f32).clamp(2.0, 10.0);
        ui.horizontal_wrapped(|ui| {
            for (_, m) in self.tb.x.glyphs.iter().take(64) {
                draw_mono(ui, m, cell_px);
                ui.add_space(4.0);
            }
        });
        ui.add_space(8.0);
        let mut code = String::new();
        let name = bitmap::c_ident(&c.fm_name);
        code.push_str(&format!(
            "/* {} {}x{}, {}, {} */\n",
            file_name(&c.fm_font),
            c.fm_w,
            c.fm_h,
            scan_label(c.fm_scan),
            if c.fm_msb { "MSB first" } else { "LSB first" }
        ));
        let per = bitmap::encode(&Mono::new(c.fm_w as usize, c.fm_h as usize), c.fm_scan, true, false).len();
        code.push_str(&format!("const unsigned char {name}[][{per}] = {{\n"));
        for (ch, m) in &self.tb.x.glyphs {
            let b = bitmap::encode(m, c.fm_scan, c.fm_msb, c.fm_invert);
            code.push_str(&format!("    /* \"{ch}\" U+{:04X} */\n", *ch as u32));
            code.push_str("    {\n");
            let mut s = String::new();
            bitmap::c_bytes(&mut s, &b, 16);
            for l in s.lines() {
                code.push_str("    ");
                code.push_str(l);
                code.push('\n');
            }
            code.push_str("    },\n");
        }
        code.push_str("};\n");
        code_block(ui, &code, t("C 数组"), "font.h");
    }

    fn tb_image(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.x;
        let mut pick = false;
        settings_card(ui, |ui| {
            form_row(ui, t("图片"), |ui| {
                let w = ui.available_width() - 90.0;
                ui.add_sized(vec2(w, 26.0), egui::TextEdit::singleline(&mut c.img_path).hint_text("logo.png").font(MONO(12.5)).margin(vec2(6.0, 4.0)));
                pick = ui.add(egui::Button::new(t("选择…")).min_size(vec2(80.0, 26.0))).clicked();
            });
            form_row(ui, t("尺寸"), |ui| {
                ui.add(egui::DragValue::new(&mut c.img_w).range(1..=1024));
                ui.label("×");
                ui.add(egui::DragValue::new(&mut c.img_h).range(1..=1024));
                toggle_chip(ui, &mut c.img_keep, t("保持比例"), t("按宽度等比缩放，高度自动算"));
                for (w, h) in [(128u16, 64u16), (128, 32), (240, 240), (320, 240)] {
                    if ui.small_button(format!("{w}×{h}")).clicked() {
                        c.img_w = w;
                        c.img_h = h;
                        c.img_keep = false;
                    }
                }
            });
            form_row(ui, t("输出"), |ui| {
                let mut col = c.img_color;
                ui.allocate_ui(vec2(160.0, 26.0), |ui| tab_strip(ui, &mut col, &[(false, t("单色")), (true, t("彩色"))], 24.0));
                c.img_color = col;
                if col {
                    egui::ComboBox::from_id_salt("img_fmt").width(160.0).selected_text(color_fmt_label(c.img_fmt)).show_ui(ui, |ui| {
                        for f in [ColorFmt::Rgb565, ColorFmt::Rgb565Be, ColorFmt::Rgb888, ColorFmt::Gray8] {
                            ui.selectable_value(&mut c.img_fmt, f, color_fmt_label(f));
                        }
                    });
                } else {
                    ui.label(RichText::new(t("阈值")).color(TEXT2()));
                    ui.add(egui::Slider::new(&mut c.fm_thr, 10..=245));
                    toggle_chip(ui, &mut c.img_dither, t("抖动"), t("Floyd–Steinberg 抖动，照片类图片效果更好"));
                }
            });
            if !c.img_color {
                Self::scan_options(ui, c);
            }
            form_row(ui, t("数组名"), |ui| {
                ui.add_sized(vec2(160.0, 26.0), egui::TextEdit::singleline(&mut c.fm_name).font(MONO(12.5)).margin(vec2(6.0, 4.0)));
            });
        });
        if pick {
            if let Some(p) = rfd::FileDialog::new().add_filter(t("图片"), &["png", "jpg", "jpeg", "bmp", "gif"]).pick_file() {
                self.s.tb.x.img_path = p.to_string_lossy().into_owned();
                self.s.tb.x.fm_name = bitmap::c_ident(&p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
            }
        }
        // 读图（路径变了才重新读）
        let path = self.s.tb.x.img_path.trim().to_string();
        if !path.is_empty() && self.tb.x.img.as_ref().is_none_or(|i| i.0 != path) && self.tb.x.img_err.as_ref().is_none_or(|_| self.tb.x.img_key != path) {
            self.tb.x.img_key = path.clone();
            match image::open(&path) {
                Ok(i) => {
                    let rgba = i.to_rgba8();
                    self.tb.x.img_err = None;
                    self.tb.x.img = Some((path.clone(), rgba));
                    self.tb.x.preview_key.clear();
                }
                Err(e) => {
                    self.tb.x.img = None;
                    self.tb.x.img_err = Some(e.to_string());
                }
            }
        }
        if let Some(e) = &self.tb.x.img_err {
            ui.label(RichText::new(tf("图片打不开：{}", &[e])).color(ERR()));
            return;
        }
        let Some((_, src)) = &self.tb.x.img else {
            ui.label(RichText::new(t("选一张 PNG / JPG / BMP 图片")).color(TEXT3()));
            return;
        };
        let c = &mut self.s.tb.x;
        if c.img_keep {
            c.img_h = ((c.img_w as f32 * src.height() as f32 / src.width().max(1) as f32).round() as u16).max(1);
        }
        let key = format!("{}|{}|{}|{}|{}|{}", path, c.img_w, c.img_h, c.fm_thr, c.img_dither, c.img_color);
        if key != self.tb.x.preview_key {
            self.tb.x.preview_key = key;
            let small = image::imageops::resize(src, c.img_w as u32, c.img_h as u32, image::imageops::FilterType::Triangle);
            let mono = bitmap::image_mono(&small, c.fm_thr, c.img_dither, false);
            // 预览纹理：彩色显示缩放后的图，单色显示点阵
            let (w, h) = (small.width() as usize, small.height() as usize);
            let pixels: Vec<Color32> = if c.img_color {
                small.pixels().map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3])).collect()
            } else {
                mono.px
                    .iter()
                    .map(|on| if *on { Color32::from_rgb(0x22, 0xD3, 0xEE) } else { Color32::from_rgb(0x10, 0x14, 0x1c) })
                    .collect()
            };
            let img = egui::ColorImage { size: [w, h], pixels };
            self.tb.x.preview = Some(ui.ctx().load_texture("img_mod", img, egui::TextureOptions::NEAREST));
            self.tb.x.img_out = Some((small, mono));
        }
        ui.add_space(10.0);
        if let Some(tex) = &self.tb.x.preview {
            let sz = tex.size_vec2();
            let scale = (360.0 / sz.x.max(sz.y)).clamp(1.0, 8.0).floor();
            ui.horizontal(|ui| {
                egui::Frame::new().fill(BG0()).stroke(st(1.0, LINE())).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                    ui.add(egui::Image::new((tex.id(), sz * scale)));
                });
                ui.label(RichText::new(tf("{} × {} 像素", &[&sz.x, &sz.y])).small().color(TEXT3()));
            });
        }
        let Some((small, mono)) = &self.tb.x.img_out else { return };
        let name = bitmap::c_ident(&c.fm_name);
        let mut code = String::new();
        let (w, h) = (small.width(), small.height());
        if c.img_color {
            let (bytes, words) = bitmap::image_color(small, c.img_fmt);
            code.push_str(&format!("/* {w}x{h} {} */\n", color_fmt_label(c.img_fmt)));
            if words.is_empty() {
                code.push_str(&format!("const unsigned char {name}[{}] = {{\n", bytes.len()));
                bitmap::c_bytes(&mut code, &bytes, 16);
            } else {
                code.push_str(&format!("const unsigned short {name}[{}] = {{\n", words.len()));
                bitmap::c_words(&mut code, &words, 12);
            }
        } else {
            let b = bitmap::encode(mono, c.fm_scan, c.fm_msb, c.fm_invert);
            code.push_str(&format!("/* {w}x{h}, {}, {} */\n", scan_label(c.fm_scan), if c.fm_msb { "MSB first" } else { "LSB first" }));
            code.push_str(&format!("const unsigned char {name}[{}] = {{\n", b.len()));
            bitmap::c_bytes(&mut code, &b, 16);
        }
        code.push_str("};\n");
        ui.add_space(8.0);
        code_block(ui, &code, t("C 数组"), "font.h");
    }

    // ------------------------------------------------------------ 颜色

    fn tb_color(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.x;
        let parsed = parse_color(&c.color);
        let mut rgb = parsed.unwrap_or([0x22, 0xD3, 0xEE]);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut c.color)
                    .hint_text("#22D3EE  /  rgb(34,211,238)  /  0x1E9D (RGB565)")
                    .font(MONO(14.0))
                    .desired_width(340.0)
                    .margin(vec2(8.0, 6.0)),
            );
            if ui.color_edit_button_srgb(&mut rgb).changed() {
                c.color = format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
            }
        });
        if parsed.is_none() && !c.color.trim().is_empty() {
            ui.label(RichText::new(t("认不出来：#RRGGBB、rgb(r,g,b)、0xRRGGBB 或 4 位 RGB565（0xF800）")).color(ERR()));
            return;
        }
        let [r, g, b] = rgb;
        ui.add_space(10.0);
        let v565 = bitmap::rgb565(r, g, b);
        let (r5, g5, b5) = bitmap::from565(v565);
        ui.horizontal(|ui| {
            for (label, col) in [(t("原色"), Color32::from_rgb(r, g, b)), ("RGB565", Color32::from_rgb(r5, g5, b5))] {
                ui.vertical(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(120.0, 64.0), Sense::hover());
                    ui.painter().rect_filled(rect, R_CARD, col);
                    ui.label(RichText::new(label).small().color(TEXT3()));
                });
                ui.add_space(8.0);
            }
        });
        ui.add_space(8.0);
        let (h, s, v) = rgb_to_hsv(r, g, b);
        out_row(ui, "HEX", &format!("#{r:02X}{g:02X}{b:02X}"));
        out_row(ui, "RGB", &format!("rgb({r}, {g}, {b})"));
        out_row(ui, "RGB888", &format!("0x{r:02X}{g:02X}{b:02X}"));
        out_row(ui, "RGB565", &format!("0x{v565:04X}"));
        out_row(ui, t("RGB565 字节"), &format!("0x{:02X}, 0x{:02X}", v565 >> 8, v565 & 0xFF));
        out_row(ui, "RGB555", &format!("0x{:04X}", ((r as u16 >> 3) << 10) | ((g as u16 >> 3) << 5) | (b as u16 >> 3)));
        out_row(ui, "HSV", &format!("{h:.0}°, {:.0}%, {:.0}%", s * 100.0, v * 100.0));
        out_row(ui, t("灰度"), &format!("{}", (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) as u8));
        out_row(ui, "egui / C", &format!("{{{r}, {g}, {b}}}"));
    }

    // ------------------------------------------------------------ ASCII

    fn tb_ascii(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.x;
        ui.horizontal(|ui| {
            toggle_chip(ui, &mut c.ascii_ext, t("显示 128–255"), t("扩展部分按 Latin-1 显示"));
            ui.label(RichText::new(t("点击复制十六进制值")).small().color(TEXT3()));
        });
        ui.add_space(8.0);
        let end = if c.ascii_ext { 256 } else { 128 };
        let cols = 4;
        let rows = end / cols;
        egui::ScrollArea::vertical().id_salt("ascii").auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("ascii").num_columns(cols * 5).spacing([10.0, 2.0]).striped(true).show(ui, |ui| {
                for _ in 0..cols {
                    for h in [t("十进制"), "HEX", t("八进制"), t("字符"), ""] {
                        ui.label(RichText::new(h).small().color(TEXT3()));
                    }
                }
                ui.end_row();
                for r in 0..rows {
                    for col in 0..cols {
                        let v = col * rows + r;
                        let f = MONO(12.0);
                        ui.label(RichText::new(v.to_string()).font(f.clone()).color(TEXT2()));
                        let hx = format!("0x{v:02X}");
                        if ui.add(egui::Label::new(RichText::new(&hx).font(f.clone()).color(ACCENT())).sense(Sense::click())).clicked() {
                            ui.ctx().copy_text(hx);
                        }
                        ui.label(RichText::new(format!("{v:03o}")).font(f.clone()).color(TEXT3()));
                        let (ch, tip) = ascii_name(v as u8);
                        ui.label(RichText::new(ch).font(f).color(if tip.is_empty() { TEXT() } else { VIOLET() })).on_hover_text(tip);
                        ui.label("");
                    }
                    ui.end_row();
                }
            });
        });
    }
}

fn scan_label(s: Scan) -> &'static str {
    match s {
        Scan::Rows => t("逐行式（横向 8 点一字节）"),
        Scan::Cols => t("逐列式（纵向 8 点一字节）"),
        Scan::PageCols => t("行列式（OLED 页模式）"),
        Scan::ColPages => t("列行式"),
    }
}

fn color_fmt_label(f: ColorFmt) -> &'static str {
    match f {
        ColorFmt::Rgb565 => t("RGB565（16 位数组）"),
        ColorFmt::Rgb565Be => t("RGB565（字节，高位在前）"),
        ColorFmt::Rgb888 => "RGB888",
        ColorFmt::Gray8 => t("8 位灰度"),
    }
}

fn file_name(p: &str) -> String {
    std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string())
}

/// 系统里常见的字体：(显示名, 路径)。只列存在的
fn font_candidates() -> Vec<(String, String)> {
    let list: &[(&str, &str)] = &[
        ("宋体 SimSun", "C:\\Windows\\Fonts\\simsun.ttc"),
        ("黑体 SimHei", "C:\\Windows\\Fonts\\simhei.ttf"),
        ("微软雅黑", "C:\\Windows\\Fonts\\msyh.ttc"),
        ("楷体", "C:\\Windows\\Fonts\\simkai.ttf"),
        ("Consolas", "C:\\Windows\\Fonts\\consola.ttf"),
        ("Arial", "C:\\Windows\\Fonts\\arial.ttf"),
        ("Noto Sans CJK", "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
        ("Noto Sans CJK", "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"),
        ("Noto Sans CJK", "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc"),
        ("文泉驿微米黑", "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc"),
        ("文泉驿正黑", "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc"),
        ("DejaVu Sans", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
        ("DejaVu Sans Mono", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"),
        ("PingFang", "/System/Library/Fonts/PingFang.ttc"),
    ];
    let mut v: Vec<(String, String)> = list.iter().filter(|(_, p)| std::path::Path::new(p).is_file()).map(|(n, p)| (n.to_string(), p.to_string())).collect();
    v.dedup_by(|a, b| a.0 == b.0);
    v
}

fn draw_mono(ui: &mut egui::Ui, m: &Mono, px: f32) {
    let size = vec2(m.w as f32 * px, m.h as f32 * px);
    let (rect, _) = ui.allocate_exact_size(size + vec2(2.0, 2.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, BG0());
    let o = rect.min + vec2(1.0, 1.0);
    for y in 0..m.h {
        for x in 0..m.w {
            if m.get(x, y) {
                let r = Rect::from_min_size(
                    o + vec2(x as f32 * px, y as f32 * px),
                    vec2(px - if px > 3.0 { 1.0 } else { 0.0 }, px - if px > 3.0 { 1.0 } else { 0.0 }),
                );
                p.rect_filled(r, 0.0, ACCENT());
            }
        }
    }
    p.rect_stroke(rect, 2.0, st(1.0, LINE()), egui::StrokeKind::Inside);
}

pub(super) fn code_block(ui: &mut egui::Ui, code: &str, title: &str, file: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).font(FontId::new(11.0, bold())).color(TEXT3()).extra_letter_spacing(1.5));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button(t("另存为…")).clicked() {
                if let Some(p) = rfd::FileDialog::new().set_file_name(file).save_file() {
                    let _ = std::fs::write(p, code);
                }
            }
            if ui.button(t("复制")).clicked() {
                ui.ctx().copy_text(code.to_string());
            }
            ui.label(RichText::new(tf("{} 行", &[&code.lines().count()])).font(FontId::new(10.5, FontFamily::Monospace)).color(TEXT3()));
        });
    });
    egui::Frame::new()
        .fill(BG0())
        .stroke(st(1.0, LINE()))
        .corner_radius(R_CARD)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::both().id_salt("code_out").max_height(320.0).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(code).font(MONO(12.0)).color(TEXT2())).selectable(true).extend());
            });
        });
}

pub(super) fn parse_color(s: &str) -> Option<[u8; 3]> {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix("rgb(").and_then(|r| r.strip_suffix(')')) {
        let v: Vec<u8> = inner.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        return (v.len() == 3).then(|| [v[0], v[1], v[2]]);
    }
    let h = s.trim_start_matches('#').trim_start_matches("0x").trim_start_matches("0X");
    match h.len() {
        6 => {
            let v = u32::from_str_radix(h, 16).ok()?;
            Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
        }
        // 4 位十六进制按 RGB565 理解
        4 if s.starts_with("0x") || s.starts_with("0X") => {
            let (r, g, b) = bitmap::from565(u16::from_str_radix(h, 16).ok()?);
            Some([r, g, b])
        }
        3 if s.starts_with('#') => {
            let v: Vec<u8> = h.chars().filter_map(|c| c.to_digit(16)).map(|d| d as u8 * 17).collect();
            (v.len() == 3).then(|| [v[0], v[1], v[2]])
        }
        _ => None,
    }
}

fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

/// 码表里显示的字符和控制字符的说明
fn ascii_name(v: u8) -> (String, &'static str) {
    const CTRL: [(&str, &str); 32] = [
        ("NUL", "Null"),
        ("SOH", "Start of Heading"),
        ("STX", "Start of Text"),
        ("ETX", "End of Text / Ctrl+C"),
        ("EOT", "End of Transmission / Ctrl+D"),
        ("ENQ", "Enquiry"),
        ("ACK", "Acknowledge"),
        ("BEL", "Bell \\a"),
        ("BS", "Backspace \\b"),
        ("HT", "Tab \\t"),
        ("LF", "Line Feed \\n"),
        ("VT", "Vertical Tab \\v"),
        ("FF", "Form Feed \\f"),
        ("CR", "Carriage Return \\r"),
        ("SO", "Shift Out"),
        ("SI", "Shift In"),
        ("DLE", "Data Link Escape"),
        ("DC1", "XON / Ctrl+Q"),
        ("DC2", "Device Control 2"),
        ("DC3", "XOFF / Ctrl+S"),
        ("DC4", "Device Control 4"),
        ("NAK", "Negative Acknowledge"),
        ("SYN", "Synchronous Idle"),
        ("ETB", "End of Block"),
        ("CAN", "Cancel / XMODEM"),
        ("EM", "End of Medium"),
        ("SUB", "Substitute / Ctrl+Z"),
        ("ESC", "Escape \\x1b"),
        ("FS", "File Separator"),
        ("GS", "Group Separator"),
        ("RS", "Record Separator"),
        ("US", "Unit Separator"),
    ];
    match v {
        0..=31 => (CTRL[v as usize].0.to_string(), CTRL[v as usize].1),
        32 => ("SP".into(), "Space"),
        127 => ("DEL".into(), "Delete"),
        128..=159 => ("·".into(), "C1 control"),
        160 => ("NBSP".into(), "No-break space"),
        _ => ((v as char).to_string(), ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#22D3EE"), Some([0x22, 0xD3, 0xEE]));
        assert_eq!(parse_color("rgb(1, 2, 3)"), Some([1, 2, 3]));
        assert_eq!(parse_color("0xF800"), Some([255, 0, 0]));
        assert_eq!(parse_color("#fff"), Some([255, 255, 255]));
        assert_eq!(parse_color("nope"), None);
        let (h, s, v) = rgb_to_hsv(255, 0, 0);
        assert_eq!((h as i32, (s * 100.0) as i32, (v * 100.0) as i32), (0, 100, 100));
    }
}
