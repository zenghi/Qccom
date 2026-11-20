//! 串口模块的固件：WiFi（ESP-01 AT）、蓝牙（HC-05 / JDY-31 / HM-10）、4G（Air780E / EC800M / ML307）、NB-IoT、GPS。
//! 收到单片机或 USB 转 TTL 发来的 AT 指令，按真实模块的格式回应；联网指令交给界面层真的去连（QCCOM 自己的 TCP / MQTT / HTTP 服务器或外网）。
//! 蓝牙模块的另一头是「手机」：在 QCCOM 串口列表里出现一个仿真串口，打开它就相当于手机连上了。

use super::Part;

/// 交给界面层的联网请求
#[derive(Clone, Debug, PartialEq)]
pub enum NetCmd {
    Open { link: u8, udp: bool, host: String, port: u16 },
    Send { link: u8, data: Vec<u8> },
    Close { link: u8 },
    Http { method: String, url: String, body: Vec<u8> },
    MqttOpen { host: String, port: u16, client: String, user: String, pass: String },
    MqttPub { topic: String, payload: Vec<u8> },
    MqttSub { topic: String },
    MqttClose,
}

/// 联网结果
#[derive(Clone, Debug)]
pub enum NetEvt {
    Opened {
        link: u8,
        ok: bool,
    },
    Data {
        link: u8,
        data: Vec<u8>,
    },
    Closed {
        link: u8,
    },
    /// status 为 0 表示没连上
    Http {
        status: u16,
        body: Vec<u8>,
    },
    MqttOpened {
        ok: bool,
    },
    MqttMsg {
        topic: String,
        payload: Vec<u8>,
    },
    MqttClosed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fam {
    Esp,
    Hc05,
    Jdy,
    Hm10,
    Luat,
    Quectel,
    Cmcc,
    Gps,
}

/// 等待「>」之后的原始数据
#[derive(Clone, Debug)]
enum Prompt {
    /// len 为 0 表示用 Ctrl+Z 结束
    Send {
        link: u8,
        len: usize,
    },
    MqttPub {
        topic: String,
        len: usize,
    },
    HttpUrl {
        len: usize,
    },
    HttpBody {
        len: usize,
        post: bool,
    },
}

#[derive(Clone, Debug)]
pub struct AtDev {
    fam: Fam,
    pub model: String,
    /// 模块串口的波特率（和单片机那边要一致）
    pub baud: u32,
    line: Vec<u8>,
    last_rx: f64,
    /// (发送时间, 数据)
    queue: Vec<(f64, Vec<u8>)>,
    pub net: Vec<NetCmd>,
    /// 给「手机」的数据（蓝牙模块）
    pub phone_out: Vec<u8>,
    prompt: Option<(Prompt, Vec<u8>)>,
    echo: bool,
    pub powered: bool,
    power_at: f64,
    /// HC-05：上电时 EN 为高进入 AT 模式（38400）
    pub at_mode: bool,
    pub phone: bool,
    /// ESP：透传模式
    transparent: bool,
    plus_count: u8,
    wifi: Option<String>,
    mux: bool,
    ip_head: bool,
    links: [bool; 6],
    mqtt_on: bool,
    mqtt_cfg: (String, String, String),
    mqtt_host: Option<(String, u16)>,
    http_url: String,
    http_body: Vec<u8>,
    http_resp: Option<(u16, Vec<u8>)>,
    http_post: bool,
    name: String,
    data_baud: u32,
    gps_next: f64,
    csq: u8,
    sim: bool,
    gps_fix: bool,
    /// WiFi 连上（拿到 IP）的时间
    wifi_at: f64,
    /// 指令以 CR 结束时，紧跟的 LF 不算数据
    skip_lf: bool,
}

const OK: &str = "\r\nOK\r\n";
const ERR: &str = "\r\nERROR\r\n";

fn fam_of(kind: &str, model: &str) -> Option<Fam> {
    Some(match kind {
        "esp01" => Fam::Esp,
        "hc05" => Fam::Hc05,
        "jdy31" => Fam::Jdy,
        "hm10" => Fam::Hm10,
        "cat1" => match model {
            "EC800M" => Fam::Quectel,
            "ML307" => Fam::Cmcc,
            _ => Fam::Luat,
        },
        "nbiot" => Fam::Quectel,
        "gps" => Fam::Gps,
        _ => return None,
    })
}

pub fn is_at(kind: &str) -> bool {
    fam_of(kind, "").is_some()
}

/// 蓝牙模块（另一头是手机，在 QCCOM 里显示成仿真串口）
pub fn is_bt(kind: &str) -> bool {
    matches!(kind, "hc05" | "jdy31" | "hm10")
}

/// 拆参数：逗号分隔，引号里的逗号不算；返回 (值, 是否带引号)
fn args(s: &str) -> Vec<(String, bool)> {
    let mut out = vec![];
    let mut cur = String::new();
    let (mut q, mut quoted) = (false, false);
    for c in s.chars() {
        match c {
            '"' => {
                q = !q;
                quoted = true;
            }
            ',' if !q => {
                out.push((cur.trim().to_string(), quoted));
                cur.clear();
                quoted = false;
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() || quoted || !out.is_empty() {
        out.push((if quoted { cur } else { cur.trim().to_string() }, quoted));
    }
    out
}

fn arg(a: &[(String, bool)], i: usize) -> &str {
    a.get(i).map_or("", |x| x.0.as_str())
}

fn num(a: &[(String, bool)], i: usize) -> Option<i64> {
    a.get(i).and_then(|x| x.0.trim().parse().ok())
}

impl AtDev {
    pub fn new(part: &Part) -> AtDev {
        let model = if part.kind == "cat1" { part.prop("model").to_string() } else { String::new() };
        let fam = fam_of(&part.kind, &model).unwrap_or(Fam::Esp);
        let name = match fam {
            Fam::Hc05 => {
                let n = part.prop("name");
                if n.is_empty() {
                    "HC-05".into()
                } else {
                    n.to_string()
                }
            }
            Fam::Jdy => "JDY-31-SPP".into(),
            Fam::Hm10 => "HMSoft".into(),
            _ => String::new(),
        };
        let mut d = AtDev {
            fam,
            model,
            baud: 9600,
            line: vec![],
            last_rx: 0.0,
            queue: vec![],
            net: vec![],
            phone_out: vec![],
            prompt: None,
            echo: true,
            powered: false,
            power_at: 0.0,
            at_mode: false,
            phone: false,
            transparent: false,
            plus_count: 0,
            wifi: None,
            mux: false,
            ip_head: false,
            links: [false; 6],
            mqtt_on: false,
            mqtt_cfg: Default::default(),
            mqtt_host: None,
            http_url: String::new(),
            http_body: vec![],
            http_resp: None,
            http_post: false,
            name,
            data_baud: 9600,
            gps_next: 0.0,
            csq: 24,
            sim: true,
            gps_fix: true,
            wifi_at: 0.0,
            skip_lf: false,
        };
        d.reload(part);
        d.baud = d.default_baud();
        d
    }

    /// 属性改了（信号强度、SIM 卡、定位）：不用重新上电
    pub fn reload(&mut self, part: &Part) {
        self.csq = part.num("csq").clamp(0.0, 31.0) as u8;
        self.sim = part.prop("sim") != "false";
        self.gps_fix = part.prop("fix") != "false";
        if part.kind == "cat1" && part.prop("model") != self.model {
            *self = AtDev::new(part);
        }
    }

    fn default_baud(&self) -> u32 {
        match self.fam {
            Fam::Esp | Fam::Luat | Fam::Quectel | Fam::Cmcc => 115200,
            Fam::Hc05 if self.at_mode => 38400,
            Fam::Hc05 => self.data_baud,
            _ => 9600,
        }
    }

    pub fn is_bt(&self) -> bool {
        matches!(self.fam, Fam::Hc05 | Fam::Jdy | Fam::Hm10)
    }

    fn say_at(&mut self, t: f64, s: impl AsRef<[u8]>) {
        let d = s.as_ref().to_vec();
        if d.is_empty() {
            return;
        }
        // 同一时间的接在后面，不打乱顺序
        let t = self.queue.last().map_or(t, |q| q.0.max(t));
        self.queue.push((t, d));
    }

    fn say(&mut self, now: f64, delay: f64, s: impl AsRef<[u8]>) {
        self.say_at(now + delay, s);
    }

    /// 上电 / 断电（EN 只在上电那一刻看）
    pub fn set_power(&mut self, on: bool, en_high: bool, t: f64) {
        if on == self.powered {
            return;
        }
        self.powered = on;
        self.queue.clear();
        self.line.clear();
        self.prompt = None;
        if !on {
            self.reset_net();
            return;
        }
        self.power_at = t;
        self.at_mode = self.fam == Fam::Hc05 && en_high;
        self.echo = !matches!(self.fam, Fam::Hc05 | Fam::Jdy | Fam::Hm10 | Fam::Gps);
        self.baud = self.default_baud();
        self.gps_next = t + 1.0;
        match self.fam {
            Fam::Esp => self.say(t, 0.3, "\r\nready\r\n"),
            Fam::Luat => {
                self.say(t, 2.0, "\r\nRDY\r\n");
                if self.sim {
                    self.say(t, 2.5, "\r\n+CPIN: READY\r\n");
                    if self.csq > 0 {
                        self.say(t, 3.5, "\r\nSMS READY\r\n");
                    }
                } else {
                    self.say(t, 2.5, "\r\n+CPIN: NOT INSERTED\r\n");
                }
            }
            Fam::Quectel => {
                self.say(t, 2.0, "\r\nRDY\r\n");
                self.say(t, 2.5, if self.sim { "\r\n+CPIN: READY\r\n" } else { "\r\n+CPIN: NOT READY\r\n" });
            }
            Fam::Cmcc => {
                self.say(t, 2.0, "\r\n+MATREADY\r\n");
                self.say(t, 2.5, if self.sim { "\r\n+CPIN: READY\r\n" } else { "\r\n+CME ERROR: 10\r\n" });
            }
            _ => {}
        }
    }

    fn reset_net(&mut self) {
        for (i, on) in self.links.iter_mut().enumerate() {
            if *on {
                self.net.push(NetCmd::Close { link: i as u8 });
                *on = false;
            }
        }
        if self.mqtt_on || self.mqtt_host.is_some() {
            self.net.push(NetCmd::MqttClose);
        }
        self.mqtt_on = false;
        self.mqtt_host = None;
        self.wifi = None;
        self.transparent = false;
    }

    /// 4G：开机 2.5 秒后注册上网络（要插卡、有信号）
    fn registered(&self, t: f64) -> bool {
        self.sim && self.csq > 0 && t - self.power_at >= 2.5
    }

    /// 蓝牙：手机连上 / 断开（QCCOM 打开 / 关闭仿真串口）
    pub fn set_phone(&mut self, on: bool, t: f64) {
        if on == self.phone {
            return;
        }
        self.phone = on;
        if self.powered && self.fam == Fam::Hm10 {
            self.say(t, 0.0, if on { "OK+CONN" } else { "OK+LOST" });
        }
    }

    /// 手机发来数据
    pub fn phone_in(&mut self, d: &[u8], t: f64) {
        if self.powered && self.phone && !self.at_mode {
            self.say(t, 0.005, d);
        }
    }

    /// 到时间该发出去的字节
    pub fn poll(&mut self, t: f64) -> Vec<u8> {
        let mut out = vec![];
        if !self.powered {
            return out;
        }
        if self.fam == Fam::Gps && t >= self.gps_next {
            self.gps_next = t.max(self.gps_next + 1.0);
            let s = self.nmea();
            self.say_at(t, s);
        }
        // HM-10 的指令没有换行：停顿 50 ms 就算一条
        if self.fam == Fam::Hm10 && !self.line.is_empty() && t - self.last_rx > 0.05 {
            let l = String::from_utf8_lossy(&std::mem::take(&mut self.line)).to_string();
            self.hm10(l.trim(), t);
        }
        let n = self.queue.iter().take_while(|q| q.0 <= t).count();
        for (_, d) in self.queue.drain(..n) {
            out.extend(d);
        }
        out
    }

    /// 单片机发来的字节
    pub fn feed(&mut self, data: &[u8], t: f64) {
        if !self.powered {
            return;
        }
        self.last_rx = t;
        for &b in data {
            if std::mem::take(&mut self.skip_lf) && b == b'\n' {
                continue;
            }
            if let Some((p, buf)) = &mut self.prompt {
                let len = match p {
                    Prompt::Send { len, .. } | Prompt::MqttPub { len, .. } | Prompt::HttpUrl { len } | Prompt::HttpBody { len, .. } => *len,
                };
                if len == 0 && b == 0x1A {
                    let (p, buf) = self.prompt.take().unwrap();
                    self.prompt_done(p, buf, t);
                    continue;
                }
                if len == 0 && b == 0x1B {
                    self.prompt = None;
                    continue;
                }
                buf.push(b);
                if len > 0 && buf.len() >= len {
                    let (p, buf) = self.prompt.take().unwrap();
                    self.prompt_done(p, buf, t);
                }
                continue;
            }
            if self.transparent {
                // 透传：单独发 +++ 退出
                if b == b'+' {
                    self.plus_count += 1;
                    if self.plus_count == 3 {
                        self.transparent = false;
                        self.plus_count = 0;
                    }
                    continue;
                }
                let mut d = vec![b'+'; self.plus_count as usize];
                d.push(b);
                self.plus_count = 0;
                self.net.push(NetCmd::Send { link: 0, data: d });
                continue;
            }
            match self.fam {
                Fam::Gps => {}
                Fam::Hm10 => {
                    if self.phone {
                        self.phone_out.push(b);
                    } else {
                        self.line.push(b);
                    }
                }
                Fam::Hc05 | Fam::Jdy if !self.at_mode && (self.phone || self.fam == Fam::Hc05) => {
                    // 数据模式：直接给手机（没连上就丢了）
                    if self.phone {
                        self.phone_out.push(b);
                    }
                }
                _ => {
                    if b == b'\r' || b == b'\n' {
                        self.skip_lf = b == b'\r';
                        if !self.line.is_empty() {
                            let l = String::from_utf8_lossy(&std::mem::take(&mut self.line)).to_string();
                            if self.echo {
                                self.say(t, 0.0, format!("{l}\r\n"));
                            }
                            self.command(l.trim(), t);
                        }
                    } else if self.line.len() < 2048 {
                        self.line.push(b);
                    }
                }
            }
        }
    }

    fn prompt_done(&mut self, p: Prompt, buf: Vec<u8>, t: f64) {
        match p {
            Prompt::Send { link, .. } => {
                let n = buf.len();
                if !self.links[link as usize] {
                    self.say(t, 0.01, if self.fam == Fam::Esp { "\r\nSEND FAIL\r\n" } else { ERR });
                    return;
                }
                self.net.push(NetCmd::Send { link, data: buf });
                match self.fam {
                    Fam::Esp => self.say(t, 0.02, format!("\r\nRecv {n} bytes\r\n\r\nSEND OK\r\n")),
                    Fam::Cmcc => self.say(t, 0.02, format!("{OK}\r\n+MIPSEND: {link},{n}\r\n")),
                    Fam::Luat if self.mux => self.say(t, 0.02, format!("\r\n{link}, SEND OK\r\n")),
                    _ => self.say(t, 0.02, "\r\nSEND OK\r\n"),
                }
            }
            Prompt::MqttPub { topic, .. } => {
                if !self.mqtt_on {
                    self.say(t, 0.01, ERR);
                    return;
                }
                self.net.push(NetCmd::MqttPub { topic, payload: buf });
                self.say(t, 0.02, format!("{OK}\r\n+QMTPUBEX: 0,0,0\r\n"));
            }
            Prompt::HttpUrl { .. } => {
                self.http_url = String::from_utf8_lossy(&buf).trim().to_string();
                self.say(t, 0.01, OK);
            }
            Prompt::HttpBody { post, .. } => {
                self.http_body = buf;
                if post {
                    // EC800M：QHTTPPOST 收完数据就发
                    self.http_post = true;
                    self.say(t, 0.0, OK);
                    self.http_go(t);
                } else {
                    self.say(t, 0.01, OK);
                }
            }
        }
    }

    fn prompt(&mut self, p: Prompt, t: f64, text: &str) {
        self.prompt = Some((p, vec![]));
        self.say(t, 0.005, text);
    }

    fn http_go(&mut self, _t: f64) {
        let mut url = self.http_url.clone();
        if !url.contains("://") {
            url = format!("http://{url}");
        }
        let method = if self.http_post { "POST" } else { "GET" };
        self.net.push(NetCmd::Http {
            method: method.into(),
            url,
            body: std::mem::take(&mut self.http_body),
        });
    }

    // ───────────────────────── 指令

    fn command(&mut self, l: &str, t: f64) {
        let up = l.to_ascii_uppercase();
        if !up.starts_with("AT") {
            if !l.is_empty() && !matches!(self.fam, Fam::Hc05 | Fam::Jdy) {
                self.say(t, 0.005, ERR);
            }
            return;
        }
        // 名字和参数：AT+XXX=a,b / AT+XXX? / AT+XXX
        let body = &l[2..];
        let (name, rest) = match body.find(['=', '?']) {
            Some(i) => (body[..i].to_ascii_uppercase(), &body[i..]),
            None => (body.to_ascii_uppercase(), ""),
        };
        let query = rest.starts_with('?') || rest == "=?";
        let a = if let Some(r) = rest.strip_prefix('=') { args(r) } else { vec![] };
        let handled = match self.fam {
            Fam::Esp => self.esp(&name, query, &a, t),
            Fam::Hc05 => self.hc05(&name, query, &a, t),
            Fam::Jdy => self.jdy(&name, rest, t),
            Fam::Luat | Fam::Quectel | Fam::Cmcc => self.cell(&name, query, &a, t),
            _ => false,
        };
        if !handled {
            self.say(t, 0.005, ERR);
        }
    }

    fn common(&mut self, name: &str, t: f64) -> bool {
        match name {
            "" => self.say(t, 0.005, OK),
            "E0" => {
                self.echo = false;
                self.say(t, 0.005, OK);
            }
            "E1" => {
                self.echo = true;
                self.say(t, 0.005, OK);
            }
            _ => return false,
        }
        true
    }

    fn esp(&mut self, name: &str, query: bool, a: &[(String, bool)], t: f64) -> bool {
        if self.common(name, t) {
            return true;
        }
        let net_ok = self.wifi.is_some() && t >= self.wifi_at;
        match name {
            "+RST" => {
                self.say(t, 0.005, OK);
                self.reset_net();
                self.say(t, 0.5, "\r\nready\r\n");
            }
            "+GMR" => self.say(
                t,
                0.005,
                format!("AT version:1.7.6.0(Jan 24 2022 08:56:02)\r\nSDK version:3.0.6-dev(4a7a3e5)\r\ncompile time:Feb 18 2022 18:06:12\r\nBin version(Wroom 02):1.7.6\r\n{OK}"),
            ),
            "+CWMODE" | "+CWMODE_CUR" | "+CWMODE_DEF" => {
                if query {
                    self.say(t, 0.005, format!("+CWMODE:1\r\n{OK}"));
                } else {
                    self.say(t, 0.005, OK);
                }
            }
            "+CWJAP" | "+CWJAP_CUR" | "+CWJAP_DEF" => {
                if query {
                    match self.wifi.clone() {
                        Some(s) => self.say(t, 0.005, format!("+CWJAP:\"{s}\",\"a4:5e:60:c1:22:08\",6,-52\r\n{OK}")),
                        None => self.say(t, 0.005, format!("No AP\r\n{OK}")),
                    }
                } else {
                    let ssid = arg(a, 0).to_string();
                    if self.wifi.is_some() {
                        self.say(t, 0.0, "WIFI DISCONNECT\r\n");
                    }
                    self.wifi = None;
                    if ssid.is_empty() {
                        self.say(t, 1.0, "+CWJAP:3\r\n\r\nFAIL\r\n");
                    } else {
                        self.wifi_at = t + 2.0;
                        self.say(t, 1.2, "WIFI CONNECTED\r\n");
                        self.say(t, 2.0, format!("WIFI GOT IP\r\n{OK}"));
                        self.wifi = Some(ssid);
                    }
                }
            }
            "+CWQAP" => {
                self.say(t, 0.005, OK);
                if self.wifi.take().is_some() {
                    self.say(t, 0.05, "WIFI DISCONNECT\r\n");
                }
            }
            "+CWLAP" => self.say(
                t,
                2.0,
                format!(
                    "+CWLAP:(3,\"QCCOM-Lab\",-48,\"a4:5e:60:c1:22:08\",6)\r\n+CWLAP:(4,\"ChinaNet-5G\",-67,\"d8:32:14:aa:01:7c\",11)\r\n+CWLAP:(3,\"MERCURY_2F\",-79,\"50:bd:5f:13:9a:40\",1)\r\n{OK}"
                ),
            ),
            "+CIFSR" => {
                let ip = if net_ok { "192.168.1.88" } else { "0.0.0.0" };
                self.say(t, 0.005, format!("+CIFSR:STAIP,\"{ip}\"\r\n+CIFSR:STAMAC,\"5c:cf:7f:12:34:56\"\r\n{OK}"));
            }
            "+CIPSTA" | "+CIPSTA_CUR" => self.say(
                t,
                0.005,
                format!("+CIPSTA:ip:\"192.168.1.88\"\r\n+CIPSTA:gateway:\"192.168.1.1\"\r\n+CIPSTA:netmask:\"255.255.255.0\"\r\n{OK}"),
            ),
            "+CIPMUX" => {
                if query {
                    self.say(t, 0.005, format!("+CIPMUX:{}\r\n{OK}", self.mux as u8));
                } else {
                    self.mux = num(a, 0) == Some(1);
                    self.say(t, 0.005, OK);
                }
            }
            "+CIPMODE" => {
                self.ip_head = false;
                self.say(t, 0.005, OK);
                // 记住透传模式：CIPSEND 不带长度时进入
                self.http_post = num(a, 0) == Some(1);
            }
            "+CIPSTART" => {
                if !net_ok {
                    self.say(t, 0.01, format!("{ERR}"));
                    return true;
                }
                let (link, k) = if self.mux { (num(a, 0).unwrap_or(0).clamp(0, 4) as u8, 1) } else { (0, 0) };
                let proto = arg(a, k).to_ascii_uppercase();
                let host = arg(a, k + 1).to_string();
                let port = num(a, k + 2).unwrap_or(0) as u16;
                if host.is_empty() || port == 0 || !matches!(proto.as_str(), "TCP" | "UDP" | "SSL") {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                if self.links[link as usize] {
                    self.say(t, 0.005, format!("ALREADY CONNECTED\r\n{ERR}"));
                    return true;
                }
                self.net.push(NetCmd::Open {
                    link,
                    udp: proto == "UDP",
                    host,
                    port,
                });
            }
            "+CIPSEND" | "+CIPSENDEX" => {
                let (link, len) = if self.mux { (num(a, 0).unwrap_or(0).clamp(0, 4) as u8, num(a, 1)) } else { (0, num(a, 0)) };
                if !self.links[link as usize] {
                    self.say(t, 0.005, format!("link is not valid\r\n{ERR}"));
                    return true;
                }
                match len {
                    Some(n) if n > 0 && n <= 2048 => self.prompt(Prompt::Send { link, len: n as usize }, t, format!("{OK}> ").as_str()),
                    None if self.http_post && !self.mux => {
                        self.transparent = true;
                        self.say(t, 0.005, format!("{OK}\r\n>"));
                    }
                    _ => self.say(t, 0.005, ERR),
                }
            }
            "+CIPCLOSE" => {
                let link = if self.mux { num(a, 0).unwrap_or(0).clamp(0, 4) as u8 } else { 0 };
                if self.links[link as usize] {
                    self.links[link as usize] = false;
                    self.net.push(NetCmd::Close { link });
                    let p = if self.mux { format!("{link},") } else { String::new() };
                    self.say(t, 0.01, format!("{p}CLOSED\r\n{OK}"));
                } else {
                    self.say(t, 0.005, ERR);
                }
            }
            "+CIPSTATUS" => {
                let st = if self.links.iter().any(|x| *x) {
                    3
                } else if net_ok {
                    2
                } else {
                    5
                };
                self.say(t, 0.005, format!("STATUS:{st}\r\n{OK}"));
            }
            "+CIPDINFO" | "+CIPRECVMODE" | "+CWDHCP" | "+CWAUTOCONN" | "+SYSSTORE" | "+UART_CUR" | "+UART_DEF" | "+CIPSERVER" | "+MQTTCLIENTID" | "+MQTTUSERNAME" | "+MQTTPASSWORD" => {
                self.say(t, 0.005, OK)
            }
            "+MQTTUSERCFG" => {
                self.mqtt_cfg = (arg(a, 2).to_string(), arg(a, 3).to_string(), arg(a, 4).to_string());
                self.say(t, 0.005, OK);
            }
            "+MQTTCONN" => {
                if query {
                    let s = match &self.mqtt_host {
                        Some((h, p)) if self.mqtt_on => format!("+MQTTCONN:0,4,0,\"{h}\",\"{p}\",\"\",1\r\n"),
                        _ => "+MQTTCONN:0,0,0,\"\",\"\",\"\",0\r\n".into(),
                    };
                    self.say(t, 0.005, format!("{s}{OK}"));
                    return true;
                }
                if !net_ok {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                let (host, port) = (arg(a, 1).to_string(), num(a, 2).unwrap_or(1883) as u16);
                self.mqtt_open(host, port, t);
            }
            "+MQTTPUB" | "+MQTTPUBRAW" => {
                if !self.mqtt_on {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                let topic = arg(a, 1).to_string();
                if name == "+MQTTPUBRAW" {
                    let n = num(a, 2).unwrap_or(0).clamp(1, 1024) as usize;
                    self.prompt = Some((Prompt::MqttPub { topic, len: n }, vec![]));
                    self.say(t, 0.005, format!("{OK}\r\n>"));
                    return true;
                }
                self.net.push(NetCmd::MqttPub {
                    topic,
                    payload: arg(a, 2).as_bytes().to_vec(),
                });
                self.say(t, 0.02, OK);
            }
            "+MQTTSUB" => {
                if query || !self.mqtt_on {
                    self.say(t, 0.005, if self.mqtt_on { OK } else { ERR });
                    return true;
                }
                self.net.push(NetCmd::MqttSub { topic: arg(a, 1).to_string() });
                self.say(t, 0.03, OK);
            }
            "+MQTTCLEAN" => {
                if self.mqtt_on || self.mqtt_host.is_some() {
                    self.net.push(NetCmd::MqttClose);
                }
                self.mqtt_on = false;
                self.mqtt_host = None;
                self.say(t, 0.01, OK);
            }
            "+HTTPCLIENT" => {
                if !net_ok {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                let opt = num(a, 0).unwrap_or(2);
                self.http_post = opt == 3;
                self.http_url = arg(a, 2).to_string();
                if self.http_url.is_empty() {
                    let (host, path) = (arg(a, 3), arg(a, 4));
                    self.http_url = format!("http://{host}{path}");
                }
                self.http_body = arg(a, 6).as_bytes().to_vec();
                self.http_go(t);
            }
            _ => return false,
        }
        true
    }

    fn mqtt_open(&mut self, host: String, port: u16, _t: f64) {
        let (c, u, p) = self.mqtt_cfg.clone();
        let client = if c.is_empty() { format!("qccom-sim-{}", self.model.to_ascii_lowercase()) } else { c };
        self.mqtt_host = Some((host.clone(), port));
        self.net.push(NetCmd::MqttOpen { host, port, client, user: u, pass: p });
    }

    fn hc05(&mut self, name: &str, query: bool, a: &[(String, bool)], t: f64) -> bool {
        if !self.at_mode {
            return true;
        }
        match name {
            "" => self.say(t, 0.005, "OK\r\n"),
            "+NAME" if query => self.say(t, 0.005, format!("+NAME:{}\r\nOK\r\n", self.name)),
            "+NAME" => {
                self.name = arg(a, 0).to_string();
                self.say(t, 0.005, "OK\r\n");
            }
            "+VERSION" => self.say(t, 0.005, "+VERSION:2.0-20100601\r\nOK\r\n"),
            "+ADDR" => self.say(t, 0.005, "+ADDR:98d3:31:fb1234\r\nOK\r\n"),
            "+ROLE" if query => self.say(t, 0.005, "+ROLE:0\r\nOK\r\n"),
            "+PSWD" | "+PIN" if query => self.say(t, 0.005, "+PSWD:1234\r\nOK\r\n"),
            "+UART" if query => self.say(t, 0.005, format!("+UART:{},0,0\r\nOK\r\n", self.data_baud)),
            "+UART" => match num(a, 0) {
                Some(b) if [4800, 9600, 19200, 38400, 57600, 115200].contains(&b) => {
                    self.data_baud = b as u32;
                    self.say(t, 0.005, "OK\r\n");
                }
                _ => self.say(t, 0.005, "ERROR:(1D)\r\n"),
            },
            "+STATE" => self.say(t, 0.005, format!("+STATE:{}\r\nOK\r\n", if self.phone { "CONNECTED" } else { "INITIALIZED" })),
            "+ORGL" => {
                self.name = "HC-05".into();
                self.data_baud = 38400;
                self.say(t, 0.005, "OK\r\n");
            }
            "+RESET" => {
                self.say(t, 0.005, "OK\r\n");
                // 复位后回到数据模式
                self.at_mode = false;
                self.baud = self.data_baud;
            }
            "+ROLE" | "+PSWD" | "+PIN" | "+CMODE" | "+INIT" | "+CLASS" | "+IAC" => self.say(t, 0.005, "OK\r\n"),
            _ => self.say(t, 0.005, "ERROR:(0)\r\n"),
        }
        true
    }

    fn jdy(&mut self, name: &str, rest: &str, t: f64) -> bool {
        // JDY-31：没连手机时收 AT 指令，格式是 AT+NAMExxx（不带等号）
        let full = format!("{name}{rest}");
        let reply = |s: &str| format!("{s}\r\n");
        if full.is_empty() {
            self.say(t, 0.005, reply("OK"));
        } else if full == "+VERSION" {
            self.say(t, 0.005, reply("+VERSION=JDY-31-V1.35,Bluetooth V3.0"));
        } else if full == "+LADDR" {
            self.say(t, 0.005, reply("+LADDR=3C:5A:B4:01:02:03"));
        } else if full == "+NAME" {
            let n = self.name.clone();
            self.say(t, 0.005, reply(&format!("+NAME={n}")));
        } else if let Some(n) = full.strip_prefix("+NAME") {
            self.name = n.trim_start_matches('=').to_string();
            self.say(t, 0.005, reply("+OK"));
        } else if full == "+BAUD" {
            self.say(t, 0.005, reply("+BAUD=4"));
        } else if full == "+PIN" {
            self.say(t, 0.005, reply("+PIN=1234"));
        } else if full.starts_with("+BAUD") || full.starts_with("+PIN") || full == "+RESET" || full == "+DISC" {
            self.say(t, 0.005, reply("+OK"));
        } else {
            self.say(t, 0.005, reply("+ERR"));
        }
        true
    }

    fn hm10(&mut self, l: &str, t: f64) {
        let up = l.to_ascii_uppercase();
        let r = if up == "AT" {
            "OK".to_string()
        } else if up == "AT+NAME?" {
            format!("OK+NAME:{}", self.name)
        } else if up.starts_with("AT+NAME") {
            self.name = l[7..].to_string();
            format!("OK+Set:{}", self.name)
        } else if up == "AT+BAUD?" {
            "OK+Get:0".into()
        } else if up == "AT+ADDR?" {
            "OK+ADDR:D43639A1B2C3".into()
        } else if up == "AT+VERS?" || up == "AT+VERR?" {
            "HMSoft V540".into()
        } else if up == "AT+ROLE?" {
            "OK+Get:0".into()
        } else if up == "AT+RESET" {
            "OK+RESET".into()
        } else if let Some(v) = up.strip_prefix("AT+") {
            let k = v.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(v.len());
            format!("OK+Set:{}", &v[k..])
        } else {
            return;
        };
        self.say(t, 0.01, r);
    }

    /// 4G / NB-IoT：通用指令 + 各家的联网指令
    fn cell(&mut self, name: &str, query: bool, a: &[(String, bool)], t: f64) -> bool {
        if self.common(name, t) {
            return true;
        }
        let reg = self.registered(t);
        let imei = "866123045678901";
        let iccid = "89860321245890123456";
        match name {
            "I" | "+CGMR" | "+GMR" => {
                let s = match self.fam {
                    Fam::Luat => "AirM2M_780E_V1159_LTE_AT".to_string(),
                    Fam::Cmcc => "ML307R-DC_V2.0.0".to_string(),
                    _ if self.model.is_empty() => "Quectel\r\nBC260Y-CN\r\nRevision: BC260YCNAAR02A06".to_string(),
                    _ => "Quectel\r\nEC800M\r\nRevision: EC800MCNGAR06A04M08".to_string(),
                };
                self.say(t, 0.005, format!("{s}\r\n{OK}"));
            }
            "+CGSN" | "+GSN" => self.say(t, 0.005, format!("{imei}\r\n{OK}")),
            "+CIMI" => {
                if self.sim {
                    self.say(t, 0.005, format!("460081234567890\r\n{OK}"));
                } else {
                    self.say(t, 0.005, "\r\n+CME ERROR: 10\r\n");
                }
            }
            "+ICCID" | "+CCID" | "+QCCID" | "+MCCID" => {
                if self.sim {
                    let p = if name == "+CCID" { String::new() } else { format!("{name}: ") };
                    self.say(t, 0.005, format!("{p}{iccid}\r\n{OK}"));
                } else {
                    self.say(t, 0.005, "\r\n+CME ERROR: 10\r\n");
                }
            }
            "+CPIN" => self.say(t, 0.005, if self.sim { format!("+CPIN: READY\r\n{OK}") } else { "\r\n+CME ERROR: 10\r\n".into() }),
            "+CSQ" => {
                let q = if self.sim { self.csq } else { 99 };
                self.say(t, 0.005, format!("+CSQ: {q},99\r\n{OK}"));
            }
            "+CREG" | "+CEREG" | "+CGREG" if query => {
                let s = if reg { 1 } else { 2 };
                self.say(t, 0.005, format!("{name}: 0,{s}\r\n{OK}"));
            }
            "+CGATT" if query => self.say(t, 0.005, format!("+CGATT: {}\r\n{OK}", reg as u8)),
            "+COPS" if query => {
                if reg {
                    self.say(t, 0.005, format!("+COPS: 0,0,\"CHINA MOBILE\",7\r\n{OK}"));
                } else {
                    self.say(t, 0.005, format!("+COPS: 0\r\n{OK}"));
                }
            }
            "+CFUN" if query => self.say(t, 0.005, format!("+CFUN: 1\r\n{OK}")),
            "+CREG" | "+CEREG" | "+CGREG" | "+CGATT" | "+CFUN" | "+CMEE" | "+IPR" | "+CGDCONT" | "+CMGF" | "+CNMI" | "+QSCLK" | "+CPSMS" | "+QICFG" | "+QMTCFG" | "+QHTTPCFG" | "+MQTTCFG"
            | "+MIPCFG" | "+MHTTPCFG" | "+CSCLK" => self.say(t, 0.005, OK),
            _ => {
                return match self.fam {
                    Fam::Luat => self.luat(name, query, a, t, reg),
                    Fam::Cmcc => self.cmcc(name, query, a, t, reg),
                    _ => self.quectel(name, query, a, t, reg),
                };
            }
        }
        true
    }

    fn luat(&mut self, name: &str, query: bool, a: &[(String, bool)], t: f64, reg: bool) -> bool {
        match name {
            "+CSTT" | "+CIPHEAD" | "+CIPQSEND" | "+CIPRXGET" | "+CIPSHOWTP" | "+CIPSRIP" | "+HTTPINIT" | "+HTTPTERM" | "+SAPBR" => {
                if name == "+CIPHEAD" {
                    self.ip_head = num(a, 0) == Some(1);
                }
                self.say(t, 0.005, OK);
            }
            "+CIPMUX" => {
                if query {
                    self.say(t, 0.005, format!("+CIPMUX: {}\r\n{OK}", self.mux as u8));
                } else {
                    self.mux = num(a, 0) == Some(1);
                    self.say(t, 0.005, OK);
                }
            }
            "+CIICR" => self.say(t, if reg { 0.5 } else { 0.005 }, if reg { OK } else { ERR }),
            "+CIFSR" => self.say(t, 0.005, if reg { "\r\n10.88.12.34\r\n" } else { ERR }),
            "+CIPSTART" => {
                let (link, k) = if self.mux { (num(a, 0).unwrap_or(0).clamp(0, 5) as u8, 1) } else { (0, 0) };
                let (proto, host, port) = (arg(a, k).to_ascii_uppercase(), arg(a, k + 1).to_string(), num(a, k + 2).unwrap_or(0) as u16);
                if !reg || host.is_empty() || port == 0 {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                self.say(t, 0.005, OK);
                self.net.push(NetCmd::Open {
                    link,
                    udp: proto == "UDP",
                    host,
                    port,
                });
            }
            "+CIPSEND" => {
                let (link, len) = if self.mux { (num(a, 0).unwrap_or(0).clamp(0, 5) as u8, num(a, 1)) } else { (0, num(a, 0)) };
                if !self.links[link as usize] {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                self.prompt(
                    Prompt::Send {
                        link,
                        len: len.unwrap_or(0).clamp(0, 1460) as usize,
                    },
                    t,
                    "\r\n> ",
                );
            }
            "+CIPCLOSE" => {
                let link = if self.mux { num(a, 0).unwrap_or(0).clamp(0, 5) as u8 } else { 0 };
                if self.links[link as usize] {
                    self.links[link as usize] = false;
                    self.net.push(NetCmd::Close { link });
                    let p = if self.mux { format!("{link}, ") } else { String::new() };
                    self.say(t, 0.01, format!("\r\n{p}CLOSE OK\r\n"));
                } else {
                    self.say(t, 0.005, ERR);
                }
            }
            "+CIPSHUT" => {
                self.reset_links();
                self.say(t, 0.05, "\r\nSHUT OK\r\n");
            }
            "+CIPSTATUS" => {
                let s = if self.links[0] {
                    "CONNECT OK"
                } else if reg {
                    "IP GPRSACT"
                } else {
                    "IP INITIAL"
                };
                self.say(t, 0.005, format!("{OK}\r\nSTATE: {s}\r\n"));
            }
            "+MCONFIG" => {
                self.mqtt_cfg = (arg(a, 0).to_string(), arg(a, 1).to_string(), arg(a, 2).to_string());
                self.say(t, 0.005, OK);
            }
            "+MIPSTART" => {
                if !reg {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                self.mqtt_host = Some((arg(a, 0).to_string(), num(a, 1).unwrap_or(1883) as u16));
                self.say(t, 0.005, OK);
                self.say(t, 0.3, "\r\nCONNECT OK\r\n");
            }
            "+MCONNECT" => match self.mqtt_host.clone() {
                Some((h, p)) => {
                    self.say(t, 0.005, OK);
                    self.mqtt_open(h, p, t);
                }
                None => self.say(t, 0.005, ERR),
            },
            "+MPUB" => {
                if !self.mqtt_on {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                self.net.push(NetCmd::MqttPub {
                    topic: arg(a, 0).to_string(),
                    payload: arg(a, 3).as_bytes().to_vec(),
                });
                self.say(t, 0.02, OK);
            }
            "+MSUB" => {
                if !self.mqtt_on {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                self.net.push(NetCmd::MqttSub { topic: arg(a, 0).to_string() });
                self.say(t, 0.005, OK);
                self.say(t, 0.1, "\r\nSUBACK\r\n");
            }
            "+MDISCONNECT" | "+MIPCLOSE" => {
                if self.mqtt_on || self.mqtt_host.is_some() {
                    self.net.push(NetCmd::MqttClose);
                }
                self.mqtt_on = false;
                if name == "+MIPCLOSE" {
                    self.mqtt_host = None;
                }
                self.say(t, 0.005, OK);
            }
            "+MQTTSTATU" => self.say(t, 0.005, format!("+MQTTSTATU :{}\r\n{OK}", self.mqtt_on as u8)),
            "+HTTPPARA" => {
                if arg(a, 0).eq_ignore_ascii_case("URL") {
                    self.http_url = arg(a, 1).to_string();
                }
                self.say(t, 0.005, OK);
            }
            "+HTTPDATA" => {
                let n = num(a, 0).unwrap_or(0).clamp(1, 4096) as usize;
                self.prompt(Prompt::HttpBody { len: n, post: false }, t, "\r\nDOWNLOAD\r\n");
            }
            "+HTTPACTION" => {
                if !reg {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                self.http_post = num(a, 0) == Some(1);
                self.say(t, 0.005, OK);
                self.http_go(t);
            }
            "+HTTPREAD" => match self.http_resp.clone() {
                Some((_, body)) => {
                    let mut d = format!("\r\n+HTTPREAD: {}\r\n", body.len()).into_bytes();
                    d.extend(body);
                    d.extend(OK.as_bytes());
                    self.say(t, 0.01, d);
                }
                None => self.say(t, 0.005, ERR),
            },
            _ => return false,
        }
        true
    }

    fn reset_links(&mut self) {
        for i in 0..self.links.len() {
            if self.links[i] {
                self.links[i] = false;
                self.net.push(NetCmd::Close { link: i as u8 });
            }
        }
    }

    fn quectel(&mut self, name: &str, query: bool, a: &[(String, bool)], t: f64, reg: bool) -> bool {
        match name {
            "+QIACT" if query => {
                if reg {
                    self.say(t, 0.005, format!("+QIACT: 1,1,1,\"10.88.12.34\"\r\n{OK}"));
                } else {
                    self.say(t, 0.005, OK);
                }
            }
            "+QIACT" | "+QIDEACT" | "+QICSGP" => self.say(t, if reg { 0.3 } else { 0.005 }, if reg { OK } else { ERR }),
            "+QIOPEN" => {
                let (link, proto, host, port) = (
                    num(a, 1).unwrap_or(0).clamp(0, 5) as u8,
                    arg(a, 2).to_ascii_uppercase(),
                    arg(a, 3).to_string(),
                    num(a, 4).unwrap_or(0) as u16,
                );
                if host.is_empty() || port == 0 {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                self.say(t, 0.005, OK);
                if !reg {
                    self.say(t, 1.0, format!("\r\n+QIOPEN: {link},561\r\n"));
                    return true;
                }
                self.net.push(NetCmd::Open {
                    link,
                    udp: proto.starts_with("UDP"),
                    host,
                    port,
                });
            }
            "+QISEND" => {
                let link = num(a, 0).unwrap_or(0).clamp(0, 5) as u8;
                if !self.links[link as usize] {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                self.prompt(
                    Prompt::Send {
                        link,
                        len: num(a, 1).unwrap_or(0).clamp(0, 1460) as usize,
                    },
                    t,
                    "\r\n> ",
                );
            }
            "+QICLOSE" => {
                let link = num(a, 0).unwrap_or(0).clamp(0, 5) as u8;
                if self.links[link as usize] {
                    self.links[link as usize] = false;
                    self.net.push(NetCmd::Close { link });
                }
                self.say(t, 0.01, OK);
            }
            "+QISTATE" => {
                let mut s = String::new();
                for (i, on) in self.links.iter().enumerate() {
                    if *on {
                        s += &format!("+QISTATE: {i},\"TCP\",\"\",0,0,2,1,{i},0,\"uart1\"\r\n");
                    }
                }
                self.say(t, 0.005, format!("{s}{OK}"));
            }
            "+QMTOPEN" => {
                if query {
                    let s = match &self.mqtt_host {
                        Some((h, p)) => format!("+QMTOPEN: 0,\"{h}\",{p}\r\n"),
                        None => String::new(),
                    };
                    self.say(t, 0.005, format!("{s}{OK}"));
                    return true;
                }
                self.say(t, 0.005, OK);
                if !reg {
                    self.say(t, 1.0, "\r\n+QMTOPEN: 0,3\r\n");
                    return true;
                }
                self.mqtt_host = Some((arg(a, 1).to_string(), num(a, 2).unwrap_or(1883) as u16));
                self.say(t, 0.3, "\r\n+QMTOPEN: 0,0\r\n");
            }
            "+QMTCONN" => {
                if query {
                    self.say(t, 0.005, format!("+QMTCONN: 0,{}\r\n{OK}", if self.mqtt_on { 3 } else { 1 }));
                    return true;
                }
                let Some((h, p)) = self.mqtt_host.clone() else {
                    self.say(t, 0.005, ERR);
                    return true;
                };
                self.mqtt_cfg = (arg(a, 1).to_string(), arg(a, 2).to_string(), arg(a, 3).to_string());
                self.say(t, 0.005, OK);
                self.mqtt_open(h, p, t);
            }
            "+QMTSUB" => {
                if !self.mqtt_on {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                let id = num(a, 1).unwrap_or(1);
                self.net.push(NetCmd::MqttSub { topic: arg(a, 2).to_string() });
                self.say(t, 0.005, OK);
                self.say(t, 0.1, format!("\r\n+QMTSUB: 0,{id},0,0\r\n"));
            }
            "+QMTPUBEX" | "+QMTPUB" => {
                if !self.mqtt_on {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                let topic = arg(a, 4).to_string();
                // 第 6 个参数：带引号是内容，数字是长度（之后给 > 提示）
                match a.get(5) {
                    Some((s, true)) => {
                        self.net.push(NetCmd::MqttPub {
                            topic,
                            payload: s.as_bytes().to_vec(),
                        });
                        self.say(t, 0.02, format!("{OK}\r\n+QMTPUB: 0,0,0\r\n"));
                    }
                    Some((s, false)) => {
                        let n = s.parse::<usize>().unwrap_or(0).clamp(1, 1024);
                        self.prompt(Prompt::MqttPub { topic, len: n }, t, "\r\n> ");
                    }
                    None => self.prompt(Prompt::MqttPub { topic, len: 0 }, t, "\r\n> "),
                }
            }
            "+QMTDISC" | "+QMTCLOSE" => {
                if self.mqtt_on || self.mqtt_host.is_some() {
                    self.net.push(NetCmd::MqttClose);
                }
                self.mqtt_on = false;
                self.mqtt_host = None;
                self.say(t, 0.005, OK);
                self.say(t, 0.05, format!("\r\n{name}: 0,0\r\n"));
            }
            "+QHTTPURL" => {
                let n = num(a, 0).unwrap_or(0).clamp(1, 2048) as usize;
                self.prompt(Prompt::HttpUrl { len: n }, t, "\r\nCONNECT\r\n");
            }
            "+QHTTPGET" => {
                if !reg {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                self.http_post = false;
                self.say(t, 0.005, OK);
                self.http_go(t);
            }
            "+QHTTPPOST" => {
                if !reg {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                let n = num(a, 0).unwrap_or(0).clamp(1, 4096) as usize;
                self.prompt(Prompt::HttpBody { len: n, post: true }, t, "\r\nCONNECT\r\n");
            }
            "+QHTTPREAD" => match self.http_resp.clone() {
                Some((_, body)) => {
                    let mut d = b"\r\nCONNECT\r\n".to_vec();
                    d.extend(body);
                    d.extend(format!("{OK}\r\n+QHTTPREAD: 0\r\n").as_bytes());
                    self.say(t, 0.01, d);
                }
                None => self.say(t, 0.005, ERR),
            },
            _ => return false,
        }
        true
    }

    fn cmcc(&mut self, name: &str, query: bool, a: &[(String, bool)], t: f64, reg: bool) -> bool {
        match name {
            "+MIPCALL" if query => {
                if reg {
                    self.say(t, 0.005, format!("+MIPCALL: 1,1,\"10.88.12.34\"\r\n{OK}"));
                } else {
                    self.say(t, 0.005, format!("+MIPCALL: 0\r\n{OK}"));
                }
            }
            "+MIPCALL" => self.say(t, 0.3, if reg { OK } else { ERR }),
            "+MIPOPEN" => {
                let (link, proto, host, port) = (
                    num(a, 0).unwrap_or(0).clamp(0, 5) as u8,
                    arg(a, 1).to_ascii_uppercase(),
                    arg(a, 2).to_string(),
                    num(a, 3).unwrap_or(0) as u16,
                );
                if host.is_empty() || port == 0 {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                self.say(t, 0.005, OK);
                if !reg {
                    self.say(t, 1.0, format!("\r\n+MIPOPEN: {link},571\r\n"));
                    return true;
                }
                self.net.push(NetCmd::Open {
                    link,
                    udp: proto == "UDP",
                    host,
                    port,
                });
            }
            "+MIPSEND" => {
                let link = num(a, 0).unwrap_or(0).clamp(0, 5) as u8;
                if !self.links[link as usize] {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                match a.get(2) {
                    Some((s, _)) => {
                        let d = s.as_bytes().to_vec();
                        let n = d.len();
                        self.net.push(NetCmd::Send { link, data: d });
                        self.say(t, 0.02, format!("{OK}\r\n+MIPSEND: {link},{n}\r\n"));
                    }
                    None => self.prompt(
                        Prompt::Send {
                            link,
                            len: num(a, 1).unwrap_or(0).clamp(0, 1460) as usize,
                        },
                        t,
                        "\r\n>",
                    ),
                }
            }
            "+MIPCLOSE" => {
                let link = num(a, 0).unwrap_or(0).clamp(0, 5) as u8;
                if self.links[link as usize] {
                    self.links[link as usize] = false;
                    self.net.push(NetCmd::Close { link });
                }
                self.say(t, 0.005, OK);
                self.say(t, 0.02, format!("\r\n+MIPCLOSE: {link}\r\n"));
            }
            "+MIPSTATE" => {
                let link = num(a, 0).unwrap_or(0).clamp(0, 5) as usize;
                let s = if self.links[link] { "CONNECTED" } else { "INITIAL" };
                self.say(t, 0.005, format!("+MIPSTATE: {link},,,,\"{s}\"\r\n{OK}"));
            }
            "+MQTTCONN" => {
                if query {
                    self.say(t, 0.005, format!("+MQTTSTATE: {}\r\n{OK}", if self.mqtt_on { 2 } else { 0 }));
                    return true;
                }
                let (host, port) = (arg(a, 1).to_string(), num(a, 2).unwrap_or(1883) as u16);
                self.mqtt_cfg = (arg(a, 3).to_string(), arg(a, 4).to_string(), arg(a, 5).to_string());
                self.say(t, 0.005, OK);
                if !reg {
                    self.say(t, 1.0, "\r\n+MQTTURC: \"conn\",0,2\r\n");
                    return true;
                }
                self.mqtt_open(host, port, t);
            }
            "+MQTTSUB" => {
                if !self.mqtt_on {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                self.net.push(NetCmd::MqttSub { topic: arg(a, 1).to_string() });
                self.say(t, 0.005, OK);
                self.say(t, 0.1, "\r\n+MQTTURC: \"suback\",0,1,0\r\n");
            }
            "+MQTTPUB" => {
                if !self.mqtt_on {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                let topic = arg(a, 1).to_string();
                // AT+MQTTPUB=0,"topic",qos,retain,dup,len,"payload"；没有内容就等 > 之后的数据
                match a.get(6) {
                    Some((s, _)) => {
                        self.net.push(NetCmd::MqttPub {
                            topic,
                            payload: s.as_bytes().to_vec(),
                        });
                        self.say(t, 0.02, OK);
                    }
                    None => {
                        let n = num(a, 5).unwrap_or(0).clamp(0, 1024) as usize;
                        self.prompt(Prompt::MqttPub { topic, len: n }, t, "\r\n>");
                    }
                }
            }
            "+MQTTDISC" => {
                if self.mqtt_on {
                    self.net.push(NetCmd::MqttClose);
                }
                self.mqtt_on = false;
                self.say(t, 0.005, OK);
                self.say(t, 0.05, "\r\n+MQTTURC: \"conn\",0,1\r\n");
            }
            "+MHTTPCREATE" => {
                self.http_url = arg(a, 0).trim_end_matches('/').to_string();
                self.say(t, 0.005, format!("+MHTTPCREATE: 0\r\n{OK}"));
            }
            "+MHTTPCONTENT" => {
                self.http_body = arg(a, 3).as_bytes().to_vec();
                self.say(t, 0.005, OK);
            }
            "+MHTTPHEADER" | "+MHTTPDEL" => self.say(t, 0.005, OK),
            "+MHTTPREQUEST" => {
                if !reg || self.http_url.is_empty() {
                    self.say(t, 0.005, ERR);
                    return true;
                }
                self.http_post = num(a, 1) == Some(2);
                let path = arg(a, 3);
                let base = std::mem::take(&mut self.http_url);
                self.http_url = format!("{base}{}{path}", if path.starts_with('/') || path.is_empty() { "" } else { "/" });
                self.say(t, 0.005, OK);
                self.http_go(t);
                self.http_url = base;
            }
            _ => {
                let _ = query;
                return false;
            }
        }
        true
    }

    // ───────────────────────── 联网结果

    pub fn net_evt(&mut self, e: NetEvt, t: f64) {
        if !self.powered {
            return;
        }
        let mux = self.mux;
        match e {
            NetEvt::Opened { link, ok } => {
                let i = link as usize;
                if i >= self.links.len() {
                    return;
                }
                self.links[i] = ok;
                let s = match (self.fam, ok) {
                    (Fam::Esp, true) if mux => format!("{link},CONNECT\r\n{OK}"),
                    (Fam::Esp, true) => format!("CONNECT\r\n{OK}"),
                    (Fam::Esp, false) => format!("{ERR}CLOSED\r\n"),
                    (Fam::Luat, true) if mux => format!("\r\n{link}, CONNECT OK\r\n"),
                    (Fam::Luat, true) => "\r\nCONNECT OK\r\n".into(),
                    (Fam::Luat, false) if mux => format!("\r\n{link}, CONNECT FAIL\r\n"),
                    (Fam::Luat, false) => "\r\nCONNECT FAIL\r\n".into(),
                    (Fam::Cmcc, ok) => format!("\r\n+MIPOPEN: {link},{}\r\n", if ok { 0 } else { 571 }),
                    (_, ok) => format!("\r\n+QIOPEN: {link},{}\r\n", if ok { 0 } else { 566 }),
                };
                self.say(t, 0.0, s);
            }
            NetEvt::Data { link, data } => {
                if self.transparent {
                    self.say(t, 0.0, data);
                    return;
                }
                let n = data.len();
                let head: Vec<u8> = match self.fam {
                    Fam::Esp if mux => format!("\r\n+IPD,{link},{n}:").into(),
                    Fam::Esp => format!("\r\n+IPD,{n}:").into(),
                    Fam::Luat if mux => format!("\r\n+RECEIVE,{link},{n}:\r\n").into(),
                    Fam::Luat if self.ip_head => format!("\r\n+IPD,{n}:").into(),
                    Fam::Luat => vec![],
                    Fam::Cmcc => format!("\r\n+MIPURC: \"rtcp\",{link},{n},").into(),
                    _ => format!("\r\n+QIURC: \"recv\",{link},{n}\r\n").into(),
                };
                let mut d = head;
                d.extend(data);
                if self.fam != Fam::Luat || mux || self.ip_head {
                    d.extend(b"\r\n");
                }
                self.say(t, 0.0, d);
            }
            NetEvt::Closed { link } => {
                let i = link as usize;
                if i >= self.links.len() || !self.links[i] {
                    return;
                }
                self.links[i] = false;
                self.transparent = false;
                let s = match self.fam {
                    Fam::Esp if mux => format!("{link},CLOSED\r\n"),
                    Fam::Esp => "CLOSED\r\n".into(),
                    Fam::Luat if mux => format!("\r\n{link}, CLOSED\r\n"),
                    Fam::Luat => "\r\nCLOSED\r\n".into(),
                    Fam::Cmcc => format!("\r\n+MIPURC: \"disconn\",{link},1\r\n"),
                    _ => format!("\r\n+QIURC: \"closed\",{link}\r\n"),
                };
                self.say(t, 0.0, s);
            }
            NetEvt::Http { status, body } => {
                let n = body.len();
                self.http_resp = (status != 0).then(|| (status, body.clone()));
                let mut d: Vec<u8>;
                match self.fam {
                    Fam::Esp => {
                        if status == 0 || status >= 400 {
                            d = ERR.into();
                        } else {
                            d = format!("+HTTPCLIENT:{n},").into();
                            d.extend(&body);
                            d.extend(format!("\r\n{OK}").as_bytes());
                        }
                    }
                    Fam::Luat => d = format!("\r\n+HTTPACTION: {},{},{n}\r\n", self.http_post as u8, if status == 0 { 601 } else { status }).into(),
                    Fam::Cmcc => {
                        if status == 0 {
                            d = "\r\n+MHTTPURC: \"err\",0,4\r\n".into();
                        } else {
                            d = format!("\r\n+MHTTPURC: \"header\",0,{status},0,\r\n\r\n+MHTTPURC: \"content\",0,{n},{n},{n},").into();
                            d.extend(&body);
                            d.extend(b"\r\n");
                        }
                    }
                    _ => {
                        let cmd = if self.http_post { "QHTTPPOST" } else { "QHTTPGET" };
                        d = if status == 0 { format!("\r\n+{cmd}: 702\r\n") } else { format!("\r\n+{cmd}: 0,{status},{n}\r\n") }.into();
                    }
                }
                self.say(t, 0.0, d);
            }
            NetEvt::MqttOpened { ok } => {
                self.mqtt_on = ok;
                let s = match (self.fam, ok) {
                    (Fam::Esp, true) => {
                        let (h, p) = self.mqtt_host.clone().unwrap_or_default();
                        format!("+MQTTCONNECTED:0,1,\"{h}\",\"{p}\",\"\",1\r\n{OK}")
                    }
                    (Fam::Esp, false) => format!("+MQTTDISCONNECTED:0\r\n{ERR}"),
                    (Fam::Luat, true) => "\r\nCONNACK OK\r\n".into(),
                    (Fam::Luat, false) => "\r\nCLOSED\r\n".into(),
                    (Fam::Cmcc, ok) => format!("\r\n+MQTTURC: \"conn\",0,{}\r\n", if ok { 0 } else { 2 }),
                    (_, true) => "\r\n+QMTCONN: 0,0,0\r\n".into(),
                    (_, false) => "\r\n+QMTSTAT: 0,1\r\n".into(),
                };
                self.say(t, 0.0, s);
            }
            NetEvt::MqttMsg { topic, payload } => {
                let n = payload.len();
                let mut d: Vec<u8> = match self.fam {
                    Fam::Esp => format!("+MQTTSUBRECV:0,\"{topic}\",{n},").into(),
                    Fam::Luat => format!("\r\n+MSUB: \"{topic}\",{n} byte,").into(),
                    Fam::Cmcc => format!("\r\n+MQTTURC: \"publish\",0,0,\"{topic}\",{n},{n},").into(),
                    _ => format!("\r\n+QMTRECV: 0,0,\"{topic}\",\"").into(),
                };
                d.extend(payload);
                if !matches!(self.fam, Fam::Esp | Fam::Luat | Fam::Cmcc) {
                    d.push(b'"');
                }
                d.extend(b"\r\n");
                self.say(t, 0.0, d);
            }
            NetEvt::MqttClosed => {
                if !self.mqtt_on {
                    return;
                }
                self.mqtt_on = false;
                let s = match self.fam {
                    Fam::Esp => "+MQTTDISCONNECTED:0\r\n",
                    Fam::Luat => "\r\nCLOSED\r\n",
                    Fam::Cmcc => "\r\n+MQTTURC: \"conn\",0,1\r\n",
                    _ => "\r\n+QMTSTAT: 0,1\r\n",
                };
                self.say(t, 0.0, s);
            }
        }
    }

    // ───────────────────────── GPS

    fn nmea(&self) -> String {
        let p = if self.model == "NEO-6M" { "GP" } else { "GN" };
        let now = chrono::Utc::now();
        let hms = now.format("%H%M%S.00").to_string();
        let dmy = now.format("%d%m%y").to_string();
        let sum = |s: &str| s.bytes().fold(0u8, |a, b| a ^ b);
        let mut out = String::new();
        let lines: Vec<String> = if self.gps_fix {
            vec![
                format!("{p}GGA,{hms},2232.5880,N,11403.4740,E,1,09,0.9,25.3,M,-3.2,M,,"),
                format!("{p}RMC,{hms},A,2232.5880,N,11403.4740,E,0.12,85.6,{dmy},,,A"),
                format!("{p}VTG,85.6,T,,M,0.12,N,0.22,K,A"),
            ]
        } else {
            vec![format!("{p}GGA,{hms},,,,,0,00,99.99,,,,,,"), format!("{p}RMC,{hms},V,,,,,,,{dmy},,,N"), format!("{p}VTG,,,,,,,,,N")]
        };
        for l in lines {
            out += &format!("${l}*{:02X}\r\n", sum(&l));
        }
        out
    }

    /// 给属性面板看的一行状态
    pub fn status(&self) -> String {
        use crate::i18n::t;
        if !self.powered {
            return t("未上电").into();
        }
        let mut v: Vec<String> = vec![format!("{} {}", self.baud, t("波特率"))];
        match self.fam {
            Fam::Esp => v.push(match &self.wifi {
                Some(s) => format!("WiFi {s}"),
                None => t("WiFi 未连接").into(),
            }),
            Fam::Hc05 | Fam::Jdy | Fam::Hm10 => {
                if self.at_mode {
                    v.push(t("AT 模式").into());
                }
                v.push(if self.phone { t("手机已连接") } else { t("等待手机连接") }.into());
            }
            Fam::Gps => v.push(if self.gps_fix { t("已定位") } else { t("搜星中") }.into()),
            _ => v.push(if !self.sim {
                t("没插 SIM 卡").into()
            } else if self.csq == 0 {
                t("无信号").into()
            } else {
                format!("CSQ {}", self.csq)
            }),
        }
        let n = self.links.iter().filter(|x| **x).count();
        if n > 0 {
            v.push(format!("TCP×{n}"));
        }
        if self.mqtt_on {
            v.push("MQTT".into());
        }
        v.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(kind: &str) -> Part {
        let mut c = super::super::Circuit::default();
        c.add_part(kind, 0, 0);
        c.parts.pop().unwrap()
    }

    fn dev(kind: &str, model: &str) -> AtDev {
        let mut p = part(kind);
        if !model.is_empty() {
            p.props.insert("model".into(), model.into());
        }
        let mut d = AtDev::new(&p);
        d.set_power(true, false, 0.0);
        d
    }

    fn run(d: &mut AtDev, cmd: &str, t: &mut f64) -> String {
        d.feed(cmd.as_bytes(), *t);
        *t += 3.0;
        String::from_utf8_lossy(&d.poll(*t)).to_string()
    }

    #[test]
    fn esp01_wifi_and_tcp() {
        let mut d = dev("esp01", "");
        let mut t = 0.0;
        assert!(run(&mut d, "", &mut t).contains("ready"));
        assert!(run(&mut d, "AT\r\n", &mut t).contains("OK"));
        let r = run(&mut d, "AT+CIPSTART=\"TCP\",\"127.0.0.1\",8080\r\n", &mut t);
        assert!(r.contains("ERROR"), "{r}");
        let r = run(&mut d, "AT+CWJAP=\"Lab\",\"12345678\"\r\n", &mut t);
        assert!(r.contains("WIFI GOT IP") && r.contains("OK"), "{r}");
        run(&mut d, "AT+CIPSTART=\"TCP\",\"127.0.0.1\",8080\r\n", &mut t);
        assert_eq!(
            d.net.pop(),
            Some(NetCmd::Open {
                link: 0,
                udp: false,
                host: "127.0.0.1".into(),
                port: 8080
            })
        );
        d.net_evt(NetEvt::Opened { link: 0, ok: true }, t);
        assert!(String::from_utf8_lossy(&d.poll(t)).contains("CONNECT"));
        let r = run(&mut d, "AT+CIPSEND=5\r\n", &mut t);
        assert!(r.ends_with("> "), "{r}");
        let r = run(&mut d, "hello", &mut t);
        assert!(r.contains("SEND OK"), "{r}");
        assert_eq!(d.net.pop(), Some(NetCmd::Send { link: 0, data: b"hello".to_vec() }));
        d.net_evt(NetEvt::Data { link: 0, data: b"hi".to_vec() }, t);
        assert!(String::from_utf8_lossy(&d.poll(t)).contains("+IPD,2:hi"));
    }

    #[test]
    fn cat1_dialects() {
        for (model, open, ok) in [
            ("Air780E", "AT+CIPSTART=\"TCP\",\"1.2.3.4\",80\r\n", "CONNECT OK"),
            ("EC800M", "AT+QIOPEN=1,0,\"TCP\",\"1.2.3.4\",80,0,1\r\n", "+QIOPEN: 0,0"),
            ("ML307", "AT+MIPOPEN=0,\"TCP\",\"1.2.3.4\",80\r\n", "+MIPOPEN: 0,0"),
        ] {
            let mut d = dev("cat1", model);
            let mut t = 0.0;
            let r = run(&mut d, "", &mut t);
            assert!(r.contains("READY"), "{model}: {r}");
            assert!(run(&mut d, "AT+CSQ\r\n", &mut t).contains("+CSQ: 24,99"));
            assert!(run(&mut d, "AT+CREG?\r\n", &mut t).contains("+CREG: 0,1"), "{model}");
            run(&mut d, open, &mut t);
            assert!(matches!(d.net.pop(), Some(NetCmd::Open { port: 80, .. })), "{model}");
            d.net_evt(NetEvt::Opened { link: 0, ok: true }, t);
            let r = String::from_utf8_lossy(&d.poll(t)).to_string();
            assert!(r.contains(ok), "{model}: {r}");
        }
    }

    #[test]
    fn mqtt_quectel() {
        let mut d = dev("cat1", "EC800M");
        let mut t = 0.0;
        run(&mut d, "", &mut t);
        assert!(run(&mut d, "AT+QMTOPEN=0,\"127.0.0.1\",1883\r\n", &mut t).contains("+QMTOPEN: 0,0"));
        run(&mut d, "AT+QMTCONN=0,\"dev1\"\r\n", &mut t);
        assert!(matches!(d.net.pop(), Some(NetCmd::MqttOpen { port: 1883, .. })));
        d.net_evt(NetEvt::MqttOpened { ok: true }, t);
        assert!(String::from_utf8_lossy(&d.poll(t)).contains("+QMTCONN: 0,0,0"));
        let r = run(&mut d, "AT+QMTPUBEX=0,0,0,0,\"a/b\",3\r\n", &mut t);
        assert!(r.contains('>'));
        run(&mut d, "xyz", &mut t);
        assert_eq!(
            d.net.pop(),
            Some(NetCmd::MqttPub {
                topic: "a/b".into(),
                payload: b"xyz".to_vec()
            })
        );
    }

    #[test]
    fn hc05_at_mode_and_gps() {
        let mut p = part("hc05");
        p.props.insert("name".into(), "Car".into());
        let mut d = AtDev::new(&p);
        d.set_power(true, true, 0.0);
        assert_eq!(d.baud, 38400);
        let mut t = 0.0;
        assert!(run(&mut d, "AT+NAME?\r\n", &mut t).contains("+NAME:Car"));
        let mut g = dev("gps", "");
        let mut t = 0.0;
        let r = run(&mut g, "", &mut t);
        assert!(r.contains("$GNRMC") && r.contains(",A,"), "{r}");
    }
}
