//! Bounded, line-preserving rotation for local log recordings.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) const DEFAULT_SPLIT_MIB: u32 = 64;

#[derive(Clone, Copy, Default)]
pub(crate) struct Limits {
    bytes: u64,
    age: Duration,
}

impl Limits {
    pub(crate) fn new(mib: u32, minutes: u32) -> Self {
        Self {
            bytes: u64::from(mib) * 1024 * 1024,
            age: Duration::from_secs(u64::from(minutes) * 60),
        }
    }
}

pub(crate) struct Recorder {
    writer: BufWriter<File>,
    base: PathBuf,
    path: PathBuf,
    bytes: u64,
    started: Option<Instant>,
    part: u64,
}

impl Recorder {
    pub(crate) fn start(path: PathBuf) -> io::Result<Self> {
        let file = create_new(&path).map_err(|e| with_path(e, &path))?;
        Ok(Self {
            writer: BufWriter::new(file),
            base: path.clone(),
            path,
            bytes: 0,
            started: None,
            part: 1,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Rotate before the next line, not after a whole UI batch. An individual
    /// line larger than the limit gets its own file rather than being broken.
    pub(crate) fn append(&mut self, line: &str, limits: Limits) -> io::Result<bool> {
        self.append_at(line, limits, Instant::now())
    }

    fn append_at(&mut self, line: &str, limits: Limits, now: Instant) -> io::Result<bool> {
        let bytes = line.len() as u64 + 1; // UTF-8 bytes plus the exact on-disk LF
        let size_reached = limits.bytes != 0 && self.bytes.saturating_add(bytes) > limits.bytes;
        let age_reached = !limits.age.is_zero() && self.started.is_some_and(|at| now.saturating_duration_since(at) >= limits.age);
        let rotate = self.bytes != 0 && (size_reached || age_reached);
        if rotate {
            self.rotate()?;
        }
        self.writer
            .write_all(line.as_bytes())
            .and_then(|()| self.writer.write_all(b"\n"))
            .map_err(|e| with_path(e, &self.path))?;
        self.bytes = self.bytes.saturating_add(bytes);
        self.started.get_or_insert(now);
        Ok(rotate)
    }

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        self.writer.flush().map_err(|e| with_path(e, &self.path))
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.flush()?;
        let mut part = self.part;
        loop {
            part = part.checked_add(1).ok_or_else(|| io::Error::other("recording part number overflow"))?;
            let path = part_path(&self.base, part);
            // Atomic even if another process creates this name concurrently.
            match create_new(&path) {
                Ok(file) => {
                    self.writer = BufWriter::new(file);
                    self.path = path;
                    self.bytes = 0;
                    self.started = None;
                    self.part = part;
                    return Ok(());
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(with_path(e, &path)),
            }
        }
    }
}

fn create_new(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

fn with_path(error: io::Error, path: &Path) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

fn part_path(base: &Path, part: u64) -> PathBuf {
    let mut name = base.file_stem().map(OsString::from).unwrap_or_default();
    name.push(format!("_{part:03}"));
    if let Some(ext) = base.extension() {
        name.push(".");
        name.push(ext);
    }
    base.with_file_name(name)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(crate) struct TempDir(pub(crate) PathBuf);

    impl TempDir {
        pub(crate) fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            loop {
                let path = std::env::temp_dir().join(format!("qccom-rec-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("cannot create test directory: {e}"),
                }
            }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn size(bytes: u64) -> Limits {
        Limits { bytes, ..Limits::default() }
    }

    #[test]
    fn rotates_inside_a_batch_and_counts_utf8_plus_lf() {
        let dir = TempDir::new();
        let base = dir.0.join("串口.log");
        let mut rec = Recorder::start(base.clone()).unwrap();
        assert!(!rec.append("你好", size(11)).unwrap()); // 7 bytes
        assert!(!rec.append("123", size(11)).unwrap()); // 4 bytes, exactly at limit
        rec.flush().unwrap();
        assert!(!dir.0.join("串口_002.log").exists()); // No empty trailing file
        assert!(rec.append("456", size(11)).unwrap());
        assert!(rec.append("再见", size(10)).unwrap()); // New limit takes effect
        rec.flush().unwrap();
        assert_eq!(fs::read(&base).unwrap(), "你好\n123\n".as_bytes());
        assert_eq!(fs::read(dir.0.join("串口_002.log")).unwrap(), b"456\n");
        assert_eq!(fs::read(rec.path()).unwrap(), "再见\n".as_bytes());
    }

    #[test]
    fn zero_disables_size_rotation_and_oversize_lines_stay_whole() {
        let dir = TempDir::new();
        let base = dir.0.join("capture");
        let mut rec = Recorder::start(base.clone()).unwrap();
        assert!(!rec.append("over limit", size(1)).unwrap());
        assert!(rec.append("b", size(1)).unwrap());
        assert!(!rec.append("ccc", Limits::new(0, 0)).unwrap());
        rec.flush().unwrap();
        assert_eq!(fs::read(&base).unwrap(), b"over limit\n");
        assert_eq!(rec.path(), dir.0.join("capture_002"));
        assert_eq!(fs::read(rec.path()).unwrap(), b"b\nccc\n");
    }

    #[test]
    fn duration_starts_on_first_line_and_rotates_when_data_resumes() {
        let dir = TempDir::new();
        let mut rec = Recorder::start(dir.0.join("capture.log")).unwrap();
        let start = Instant::now() + Duration::from_secs(600);
        let policy = Limits::new(0, 1);
        assert!(!rec.append_at("first", policy, start).unwrap());
        assert!(!rec.append_at("second", policy, start + Duration::from_secs(59)).unwrap());
        assert!(rec.append_at("third", policy, start + Duration::from_secs(60)).unwrap());
        rec.flush().unwrap();
        assert_eq!(fs::read(rec.path()).unwrap(), b"third\n");
        assert!(!dir.0.join("capture_003.log").exists());
    }

    #[test]
    fn never_overwrites_existing_base_or_parts() {
        let dir = TempDir::new();
        let old = dir.0.join("old.log");
        fs::write(&old, b"old capture").unwrap();
        assert_eq!(Recorder::start(old.clone()).err().unwrap().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(old).unwrap(), b"old capture");

        let mut rec = Recorder::start(dir.0.join("new.log")).unwrap();
        let occupied = dir.0.join("new_002.log");
        fs::write(&occupied, b"another capture").unwrap();
        assert!(!rec.append("a", size(2)).unwrap());
        assert!(rec.append("b", size(2)).unwrap());
        assert_eq!(rec.path(), dir.0.join("new_003.log"));
        assert_eq!(fs::read(occupied).unwrap(), b"another capture");
    }

    #[cfg(unix)]
    #[test]
    fn skips_symlink_without_creating_its_target() {
        let dir = TempDir::new();
        let base = dir.0.join("capture");
        let dangling = dir.0.join("other_file");
        std::os::unix::fs::symlink(&dangling, dir.0.join("capture_002")).unwrap();
        let mut rec = Recorder::start(base).unwrap();
        assert!(!rec.append("a", size(2)).unwrap());
        assert!(rec.append("b", size(2)).unwrap());
        assert_eq!(rec.path(), dir.0.join("capture_003"));
        assert!(!dangling.exists());
    }

    #[test]
    fn rotation_error_preserves_previous_part() {
        let dir = TempDir::new();
        let path = dir.0.join("capture.log");
        let mut rec = Recorder::start(path.clone()).unwrap();
        assert!(!rec.append("saved", size(6)).unwrap());
        // Force the next part's directory to be missing without disturbing
        // the open file (portable across file locking semantics).
        rec.base = dir.0.join("missing").join("capture.log");
        assert_eq!(rec.append("unsaved", size(6)).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(rec.path(), path);
        assert_eq!(fs::read(&path).unwrap(), b"saved\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn flush_reports_disk_write_error() {
        let dir = TempDir::new();
        let mut rec = Recorder::start(dir.0.join("capture.log")).unwrap();
        rec.writer = BufWriter::new(File::options().write(true).open("/dev/full").unwrap());
        rec.append("buffered", size(0)).unwrap();
        assert!(rec.flush().is_err());
    }
}
