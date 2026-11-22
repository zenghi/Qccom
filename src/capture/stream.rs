//! Sequential capture file. GUI producers only enqueue bounded chunks; a slow disk is reported.
use super::*;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, SyncSender};
const MAGIC: &[u8; 8] = b"QCREC01\n";
const CHUNK: usize = 64 * 1024;
pub struct Stream {
    tx: Option<SyncSender<Vec<u8>>>,
    status: Receiver<io::Result<()>>,
    buf: Vec<u8>,
}
impl Stream {
    pub fn create(path: &Path, c: &Capture) -> io::Result<Self> {
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let mut w = BufWriter::new(file);
        w.write_all(MAGIC)?;
        let meta = serde_json::to_vec(&(c.project.as_str(), c.port.as_str(), c.firmware.as_str(), &c.parameters))?;
        w.write_all(&(meta.len() as u32).to_le_bytes())?;
        w.write_all(&meta)?;
        w.flush()?;
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(32);
        let (done, status) = mpsc::channel();
        std::thread::Builder::new().name("qccom-recorder".into()).spawn(move || {
            let r = (|| {
                while let Ok(buf) = rx.recv() {
                    w.write_all(&buf)?
                }
                w.flush()?;
                w.get_ref().sync_data()
            })();
            let _ = done.send(r);
        })?;
        let mut s = Self {
            tx: Some(tx),
            status,
            buf: Vec::with_capacity(CHUNK + 1024),
        };
        for (i, ch) in c.channels.iter().enumerate() {
            s.channel(i, &ch.name, &ch.unit)
        }
        Ok(s)
    }
    fn string(&mut self, s: &str) {
        let mut n = s.len().min(4096);
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        self.buf.extend((n as u16).to_le_bytes());
        self.buf.extend(&s.as_bytes()[..n]);
    }
    pub fn channel(&mut self, id: usize, name: &str, unit: &str) {
        self.buf.extend([1, id as u8]);
        self.string(name);
        self.string(unit);
    }
    pub fn sample(&mut self, t: f64, id: usize, v: f64) {
        self.buf.extend([2, id as u8]);
        self.buf.extend(t.to_le_bytes());
        self.buf.extend(v.to_le_bytes());
    }
    pub fn event(&mut self, t: f64, source: &str, text: &str) {
        self.buf.push(3);
        self.buf.extend(t.to_le_bytes());
        self.string(source);
        self.string(text);
    }
    pub fn flush_batch(&mut self, force: bool) -> io::Result<()> {
        if let Ok(r) = self.status.try_recv() {
            r?;
            return Err(io::Error::other("Recorder stopped"));
        }
        if self.buf.is_empty() || !force && self.buf.len() < CHUNK {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK + 1024));
        self.tx
            .as_ref()
            .ok_or_else(|| io::Error::other("Recorder closed"))?
            .try_send(chunk)
            .map_err(|_| io::Error::other("Recording stopped: disk queue is full or writer failed"))
    }
    pub fn finish(mut self) -> io::Result<()> {
        self.flush_batch(true)?;
        self.tx.take();
        self.status.recv().map_err(|_| io::Error::other("Recorder disconnected"))?
    }
}
// Dispose the last partial chunk and wait only when this session/process closes.
impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            if !self.buf.is_empty() {
                let _ = tx.send(std::mem::take(&mut self.buf));
            }
            drop(tx);
            let _ = self.status.recv();
        }
    }
}
pub fn load(path: &Path, window: Option<(f64, f64)>) -> io::Result<Capture> {
    let mut r = BufReader::with_capacity(128 * 1024, File::open(path)?);
    let err = || io::Error::other("Invalid QCCOM stream");
    let mut magic = [0; 8];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(err());
    }
    let mut n = [0; 4];
    r.read_exact(&mut n)?;
    let n = u32::from_le_bytes(n) as usize;
    if n > 65536 {
        return Err(err());
    }
    let mut meta = vec![0; n];
    r.read_exact(&mut meta)?;
    let (project, port, firmware, parameters) = serde_json::from_slice(&meta)?;
    let mut c = Capture {
        project,
        port,
        firmware,
        parameters,
        ..Default::default()
    };
    let f64read = |r: &mut BufReader<File>| -> io::Result<f64> {
        let mut data = [0; 8];
        r.read_exact(&mut data)?;
        let x = f64::from_le_bytes(data);
        if !x.is_finite() {
            return Err(err());
        }
        Ok(x)
    };
    let string = |r: &mut BufReader<File>| -> io::Result<String> {
        let mut len = [0; 2];
        r.read_exact(&mut len)?;
        let n = u16::from_le_bytes(len) as usize;
        if n > 4096 {
            return Err(err());
        }
        let mut s = vec![0; n];
        r.read_exact(&mut s)?;
        String::from_utf8(s).map_err(|_| err())
    };
    let (a, b) = window.unwrap_or((0., f64::MAX));
    let mut byte = [0];
    while r.read(&mut byte)? == 1 {
        match byte[0] {
            1 => {
                r.read_exact(&mut byte)?;
                let id = byte[0] as usize;
                if id > c.channels.len() || id >= CHANNELS {
                    return Err(err());
                }
                let name = string(&mut r)?;
                let unit = string(&mut r)?;
                if id == c.channels.len() {
                    c.channels.push(Channel { name, unit, ..Default::default() });
                } else {
                    c.channels[id].name = name;
                    c.channels[id].unit = unit;
                }
            }
            2 => {
                r.read_exact(&mut byte)?;
                let id = byte[0] as usize;
                if id >= c.channels.len() {
                    return Err(err());
                }
                let t = f64read(&mut r)?;
                let v = f64read(&mut r)?;
                if (a..=b).contains(&t) {
                    if c.channels[id].points.back().is_some_and(|p| p.0 > t - a) {
                        return Err(err());
                    }
                    c.point(id, t - a, v);
                }
            }
            3 => {
                let t = f64read(&mut r)?;
                let source = string(&mut r)?;
                let text = string(&mut r)?;
                if (a..=b).contains(&t) {
                    c.event(t - a, &source, text);
                }
            }
            _ => return Err(err()),
        }
    }
    c.frames = c.channels.iter().map(|ch| ch.points.len() as u64).max().unwrap_or(0);
    Ok(c)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streamed_data_can_be_reopened_in_a_time_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("motor.qrec");
        let mut rec = Stream::create(&path, &Capture::default()).unwrap();
        rec.channel(0, "Speed", "rpm");
        for n in 0..10000 {
            rec.sample(n as f64 / 1000., 0, n as f64);
            rec.flush_batch(false).unwrap();
        }
        rec.event(2.0, "PARAM", "kp=0.25");
        rec.finish().unwrap();
        let c = load(&path, Some((2., 3.))).unwrap();
        assert_eq!(c.channels[0].points.len(), 1001);
        assert_eq!(c.channels[0].points.front(), Some(&(0., 2000.)));
        assert_eq!(c.events[0].t, 0.);
    }
}
