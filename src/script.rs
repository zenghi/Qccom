//! 自动化脚本（Rhai 语言）：在后台线程里跑，通过 channel 和界面交换数据。
//!
//! 脚本里能用的函数：
//!   send("text")          发文字（不加换行）
//!   sendln("AT")          发文字 + 命令栏设置的换行符
//!   send_hex("AA 55 01")  发十六进制字节
//!   sleep(500)            等待毫秒
//!   wait("OK", 2000)      等到收到包含 "OK" 的一行，返回 true；超时返回 false
//!   wait_re("v(\\d+)", 2000)  等到匹配正则的一行，返回第一个括号里的内容（没有括号就是整段匹配）；超时返回 ""
//!   read_line(1000)       读下一行，超时返回 ""
//!   clear()               丢掉还没读的行
//!   log("msg")  print()   在日志里显示一行
//!   dtr(true)  rts(false) 设置信号线
//!   now()                 脚本开始以来的毫秒数

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use rhai::{Dynamic, Engine, EvalAltResult};

use crate::i18n::t;
use crate::logbuf::Encoding;

const SCRIPT_QUEUE_LIMIT: usize = 256;
const SCRIPT_STRING_BYTES: usize = 64 * 1024;
const RUNNING: u8 = 0;
const STOPPED: u8 = 1;
const OVERFLOW: u8 = 2;

pub enum ScriptOut {
    Send(Vec<u8>),
    Log(String),
    Dtr(bool),
    Rts(bool),
    Done(Result<(), String>),
}

pub struct ScriptRun {
    pub name: String,
    lines: SyncSender<String>,
    pub rx: Receiver<ScriptOut>,
    stop: Arc<AtomicU8>,
    pub started: Instant,
}

impl ScriptRun {
    pub fn start(name: &str, code: String, ending: Vec<u8>, enc: Encoding, ctx: egui::Context) -> ScriptRun {
        let (lines, lines_rx) = mpsc::sync_channel::<String>(SCRIPT_QUEUE_LIMIT);
        let (out, rx) = mpsc::sync_channel(SCRIPT_QUEUE_LIMIT);
        let stop = Arc::new(AtomicU8::new(RUNNING));
        let st = stop.clone();
        let worker_out = out.clone();
        let worker_ctx = ctx.clone();
        if let Err(e) = thread::Builder::new().name("script".into()).spawn(move || {
            let r = run(&code, ending, enc, lines_rx, worker_out.clone(), st, worker_ctx.clone());
            let _ = worker_out.send(ScriptOut::Done(r));
            worker_ctx.request_repaint();
        }) {
            let _ = out.try_send(ScriptOut::Done(Err(e.to_string())));
            ctx.request_repaint();
        }
        ScriptRun {
            name: name.to_string(),
            lines,
            rx,
            stop,
            started: Instant::now(),
        }
    }

    /// 收到的每一行（去掉 ANSI 后的文字）都交给脚本
    pub fn feed_line(&self, s: &str) {
        if self.stop.load(Ordering::Relaxed) != RUNNING {
            return;
        }
        if s.len() > SCRIPT_STRING_BYTES || matches!(self.lines.try_send(s.to_string()), Err(TrySendError::Full(_))) {
            request_stop(&self.stop, OVERFLOW);
        }
    }

    pub fn stop(&self) {
        request_stop(&self.stop, STOPPED);
    }
}

impl Drop for ScriptRun {
    fn drop(&mut self) {
        self.stop();
    }
}

fn request_stop(state: &AtomicU8, reason: u8) {
    let _ = state.compare_exchange(RUNNING, reason, Ordering::Relaxed, Ordering::Relaxed);
}

#[derive(Clone, Copy)]
struct Timeout {
    start: Instant,
    duration: Duration,
}

impl Timeout {
    fn after_ms(ms: i64) -> Self {
        Self {
            start: Instant::now(),
            duration: Duration::from_millis(ms.max(0) as u64),
        }
    }

    fn remaining(self) -> Duration {
        // Instant + Duration can panic on Windows for sufficiently large
        // script-supplied values. Measuring elapsed time avoids that addition.
        self.duration.saturating_sub(self.start.elapsed())
    }
}

type Rt = Box<EvalAltResult>;

fn run(code: &str, ending: Vec<u8>, enc: Encoding, lines_rx: Receiver<String>, out: SyncSender<ScriptOut>, stop: Arc<AtomicU8>, ctx: egui::Context) -> Result<(), String> {
    let mut e = Engine::new();
    e.set_max_expr_depths(64, 64);
    e.set_max_string_size(SCRIPT_STRING_BYTES);
    let t0 = Instant::now();
    let lines_rx = std::rc::Rc::new(lines_rx);

    let emit = {
        let out = out.clone();
        let ctx = ctx.clone();
        let stop = stop.clone();
        move |o: ScriptOut| match out.try_send(o) {
            Ok(()) => ctx.request_repaint(),
            Err(TrySendError::Full(_)) => request_stop(&stop, OVERFLOW),
            Err(TrySendError::Disconnected(_)) => request_stop(&stop, STOPPED),
        }
    };
    let stopped = {
        let stop = stop.clone();
        move || stop.load(Ordering::Relaxed) != RUNNING
    };
    let s1 = stop.clone();
    e.on_progress(move |_| if s1.load(Ordering::Relaxed) != RUNNING { Some(Dynamic::UNIT) } else { None });
    {
        let emit = emit.clone();
        e.on_print(move |s| emit(ScriptOut::Log(s.to_string())));
    }
    {
        let emit = emit.clone();
        e.on_debug(move |s, _, _| emit(ScriptOut::Log(s.to_string())));
    }
    {
        let emit = emit.clone();
        e.register_fn("log", move |s: &str| emit(ScriptOut::Log(s.to_string())));
    }
    {
        let emit = emit.clone();
        e.register_fn("send", move |s: &str| emit(ScriptOut::Send(enc.encode(s))));
    }
    {
        let emit = emit.clone();
        e.register_fn("sendln", move |s: &str| {
            let mut d = enc.encode(s);
            d.extend_from_slice(&ending);
            emit(ScriptOut::Send(d))
        });
    }
    {
        let emit = emit.clone();
        e.register_fn("send_hex", move |s: &str| -> Result<(), Rt> {
            let d = crate::app::parse_hex(s).map_err(|e| Rt::from(e))?;
            emit(ScriptOut::Send(d));
            Ok(())
        });
    }
    {
        let emit = emit.clone();
        e.register_fn("dtr", move |v: bool| emit(ScriptOut::Dtr(v)));
    }
    {
        let emit = emit.clone();
        e.register_fn("rts", move |v: bool| emit(ScriptOut::Rts(v)));
    }
    {
        let stopped = stopped.clone();
        e.register_fn("sleep", move |ms: i64| -> Result<(), Rt> {
            let timeout = Timeout::after_ms(ms);
            loop {
                if stopped() {
                    return Err("stopped".into());
                }
                let left = timeout.remaining();
                if left.is_zero() {
                    return Ok(());
                }
                thread::sleep(left.min(Duration::from_millis(20)));
            }
        });
    }
    e.register_fn("now", move || t0.elapsed().as_millis() as i64);

    // 取下一行：超时返回 None；停止或关闭连接返回错误
    let next_line = {
        let lines_rx = lines_rx.clone();
        let stopped = stopped.clone();
        move |timeout: Timeout| -> Result<Option<String>, Rt> {
            loop {
                if stopped() {
                    return Err("stopped".into());
                }
                let left = timeout.remaining();
                if left.is_zero() {
                    return Ok(None);
                }
                match lines_rx.recv_timeout(left.min(Duration::from_millis(50))) {
                    Ok(l) => return Ok(Some(l)),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return Err("stopped".into()),
                }
            }
        }
    };
    {
        let next_line = next_line.clone();
        e.register_fn("wait", move |pat: &str, ms: i64| -> Result<bool, Rt> {
            let timeout = Timeout::after_ms(ms);
            while let Some(l) = next_line(timeout)? {
                if l.contains(pat) {
                    return Ok(true);
                }
            }
            Ok(false)
        });
    }
    {
        let next_line = next_line.clone();
        e.register_fn("wait_re", move |pat: &str, ms: i64| -> Result<String, Rt> {
            let re = regex_lite::Regex::new(pat).map_err(|e| Rt::from(e.to_string()))?;
            let timeout = Timeout::after_ms(ms);
            while let Some(l) = next_line(timeout)? {
                if let Some(c) = re.captures(&l) {
                    let m = c.get(1).or_else(|| c.get(0)).map_or("", |m| m.as_str());
                    return Ok(m.to_string());
                }
            }
            Ok(String::new())
        });
    }
    {
        let next_line = next_line.clone();
        e.register_fn("read_line", move |ms: i64| -> Result<String, Rt> { Ok(next_line(Timeout::after_ms(ms))?.unwrap_or_default()) });
    }
    {
        e.register_fn("clear", move || {
            for _ in 0..SCRIPT_QUEUE_LIMIT {
                if lines_rx.try_recv().is_err() {
                    break;
                }
            }
        });
    }
    let result = e.run(code);
    match stop.load(Ordering::Relaxed) {
        OVERFLOW => Err(t("脚本输入或输出积压过多，已停止").into()),
        STOPPED => Err("stopped".into()),
        _ => result.map_err(|err| err.to_string()),
    }
}

pub const EXAMPLE: &str = r#"// send AT, wait for OK, three times
for i in 0..3 {
    sendln("AT");
    if wait("OK", 1000) {
        log(`#${i + 1}: OK`);
    } else {
        log(`#${i + 1}: no reply`);
    }
    sleep(500);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(r: &ScriptRun) -> (Vec<Vec<u8>>, Vec<String>, Result<(), String>) {
        let mut sent = vec![];
        let mut logs = vec![];
        loop {
            match r.rx.recv_timeout(Duration::from_secs(5)).expect("script finished") {
                ScriptOut::Send(d) => sent.push(d),
                ScriptOut::Log(s) => logs.push(s),
                ScriptOut::Done(res) => return (sent, logs, res),
                _ => {}
            }
        }
    }

    #[test]
    fn send_and_wait() {
        let code = r#"
            sendln("AT");
            if wait("OK", 2000) { log("got"); }
            let v = wait_re("ver (\\d+)", 2000);
            log(v);
            send_hex("AA 55");
        "#;
        let r = ScriptRun::start("t", code.into(), b"\r\n".to_vec(), Encoding::Utf8, egui::Context::default());
        r.feed_line("noise");
        r.feed_line("OK");
        r.feed_line("ver 42");
        let (sent, logs, res) = collect(&r);
        assert_eq!(res, Ok(()));
        assert_eq!(sent, vec![b"AT\r\n".to_vec(), vec![0xAA, 0x55]]);
        assert_eq!(logs, vec!["got".to_string(), "42".to_string()]);
    }

    #[test]
    fn huge_sleep_is_cancellable() {
        let r = ScriptRun::start("t", "log(\"ready\"); sleep(9223372036854775807);".into(), vec![], Encoding::Utf8, egui::Context::default());
        assert!(matches!(r.rx.recv_timeout(Duration::from_secs(5)), Ok(ScriptOut::Log(s)) if s == "ready"));
        r.stop();
        assert_eq!(collect(&r).2, Err("stopped".into()));
    }

    #[test]
    fn stalled_script_reports_input_backlog_instead_of_growing_it() {
        let r = ScriptRun::start("t", "sleep(60000);".into(), vec![], Encoding::Utf8, egui::Context::default());
        for _ in 0..=SCRIPT_QUEUE_LIMIT {
            r.feed_line("incoming");
        }
        assert_eq!(r.stop.load(Ordering::Relaxed), OVERFLOW);
        assert!(collect(&r).2.is_err());
    }

    #[test]
    fn stop_and_errors() {
        let r = ScriptRun::start("t", "loop { sleep(10); }".into(), vec![], Encoding::Utf8, egui::Context::default());
        thread::sleep(Duration::from_millis(50));
        r.stop();
        assert_eq!(collect(&r).2, Err("stopped".into()));
        let r = ScriptRun::start("t", "let x = ;".into(), vec![], Encoding::Utf8, egui::Context::default());
        assert!(collect(&r).2.is_err());
        let r = ScriptRun::start("t", "if !wait(\"never\", 50) { log(\"timeout\"); }".into(), vec![], Encoding::Utf8, egui::Context::default());
        assert_eq!(collect(&r).1, vec!["timeout".to_string()]);
    }
}
