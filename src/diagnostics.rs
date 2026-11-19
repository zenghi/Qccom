//! Best-effort local evidence of Rust panics and prolonged active UI frames.
//! Deliberately never reads the panic payload, backtrace, application state, or logs.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant, SystemTime};

mod dialog;
pub use dialog::file_dialog;

const MAX_RECORD: u64 = 128;
const MAX_CONSENT: u64 = 1024;
const SCHEMA: &str = "QCCOM-DIAGNOSTIC-1";
const POLL: Duration = Duration::from_secs(1);
const MAX_SAMPLE_GAP: Duration = Duration::from_secs(3);
const STALL: Duration = Duration::from_secs(20);
const AUTO_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Panic,
    UiStall,
}

impl Event {
    pub fn code(self) -> &'static str {
        match self {
            Self::Panic => "rust-panic",
            Self::UiStall => "possible-ui-stall",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "rust-panic" => Some(Self::Panic),
            "possible-ui-stall" => Some(Self::UiStall),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub event: Event,
    pub version: [u16; 3],
    pub os: &'static str,
    pub arch: &'static str,
}

fn version(s: &str) -> Option<[u16; 3]> {
    let mut parts = s.split('.');
    let mut next = || {
        let p = parts.next()?;
        (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())).then(|| p.parse::<u16>().ok()).flatten()
    };
    let v = [next()?, next()?, next()?];
    parts.next().is_none().then_some(v)
}

fn platform(s: &str) -> Option<&'static str> {
    match s {
        "linux" => Some("linux"),
        "windows" => Some("windows"),
        "macos" => Some("macos"),
        "other" => Some("other"),
        _ => None,
    }
}

fn cpu(s: &str) -> Option<&'static str> {
    match s {
        "x86_64" => Some("x86_64"),
        "x86" => Some("x86"),
        "aarch64" => Some("aarch64"),
        "arm" => Some("arm"),
        "other" => Some("other"),
        _ => None,
    }
}

impl Report {
    fn current(event: Event) -> Self {
        let version = [
            env!("CARGO_PKG_VERSION_MAJOR").parse().unwrap_or(0),
            env!("CARGO_PKG_VERSION_MINOR").parse().unwrap_or(0),
            env!("CARGO_PKG_VERSION_PATCH").parse().unwrap_or(0),
        ];
        Self {
            event,
            version,
            os: platform(std::env::consts::OS).unwrap_or("other"),
            arch: cpu(std::env::consts::ARCH).unwrap_or("other"),
        }
    }

    pub fn version_string(&self) -> String {
        let [major, minor, patch] = self.version;
        format!("{major}.{minor}.{patch}")
    }

    fn parse(s: &str) -> Option<Self> {
        let mut lines = s.lines();
        if lines.next()? != SCHEMA {
            return None;
        }
        let r = Self {
            event: Event::parse(lines.next()?)?,
            version: version(lines.next()?)?,
            os: platform(lines.next()?)?,
            arch: cpu(lines.next()?)?,
        };
        lines.next().is_none().then_some(r)
    }

    fn write(&self, f: &mut File) -> io::Result<()> {
        let [major, minor, patch] = self.version;
        write!(f, "{SCHEMA}\n{}\n{major}.{minor}.{patch}\n{}\n{}\n", self.event.code(), self.os, self.arch)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Consent {
    pub account: i64,
    pub server: String,
}

impl Consent {
    pub fn new(account: i64, server: String) -> Option<Self> {
        let url = server.strip_prefix("https://")?;
        let host = url.split('/').next()?;
        (account > 0 && !host.is_empty() && server.len() <= 512 && !server.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '@' | '?' | '#' | '\\')))
            .then_some(Self { account, server })
    }

    fn parse(s: &str) -> Option<Self> {
        let (account, server) = s.split_once('\n')?;
        Self::new(account.parse().ok()?, server.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
    slot: u16,
}

impl Store {
    pub fn new(dir: PathBuf, slot: u16) -> Self {
        Self { dir, slot }
    }

    fn path(&self, ext: &str) -> PathBuf {
        self.dir.join(format!("window-{}.{}", self.slot, ext))
    }

    pub fn consent(&self) -> Option<Consent> {
        Consent::parse(&read_bounded(&self.path("consent"), MAX_CONSENT)?)
    }

    pub fn set_consent(&self, consent: Option<&Consent>) -> io::Result<()> {
        let dest = self.path("consent");
        let Some(c) = consent else {
            delete(&dest)?;
            return if self.dir.is_dir() { sync_dir(&self.dir) } else { Ok(()) };
        };
        self.replace_file("consent", |f| write!(f, "{}\n{}", c.account, c.server))
    }

    fn replace_file(&self, ext: &str, write_record: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let tmp = self.path(&format!("{ext}.tmp"));
        delete(&tmp)?;
        let mut f = private_file(&tmp)?;
        let result = write_record(&mut f).and_then(|_| f.sync_all());
        drop(f);
        result.and_then(|_| fs::rename(tmp, self.path(ext)))?;
        sync_dir(&self.dir)
    }

    pub fn auto_ready(&self, now: SystemTime) -> bool {
        match fs::symlink_metadata(self.path("auto")) {
            Err(e) => e.kind() == io::ErrorKind::NotFound,
            Ok(stamp) => stamp.is_file() && stamp.modified().ok().and_then(|t| now.duration_since(t).ok()).is_some_and(|d| d >= AUTO_INTERVAL),
        }
    }

    pub fn mark_auto_attempt(&self) -> io::Result<()> {
        // The same per-window instance lock protects this marker. Its mtime is
        // local cooldown state only, not part of the report or post.
        if !self.auto_ready(SystemTime::now()) {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "automatic diagnostic cooldown"));
        }
        self.replace_file("auto", |f| f.write_all(b"1\n"))
    }

    pub fn previous(&self) -> Option<(Report, bool)> {
        for (ext, attempted) in [("attempted", true), ("pending", false)] {
            let path = self.path(ext);
            if let Some(r) = read_bounded(&path, MAX_RECORD).and_then(|s| Report::parse(&s)) {
                return Some((r, attempted));
            }
            // An invalid/partial marker is not a report; never send it or let it block future captures.
            let _ = delete(&path);
        }
        None
    }

    fn write(&self, report: &Report) -> io::Result<()> {
        let pending = self.path("pending");
        if pending.try_exists()? || self.path("attempted").try_exists()? {
            return Ok(());
        }
        let tmp = self.path("record.tmp");
        let mut f = private_file(&tmp)?;
        let result = report.write(&mut f);
        drop(f);
        result.and_then(|_| fs::rename(tmp, pending))
    }

    /// Durable single-attempt claim written before sending. A process exit or an
    /// ambiguous response after this point must not trigger a second automatic POST.
    pub fn claim(&self, report: &Report) -> io::Result<()> {
        let from = self.path("pending");
        let to = self.path("attempted");
        let current = read_bounded(&from, MAX_RECORD).and_then(|s| Report::parse(&s));
        if current.as_ref() != Some(report) || to.try_exists()? {
            return Err(io::Error::new(io::ErrorKind::NotFound, "diagnostic no longer pending"));
        }
        OpenOptions::new().write(true).open(&from)?.sync_all()?;
        fs::rename(from, to)?;
        // Persist the name change before transmission too on Unix file systems.
        sync_dir(&self.dir)
    }

    pub fn release_claim(&self, report: &Report) -> io::Result<()> {
        let attempted = self.path("attempted");
        let pending = self.path("pending");
        let current = read_bounded(&attempted, MAX_RECORD).and_then(|s| Report::parse(&s));
        if current.as_ref() != Some(report) || pending.try_exists()? {
            return Err(io::Error::new(io::ErrorKind::NotFound, "diagnostic no longer claimed"));
        }
        fs::rename(attempted, pending)?;
        sync_dir(&self.dir)
    }

    pub fn discard(&self, attempted: bool) -> io::Result<()> {
        delete(&self.path(if attempted { "attempted" } else { "pending" }))
    }
}

fn read_bounded(path: &Path, max: u64) -> Option<String> {
    let m = fs::symlink_metadata(path).ok()?;
    if !m.is_file() || m.len() > max {
        return None;
    }
    let mut s = String::new();
    File::open(path).ok()?.take(max + 1).read_to_string(&mut s).ok()?;
    (s.len() as u64 <= max).then_some(s)
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

fn private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn delete(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

struct Capture {
    store: Store,
    ui_thread: ThreadId,
    // Zero = outside update, unfocused/minimized, or in a native modal dialog.
    // A new ticket for each active update means distinct frames cannot add up to a hang.
    active: AtomicU64,
    next_ticket: AtomicU64,
    captured: AtomicBool,
    stalled: AtomicBool,
}

static CAPTURE: OnceLock<Capture> = OnceLock::new();

pub fn init(dir: PathBuf, slot: u16) {
    // The existing instance lock makes slots 1 through 9 exclusive per process.
    if !(1..=9).contains(&slot) {
        return;
    }
    let store = Store::new(dir.join("diagnostics"), slot);
    let _ = fs::create_dir_all(&store.dir);
    let _ = delete(&store.path("record.tmp"));
    let capture = Capture {
        store,
        ui_thread: thread::current().id(),
        active: AtomicU64::new(0),
        next_ticket: AtomicU64::new(1),
        captured: AtomicBool::new(false),
        stalled: AtomicBool::new(false),
    };
    if CAPTURE.set(capture).is_err() {
        return;
    }
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record(Event::Panic);
        previous_hook(info);
    }));
    let _ = thread::Builder::new().name("qccom-ui-watchdog".into()).spawn(watchdog);
}

pub fn store() -> Option<Store> {
    CAPTURE.get().map(|c| c.store.clone())
}

/// A watchdog-generated report can be kept locally even if the app recovers.
/// Subsequent signals in the process do not overwrite a pending/attempted report.
fn record(event: Event) {
    if let Some(c) = CAPTURE.get() {
        if !c.captured.swap(true, Ordering::AcqRel) {
            let _ = c.store.write(&Report::current(event));
        }
    }
}

impl Capture {
    fn begin(&self) {
        self.active.store(self.next_ticket.fetch_add(1, Ordering::Relaxed), Ordering::Release);
    }
}

pub struct UiFrame(Option<&'static Capture>);

pub fn ui_frame(active: bool) -> UiFrame {
    let c = CAPTURE.get().filter(|c| active && c.ui_thread == thread::current().id());
    if let Some(c) = c {
        c.begin();
    }
    UiFrame(c)
}

impl Drop for UiFrame {
    fn drop(&mut self) {
        if let Some(c) = self.0 {
            c.active.store(0, Ordering::Release);
        }
    }
}

struct Pause(Option<&'static Capture>);

fn pause_ui() -> Pause {
    Pause(CAPTURE.get().filter(|c| c.ui_thread == thread::current().id() && c.active.swap(0, Ordering::AcqRel) != 0))
}

impl Drop for Pause {
    fn drop(&mut self) {
        if let Some(c) = self.0 {
            c.begin();
        }
    }
}

#[derive(Default)]
struct Watch {
    ticket: u64,
    observed: Duration,
}

impl Watch {
    fn sample(&mut self, ticket: u64, gap: Duration) -> bool {
        if ticket == 0 || ticket != self.ticket || gap > MAX_SAMPLE_GAP {
            self.ticket = ticket;
            self.observed = Duration::ZERO;
            return false;
        }
        self.observed = self.observed.saturating_add(gap);
        self.observed >= STALL
    }
}

fn watchdog() {
    let Some(c) = CAPTURE.get() else { return };
    let mut watch = Watch::default();
    let mut last = Instant::now();
    loop {
        thread::sleep(POLL);
        let now = Instant::now();
        let ticket = c.active.load(Ordering::Acquire);
        if watch.sample(ticket, now.duration_since(last)) && c.active.load(Ordering::Acquire) == ticket {
            c.stalled.store(true, Ordering::Release);
            record(Event::UiStall);
            return;
        }
        last = now;
    }
}

pub fn recovered_stall() -> bool {
    CAPTURE.get().is_some_and(|c| c.stalled.swap(false, Ordering::AcqRel))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("qccom-diag-test-{}-{nanos}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn panic_hook_records_event_without_payload() {
        const CHILD: &str = "QCCOM_TEST_PANIC_HOOK_DIR";
        const PRIVATE: &str = "private-serial-data-token";
        if let Some(dir) = std::env::var_os(CHILD) {
            init(PathBuf::from(dir), 1);
            let result = std::panic::catch_unwind(|| panic!("{PRIVATE}"));
            assert!(result.is_err());
            return;
        }

        let dir = test_dir();
        let run = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "diagnostics::tests::panic_hook_records_event_without_payload"])
            .env(CHILD, &dir)
            .output()
            .unwrap();
        assert!(run.status.success());
        let s = Store::new(dir.join("diagnostics"), 1);
        let saved = fs::read_to_string(s.path("pending")).unwrap();
        assert!(!saved.contains(PRIVATE));
        assert_eq!(s.previous(), Some((Report::current(Event::Panic), false)));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn strict_schema_does_not_accept_extras_or_arbitrary_text() {
        let good = format!("{SCHEMA}\nrust-panic\n1.9.8\nwindows\nx86_64\n");
        assert_eq!(Report::parse(&good).unwrap().version, [1, 9, 8]);
        assert!(Report::parse(&format!("{good}extra\n")).is_none());
        assert!(Report::parse(&good.replace("windows", "/home/user")).is_none());
        assert!(Report::parse(&good.replace("1.9.8", "1.9.8-token")).is_none());
        assert!(Report::parse(&good.replace("rust-panic", "panic message")).is_none());
    }

    #[test]
    fn consent_rejects_plaintext_and_embedded_credentials() {
        assert!(Consent::new(42, "http://example.invalid/api/v1".into()).is_none());
        assert!(Consent::new(42, "https://user:token@example.invalid/api/v1".into()).is_none());
        assert!(Consent::new(42, "https://example.invalid/api/v1?token=private".into()).is_none());
        assert!(Consent::new(42, "https://example.invalid/api/v1".into()).is_some());
    }

    #[test]
    fn ignores_idle_new_frames_dialog_pause_and_scheduled_or_suspend_gap() {
        let mut w = Watch::default();
        for i in 1..120 {
            assert!(!w.sample(0, POLL));
            assert!(!w.sample(i, POLL));
        }
        for _ in 0..19 {
            assert!(!w.sample(119, POLL));
        }
        assert!(!w.sample(119, Duration::from_secs(100)));
        for _ in 0..19 {
            assert!(!w.sample(119, POLL));
        }
        assert!(!w.sample(0, POLL));
        for _ in 0..120 {
            assert!(!w.sample(0, POLL));
        }
        for _ in 0..20 {
            assert!(!w.sample(120, POLL));
        }
        assert!(w.sample(120, POLL));
    }

    #[test]
    fn automatic_cooldown_uses_local_time_window_and_does_not_block_manual_claims() {
        let dir = test_dir();
        let s = Store::new(dir.clone(), 1);
        assert!(s.auto_ready(SystemTime::now()));
        s.mark_auto_attempt().unwrap();
        assert!(s.mark_auto_attempt().is_err());
        let stamp = fs::metadata(s.path("auto")).unwrap().modified().unwrap();
        assert!(!s.auto_ready(stamp - Duration::from_secs(1)));
        assert!(!s.auto_ready(stamp + AUTO_INTERVAL - Duration::from_secs(1)));
        assert!(s.auto_ready(stamp + AUTO_INTERVAL));
        let report = Report::current(Event::Panic);
        s.write(&report).unwrap();
        s.claim(&report).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn durable_claim_prevents_a_second_attempt_and_capture_is_bounded() {
        let dir = test_dir();
        let s = Store::new(dir.clone(), 1);
        let r = Report::current(Event::Panic);
        s.write(&r).unwrap();
        s.write(&Report::current(Event::UiStall)).unwrap();
        assert_eq!(s.previous(), Some((r.clone(), false)));
        s.claim(&r).unwrap();
        assert!(s.claim(&r).is_err());
        assert_eq!(s.previous(), Some((r.clone(), true)));
        s.release_claim(&r).unwrap();
        assert_eq!(s.previous(), Some((r.clone(), false)));
        s.claim(&r).unwrap();
        s.discard(true).unwrap();
        assert_eq!(s.previous(), None);
        assert_eq!(s.consent(), None);
        let c = Consent::new(42, "https://example.invalid/api/v1".into()).unwrap();
        s.set_consent(Some(&c)).unwrap();
        assert_eq!(s.consent(), Some(c));
        s.set_consent(None).unwrap();
        assert_eq!(s.consent(), None);
        fs::remove_dir_all(dir).unwrap();
    }
}
