//! Per-connection instrumentation. Opening this view opts the session into acquisition.
mod ai;
mod ui;
use super::*;
use crate::capture::{self, Capture, Decoder, Point, Protocol, CHANNELS};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{mpsc, Arc};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Config {
    protocol: Protocol,
    filter_samples: bool,
    hz: f64,
    aliases: Vec<String>,
    hidden: Vec<usize>,
    presets: BTreeMap<String, (f64, f64, f64)>,
    template: String,
    low: f64,
    high: f64,
    value: f64,
    threshold: f64,
    trigger_ch: usize,
    pre: f64,
    post: f64,
    can: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            protocol: Protocol::FireWater,
            filter_samples: true,
            hz: 1000.,
            aliases: vec![],
            hidden: vec![],
            presets: BTreeMap::new(),
            template: "kp={value}".into(),
            low: 0.,
            high: 1.,
            value: 0.25,
            threshold: 3.,
            trigger_ch: 0,
            pre: 3.,
            post: 5.,
            can: false,
        }
    }
}
#[derive(PartialEq, Default)]
enum View {
    #[default]
    Wave,
    Fft,
    Compare,
}
#[derive(Default)]
enum Recording {
    #[default]
    Idle,
    Manual(f64),
    Armed,
    Triggered(f64, f64),
}
#[derive(PartialEq, Copy, Clone)]
enum Panel {
    Channels,
    Tuning,
    Trigger,
}
enum Loaded {
    StreamPath(std::path::PathBuf),
    Firmware(String),
    File(Arc<Capture>, bool),
    Db(capture::dbc::Database),
    Saved,
}
pub(super) struct Lab {
    pub active: bool,
    pub visible: bool,
    port: String,
    key: String,
    cfg: Config,
    batch: Vec<f64>,
    widths: Vec<u8>,
    last_packet: Option<f64>,
    decoder: Decoder,
    t0: Instant,
    live: Capture,
    serial: Vec<usize>,
    can_ids: BTreeMap<(bool, u32), BTreeMap<String, usize>>,
    dbc: capture::dbc::Database,
    last_can: (Instant, u64),
    rec: Recording,
    prev: Option<f64>,
    saved: Option<Arc<Capture>>,
    reference: Option<Arc<Capture>>,
    review: bool,
    view: View,
    selected: usize,
    span: f64,
    cursor: f64,
    playing: bool,
    speed: f64,
    play_at: Instant,
    mark: [Option<f64>; 2],
    stream: Option<capture::stream::Stream>,
    next_mark: usize,
    status: String,
    job: Option<mpsc::Receiver<Result<Loaded, String>>>,
    panel: Option<Panel>,
    file_range: bool,
    file_from: f64,
    file_to: f64,
    visible_events: bool,
}
impl Default for Lab {
    fn default() -> Self {
        Self {
            active: false,
            visible: false,
            port: String::new(),
            key: String::new(),
            cfg: Config::default(),
            batch: vec![],
            widths: vec![],
            last_packet: None,
            decoder: Decoder::default(),
            t0: Instant::now(),
            live: Capture::default(),
            serial: vec![],
            can_ids: BTreeMap::new(),
            dbc: Default::default(),
            last_can: (Instant::now(), 0),
            rec: Recording::Idle,
            prev: None,
            saved: None,
            reference: None,
            review: false,
            view: View::Wave,
            selected: 0,
            span: 10.,
            cursor: 0.,
            playing: false,
            speed: 1.,
            play_at: Instant::now(),
            mark: [None, None],
            stream: None,
            next_mark: 0,
            status: String::new(),
            job: None,
            panel: None,
            file_range: false,
            file_from: 0.,
            file_to: 30.,
            visible_events: true,
        }
    }
}
impl Lab {
    pub(super) fn reset_decoder(&mut self) {
        self.decoder = Decoder::default();
        self.prev = None;
        self.last_packet = None;
    }
    pub(super) fn log_event(&mut self, line: &crate::logbuf::Line) {
        if self.cfg.protocol == Protocol::JustFloat && matches!(line.dir, Dir::Rx) {
            return;
        }
        let text = line.plain.trim();
        if text.is_empty() {
            return;
        }
        if self.cfg.protocol == Protocol::FireWater
            && !self.cfg.filter_samples
            && matches!(line.dir, Dir::Rx)
            && text.rsplit_once(':').map_or(text, |(_, s)| s).split(',').all(|s| s.trim().parse::<f64>().is_ok())
        {
            return;
        }
        self.event(
            match line.dir {
                Dir::Tx => "TX",
                Dir::Rx2 => "RX2",
                Dir::Rx => "RX",
                _ => "LOG",
            },
            text.chars().take(512).collect(),
        );
    }

    fn now(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }
    fn data(&self) -> &Capture {
        if self.review {
            self.saved.as_deref().unwrap_or(&self.live)
        } else {
            &self.live
        }
    }
    fn event(&mut self, source: &str, text: String) {
        let t = self.now();
        if let Some(s) = &mut self.stream {
            s.event(t, source, &text)
        }
        self.live.event(t, source, text)
    }
    fn feed(&mut self, d: &[u8], logs: &mut Vec<u8>) {
        let t = self.now();
        let cfg = &self.cfg;
        let c = &mut self.live;
        let serial = &mut self.serial;
        let stream = &mut self.stream;
        let prev = &mut self.prev;
        let rec = &mut self.rec;
        self.batch.clear();
        self.widths.clear();
        self.decoder.feed(
            cfg.protocol,
            d,
            |values| {
                self.widths.push(values.len() as u8);
                self.batch.extend_from_slice(values);
            },
            |line| {
                if cfg.filter_samples {
                    logs.extend_from_slice(line)
                }
            },
        );
        if self.widths.is_empty() {
            return;
        }
        let count = self.widths.len() as f64;
        let step = (1. / cfg.hz.clamp(1., 1e8)).min((t - self.last_packet.unwrap_or(0.)).max(0.) / count);
        self.last_packet = Some(t);
        let mut offset = 0;
        for (k, &n) in self.widths.iter().enumerate() {
            let stamp = t - (count - 1. - k as f64) * step;
            c.frames += 1;
            for i in 0..n as usize {
                let v = self.batch[offset + i];
                if i >= serial.len() {
                    if c.channels.len() >= CHANNELS {
                        break;
                    }
                    let id = c.channels.len();
                    serial.push(id);
                    c.channels.push(capture::Channel {
                        name: cfg.aliases.get(i).filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| format!("CH{}", i + 1)),
                        ..Default::default()
                    });
                    if let Some(s) = stream.as_mut() {
                        s.channel(id, &c.channels[id].name, "");
                    }
                }
                let id = serial[i];
                c.point(id, stamp, v);
                if v.is_finite() {
                    if let Some(s) = stream.as_mut() {
                        s.sample(stamp, id, v);
                    }
                }
                if id == cfg.trigger_ch && v.is_finite() {
                    check_trigger(rec, prev, c, stream, cfg, stamp, v)
                }
            }
            offset += n as usize;
        }
    }
    fn snapshot(&mut self, a: f64, b: f64) {
        if self.live.channels.iter().any(|ch| ch.points.len() == capture::HISTORY && ch.points.front().is_some_and(|p| p.0 > a)) {
            self.status = t("部分数据超出内存窗口，长时间采集请使用持续写盘").into();
        }
        self.saved = Some(Arc::new(self.live.slice(a, b)));
        self.review = true;
        self.cursor = b - a;
        self.playing = false;
        self.rec = Recording::Idle;
    }
    fn tick_record(&mut self) {
        if let Some(s) = &mut self.stream {
            if let Err(e) = s.flush_batch(false) {
                self.status = e.to_string();
                if let Some(stream) = self.stream.take() {
                    std::thread::spawn(move || drop(stream));
                }
            }
        }
        if let Recording::Triggered(start, end) = self.rec {
            if self.now() >= end {
                self.snapshot(start, end);
            }
        }
    }
    fn record(&mut self, action: &str) -> Result<(), String> {
        match (action, &self.rec) {
            ("start", Recording::Idle) => {
                self.review = false;
                self.rec = Recording::Manual(self.now());
            }
            ("stop", Recording::Manual(a)) => self.snapshot(*a, self.now()),
            ("stop", Recording::Triggered(a, b)) => self.snapshot(*a, self.now().min(*b)),
            ("stop", Recording::Armed) => self.rec = Recording::Idle,
            _ => return Err("Capture action unavailable".into()),
        }
        Ok(())
    }
    pub(super) fn open_path(&mut self, path: std::path::PathBuf) {
        let (tx, rx) = mpsc::channel();
        self.job = Some(rx);
        std::thread::spawn(move || {
            let result = capture::file::load_range(&path, None).map(|c| Loaded::File(Arc::new(c), false)).map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }
    fn input_file(&mut self, compare: bool) {
        if self.job.is_some() {
            return;
        }
        let window = self.file_range.then_some((self.file_from, self.file_to));
        if window.is_some_and(|(a, b)| a >= b) {
            self.status = "Invalid time range".into();
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.job = Some(rx);
        std::thread::spawn(move || {
            let path = rfd::FileDialog::new().add_filter("Capture", &["qcap", "mf4", "qrec"]).pick_file();
            let r = path.map_or(Ok(Loaded::Saved), |p| {
                capture::file::load_range(&p, window).map(|c| Loaded::File(Arc::new(c), compare)).map_err(|e| e.to_string())
            });
            let _ = tx.send(r);
        });
    }
    fn output_file(&mut self, ext: &'static str) {
        if self.job.is_some() {
            return;
        }
        let c = if self.review { self.saved.clone().unwrap_or_default() } else { Arc::new(self.live.clone()) };
        let (tx, rx) = mpsc::channel();
        self.job = Some(rx);
        std::thread::spawn(move || {
            let r = rfd::FileDialog::new()
                .set_file_name(format!("capture.{ext}"))
                .add_filter(ext, &[ext])
                .save_file()
                .map_or(Ok(()), |path| capture::file::save(&path, &c));
            let _ = tx.send(r.map(|_| Loaded::Saved).map_err(|e| e.to_string()));
        });
    }
    fn file_choice(&mut self, firmware: bool) {
        if self.job.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.job = Some(rx);
        std::thread::spawn(move || {
            let r = (|| -> Result<Loaded, String> {
                if firmware {
                    use sha2::{Digest, Sha256};
                    let Some(path) = rfd::FileDialog::new().pick_file() else { return Ok(Loaded::Saved) };
                    let mut f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
                    let mut hash = Sha256::new();
                    let mut buf = [0u8; 65536];
                    loop {
                        let n = std::io::Read::read(&mut f, &mut buf).map_err(|e| e.to_string())?;
                        if n == 0 {
                            break;
                        }
                        hash.update(&buf[..n]);
                    }
                    Ok(Loaded::Firmware(format!("{} · sha256:{:x}", path.display(), hash.finalize())))
                } else {
                    Ok(rfd::FileDialog::new()
                        .add_filter("QCCOM stream", &["qrec"])
                        .set_file_name("capture.qrec")
                        .save_file()
                        .map_or(Loaded::Saved, Loaded::StreamPath))
                }
            })();
            let _ = tx.send(r);
        });
    }
    fn stop_stream(&mut self) {
        if self.job.is_some() {
            return;
        }
        if let Some(stream) = self.stream.take() {
            let (tx, rx) = mpsc::channel();
            self.job = Some(rx);
            std::thread::spawn(move || {
                let _ = tx.send(stream.finish().map(|_| Loaded::Saved).map_err(|e| e.to_string()));
            });
        }
    }
    fn load_dbc(&mut self) {
        if self.job.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.job = Some(rx);
        std::thread::spawn(move || {
            let r = (|| {
                let Some(path) = rfd::FileDialog::new().add_filter("CAN DBC", &["dbc"]).pick_file() else {
                    return Ok(Loaded::Saved);
                };
                if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 8 << 20 {
                    return Err("DBC exceeds 8 MiB".into());
                }
                let data = std::fs::read(path).map_err(|e| e.to_string())?;
                capture::dbc::Database::parse(&String::from_utf8_lossy(&data)).map(Loaded::Db)
            })();
            let _ = tx.send(r);
        });
    }
    fn poll_file(&mut self) {
        let Some(job) = &self.job else { return };
        let r = match job.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(_) => Err("File worker stopped".into()),
        };
        self.job = None;
        match r {
            Ok(Loaded::Firmware(id)) => {
                self.live.firmware = id.clone();
                self.event("FW", id);
            }
            Ok(Loaded::StreamPath(path)) => match capture::stream::Stream::create(&path, &self.live) {
                Ok(s) => {
                    self.stream = Some(s);
                    self.status = path.display().to_string();
                }
                Err(e) => self.status = e.to_string(),
            },
            Err(e) => self.status = e,
            Ok(Loaded::File(c, true)) => {
                self.reference = Some(c);
                self.view = View::Compare
            }
            Ok(Loaded::File(c, false)) => {
                self.cursor = c.end();
                self.saved = Some(c);
                self.review = true;
                self.playing = false
            }
            Ok(Loaded::Db(db)) => {
                self.status = format!("DBC · {} IDs", db.0.len());
                self.dbc = db;
                self.cfg.can = true
            }
            Ok(Loaded::Saved) => {}
        }
    }
}
fn check_trigger(rec: &mut Recording, prev: &mut Option<f64>, cap: &mut Capture, stream: &mut Option<capture::stream::Stream>, cfg: &Config, t: f64, v: f64) {
    if matches!(rec, Recording::Armed) && prev.is_some_and(|p| p <= cfg.threshold) && v > cfg.threshold {
        let text = format!("CH{} > {}", cfg.trigger_ch + 1, cfg.threshold);
        if let Some(s) = stream {
            s.event(t, "TRIG", &text);
        }
        cap.event(t, "TRIG", text);
        *rec = Recording::Triggered((t - cfg.pre).max(0.), t + cfg.post);
    }
    *prev = Some(v);
}
impl App {
    fn device_key(&self) -> String {
        format!(
            "{}|{:?}|{}",
            self.shell_cwd().display(),
            self.s.transport,
            if self.s.transport == Transport::Serial {
                self.s.port.clone()
            } else {
                self.s.net.label(self.s.transport)
            }
        )
    }
    pub(super) fn open_lab(&mut self) {
        let key = self.device_key();
        let port = if self.s.transport == Transport::Serial {
            self.s.port.clone()
        } else {
            self.s.net.label(self.s.transport)
        };
        if self.plot.lab.key != key {
            let mut lab = Lab::default();
            lab.cfg = self.s.labs.get(&key).cloned().unwrap_or_default();
            lab.key = key;
            lab.live.port = port.clone();
            lab.live.project = self.shell_cwd().display().to_string();
            lab.live.firmware = self.shells.instrument_context().get("firmware").and_then(Value::as_str).unwrap_or("").into();
            lab.port = port;
            let now = chrono::Local::now();
            let first = self.buf.lines.len().saturating_sub(128);
            for line in self.buf.lines.iter().skip(first) {
                if !line.plain.trim().is_empty() {
                    let time = (line.ts - now).num_microseconds().unwrap_or(0) as f64 / 1e6;
                    lab.live.event(
                        time.min(0.),
                        match line.dir {
                            Dir::Rx => "RX",
                            Dir::Tx => "TX",
                            _ => "LOG",
                        },
                        line.plain.chars().take(512).collect(),
                    );
                }
            }
            self.plot.lab = lab;
        }
        self.plot.lab.active = true;
        self.plot.lab.visible = true;
        self.s.terminal = false;
    }
    pub(super) fn instrument_rx(&mut self, data: &[u8]) -> Option<Vec<u8>> {
        if !self.plot.lab.active {
            return None;
        }
        if self.s.transport == Transport::Serial && self.s.port != self.plot.lab.port {
            self.plot.lab.active = false;
            self.plot.lab.visible = false;
            return None;
        }
        let lab = &mut self.plot.lab;
        let mut logs = Vec::new();
        lab.feed(data, &mut logs);
        if lab.stream.is_some() {
            lab.tick_record();
        }
        (lab.cfg.protocol == Protocol::JustFloat || lab.cfg.protocol == Protocol::FireWater && lab.cfg.filter_samples).then_some(logs)
    }
    pub(super) fn instrument_tick(&mut self) {
        let lab = &mut self.plot.lab;
        if lab.job.is_some() {
            lab.poll_file();
        }
        if !lab.active {
            return;
        }
        lab.tick_record();
        if lab.cfg.can {
            let batch = self.tb.can_st.try_lock().ok().map(|st| {
                let a = st.trace.partition_point(|item| (item.at, item.seq) <= lab.last_can);
                let b = (a + 512).min(st.trace.len());
                if b < st.trace.len() {
                    self.ctx.request_repaint();
                }
                st.trace[a..b].to_vec()
            });
            if let Some(batch) = batch {
                for item in batch {
                    let stamp = item.at.saturating_duration_since(lab.t0).as_secs_f64();
                    lab.last_can = (item.at, item.seq);
                    let f = &item.f;
                    let source = if item.tx { "CAN TX" } else { "CAN RX" };
                    let raw = f.encode();
                    if let Some(s) = &mut lab.stream {
                        s.event(stamp, source, &raw);
                    }
                    lab.live.event(stamp, source, raw);
                    if !f.rtr {
                        lab.dbc.decode(f.id, f.ext, &f.data, |name, unit, v| {
                            let signals = lab.can_ids.entry((f.ext, f.id)).or_default();
                            let id = if let Some(&id) = signals.get(name) {
                                id
                            } else {
                                let id = lab.live.channels.len();
                                if id >= CHANNELS {
                                    return;
                                }
                                lab.live.channels.push(capture::Channel {
                                    name: format!("{:X}.{name}", f.id),
                                    unit: unit.into(),
                                    ..Default::default()
                                });
                                signals.insert(name.into(), id);
                                if let Some(s) = &mut lab.stream {
                                    s.channel(id, &lab.live.channels[id].name, unit)
                                }
                                id
                            };
                            lab.live.point(id, stamp, v);
                            if let Some(s) = &mut lab.stream {
                                if v.is_finite() {
                                    s.sample(stamp, id, v);
                                }
                            }
                            if id == lab.cfg.trigger_ch {
                                check_trigger(&mut lab.rec, &mut lab.prev, &mut lab.live, &mut lab.stream, &lab.cfg, stamp, v)
                            }
                        });
                    }
                }
            }
        }
    }
    fn send_tune(&mut self, value: f64) -> Result<(), String> {
        let cfg = &self.plot.lab.cfg;
        if !value.is_finite() || value < cfg.low || value > cfg.high || !cfg.template.contains("{value}") {
            return Err("Check range and {value} template".into());
        }
        if !self.can_send() {
            return Err(t("未连接").into());
        }
        let cmd = cfg.template.replace("{value}", &value.to_string());
        let mut bytes = self.s.encoding.encode(&cmd);
        bytes.extend(self.line_ending().bytes());
        self.send_bytes(bytes);
        self.plot.lab.cfg.value = value;
        self.plot.lab.live.parameters.insert(self.plot.lab.cfg.template.clone(), value);
        self.plot.lab.event("PARAM", cmd);
        Ok(())
    }
    fn instrument_task(&mut self, flash: bool) -> Result<(), String> {
        let cwd = self.shell_cwd();
        let port = if self.s.transport == Transport::Serial {
            self.s.port.clone()
        } else {
            self.serial_port_for(self.serial_sidebar_slot()).to_owned()
        };
        self.shells.instrument_task(&self.ctx, &cwd, flash, &port)
    }
}
#[derive(Default)]
pub(super) struct Ai {
    pub dialog: bool,
    bridge: Option<crate::mcp::Bridge>,
    slot: usize,
    key: String,
    origin: Option<Instant>,
    device: bool,
    build: bool,
    circuit: bool,
    last: String,
}
impl Ai {
    pub(super) fn open_for(&mut self, slot: usize) {
        if self.bridge.is_none() {
            self.slot = slot;
        }
        self.dialog = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> App {
        let ctx = eframe::CreationContext::_new_kittest(egui::Context::default());
        let mut app = App::with_settings(
            &ctx,
            Settings {
                telemetry: false,
                auto_update: false,
                ..Default::default()
            },
            vec![],
            vec![],
        );
        app.s.port = "COM30".into();
        app.open_lab();
        app
    }
    #[test]
    fn session_decoding_trigger_and_ai() {
        let mut app = app();
        app.plot.lab.cfg.threshold = 2.;
        app.plot.lab.cfg.post = 0.;
        app.plot.lab.rec = Recording::Armed;
        app.on_rx(b"ready\nd:1,0\nd:3,1\n".to_vec());
        app.tick_conn();
        assert!(app.buf.lines.iter().any(|l| l.plain == "ready"));
        assert!(!app.buf.lines.iter().any(|l| l.plain.starts_with("d:")));
        assert!(app.plot.lab.review);
        assert!(app.plot.lab.saved.as_ref().unwrap().events.iter().any(|e| e.source == "TRIG"));
        assert!(app.ai_call("saved_command", &json!({"name":"anything"})).is_err());
        let stats = app.ai_call("read_signal", &json!({"channel":1})).unwrap();
        assert_eq!(stats["max"], 3.);
        app.plot.lab.cfg.protocol = Protocol::JustFloat;
        let slot = app.session_slot;
        let other = app.new_serial_log_slot();
        app.use_session(other);
        assert!(!app.plot.lab.active);
        app.use_session(slot);
        assert_eq!(app.plot.lab.cfg.protocol, Protocol::JustFloat);
    }
    #[test]
    fn mcp_uses_selected_session_and_requires_circuit_sharing() {
        let mut app = app();
        let original = app.session_slot;
        let other = app.new_serial_log_slot();
        app.with_session(other, |app| {
            app.s.port = "COM41".into();
            app.ai.open_for(app.session_slot);
        });
        assert_eq!(app.ai.slot, other);
        let info = app.with_session(app.ai.slot, |app| app.ai_call("session_info", &json!({})).unwrap());
        assert_eq!(info["port"], "COM41");
        assert_eq!(app.session_slot, original);
        assert!(app.ai_call("circuit_inspect", &json!({})).unwrap_err().contains("sharing"));
        app.ai.circuit = true;
        assert!(app.ai_call("circuit_inspect", &json!({})).unwrap_err().contains("Open"));
        let mut view = crate::circuitview::CircuitView::new();
        view.c = crate::circuit::examples::find("A4").unwrap().build().unwrap();
        app.circ = Some(Box::new(view));
        assert!(app.ai_call("circuit_inspect", &json!({})).unwrap()["parts"].as_array().unwrap().len() >= 3);
    }
    #[test]
    fn capture_can_alignment_and_tuning() {
        let mut app = app();
        app.plot.lab.cfg.can = true;
        app.plot.lab.dbc = capture::dbc::Database::parse("BO_ 385 Speed: 8 ECU\n SG_ RPM : 0|16@1+ (1,0) [0|65535] \"rpm\" ECU").unwrap();
        let f = crate::toolbox::slcan::Frame::decode("t1812DC05").unwrap();
        app.tb.can_st.lock().unwrap().trace.push(crate::toolbox::slcan::TraceItem {
            seq: 1,
            at: Instant::now(),
            t: 0.,
            tx: false,
            f,
        });
        app.instrument_tick();
        app.instrument_tick();
        assert_eq!(app.plot.lab.live.channels[0].points.len(), 1);
        assert_eq!(app.plot.lab.live.channels[0].points[0].1, 1500.);
        let (cmd, rx) = mpsc::channel();
        app.conn = Some(Connection { cmd });
        app.ai.device = true;
        app.ai_call("set_parameter", &json!({"value":0.25})).unwrap();
        assert!(matches!(rx.try_recv(),Ok(Cmd::Send(s)) if s.starts_with(b"kp=0.25")));
        assert_eq!(app.plot.lab.live.events.back().unwrap().source, "PARAM");
    }
}

#[cfg(test)]
mod performance {
    use super::*;
    #[test]
    #[ignore = "manual optimized acquisition benchmark"]
    fn capture_throughput() {
        let mut lab = Lab::default();
        lab.cfg.protocol = Protocol::JustFloat;
        let mut packet = Vec::new();
        for i in 0..4096 {
            for ch in 0..8 {
                packet.extend_from_slice(&((i + ch) as f32).to_le_bytes());
            }
            packet.extend_from_slice(&[0, 0, 128, 127]);
        }
        let mut logs = Vec::new();
        let start = Instant::now();
        for _ in 0..256 {
            for chunk in packet.chunks(4096) {
                lab.feed(std::hint::black_box(chunk), &mut logs);
            }
        }
        let secs = start.elapsed().as_secs_f64();
        assert_eq!(lab.live.frames, 1_048_576);
        let mut timings = Vec::with_capacity(101);
        let mut vertices = 0;
        for _ in 0..101 {
            let start = Instant::now();
            vertices = lab
                .live
                .channels
                .iter()
                .map(|ch| std::hint::black_box(ch.envelope(ch.points[0].0, ch.points.back().unwrap().0, 1200)).len())
                .sum::<usize>();
            timings.push(start.elapsed().as_micros());
        }
        timings.sort_unstable();
        eprintln!(
            "8 channels, {} frames, {:.3}s, {:.1} MiB/s; envelope 8 x 65536 @ 1200 px: {} vertices, median {} us, p95 {} us",
            lab.live.frames,
            secs,
            36. / secs,
            vertices,
            timings[50],
            timings[95]
        );
        assert!(vertices <= 8 * 2402);
    }
}
