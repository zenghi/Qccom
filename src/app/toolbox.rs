//! 活动栏「工具箱」页：左边是工具列表，右边是选中的工具。
//! 最上面是通信调试（TCP / UDP / MQTT / BLE / WebSocket / HTTP API / Modbus 从站），选中后右边就是和串口一样的
//! 日志、终端、快捷命令；下面是文件服务、网络、计算、显示、总线这些小工具（新工具在 toolbox2.rs）。

use std::sync::{Arc, Mutex};

use crate::toolbox::{baud, conv, discover, hid, httpd, slcan, tftp};

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Debug)]
pub enum Tool {
    #[default]
    Tcp,
    Udp,
    Mqtt,
    Ble,
    Ws,
    Ssh,
    Api,
    MbSlave,
    Tftp,
    Http,
    Syslog,
    Discover,
    Ping,
    Subnet,
    Broker,
    Convert,
    Baud,
    Checksum,
    Timestamp,
    Regex,
    Font,
    Color,
    Ascii,
    Can,
    Hid,
    FwConv,
    FwPack,
    SizeDiff,
    Stack,
    Vectors,
    Svd,
    Project,
    Ioc,
    Resistor,
    Adc,
    Pwm,
    I2cPull,
    Struct,
    Decode,
    Vcd,
    Qr,
}

/// 工具列表的分组
pub(super) fn groups() -> [(&'static str, &'static [Tool]); 10] {
    [
        (t("通信调试"), &[Tool::Tcp, Tool::Udp, Tool::Mqtt, Tool::Ble, Tool::Ws, Tool::Ssh, Tool::Api, Tool::MbSlave]),
        (t("服务器"), &[Tool::Tftp, Tool::Http, Tool::Syslog, Tool::Broker]),
        (t("网络"), &[Tool::Discover, Tool::Ping, Tool::Subnet]),
        (t("固件"), &[Tool::FwConv, Tool::FwPack, Tool::SizeDiff, Tool::Stack, Tool::Vectors]),
        (t("芯片和工程"), &[Tool::Svd, Tool::Project, Tool::Ioc]),
        (t("计算"), &[Tool::Convert, Tool::Baud, Tool::Checksum, Tool::Timestamp, Tool::Regex]),
        (t("硬件计算"), &[Tool::Resistor, Tool::Adc, Tool::Pwm, Tool::I2cPull]),
        (t("数据和协议"), &[Tool::Struct, Tool::Decode, Tool::Vcd, Tool::Qr]),
        (t("显示"), &[Tool::Font, Tool::Color, Tool::Ascii]),
        (t("总线"), &[Tool::Can, Tool::Hid]),
    ]
}

/// 网络连接类工具对应的会话槽位
pub(super) fn comm_slot(tool: Tool) -> Option<usize> {
    match tool {
        Tool::Tcp => Some(1),
        Tool::Udp => Some(2),
        Tool::Mqtt => Some(3),
        Tool::Ble => Some(4),
        Tool::Ws => Some(5),
        _ => None,
    }
}

pub(super) fn tool_of_slot(k: usize) -> Option<Tool> {
    [None, Some(Tool::Tcp), Some(Tool::Udp), Some(Tool::Mqtt), Some(Tool::Ble), Some(Tool::Ws)].get(k).copied().flatten()
}

/// 记在设置里的部分
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolboxCfg {
    pub tool: Tool,
    pub tftp_root: String,
    pub tftp_port: u16,
    pub tftp_write: bool,
    pub http_root: String,
    pub http_port: u16,
    pub http_upload: bool,
    pub conv_in: conv::Input,
    pub conv_text: String,
    pub conv_gbk: bool,
    pub baud_clk: String,
    pub baud_target: String,
    pub can_port: String,
    pub can_serial_baud: u32,
    pub can_bitrate: u32,
    pub can_listen: bool,
    pub can_id: String,
    pub can_data: String,
    pub can_ext: bool,
    pub can_period: u32,
    pub hid_data: String,
    pub hid_feature_len: u16,
    /// 收藏的工具，排在列表最上面
    pub favs: Vec<Tool>,
    /// v1.4 新工具的设置
    pub x: super::toolbox2::Cfg2,
    /// v1.6 嵌入式工具的设置
    pub z: super::toolbox3::Cfg3,
    /// SSH / Telnet 保存的主机
    pub ssh_hosts: Vec<crate::remote::Host>,
}

impl Default for ToolboxCfg {
    fn default() -> Self {
        let home = std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_default();
        ToolboxCfg {
            tool: Tool::Tftp,
            tftp_root: home.clone(),
            tftp_port: if cfg!(windows) { 69 } else { 6969 },
            tftp_write: false,
            http_root: home,
            http_port: 8000,
            http_upload: false,
            conv_in: conv::Input::Hex,
            conv_text: "00 00 80 3F".into(),
            conv_gbk: false,
            baud_clk: "72M".into(),
            baud_target: "115200".into(),
            can_port: String::new(),
            can_serial_baud: 115200,
            can_bitrate: 500_000,
            can_listen: false,
            can_id: "123".into(),
            can_data: "11 22 33 44".into(),
            can_ext: false,
            can_period: 100,
            hid_data: "00 01 02 03".into(),
            hid_feature_len: 64,
            favs: vec![],
            x: Default::default(),
            z: Default::default(),
            ssh_hosts: vec![],
        }
    }
}

/// 运行时状态
pub struct ToolboxUi {
    tftp: Option<crate::toolbox::Shared>,
    http: Option<crate::toolbox::Shared>,
    disc: discover::Shared,
    can: Option<slcan::Link>,
    can_st: slcan::Shared,
    can_by_id: bool,
    can_repeat: Option<Instant>,
    hid: Option<hid::Hid>,
    hid_list: Vec<hid::Info>,
    hid_sel: Option<String>,
    hid_dev: Option<hid::Dev>,
    hid_st: hid::Shared,
    hid_all: bool,
    /// 工具列表的搜索
    filter: String,
    pub(super) x: super::toolbox2::Ui2,
    pub(super) z: super::toolbox3::Ui3,
    pub(super) ssh: super::sshview::SshUi,
}

impl Default for ToolboxUi {
    fn default() -> Self {
        ToolboxUi {
            tftp: None,
            http: None,
            disc: Default::default(),
            can: None,
            can_st: Arc::new(Mutex::new(Default::default())),
            can_by_id: true,
            can_repeat: None,
            hid: None,
            hid_list: vec![],
            hid_sel: None,
            hid_dev: None,
            hid_st: Arc::new(Mutex::new(Default::default())),
            hid_all: false,
            filter: String::new(),
            x: Default::default(),
            z: Default::default(),
            ssh: Default::default(),
        }
    }
}

pub(super) const MONO: fn(f32) -> FontId = |s| FontId::new(s, FontFamily::Monospace);

impl App {
    pub(super) fn toolbox_page(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("toolbox_nav")
            .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                left: 14,
                right: 14,
                top: 16,
                bottom: 8,
            }))
            .exact_width(210.0)
            .resizable(false)
            .show(ctx, |ui| self.toolbox_nav(ui));
        self.tick_toolbox2();
        // 网络连接和 API：右边就是调试页那一套（侧边栏、日志、终端、快捷命令）
        if comm_slot(self.s.tb.tool).is_some() || self.s.tb.tool == Tool::Api {
            self.debug_page(ctx);
            return;
        }
        if self.s.tb.tool == Tool::Ssh {
            self.ssh_page(ctx);
            return;
        }
        // CAN 循环发送
        if let (Some(at), Some(link)) = (self.tb.can_repeat, &self.tb.can) {
            if at.elapsed().as_millis() as u32 >= self.s.tb.can_period.max(1) {
                if let Some(f) = can_frame(&self.s.tb) {
                    let _ = link.tx.send(slcan::Cmd::Send(f));
                }
                self.tb.can_repeat = Some(Instant::now());
            }
            ctx.request_repaint_after(Duration::from_millis(self.s.tb.can_period.clamp(1, 1000) as u64));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                left: 24,
                right: 24,
                top: 18,
                bottom: 12,
            }))
            .show(ctx, |ui| {
                let (title, desc) = tool_text(self.s.tb.tool);
                ui.label(RichText::new(title).font(FontId::new(20.0, bold())).color(TEXT()));
                ui.add_space(2.0);
                ui.label(RichText::new(desc).color(TEXT3()));
                ui.add_space(14.0);
                match self.s.tb.tool {
                    Tool::Tftp => self.tb_tftp(ui),
                    Tool::Http => self.tb_http(ui),
                    Tool::Discover => self.tb_discover(ui),
                    Tool::Convert => self.tb_convert(ui),
                    Tool::Baud => self.tb_baud(ui),
                    Tool::Can => self.tb_can(ui),
                    Tool::Hid => self.tb_hid(ui),
                    other => self.tb_more(ui, other),
                }
            });
    }

    fn toolbox_nav(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(t("工具箱")).font(FontId::new(15.0, bold())).color(TEXT()));
        ui.add_space(10.0);
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.tb.filter)
                .hint_text(t("搜索工具"))
                .desired_width(f32::INFINITY)
                .margin(vec2(8.0, 5.0)),
        );
        ui.add_space(10.0);
        let q = self.tb.filter.trim().to_lowercase();
        // 多个词都要命中（名字、说明、分组名、内部名任意一处），比如“firmware pack”
        let all_groups = groups();
        let matches = |tool: Tool| {
            if q.is_empty() {
                return true;
            }
            let (a, b) = tool_text(tool);
            let g = all_groups.iter().find(|(_, ts)| ts.contains(&tool)).map_or("", |(g, _)| *g);
            let hay = format!("{a} {b} {g} {tool:?}").to_lowercase();
            q.split_whitespace().all(|w| hay.contains(w))
        };
        let favs: Vec<Tool> = self.s.tb.favs.iter().copied().filter(|t| matches(*t)).collect();
        let mut list: Vec<(&str, Vec<Tool>)> = vec![];
        if !favs.is_empty() {
            list.push((t("收藏"), favs));
        }
        for (g, tools) in all_groups.iter() {
            let v: Vec<Tool> = tools.iter().copied().filter(|t| matches(*t) && (!q.is_empty() || !self.s.tb.favs.contains(t))).collect();
            if !v.is_empty() {
                list.push((g, v));
            }
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if list.is_empty() {
                ui.label(RichText::new(t("没有匹配的工具")).color(TEXT3()));
            }
            for (g, tools) in list {
                eyebrow(ui, g, "");
                for tool in tools {
                    let state = self.tool_state(tool);
                    let r = nav_row(ui, tool_text(tool).0, self.s.tb.tool == tool, state);
                    if r.clicked() {
                        self.s.tb.tool = tool;
                    }
                    let fav = self.s.tb.favs.contains(&tool);
                    r.context_menu(|ui| {
                        if ui.button(if fav { t("取消收藏") } else { t("收藏到最上面") }).clicked() {
                            if fav {
                                self.s.tb.favs.retain(|x| *x != tool);
                            } else {
                                self.s.tb.favs.push(tool);
                            }
                            ui.close_menu();
                        }
                    });
                }
                ui.add_space(10.0);
            }
        });
        if !q.is_empty() && search.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            // 名字里带关键字的优先，其次才是说明里带的
            let gs = groups();
            let all = || gs.iter().flat_map(|g| g.1.iter()).copied();
            let by_name = all().find(|t| tool_text(*t).0.to_lowercase().contains(&q) || format!("{t:?}").to_lowercase().contains(&q));
            if let Some(first) = by_name.or_else(|| all().find(|t| matches(*t))) {
                self.s.tb.tool = first;
            }
        }
    }

    /// 列表上的状态点：0 无，1 运行 / 已连接，2 刚收到数据（闪一下）
    fn tool_state(&self, tool: Tool) -> u8 {
        if let Some(k) = comm_slot(tool) {
            return match (self.slot_online(k), self.slot_active(k)) {
                (true, true) => 2,
                (true, false) => 1,
                _ => 0,
            };
        }
        if tool == Tool::Ssh {
            let flash = self.tb.ssh.tabs.iter().any(|s| s.alive() && s.rx_at.is_some_and(|t| t.elapsed() < Duration::from_millis(400)));
            return if flash { 2 } else { self.ssh_online() as u8 };
        }
        let running = match tool {
            Tool::Tftp => self.tb.tftp.is_some(),
            Tool::Http => self.tb.http.is_some(),
            Tool::Discover => self.tb.disc.lock().unwrap().running,
            Tool::Can => self.tb.can.is_some(),
            Tool::Hid => self.tb.hid_dev.is_some(),
            other => self.tool2_running(other),
        };
        running as u8
    }

    /// 打开某个工具（命令面板、设备发现、状态栏都走这里）
    pub(super) fn open_tool(&mut self, tool: Tool) {
        self.s.tb.tool = tool;
        self.set_page(Page::Tools);
        if let Some(k) = comm_slot(tool) {
            self.use_slot(k);
        }
    }

    // ------------------------------------------------------------ TFTP

    fn tb_tftp(&mut self, ui: &mut egui::Ui) {
        let running = self.tb.tftp.is_some();
        let ip = crate::toolbox::local_ipv4().map(|i| i.to_string()).unwrap_or_else(|| "127.0.0.1".into());
        settings_card(ui, |ui| {
            ui.add_enabled_ui(!running, |ui| {
                folder_row(ui, &mut self.s.tb.tftp_root);
                form_row(ui, t("端口"), |ui| {
                    ui.allocate_ui(vec2(90.0, 26.0), |ui| num_field(ui, "tftp_port", &mut self.s.tb.tftp_port));
                    ui.add_space(12.0);
                    let mut w = self.s.tb.tftp_write;
                    toggle_chip(ui, &mut w, t("允许上传"), t("允许客户端用 tftp put 把文件写进这个文件夹"));
                    self.s.tb.tftp_write = w;
                });
            });
            ui.add_space(6.0);
            if server_button(ui, running) {
                if running {
                    if let Some(s) = self.tb.tftp.take() {
                        s.stop();
                    }
                } else {
                    let cfg = tftp::Config {
                        root: PathBuf::from(&self.s.tb.tftp_root),
                        port: self.s.tb.tftp_port,
                        allow_write: self.s.tb.tftp_write,
                    };
                    match tftp::start(cfg, self.ctx.clone()) {
                        Ok(s) => self.tb.tftp = Some(s),
                        Err(e) => self.toast(e, ERR()),
                    }
                }
            }
        });
        if running {
            ui.add_space(10.0);
            let port = self.s.tb.tftp_port;
            let cmd = if port == 69 {
                format!("setenv serverip {ip}; tftpboot ${{loadaddr}} zImage")
            } else {
                format!("setenv serverip {ip}; setenv tftpdstp {port}; tftpboot ${{loadaddr}} zImage")
            };
            hint_line(ui, t("U-Boot 里这样下载："), &cmd);
        }
        ui.add_space(14.0);
        let log = self.tb.tftp.as_ref().map(|s| s.log.lock().unwrap().clone()).unwrap_or_default();
        xfer_table(ui, &log, false);
    }

    // ------------------------------------------------------------ HTTP

    fn tb_http(&mut self, ui: &mut egui::Ui) {
        let running = self.tb.http.is_some();
        let ip = crate::toolbox::local_ipv4().map(|i| i.to_string()).unwrap_or_else(|| "127.0.0.1".into());
        settings_card(ui, |ui| {
            ui.add_enabled_ui(!running, |ui| {
                folder_row(ui, &mut self.s.tb.http_root);
                form_row(ui, t("端口"), |ui| {
                    ui.allocate_ui(vec2(90.0, 26.0), |ui| num_field(ui, "http_port", &mut self.s.tb.http_port));
                    ui.add_space(12.0);
                    let mut w = self.s.tb.http_upload;
                    toggle_chip(ui, &mut w, t("允许 PUT 上传"), t("允许设备用 HTTP PUT 把文件（日志、截图）传上来"));
                    self.s.tb.http_upload = w;
                });
            });
            ui.add_space(6.0);
            if server_button(ui, running) {
                if running {
                    if let Some(s) = self.tb.http.take() {
                        s.stop();
                    }
                } else {
                    let cfg = httpd::Config {
                        root: PathBuf::from(&self.s.tb.http_root),
                        port: self.s.tb.http_port,
                        allow_upload: self.s.tb.http_upload,
                    };
                    match httpd::start(cfg, self.ctx.clone()) {
                        Ok(s) => self.tb.http = Some(s),
                        Err(e) => self.toast(e, ERR()),
                    }
                }
            }
        });
        if running {
            ui.add_space(10.0);
            let url = format!("http://{ip}:{}/", self.s.tb.http_port);
            ui.horizontal(|ui| {
                hint_line(ui, t("地址："), &url);
                if ui.add(egui::Button::new(RichText::new(t("在浏览器打开")).size(12.0))).clicked() {
                    open_url(&url);
                }
            });
        }
        ui.add_space(14.0);
        let log = self.tb.http.as_ref().map(|s| s.log.lock().unwrap().clone()).unwrap_or_default();
        xfer_table(ui, &log, true);
    }

    // ------------------------------------------------------------ 设备发现

    fn tb_discover(&mut self, ui: &mut egui::Ui) {
        let (running, done, total, net, err, hosts) = {
            let s = self.tb.disc.lock().unwrap();
            (s.running, s.done, s.total, s.net.clone(), s.err.clone(), s.hosts.values().cloned().collect::<Vec<_>>())
        };
        ui.horizontal(|ui| {
            let label = if running { t("扫描中…") } else { t("开始扫描") };
            if ui
                .add_enabled(!running, egui::Button::new(RichText::new(label).color(ON_ACCENT()).strong()).fill(ACCENT()).min_size(vec2(110.0, 30.0)))
                .clicked()
            {
                discover::start(&self.tb.disc, self.ctx.clone());
            }
            if !net.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new(tf("网段 {}", &[&net])).font(MONO(12.0)).color(TEXT3()));
            }
            if running && total > 0 {
                ui.add_space(8.0);
                ui.add(egui::ProgressBar::new(done as f32 / total as f32).desired_width(180.0).desired_height(6.0).fill(ACCENT()));
            }
        });
        if let Some(e) = err {
            ui.add_space(6.0);
            ui.label(RichText::new(e).color(ERR()));
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new(t("mDNS 查询 + 扫描常用端口（SSH、Telnet、HTTP、Modbus、MQTT、ADB…）。点端口可以直接用对应方式连接。"))
                .small()
                .color(TEXT3()),
        );
        ui.add_space(12.0);
        if hosts.is_empty() {
            ui.label(
                RichText::new(if running {
                    t("正在扫描，发现的设备会陆续出现在这里")
                } else {
                    t("还没有结果")
                })
                .color(TEXT3()),
            );
            return;
        }
        let mut action: Option<(String, u16)> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for h in &hosts {
                egui::Frame::new()
                    .fill(BG2())
                    .stroke(st(1.0, LINE()))
                    .corner_radius(R_CARD)
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let r = ui.add(egui::Label::new(RichText::new(h.ip.to_string()).font(FontId::new(14.0, mono_bold())).color(TEXT())).sense(Sense::click()));
                            r.on_hover_text(t("点击复制")).clicked().then(|| {
                                ui.ctx().copy_text(h.ip.to_string());
                            });
                            if !h.name.is_empty() {
                                ui.label(RichText::new(h.name.trim_end_matches('.')).color(TEXT2()));
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                for &p in h.ports.iter().rev() {
                                    let name = discover::port_name(p);
                                    let txt = if name.is_empty() { p.to_string() } else { format!("{p} {name}") };
                                    let b = egui::Button::new(RichText::new(txt).font(MONO(11.0)).color(ACCENT()))
                                        .fill(ACCENT().gamma_multiply(0.08))
                                        .stroke(st(1.0, ACCENT().gamma_multiply(0.3)));
                                    if ui.add(b).on_hover_text(port_action_tip(p)).clicked() {
                                        action = Some((h.ip.to_string(), p));
                                    }
                                }
                            });
                        });
                        if !h.services.is_empty() {
                            ui.add_space(2.0);
                            ui.horizontal_wrapped(|ui| {
                                for (typ, name, port) in &h.services {
                                    pill(ui, &format!("{typ} · {name} :{port}"), VIOLET());
                                }
                            });
                        }
                    });
                ui.add_space(6.0);
            }
        });
        if let Some((ip, port)) = action {
            self.connect_to(&ip, port);
        }
    }

    /// 点设备发现里的端口：HTTP 打开 API 标签，MQTT 填好 MQTT，其它用 TCP 客户端
    fn connect_to(&mut self, ip: &str, port: u16) {
        let slot = match port {
            80 | 443 | 8080 => None,
            1883 | 8883 => Some(3),
            _ => Some(1),
        };
        if slot.is_some_and(|k| self.slot_online(k)) {
            self.toast(t("先断开当前连接再切换"), WARN());
            return;
        }
        match port {
            80 | 443 | 8080 => {
                let scheme = if port == 443 { "https" } else { "http" };
                let url = if port == 80 || port == 443 {
                    format!("{scheme}://{ip}/")
                } else {
                    format!("{scheme}://{ip}:{port}/")
                };
                self.s.api.current = crate::api::Request { url, ..Default::default() };
                self.open_tool(Tool::Api);
            }
            1883 | 8883 => {
                self.s.net.mqtt.host = ip.into();
                self.s.net.mqtt.port = port;
                self.s.net.mqtt.tls = port == 8883;
                self.open_tool(Tool::Mqtt);
            }
            _ => {
                // TCP 会话要切成客户端：先换到这个会话再改
                self.open_tool(Tool::Tcp);
                self.use_slot(1);
                self.s.transport = Transport::TcpClient;
                self.s.net.tcp_host = ip.into();
                self.s.net.tcp_port = port;
            }
        }
    }

    // ------------------------------------------------------------ 数据转换

    fn tb_convert(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb;
        ui.horizontal(|ui| {
            let mut k = c.conv_in;
            ui.label(RichText::new(t("输入")).color(TEXT2()));
            ui.allocate_ui(vec2(320.0, 30.0), |ui| {
                tab_strip(
                    ui,
                    &mut k,
                    &[
                        (conv::Input::Hex, "HEX"),
                        (conv::Input::Text, t("文字")),
                        (conv::Input::Base64, "Base64"),
                        (conv::Input::Number, t("数字")),
                    ],
                    28.0,
                );
            });
            if k != c.conv_in {
                // 换输入方式时把当前内容转过去，不用重新输入
                if let Ok(b) = conv::parse(c.conv_in, &c.conv_text, c.conv_gbk) {
                    c.conv_text = match k {
                        conv::Input::Hex => conv::hex(&b),
                        conv::Input::Text => conv::text(&b, c.conv_gbk),
                        conv::Input::Base64 => conv::base64(&b),
                        conv::Input::Number => c.conv_text.clone(),
                    };
                }
                c.conv_in = k;
            }
            ui.add_space(12.0);
            toggle_chip(ui, &mut c.conv_gbk, "GBK", t("文字按 GBK 编码（默认 UTF-8）"));
        });
        ui.add_space(8.0);
        let hint = match c.conv_in {
            conv::Input::Hex => "48 65 6C 6C 6F  /  0x12,0x34  /  1234ABCD",
            conv::Input::Text => "Hello\\r\\n   (\\r \\n \\t \\0 \\xNN)",
            conv::Input::Base64 => "SGVsbG8=",
            conv::Input::Number => "1234  /  0x4D2  /  -1  /  3.14",
        };
        ui.add(
            egui::TextEdit::multiline(&mut c.conv_text)
                .font(MONO(13.0))
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text(hint),
        );
        ui.add_space(10.0);
        let bytes = conv::parse(c.conv_in, &c.conv_text, c.conv_gbk);
        let gbk = c.conv_gbk;
        let float_in = (c.conv_in == conv::Input::Number)
            .then(|| conv::float32_bytes(&c.conv_text))
            .flatten()
            .filter(|_| c.conv_text.contains('.'));
        let b = match bytes {
            Ok(b) => b,
            Err(e) => {
                ui.label(RichText::new(e).color(ERR()));
                return;
            }
        };
        let mut to_send: Option<String> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            card(ui, |ui| {
                let rows: Vec<(&str, String)> = vec![
                    ("HEX", conv::hex(&b)),
                    (if gbk { "GBK" } else { "UTF-8" }, conv::text(&b, gbk)),
                    ("Base64", conv::base64(&b)),
                    (t("C 数组"), conv::c_array(&b)),
                ];
                for (k, v) in rows {
                    out_row(ui, k, &v);
                }
                if let Some((le, be)) = &float_in {
                    out_row(ui, "float LE", le);
                    out_row(ui, "float BE", be);
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tf("{} 字节", &[&b.len()])).small().color(TEXT3()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add_enabled(!b.is_empty(), egui::Button::new(RichText::new(t("放到发送栏（HEX）")).size(12.0))).clicked() {
                            to_send = Some(conv::hex(&b));
                        }
                    });
                });
            });
            ui.add_space(12.0);
            let nums = conv::numbers(&b);
            if !nums.is_empty() {
                eyebrow(ui, t("按数值解释（从第一个字节开始）"), "");
                card(ui, |ui| {
                    egui::Grid::new("conv_nums").num_columns(3).spacing([28.0, 6.0]).show(ui, |ui| {
                        ui.label(RichText::new(t("类型")).small().color(TEXT3()));
                        ui.label(RichText::new(t("小端")).small().color(TEXT3()));
                        ui.label(RichText::new(t("大端")).small().color(TEXT3()));
                        ui.end_row();
                        for (k, le, be) in nums {
                            ui.label(RichText::new(k).font(MONO(12.5)).color(VIOLET()));
                            copy_label(ui, &le);
                            copy_label(ui, &be);
                            ui.end_row();
                        }
                    });
                });
            }
        });
        if let Some(h) = to_send {
            self.s.hex_send = true;
            self.set_page(Page::Debug);
            self.use_slot(0);
            self.send_text = h;
            self.focus_send = true;
        }
    }

    // ------------------------------------------------------------ 波特率

    fn tb_baud(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb;
        settings_card(ui, |ui| {
            form_row(ui, t("时钟"), |ui| {
                ui.add(egui::TextEdit::singleline(&mut c.baud_clk).font(MONO(13.0)).desired_width(120.0).hint_text("72M"));
                ui.label(RichText::new(t("外设时钟，如 72M、16MHz、80000000")).small().color(TEXT3()));
            });
            form_row(ui, t("波特率"), |ui| {
                ui.add(egui::TextEdit::singleline(&mut c.baud_target).font(MONO(13.0)).desired_width(120.0));
                egui::ComboBox::from_id_salt("tb_baud_pick").width(24.0).selected_text("").show_ui(ui, |ui| {
                    for b in COMMON_BAUDS {
                        if ui.selectable_label(false, RichText::new(b.to_string()).monospace()).clicked() {
                            c.baud_target = b.to_string();
                        }
                    }
                });
            });
        });
        ui.add_space(14.0);
        let (Some(clk), Ok(target)) = (baud::parse_hz(&c.baud_clk), c.baud_target.trim().parse::<f64>()) else {
            ui.label(RichText::new(t("请输入时钟和波特率")).color(TEXT3()));
            return;
        };
        let rows = baud::table(clk, target);
        card(ui, |ui| {
            egui::Grid::new("baud_tbl").num_columns(4).spacing([28.0, 8.0]).show(ui, |ui| {
                for h in [t("寄存器"), t("值"), t("实际波特率"), t("误差")] {
                    ui.label(RichText::new(h).small().color(TEXT3()));
                }
                ui.end_row();
                for r in rows {
                    ui.label(RichText::new(r.name).color(TEXT2()));
                    copy_label(ui, &r.reg);
                    ui.label(RichText::new(format!("{:.1}", r.actual)).font(MONO(12.5)).color(TEXT()));
                    ui.label(RichText::new(format!("{:+.2} %", r.err)).font(MONO(12.5)).color(err_color(r.err)));
                    ui.end_row();
                }
            });
        });
        ui.add_space(14.0);
        eyebrow(ui, t("常用波特率在这个时钟下的误差（STM32 OVER16）"), "");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for (b, e) in baud::common_errors(clk) {
                let (txt, col) = match e {
                    Some(e) => (format!("{b}  {e:+.2}%"), err_color(e)),
                    None => (format!("{b}  —"), TEXT3()),
                };
                let chip = egui::Button::new(RichText::new(txt).font(MONO(11.5)).color(col))
                    .fill(col.gamma_multiply(0.08))
                    .stroke(st(1.0, col.gamma_multiply(0.25)))
                    .wrap_mode(egui::TextWrapMode::Extend);
                if ui.add(chip).on_hover_text(t("点击算这个波特率")).clicked() {
                    c.baud_target = b.to_string();
                }
            }
        });
        ui.add_space(6.0);
        ui.label(RichText::new(t("误差在 ±2% 以内一般能通信，超过 ±3% 容易出乱码。")).small().color(TEXT3()));
    }

    // ------------------------------------------------------------ CAN

    fn tb_can(&mut self, ui: &mut egui::Ui) {
        let open = self.tb.can.is_some();
        let (alive, err) = {
            let s = self.tb.can_st.lock().unwrap();
            (s.open, s.err.clone())
        };
        if open && !alive {
            self.tb.can = None; // 适配器拔掉了
            self.tb.can_repeat = None;
        }
        let ports: Vec<String> = self.ports.iter().map(|p| p.name.clone()).chain(self.extra_port.clone()).collect();
        let c = &mut self.s.tb;
        if c.can_port.is_empty() {
            if let Some(p) = ports.first() {
                c.can_port = p.clone();
            }
        }
        let mut toggle = false;
        settings_card(ui, |ui| {
            ui.add_enabled_ui(!open, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("适配器")).color(TEXT2()));
                    egui::ComboBox::from_id_salt("can_port")
                        .width(200.0)
                        .selected_text(RichText::new(&c.can_port).monospace())
                        .show_ui(ui, |ui| {
                            for p in &ports {
                                ui.selectable_value(&mut c.can_port, p.clone(), RichText::new(p).monospace());
                            }
                        });
                    ui.add_space(12.0);
                    ui.label(RichText::new(t("位速率")).color(TEXT2()));
                    let cur = slcan::BITRATES.iter().find(|(b, _)| *b == c.can_bitrate).map_or("?".to_string(), |(b, _)| kbps(*b));
                    egui::ComboBox::from_id_salt("can_rate").width(90.0).selected_text(cur).show_ui(ui, |ui| {
                        for (b, _) in slcan::BITRATES {
                            ui.selectable_value(&mut c.can_bitrate, *b, kbps(*b));
                        }
                    });
                    ui.add_space(12.0);
                    toggle_chip(ui, &mut c.can_listen, t("只听"), t("Listen-only：不发 ACK，不影响总线（抓包用）"));
                });
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let (label, fill, tc) = if open { (t("关闭"), BG3(), ERR()) } else { (t("打开"), ACCENT(), ON_ACCENT()) };
                if ui
                    .add_enabled(
                        open || !c.can_port.is_empty(),
                        egui::Button::new(RichText::new(label).color(tc).strong()).fill(fill).min_size(vec2(96.0, 28.0)),
                    )
                    .clicked()
                {
                    toggle = true;
                }
                ui.label(RichText::new(t("SLCAN 协议（CANable、USBtin 等 USB-CAN 适配器）")).small().color(TEXT3()));
            });
        });
        if toggle {
            if open {
                self.tb.can = None;
                self.tb.can_repeat = None;
            } else {
                let c = &self.s.tb;
                match slcan::open(&c.can_port, c.can_serial_baud, c.can_bitrate, c.can_listen, &self.tb.can_st, self.ctx.clone()) {
                    Ok(l) => self.tb.can = Some(l),
                    Err(e) => self.toast(tf("打开 {} 失败：{}", &[&c.can_port, &e]), ERR()),
                }
            }
        }
        if let Some(e) = err {
            ui.label(RichText::new(e).color(ERR()));
        }
        ui.add_space(10.0);

        // 发送
        let open = self.tb.can.is_some();
        let c = &mut self.s.tb;
        let mut send = false;
        ui.add_enabled_ui(open && !c.can_listen, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("ID").color(TEXT2()));
                ui.add(egui::TextEdit::singleline(&mut c.can_id).font(MONO(13.0)).desired_width(if c.can_ext { 90.0 } else { 50.0 }));
                toggle_chip(ui, &mut c.can_ext, t("扩展帧"), t("29 位 ID"));
                ui.add_space(6.0);
                ui.label(RichText::new(t("数据")).color(TEXT2()));
                ui.add(egui::TextEdit::singleline(&mut c.can_data).font(MONO(13.0)).desired_width(200.0).hint_text("11 22 33"));
                if ui.add(egui::Button::new(RichText::new(t("发送")).strong()).min_size(vec2(64.0, 26.0))).clicked() {
                    send = true;
                }
                ui.add_space(6.0);
                let mut rep = self.tb.can_repeat.is_some();
                if toggle_chip(ui, &mut rep, t("循环发送"), t("按下面的周期一直发")) {
                    self.tb.can_repeat = rep.then(Instant::now);
                }
                ui.add(egui::DragValue::new(&mut c.can_period).range(1..=60000).suffix(" ms").speed(5.0))
                    .on_hover_text(t("循环发送的周期"));
            });
        });
        if send {
            match can_frame(&self.s.tb) {
                Some(f) => {
                    if let Some(l) = &self.tb.can {
                        let _ = l.tx.send(slcan::Cmd::Send(f));
                    }
                }
                None => self.toast(t("ID 或数据格式不对（数据最多 8 字节）"), WARN()),
            }
        }
        ui.add_space(10.0);
        let mut by_id = self.tb.can_by_id;
        let s = self.tb.can_st.lock().unwrap();
        ui.horizontal(|ui| {
            ui.allocate_ui(vec2(220.0, 28.0), |ui| tab_strip(ui, &mut by_id, &[(true, t("按 ID")), (false, t("时间顺序"))], 26.0));
            ui.add_space(12.0);
            ui.label(RichText::new(format!("RX {}   TX {}   ID {}", s.rx, s.tx, s.ids.len())).font(MONO(12.0)).color(TEXT2()));
            if s.errors > 0 {
                ui.label(RichText::new(tf("错误 {}", &[&s.errors])).font(MONO(12.0)).color(ERR()));
            }
        });
        ui.add_space(6.0);
        let now = Instant::now();
        egui::Frame::new()
            .fill(BG0())
            .stroke(st(1.0, LINE()))
            .corner_radius(R_CARD)
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(ui.available_height() - 24.0);
                let f = MONO(12.5);
                let empty = |ui: &mut egui::Ui| {
                    if s.rx + s.tx == 0 {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(if self.tb.can.is_some() {
                                t("等待 CAN 帧…")
                            } else {
                                t("选择适配器所在的串口，点「打开」")
                            })
                            .color(TEXT3()),
                        );
                    }
                };
                if by_id {
                    header_row(ui, &[(t("ID"), 100.0), ("DLC", 40.0), (t("数据"), 260.0), (t("次数"), 80.0), (t("周期"), 80.0)]);
                    empty(ui);
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        dense_rows(ui);
                        for ((_, _), x) in s.ids.iter() {
                            ui.horizontal(|ui| {
                                cell(ui, 100.0, RichText::new(x.last.id_str()).font(f.clone()).color(ACCENT()));
                                cell(ui, 40.0, RichText::new(x.last.dlc.to_string()).font(f.clone()).color(TEXT3()));
                                ui.allocate_ui_with_layout(vec2(260.0, 18.0), Layout::left_to_right(Align::Center), |ui| {
                                    ui.set_min_width(260.0);
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    for (i, b) in x.last.data.iter().enumerate() {
                                        // 一秒内变过的字节高亮
                                        let fresh = x.changed[i].is_some_and(|t| now.duration_since(t).as_secs_f32() < 1.0);
                                        ui.label(RichText::new(format!("{b:02X}")).font(f.clone()).color(if fresh { WARN() } else { TEXT() }));
                                    }
                                    if x.last.rtr {
                                        ui.label(RichText::new("RTR").font(f.clone()).color(VIOLET()));
                                    }
                                });
                                cell(ui, 80.0, RichText::new(x.count.to_string()).font(f.clone()).color(TEXT2()));
                                let p = x.period_ms.map_or("—".to_string(), |p| format!("{p:.0} ms"));
                                cell(ui, 80.0, RichText::new(p).font(f.clone()).color(TEXT2()));
                            });
                        }
                    });
                } else {
                    header_row(ui, &[(t("时间"), 100.0), ("", 30.0), (t("ID"), 100.0), ("DLC", 40.0), (t("数据"), 260.0)]);
                    empty(ui);
                    let rows = s.trace.len();
                    egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show_rows(ui, ROW_H, rows, |ui, range| {
                        dense_rows(ui);
                        for it in &s.trace[range] {
                            ui.horizontal(|ui| {
                                cell(ui, 100.0, RichText::new(format!("{:.3}", it.t)).font(f.clone()).color(TEXT3()));
                                cell(ui, 30.0, RichText::new(if it.tx { "TX" } else { "RX" }).font(f.clone()).color(if it.tx { VIOLET() } else { OK() }));
                                cell(ui, 100.0, RichText::new(it.f.id_str()).font(f.clone()).color(ACCENT()));
                                cell(ui, 40.0, RichText::new(it.f.dlc.to_string()).font(f.clone()).color(TEXT3()));
                                let d = if it.f.rtr { "RTR".to_string() } else { conv::hex(&it.f.data) };
                                ui.label(RichText::new(d).font(f.clone()).color(TEXT()));
                            });
                        }
                    });
                }
            });
        drop(s);
        self.tb.can_by_id = by_id;
        if self.tb.can.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
    }

    // ------------------------------------------------------------ USB HID

    fn tb_hid(&mut self, ui: &mut egui::Ui) {
        if self.tb.hid.is_none() {
            let mut h = hid::Hid::new();
            self.tb.hid_list = h.list();
            self.tb.hid = Some(h);
        }
        if let Some(e) = self.tb.hid.as_ref().and_then(|h| h.err.clone()) {
            ui.label(RichText::new(e).color(ERR()));
        }
        let dev_err = self.tb.hid_st.lock().unwrap().err.clone();
        if dev_err.is_some() && self.tb.hid_dev.is_some() {
            self.tb.hid_dev = None;
        }
        let w = ui.available_width();
        let h = ui.available_height();
        ui.horizontal_top(|ui| {
            // 左：设备列表
            ui.allocate_ui_with_layout(vec2(300.0, h), Layout::top_down(Align::Min), |ui| {
                ui.horizontal(|ui| {
                    eyebrow(ui, t("设备"), "");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, Icon::Refresh, t("刷新"), false).clicked() {
                            if let Some(hh) = self.tb.hid.as_mut() {
                                self.tb.hid_list = hh.list();
                            }
                        }
                        let mut all = self.tb.hid_all;
                        toggle_chip(ui, &mut all, t("含键鼠"), t("也列出键盘、鼠标（系统通常独占，打不开）"));
                        self.tb.hid_all = all;
                    });
                });
                egui::ScrollArea::vertical().id_salt("hid_list").auto_shrink([false, false]).max_height(h - 40.0).show(ui, |ui| {
                    let mut pick = None;
                    for d in self.tb.hid_list.iter().filter(|d| self.tb.hid_all || !d.is_system()) {
                        let sel = self.tb.hid_sel.as_deref() == Some(d.path.as_str());
                        if hid_card(ui, d, sel, sel && self.tb.hid_dev.is_some()).clicked() {
                            pick = Some(d.path.clone());
                        }
                    }
                    if self.tb.hid_list.is_empty() {
                        ui.label(RichText::new(t("没有找到 HID 设备")).color(TEXT3()));
                    }
                    if let Some(p) = pick {
                        if self.tb.hid_sel.as_ref() != Some(&p) {
                            self.tb.hid_dev = None;
                        }
                        self.tb.hid_sel = Some(p);
                    }
                });
            });
            ui.add_space(16.0);
            // 右：报告收发
            ui.allocate_ui_with_layout(vec2(w - 316.0, h), Layout::top_down(Align::Min), |ui| self.hid_panel(ui));
        });
    }

    fn hid_panel(&mut self, ui: &mut egui::Ui) {
        let Some(info) = self.tb.hid_sel.as_ref().and_then(|p| self.tb.hid_list.iter().find(|d| &d.path == p)).cloned() else {
            ui.label(RichText::new(t("在左边选一个设备")).color(TEXT3()));
            return;
        };
        let open = self.tb.hid_dev.is_some();
        ui.horizontal(|ui| {
            ui.label(RichText::new(info.title()).font(FontId::new(14.0, bold())).color(TEXT()));
            ui.label(RichText::new(format!("{:04X}:{:04X}", info.vid, info.pid)).font(MONO(12.0)).color(TEXT3()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (label, fill, tc) = if open { (t("关闭"), BG3(), ERR()) } else { (t("打开"), ACCENT(), ON_ACCENT()) };
                if ui.add(egui::Button::new(RichText::new(label).color(tc).strong()).fill(fill).min_size(vec2(84.0, 28.0))).clicked() {
                    if open {
                        self.tb.hid_dev = None;
                    } else if let Some(h) = &self.tb.hid {
                        match h.open(&info, &self.tb.hid_st, self.ctx.clone()) {
                            Ok(d) => self.tb.hid_dev = Some(d),
                            Err(e) => self.toast(e, ERR()),
                        }
                    }
                }
            });
        });
        ui.label(
            RichText::new(format!(
                "Usage Page 0x{:04X} · Usage 0x{:04X} · Interface {}{}",
                info.usage_page,
                info.usage,
                info.interface,
                if info.serial.is_empty() { String::new() } else { format!(" · SN {}", info.serial) }
            ))
            .font(MONO(11.0))
            .color(TEXT3()),
        );
        ui.add_space(10.0);
        let c = &mut self.s.tb;
        let mut act: Option<char> = None;
        ui.add_enabled_ui(open, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("报告")).color(TEXT2()));
                ui.add(
                    egui::TextEdit::singleline(&mut c.hid_data)
                        .font(MONO(13.0))
                        .desired_width(ui.available_width() - 330.0)
                        .hint_text(t("第一个字节是报告 ID，没有编号填 00")),
                );
                if ui.add(egui::Button::new(RichText::new(t("发送输出报告")).strong())).clicked() {
                    act = Some('O');
                }
                ui.menu_button(t("特征报告 ▾"), |ui| {
                    if ui.button(t("写特征报告")).clicked() {
                        act = Some('f');
                        ui.close_menu();
                    }
                    ui.horizontal(|ui| {
                        if ui.button(t("读特征报告")).clicked() {
                            act = Some('F');
                            ui.close_menu();
                        }
                        ui.add(egui::DragValue::new(&mut c.hid_feature_len).range(2..=4096).suffix(t(" 字节")));
                    });
                });
            });
        });
        if let (Some(a), Some(dev)) = (act, &self.tb.hid_dev) {
            match conv::parse_hex(&c.hid_data) {
                Ok(d) if !d.is_empty() => {
                    let r = match a {
                        'O' => dev.write(&d),
                        'f' => dev.set_feature(&d),
                        _ => dev.get_feature(d[0], c.hid_feature_len as usize),
                    };
                    if let Err(e) = r {
                        self.toast(e, ERR());
                    }
                }
                Ok(_) => self.toast(t("请输入要发送的字节"), WARN()),
                Err(e) => self.toast(e, ERR()),
            }
        }
        ui.add_space(10.0);
        let s = self.tb.hid_st.lock().unwrap();
        ui.label(RichText::new(format!("IN {}   OUT {}", s.rx, s.tx)).font(MONO(12.0)).color(TEXT2()));
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(BG0())
            .stroke(st(1.0, LINE()))
            .corner_radius(R_CARD)
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(ui.available_height() - 4.0);
                let f = MONO(12.5);
                let n = s.log.len();
                egui::ScrollArea::both()
                    .id_salt("hid_log")
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show_rows(ui, 18.0, n, |ui, range| {
                        for e in &s.log[range] {
                            ui.horizontal(|ui| {
                                cell(ui, 80.0, RichText::new(format!("{:.3}", e.t)).font(f.clone()).color(TEXT3()));
                                let (k, col) = match e.kind {
                                    'I' => ("IN ", OK()),
                                    'O' => ("OUT", VIOLET()),
                                    'F' => ("FEA", WARN()),
                                    _ => ("FEA", VIOLET()),
                                };
                                cell(ui, 40.0, RichText::new(k).font(f.clone()).color(col));
                                ui.label(RichText::new(conv::hex(&e.data)).font(f.clone()).color(TEXT()));
                                let ascii: String = e.data.iter().map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '·' }).collect();
                                ui.label(RichText::new(ascii).font(f.clone()).color(TEXT3()));
                            });
                        }
                    });
                if n == 0 {
                    ui.label(RichText::new(if open { t("等待输入报告…") } else { t("点「打开」开始接收") }).color(TEXT3()));
                }
            });
    }
}

// ------------------------------------------------------------ 小部件

pub(super) fn tool_text(t_: Tool) -> (&'static str, &'static str) {
    match t_ {
        Tool::Tcp => ("TCP", t("TCP 客户端或服务器，和串口一样看日志、开终端、发快捷命令")),
        Tool::Udp => ("UDP", t("收发 UDP 数据包，每包一行并标出来源")),
        Tool::Mqtt => ("MQTT", t("连接 MQTT 服务器，订阅主题、发布消息")),
        Tool::Ble => (t("低功耗蓝牙"), t("扫描 BLE 设备，订阅通知、读写特征")),
        Tool::Ssh => ("SSH / Telnet", t("远程登录开发板、网关和服务器：多个会话同时在线，拖文件就能上传")),
        Tool::Ws => ("WebSocket", t("连接 ws:// 或 wss:// 服务，收发文本 / 二进制消息")),
        Tool::Api => (t("HTTP 接口"), t("像 Postman 一样调试设备上的 REST 接口")),
        Tool::Tftp => (t("TFTP 服务器"), t("把一个文件夹通过 TFTP 共享出去，U-Boot、交换机、IP 电话用它下载固件")),
        Tool::Http => (t("HTTP 文件服务器"), t("把一个文件夹通过 HTTP 共享出去，开发板做 OTA 升级时从这里下载，支持断点续传")),
        Tool::Discover => (t("设备发现"), t("找出局域网里的开发板、网关和服务：不知道 DHCP 分到的 IP 时很有用")),
        Tool::Convert => (t("数据转换"), t("HEX、文字、Base64、C 数组和各种整数 / 浮点数之间互相转换")),
        Tool::Baud => (t("波特率计算"), t("算 UART 分频寄存器的值和实际误差：STM32、AVR、ESP32、16550")),
        Tool::Can => (t("CAN 调试"), t("用 USB-CAN 适配器（SLCAN）收发 CAN 帧，按 ID 汇总、看周期和变化的字节")),
        Tool::Hid => (t("USB HID"), t("给自定义 HID 设备收发报告：输入、输出和特征报告")),
        other => super::toolbox2::tool_text2(other),
    }
}

fn nav_row(ui: &mut egui::Ui, text: &str, on: bool, state: u8) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let p = ui.painter();
    if on {
        p.rect_filled(rect, R_WIDGET, BG3());
        p.rect_filled(Rect::from_min_size(rect.min + vec2(0.0, 7.0), vec2(2.5, 16.0)), 1.5, ACCENT());
    } else if resp.hovered() {
        p.rect_filled(rect, R_WIDGET, BG2());
    }
    p.text(
        rect.left_center() + vec2(12.0, 0.0),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(13.0),
        if on { TEXT() } else { TEXT2() },
    );
    if state > 0 {
        let c = rect.right_center() - vec2(10.0, 0.0);
        let col = if state == 2 { ACCENT() } else { OK() };
        p.circle_filled(c, if state == 2 { 6.5 } else { 5.5 }, col.gamma_multiply(0.22));
        p.circle_filled(c, 3.0, col);
        if state == 2 {
            ui.ctx().request_repaint_after(Duration::from_millis(400));
        }
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.add_space(2.0);
    resp
}

pub(super) fn settings_card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(BG2())
        .stroke(st(1.0, LINE()))
        .corner_radius(R_CARD)
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        });
}

pub(super) fn folder_row(ui: &mut egui::Ui, root: &mut String) {
    form_row(ui, t("文件夹"), |ui| {
        let w = ui.available_width() - 80.0;
        ui.add_sized(vec2(w, 26.0), egui::TextEdit::singleline(root).font(MONO(12.5)).margin(vec2(6.0, 4.0)));
        if ui.add(egui::Button::new(RichText::new(t("选择…")).size(12.0)).min_size(vec2(70.0, 26.0))).clicked() {
            if let Some(p) = rfd::FileDialog::new().set_directory(&*root).pick_folder() {
                *root = p.to_string_lossy().into_owned();
            }
        }
    });
}

pub(super) fn server_button(ui: &mut egui::Ui, running: bool) -> bool {
    let (label, fill, tc) = if running { (t("停止服务"), BG3(), ERR()) } else { (t("启动服务"), ACCENT(), ON_ACCENT()) };
    ui.horizontal(|ui| {
        ui.add_space(66.0);
        let r = ui.add(egui::Button::new(RichText::new(label).color(tc).strong()).fill(fill).min_size(vec2(110.0, 30.0)));
        if running {
            ui.label(RichText::new(t("● 运行中")).color(OK()));
        }
        r.clicked()
    })
    .inner
}

pub(super) fn hint_line(ui: &mut egui::Ui, label: &str, code: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(TEXT2()));
        let r = ui.add(egui::Label::new(RichText::new(code).font(MONO(12.5)).color(ACCENT()).background_color(BG2())).sense(Sense::click()));
        if r.on_hover_text(t("点击复制")).clicked() {
            ui.ctx().copy_text(code.to_string());
        }
    });
}

pub(super) fn header_row(ui: &mut egui::Ui, cols: &[(&str, f32)]) {
    ui.horizontal(|ui| {
        for (h, w) in cols {
            cell(ui, *w, RichText::new(*h).small().color(TEXT3()));
        }
    });
    ui.add_space(2.0);
}

pub(super) const ROW_H: f32 = 22.0;

/// 表格行紧凑排列：行高固定 ROW_H，行间不留空（show_rows 需要行高准确）
pub(super) fn dense_rows(ui: &mut egui::Ui) {
    ui.spacing_mut().item_spacing.y = 0.0;
    ui.spacing_mut().interact_size.y = ROW_H;
}

pub(super) fn cell(ui: &mut egui::Ui, w: f32, text: RichText) {
    ui.allocate_ui_with_layout(vec2(w, 18.0), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_width(w);
        ui.add(egui::Label::new(text).truncate());
    });
}

fn xfer_table(ui: &mut egui::Ui, log: &[crate::toolbox::Xfer], http: bool) {
    eyebrow(ui, t("传输记录"), &log.len().to_string());
    if log.is_empty() {
        ui.label(RichText::new(t("还没有请求")).color(TEXT3()));
        return;
    }
    egui::Frame::new()
        .fill(BG0())
        .stroke(st(1.0, LINE()))
        .corner_radius(R_CARD)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let fw = (ui.available_width() - 80.0 - 120.0 - 200.0 - 70.0 - 4.0 * ui.spacing().item_spacing.x).max(160.0);
            header_row(
                ui,
                &[
                    (t("时间"), 80.0),
                    (t("客户端"), 120.0),
                    (if http { t("请求") } else { t("文件") }, fw),
                    (t("进度"), 200.0),
                    (t("结果"), 70.0),
                ],
            );
            egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                let f = MONO(12.0);
                for x in log {
                    ui.horizontal(|ui| {
                        cell(ui, 80.0, RichText::new(&x.at).font(f.clone()).color(TEXT3()));
                        cell(ui, 120.0, RichText::new(&x.peer).font(f.clone()).color(TEXT2()));
                        let arrow = if x.upload { "↑ " } else { "↓ " };
                        cell(ui, fw, RichText::new(format!("{arrow}{}", x.file)).font(f.clone()).color(TEXT()));
                        ui.allocate_ui_with_layout(vec2(200.0, 18.0), Layout::left_to_right(Align::Center), |ui| {
                            ui.set_min_width(200.0);
                            let frac = x.total.filter(|&t| t > 0).map(|t| x.bytes as f32 / t as f32);
                            if let (Some(fr), None) = (frac, &x.done) {
                                ui.add(egui::ProgressBar::new(fr).desired_width(110.0).desired_height(5.0).fill(ACCENT()));
                            }
                            let size = match x.total {
                                Some(t) if x.done.is_none() => format!("{} / {}", human_bytes(x.bytes), human_bytes(t)),
                                _ => human_bytes(x.bytes),
                            };
                            ui.label(RichText::new(size).font(f.clone()).color(TEXT2()));
                        });
                        let (txt, col) = match &x.done {
                            None => (t("传输中").to_string(), ACCENT()),
                            Some(Ok(())) if http => (x.status.to_string(), if x.status < 400 { OK() } else { WARN() }),
                            Some(Ok(())) => ("✓".to_string(), OK()),
                            Some(Err(_)) => (if http && x.status > 0 { x.status.to_string() } else { "✗".to_string() }, ERR()),
                        };
                        ui.label(RichText::new(txt).font(f.clone()).color(col)).on_hover_text(match &x.done {
                            Some(Err(e)) => e.clone(),
                            _ => String::new(),
                        });
                    });
                }
            });
        });
}

pub(super) fn out_row(ui: &mut egui::Ui, k: &str, v: &str) {
    ui.horizontal_top(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(80.0, 18.0), Sense::hover());
        ui.painter().text(r.left_center(), Align2::LEFT_CENTER, k, FontId::proportional(12.0), TEXT3());
        let w = ui.available_width() - 30.0;
        ui.allocate_ui(vec2(w, 18.0), |ui| {
            ui.set_width(w);
            ui.add(egui::Label::new(RichText::new(v).font(MONO(12.5)).color(TEXT())).wrap());
        });
        let (cr, resp) = ui.allocate_exact_size(vec2(20.0, 18.0), Sense::click());
        draw_icon(ui.painter(), cr, Icon::Copy, if resp.hovered() { ACCENT() } else { TEXT3() });
        if resp.on_hover_text(t("复制")).clicked() {
            ui.ctx().copy_text(v.to_string());
        }
    });
    ui.add_space(4.0);
}

pub(super) fn copy_label(ui: &mut egui::Ui, v: &str) {
    let r = ui.add(egui::Label::new(RichText::new(v).font(MONO(12.5)).color(TEXT())).sense(Sense::click()));
    if r.on_hover_text(t("点击复制")).clicked() {
        ui.ctx().copy_text(v.to_string());
    }
}

fn err_color(e: f64) -> Color32 {
    let a = e.abs();
    if a < 1.0 {
        OK()
    } else if a < 2.5 {
        WARN()
    } else {
        ERR()
    }
}

fn kbps(b: u32) -> String {
    if b >= 1_000_000 {
        format!("{} Mbit/s", b / 1_000_000)
    } else {
        format!("{} kbit/s", b / 1000)
    }
}

fn port_action_tip(p: u16) -> &'static str {
    match p {
        80 | 443 | 8080 => t("在 API 标签里打开"),
        1883 | 8883 => t("用 MQTT 连接"),
        _ => t("用 TCP 客户端连接"),
    }
}

fn can_frame(c: &ToolboxCfg) -> Option<slcan::Frame> {
    let id = u32::from_str_radix(c.can_id.trim().trim_start_matches("0x"), 16).ok()?;
    if (!c.can_ext && id > 0x7FF) || id > 0x1FFF_FFFF {
        return None;
    }
    let data = conv::parse_hex(&c.can_data).ok()?;
    if data.len() > 8 {
        return None;
    }
    Some(slcan::Frame {
        id,
        ext: c.can_ext,
        rtr: false,
        dlc: data.len() as u8,
        data,
    })
}

fn hid_card(ui: &mut egui::Ui, d: &hid::Info, sel: bool, live: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
    let p = ui.painter();
    let hov = resp.hovered();
    p.rect(
        rect,
        CornerRadius::same(7),
        if sel {
            BG3()
        } else if hov {
            BG2()
        } else {
            BG1()
        },
        st(
            1.0,
            if live {
                OK().gamma_multiply(0.5)
            } else if sel {
                LINE_HI()
            } else {
                Color32::TRANSPARENT
            },
        ),
        egui::StrokeKind::Inside,
    );
    let x = rect.left() + 12.0;
    p.text(
        pos2(x, rect.top() + 14.0),
        Align2::LEFT_CENTER,
        truncate(&d.title(), 30),
        FontId::new(13.0, bold()),
        if sel { TEXT() } else { TEXT2() },
    );
    let sub = format!("{:04X}:{:04X} · UP {:04X} · IF {}", d.vid, d.pid, d.usage_page, d.interface);
    p.text(pos2(x, rect.top() + 31.0), Align2::LEFT_CENTER, sub, MONO(10.5), TEXT3());
    if live {
        p.text(rect.right_center() - vec2(10.0, 0.0), Align2::RIGHT_CENTER, "LIVE", FontId::new(10.0, mono_bold()), OK());
    }
    if hov {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.add_space(2.0);
    resp.on_hover_text(&d.path)
}
