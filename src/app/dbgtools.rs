//! 调试工具窗口：Modbus 主站、校验计算器、发送文件（XMODEM / YMODEM / 原始数据）。
//! 都从日志工具栏的「更多」菜单或命令面板打开，平时不占地方。

use super::logx::{check_name, small_chip, tool_window};
use super::*;
use crate::checksum::{self, Check};
use crate::modbus::{self, Func, Reply};
use crate::xfer::{self, Out, Proto};

// ---------------------------------------------------------------- Modbus

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum MbFmt {
    U16,
    I16,
    Hex,
    U32,
    I32,
    F32,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct MbCfg {
    pub unit: u8,
    pub func: Func,
    pub addr: u16,
    pub count: u16,
    /// 写入的值，逗号或空格分隔，支持 0x
    pub values: String,
    pub fmt: MbFmt,
    /// 32 位值的两个寄存器交换顺序（CDAB）
    pub swap: bool,
    pub poll_ms: u32,
    pub timeout_ms: u32,
}

impl Default for MbCfg {
    fn default() -> Self {
        MbCfg {
            unit: 1,
            func: Func::ReadHolding,
            addr: 0,
            count: 10,
            values: String::new(),
            fmt: MbFmt::U16,
            swap: false,
            poll_ms: 1000,
            timeout_ms: 500,
        }
    }
}

#[derive(Default)]
pub(super) struct Mb {
    pub open: bool,
    polling: bool,
    last_poll: Option<Instant>,
    /// 等应答：(发出时间, 功能码, 数量, TCP 事务号)
    pending: Option<(Instant, Func, u16, u16)>,
    rx: Vec<u8>,
    tid: u16,
    /// 最近一次读到的值和起始地址
    values: Vec<u16>,
    base: u16,
    bits: bool,
    status: String,
    status_err: bool,
    ok: u64,
    err: u64,
    rtt_ms: u64,
}

// ---------------------------------------------------------------- 发送文件

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum XKind {
    Raw,
    P(Proto),
}

pub(super) enum XRun {
    Proto(xfer::Sender),
    Raw { data: Vec<u8>, pos: usize, last: Instant, started: Instant },
}

pub(super) struct Xf {
    pub open: bool,
    path: Option<PathBuf>,
    kind: XKind,
    chunk: usize,
    delay_ms: u32,
    pub run: Option<XRun>,
}

impl Default for Xf {
    fn default() -> Self {
        Xf {
            open: false,
            path: None,
            kind: XKind::P(Proto::Ymodem),
            chunk: 256,
            delay_ms: 10,
            run: None,
        }
    }
}

#[derive(Default)]
pub(super) struct Calc {
    pub open: bool,
    input: String,
    hex: bool,
}

impl App {
    pub(super) fn tools_menu_items(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        if menu_item(ui, if self.s.plot.on { t("关闭波形图") } else { t("波形图") }, &self.km.label(Act::TogglePlot)) {
            self.s.plot.on = !self.s.plot.on;
            ui.close_menu();
        }
        if menu_item(ui, t("Modbus 主站…"), "") {
            self.mb.open = true;
            ui.close_menu();
        }
        if menu_item(ui, t("发送文件…"), "") {
            self.xf.open = true;
            ui.close_menu();
        }
        if menu_item(ui, t("校验计算器…"), "") {
            self.calc.open = true;
            ui.close_menu();
        }
        ui.separator();
        if menu_item(ui, t("新窗口"), &self.km.label(Act::NewWindow)) {
            ui.close_menu();
            self.new_window();
        }
    }

    pub(super) fn new_window(&mut self) {
        match std::env::current_exe().and_then(|exe| std::process::Command::new(exe).arg("--new-window").spawn()) {
            Ok(_) => self.toast(t("已打开新窗口：可以同时连接另一个端口"), OK()),
            Err(e) => self.toast(tf("无法打开新窗口：{}", &[&e]), ERR()),
        }
    }

    pub(super) fn tool_windows(&mut self, ctx: &egui::Context) {
        if self.mb.open {
            self.modbus_window(ctx);
        }
        if self.xf.open {
            self.xfer_window(ctx);
        }
        if self.calc.open {
            self.calc_window(ctx);
        }
    }

    /// 收到的数据先给正在进行的 Modbus 请求 / 文件传输看一眼
    pub(super) fn tools_rx(&mut self, d: &[u8]) {
        if self.mb.pending.is_some() {
            self.mb.rx.extend_from_slice(d);
            self.mb_parse();
        }
        if let Some(XRun::Proto(s)) = self.xf.run.as_mut() {
            let outs = s.on_rx(d);
            self.xfer_outs(outs);
        }
    }

    pub(super) fn tick_tools(&mut self) {
        // ---- Modbus：超时和轮询
        if let Some((at, ..)) = self.mb.pending {
            if at.elapsed() > Duration::from_millis(self.s.mb.timeout_ms as u64) {
                self.mb.pending = None;
                self.mb.err += 1;
                self.mb.status = t("超时，没有应答").into();
                self.mb.status_err = true;
            } else {
                self.ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
        if self.mb.polling && self.mb.pending.is_none() {
            let iv = Duration::from_millis(self.s.mb.poll_ms.max(50) as u64);
            if self.mb.last_poll.is_none_or(|t| t.elapsed() >= iv) {
                self.mb_send();
            }
            self.ctx.request_repaint_after(iv);
        }
        // ---- 文件传输
        match self.xf.run.as_mut() {
            Some(XRun::Proto(s)) => {
                let outs = s.on_tick();
                self.xfer_outs(outs);
                self.ctx.request_repaint_after(Duration::from_millis(200));
            }
            Some(XRun::Raw { data, pos, last, started }) => {
                if self.conn.is_none() {
                    self.xf.run = None;
                    self.buf.sys(t("✗ 连接断开，文件发送中止"));
                    return;
                }
                if last.elapsed() >= Duration::from_millis(self.xf.delay_ms as u64) {
                    let end = (*pos + self.xf.chunk.max(1)).min(data.len());
                    let chunk = data[*pos..end].to_vec();
                    *pos = end;
                    *last = Instant::now();
                    let done = end >= data.len();
                    let secs = started.elapsed().as_secs_f64();
                    let n = data.len();
                    self.send_raw(chunk);
                    if done {
                        self.xf.run = None;
                        self.buf.sys(tf("✓ 文件发送完成：{}，用时 {}", &[&human_bytes(n as u64), &format!("{secs:.1}s")]));
                    }
                }
                self.ctx.request_repaint_after(Duration::from_millis(self.xf.delay_ms.max(1) as u64));
            }
            None => {}
        }
    }

    // ------------------------------------------------------------ Modbus

    fn mb_values(&self) -> Result<Vec<u16>, String> {
        let mut v = vec![];
        for s in self.s.mb.values.split([',', ' ', '，', ';']).filter(|s| !s.is_empty()) {
            let n = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                i64::from_str_radix(h, 16)
            } else {
                s.parse::<i64>()
            }
            .map_err(|_| tf("不是数字：{}", &[&s]))?;
            v.push(n as u16);
        }
        Ok(v)
    }

    fn mb_send(&mut self) {
        if self.conn.is_none() {
            self.mb.polling = false;
            self.mb.status = t("未连接").into();
            self.mb.status_err = true;
            return;
        }
        let c = self.s.mb.clone();
        let vals = if c.func.is_read() {
            vec![]
        } else {
            match self.mb_values() {
                Ok(v) if !v.is_empty() => v,
                Ok(_) => {
                    self.mb.status = t("请填写要写入的值").into();
                    self.mb.status_err = true;
                    return;
                }
                Err(e) => {
                    self.mb.status = e;
                    self.mb.status_err = true;
                    return;
                }
            }
        };
        let pdu = modbus::pdu(c.func, c.addr, c.count, &vals);
        let tcp = matches!(self.s.transport, Transport::TcpClient | Transport::TcpServer);
        self.mb.tid = self.mb.tid.wrapping_add(1);
        let frame = if tcp { modbus::tcp(self.mb.tid, c.unit, &pdu) } else { modbus::rtu(c.unit, &pdu) };
        self.mb.rx.clear();
        self.mb.pending = Some((Instant::now(), c.func, c.count, self.mb.tid));
        self.mb.last_poll = Some(Instant::now());
        self.send_bytes(frame);
    }

    fn mb_parse(&mut self) {
        let Some((at, f, count, tid)) = self.mb.pending else { return };
        let tcp = matches!(self.s.transport, Transport::TcpClient | Transport::TcpServer);
        let r = if tcp {
            modbus::parse_tcp(tid, f, count, &self.mb.rx)
        } else {
            modbus::parse_rtu(self.s.mb.unit, f, count, &self.mb.rx)
        };
        match r {
            Ok(None) => {}
            Ok(Some((reply, _))) => {
                self.mb.pending = None;
                self.mb.rtt_ms = at.elapsed().as_millis() as u64;
                match reply {
                    Reply::Values(v) => {
                        self.mb.ok += 1;
                        self.mb.values = v;
                        self.mb.base = self.s.mb.addr;
                        self.mb.bits = f.is_bits();
                        self.mb.status = tf("读取成功，{} ms", &[&self.mb.rtt_ms]);
                        self.mb.status_err = false;
                    }
                    Reply::Written => {
                        self.mb.ok += 1;
                        self.mb.status = tf("写入成功，{} ms", &[&self.mb.rtt_ms]);
                        self.mb.status_err = false;
                    }
                    Reply::Exception(c) => {
                        self.mb.err += 1;
                        self.mb.status = tf("异常 {}：{}", &[&format!("{c:02X}"), &modbus::exception_name(c)]);
                        self.mb.status_err = true;
                    }
                }
            }
            Err(e) => {
                self.mb.pending = None;
                self.mb.err += 1;
                self.mb.status = tf("应答错误：{}", &[&e]);
                self.mb.status_err = true;
            }
        }
    }

    fn modbus_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut send = false;
        let tcp = matches!(self.s.transport, Transport::TcpClient | Transport::TcpServer);
        tool_window(ctx, t("Modbus 主站"), "modbus", &mut open, 470.0, |ui| {
            ui.label(
                RichText::new(if tcp {
                    t("当前是 TCP 连接：使用 Modbus TCP（MBAP 头）")
                } else {
                    t("当前是串口：使用 Modbus RTU（CRC-16）")
                })
                .small()
                .color(TEXT3()),
            );
            let c = &mut self.s.mb;
            egui::Grid::new("mbg").num_columns(4).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label(RichText::new(t("从站")).color(TEXT2()));
                ui.add(egui::DragValue::new(&mut c.unit).range(0..=247));
                ui.label(RichText::new(t("功能")).color(TEXT2()));
                egui::ComboBox::from_id_salt("mbf").width(170.0).selected_text(func_name(c.func)).show_ui(ui, |ui| {
                    for f in modbus::FUNCS {
                        ui.selectable_value(&mut c.func, f, func_name(f));
                    }
                });
                ui.end_row();
                ui.label(RichText::new(t("起始地址")).color(TEXT2()));
                ui.add(egui::DragValue::new(&mut c.addr).range(0..=65535).hexadecimal(4, false, true).prefix("0x"));
                if c.func.is_read() {
                    ui.label(RichText::new(t("数量")).color(TEXT2()));
                    ui.add(egui::DragValue::new(&mut c.count).range(1..=125));
                } else {
                    ui.label(RichText::new(t("写入值")).color(TEXT2()));
                    ui.add(egui::TextEdit::singleline(&mut c.values).desired_width(170.0).hint_text("1, 0x10, 300"));
                }
                ui.end_row();
                ui.label(RichText::new(t("显示")).color(TEXT2()));
                egui::ComboBox::from_id_salt("mbfmt").width(90.0).selected_text(fmt_name(c.fmt)).show_ui(ui, |ui| {
                    for f in [MbFmt::U16, MbFmt::I16, MbFmt::Hex, MbFmt::U32, MbFmt::I32, MbFmt::F32] {
                        ui.selectable_value(&mut c.fmt, f, fmt_name(f));
                    }
                });
                ui.label(RichText::new(t("超时")).color(TEXT2()));
                ui.add(egui::DragValue::new(&mut c.timeout_ms).range(50..=10_000).suffix(" ms"));
                ui.end_row();
            });
            if matches!(c.fmt, MbFmt::U32 | MbFmt::I32 | MbFmt::F32) {
                small_chip(ui, &mut c.swap, t("字交换 CDAB"), t("32 位值的低 16 位在前（部分设备这样存）"));
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new(RichText::new(t("发送")).color(ON_ACCENT())).fill(ACCENT()).min_size(vec2(72.0, 28.0)))
                    .clicked()
                {
                    send = true;
                }
                let poll = self.mb.polling;
                let mut p = poll;
                if toggle_chip(ui, &mut p, t("轮询"), t("按间隔重复发送这个请求")) {
                    self.mb.polling = p;
                    self.mb.last_poll = None;
                }
                ui.add(egui::DragValue::new(&mut c.poll_ms).range(50..=600_000).suffix(" ms").speed(10));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("✓ {}   ✗ {}", self.mb.ok, self.mb.err))
                            .font(FontId::new(11.5, FontFamily::Monospace))
                            .color(TEXT3()),
                    );
                });
            });
            if !self.mb.status.is_empty() {
                ui.label(RichText::new(&self.mb.status).color(if self.mb.status_err { ERR() } else { OK() }));
            }
            // ---- 结果表
            if !self.mb.values.is_empty() {
                ui.separator();
                let mono = FontId::new(12.5, FontFamily::Monospace);
                let fmt = c.fmt;
                let swap = c.swap;
                let wide = !self.mb.bits && matches!(fmt, MbFmt::U32 | MbFmt::I32 | MbFmt::F32);
                let step = if wide { 2 } else { 1 };
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    egui::Grid::new("mbres").num_columns(3).striped(true).spacing([24.0, 3.0]).show(ui, |ui| {
                        ui.label(RichText::new(t("寄存器")).small().color(TEXT3()));
                        ui.label(RichText::new(t("原始值")).small().color(TEXT3()));
                        ui.label(RichText::new(t("数值")).small().color(TEXT3()));
                        ui.end_row();
                        let v = &self.mb.values;
                        let mut i = 0;
                        while i < v.len() {
                            let a = self.mb.base as usize + i;
                            ui.label(RichText::new(format!("{a}  (0x{a:04X})")).font(mono.clone()).color(TEXT2()));
                            if self.mb.bits {
                                ui.label(RichText::new(v[i].to_string()).font(mono.clone()).color(TEXT3()));
                                ui.label(RichText::new(if v[i] != 0 { "ON" } else { "OFF" }).font(mono.clone()).color(if v[i] != 0 { OK() } else { TEXT3() }));
                            } else {
                                let raw = if wide && i + 1 < v.len() {
                                    format!("{:04X} {:04X}", v[i], v[i + 1])
                                } else {
                                    format!("{:04X}", v[i])
                                };
                                ui.label(RichText::new(raw).font(mono.clone()).color(TEXT3()));
                                let val = match fmt {
                                    MbFmt::U16 => v[i].to_string(),
                                    MbFmt::I16 => (v[i] as i16).to_string(),
                                    MbFmt::Hex => format!("0x{:04X}", v[i]),
                                    _ if i + 1 < v.len() => {
                                        let (hi, lo) = if swap { (v[i + 1], v[i]) } else { (v[i], v[i + 1]) };
                                        let u = (hi as u32) << 16 | lo as u32;
                                        match fmt {
                                            MbFmt::U32 => u.to_string(),
                                            MbFmt::I32 => (u as i32).to_string(),
                                            _ => format!("{}", f32::from_bits(u)),
                                        }
                                    }
                                    _ => "—".into(),
                                };
                                if ui
                                    .add(egui::Label::new(RichText::new(&val).font(mono.clone()).color(TEXT())).sense(Sense::click()))
                                    .on_hover_text(t("点击复制"))
                                    .clicked()
                                {
                                    ui.ctx().copy_text(val);
                                }
                            }
                            ui.end_row();
                            i += step;
                        }
                    });
                });
            }
        });
        if send {
            self.mb_send();
        }
        if !open {
            self.mb.polling = false;
        }
        self.mb.open = open;
    }

    // ------------------------------------------------------------ 发送文件

    fn xfer_outs(&mut self, outs: Vec<Out>) {
        for o in outs {
            match o {
                Out::Send(d) => self.send_raw(d),
                Out::Done => {
                    let secs = match &self.xf.run {
                        Some(XRun::Proto(s)) => s.started.elapsed().as_secs_f64(),
                        _ => 0.0,
                    };
                    self.xf.run = None;
                    self.buf.sys(tf("✓ 文件发送完成，用时 {}", &[&format!("{secs:.1}s")]));
                    self.toast(t("文件发送完成"), OK());
                }
                Out::Fail(e) => {
                    self.xf.run = None;
                    self.buf.sys(tf("✗ 文件发送失败：{}", &[&e]));
                    self.toast(tf("文件发送失败：{}", &[&e]), ERR());
                }
            }
        }
    }

    fn xfer_start(&mut self) {
        let Some(path) = self.xf.path.clone() else { return };
        if self.conn.is_none() {
            self.toast(t("未连接"), WARN());
            return;
        }
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                self.toast(tf("无法读取文件：{}", &[&e]), ERR());
                return;
            }
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match self.xf.kind {
            XKind::Raw => {
                self.buf.sys(tf("发送文件 {}（{}）", &[&name, &human_bytes(data.len() as u64)]));
                self.xf.run = Some(XRun::Raw {
                    data,
                    pos: 0,
                    last: Instant::now() - Duration::from_secs(1),
                    started: Instant::now(),
                });
            }
            XKind::P(p) => {
                self.buf.sys(tf("{} 发送 {}（{}），等待接收方开始…", &[&p.name(), &name, &human_bytes(data.len() as u64)]));
                self.xf.run = Some(XRun::Proto(xfer::Sender::new(p, &name, data)));
            }
        }
    }

    fn xfer_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut start = false;
        let mut cancel = false;
        tool_window(ctx, t("发送文件"), "xfer", &mut open, 420.0, |ui| {
            let busy = self.xf.run.is_some();
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal(|ui| {
                    let name = self.xf.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
                    ui.add(egui::Label::new(RichText::new(name.unwrap_or_else(|| t("未选择文件").into())).color(TEXT())).truncate());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(t("选择文件…")).clicked() {
                            if let Some(p) = rfd::FileDialog::new().pick_file() {
                                self.xf.path = Some(p);
                            }
                        }
                    });
                });
                if let Some(sz) = self.xf.path.as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len()) {
                    ui.label(RichText::new(human_bytes(sz)).small().color(TEXT3()));
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("协议")).color(TEXT2()));
                    for (k, n) in [
                        (XKind::P(Proto::Ymodem), "YMODEM"),
                        (XKind::P(Proto::Xmodem1k), "XMODEM-1K"),
                        (XKind::P(Proto::Xmodem), "XMODEM"),
                        (XKind::Raw, t("原始数据")),
                    ] {
                        ui.selectable_value(&mut self.xf.kind, k, n);
                    }
                });
                if self.xf.kind == XKind::Raw {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t("每包")).color(TEXT2()));
                        ui.add(egui::DragValue::new(&mut self.xf.chunk).range(1..=65536).suffix(" B"));
                        ui.label(RichText::new(t("间隔")).color(TEXT2()));
                        ui.add(egui::DragValue::new(&mut self.xf.delay_ms).range(0..=10_000).suffix(" ms"));
                    });
                } else {
                    ui.label(
                        RichText::new(t("先在设备上开始接收（如 U-Boot 的 loady / loadx），再点发送；也可以先点发送，60 秒内设备开始都行。"))
                            .small()
                            .color(TEXT3()),
                    );
                }
            });
            ui.add_space(4.0);
            match &self.xf.run {
                Some(r) => {
                    let (done, total, waiting) = match r {
                        XRun::Proto(s) => (s.sent(), s.total(), s.waiting()),
                        XRun::Raw { data, pos, .. } => (*pos, data.len(), false),
                    };
                    let frac = done as f32 / total.max(1) as f32;
                    ui.add(egui::ProgressBar::new(frac).text(if waiting {
                        t("等待接收方…").to_string()
                    } else {
                        format!("{} / {}", human_bytes(done as u64), human_bytes(total as u64))
                    }));
                    if ui.button(RichText::new(t("取消")).color(ERR())).clicked() {
                        cancel = true;
                    }
                }
                None => {
                    let can = self.xf.path.is_some() && self.conn.is_some();
                    if ui
                        .add_enabled(can, egui::Button::new(RichText::new(t("发送")).color(ON_ACCENT())).fill(ACCENT()).min_size(vec2(80.0, 28.0)))
                        .clicked()
                    {
                        start = true;
                    }
                    if self.conn.is_none() {
                        ui.label(RichText::new(t("先连接设备")).small().color(TEXT3()));
                    }
                }
            }
        });
        if start {
            self.xfer_start();
        }
        if cancel {
            if let Some(XRun::Proto(s)) = self.xf.run.as_mut() {
                let d = s.cancel();
                self.send_raw(d);
            }
            self.xf.run = None;
            self.buf.sys(t("文件发送已取消"));
        }
        self.xf.open = open;
    }

    // ------------------------------------------------------------ 校验计算器

    fn calc_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        tool_window(ctx, t("校验计算器"), "calc", &mut open, 400.0, |ui| {
            ui.horizontal(|ui| {
                small_chip(ui, &mut self.calc.hex, "HEX", t("按十六进制字节计算"));
                ui.label(RichText::new(if self.calc.hex { t("输入十六进制字节") } else { t("输入文字（按当前编码）") }).small().color(TEXT3()));
            });
            ui.add(
                egui::TextEdit::multiline(&mut self.calc.input)
                    .font(FontId::new(13.0, FontFamily::Monospace))
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text(if self.calc.hex { "01 03 00 00 00 0A" } else { "123456789" }),
            );
            let data = if self.calc.hex {
                parse_hex(&self.calc.input)
            } else {
                Ok(self.s.encoding.encode(&self.calc.input))
            };
            match data {
                Err(e) => {
                    ui.label(RichText::new(e).color(ERR()));
                }
                Ok(d) => {
                    ui.label(RichText::new(tf("{} 字节", &[&d.len()])).small().color(TEXT3()));
                    let mono = FontId::new(13.0, FontFamily::Monospace);
                    egui::Grid::new("calcg").num_columns(3).spacing([16.0, 5.0]).show(ui, |ui| {
                        for c in checksum::ALL {
                            ui.label(RichText::new(c.name()).color(TEXT2()));
                            let v = c.value(&d);
                            let w = c.len() * 2;
                            let s = format!("0x{v:0w$X}");
                            if ui
                                .add(egui::Label::new(RichText::new(&s).font(mono.clone()).color(TEXT())).sense(Sense::click()))
                                .on_hover_text(t("点击复制"))
                                .clicked()
                            {
                                ui.ctx().copy_text(s);
                            }
                            let wire: String = c.bytes(&d).iter().map(|b| format!("{b:02X} ")).collect();
                            ui.label(RichText::new(wire.trim_end()).font(mono.clone()).color(TEXT3())).on_hover_text(t("线路上的字节顺序"));
                            ui.end_row();
                        }
                    });
                }
            }
        });
        self.calc.open = open;
    }
}

fn func_name(f: Func) -> String {
    let n = match f {
        Func::ReadCoils => t("读线圈"),
        Func::ReadDiscrete => t("读离散输入"),
        Func::ReadHolding => t("读保持寄存器"),
        Func::ReadInput => t("读输入寄存器"),
        Func::WriteCoil => t("写单个线圈"),
        Func::WriteReg => t("写单个寄存器"),
        Func::WriteCoils => t("写多个线圈"),
        Func::WriteRegs => t("写多个寄存器"),
    };
    format!("{:02} {n}", f.code())
}

fn fmt_name(f: MbFmt) -> &'static str {
    match f {
        MbFmt::U16 => "UINT16",
        MbFmt::I16 => "INT16",
        MbFmt::Hex => "HEX",
        MbFmt::U32 => "UINT32",
        MbFmt::I32 => "INT32",
        MbFmt::F32 => "FLOAT",
    }
}

/// 命令栏追加校验的下拉选项
pub(super) fn check_combo(ui: &mut egui::Ui, v: &mut Check) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt("sendcheck")
        .width(70.0)
        .selected_text(if *v == Check::None { t("无校验").to_string() } else { format!("+{}", short_check(*v)) })
        .show_ui(ui, |ui| {
            changed |= ui.selectable_value(v, Check::None, check_name(Check::None)).changed();
            for c in checksum::ALL {
                changed |= ui.selectable_value(v, c, c.name()).changed();
            }
        })
        .response
        .on_hover_text(t("HEX 发送时自动在末尾加上校验"));
    changed
}

fn short_check(c: Check) -> &'static str {
    match c {
        Check::Sum8 => "SUM",
        Check::Xor8 => "XOR",
        Check::Crc8 => "CRC8",
        Check::Crc16Modbus => "CRC16",
        Check::Crc16Ccitt => "CCITT",
        Check::Crc16Xmodem => "XMODEM",
        Check::Crc32 => "CRC32",
        Check::None => "",
    }
}
