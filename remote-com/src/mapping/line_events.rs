//! Ordered com0com escape insertions. Reads and network frames can split any record.
use anyhow::{bail, Result};
pub(super) const ESC: u8 = 0xff;
#[derive(Debug, PartialEq)]
pub(super) enum Event<'a> {
    Data(&'a [u8]),
    Baud(u32),
    Format(u8, u8, u8),
    Pins(u8),
    Break(bool),
}
#[derive(Default)]
pub(super) struct Decoder(Vec<u8>);
impl Decoder {
    pub fn feed(&mut self, bytes: &[u8], mut apply: impl FnMut(Event<'_>) -> Result<()>) -> Result<()> {
        self.0.extend_from_slice(bytes);
        let mut i = 0;
        while i < self.0.len() {
            let s = &self.0[i..];
            if s[0] != ESC {
                let end = s.iter().position(|&b| b == ESC).unwrap_or(s.len());
                apply(Event::Data(&s[..end]))?;
                i += end;
                continue;
            }
            let Some(&kind) = s.get(1) else {
                break;
            };
            let len = match kind {
                0 => 2,
                1 => 4,
                2 | 3 => 3,
                16 => 6,
                17 => 5,
                _ => bail!("未知虚拟串口控制事件 {kind}"),
            };
            if s.len() < len {
                break;
            }
            match kind {
                0 => apply(Event::Data(&[ESC]))?,
                1 | 2 => {
                    apply(Event::Break(s[2] & 0x10 != 0))?;
                    if kind == 1 {
                        apply(Event::Data(&s[3..4]))?;
                    }
                }
                3 => apply(Event::Pins(s[2]))?,
                16 => apply(Event::Baud(u32::from_le_bytes(s[2..6].try_into()?)))?,
                17 => apply(Event::Format(s[2], s[3], s[4]))?,
                _ => unreachable!(),
            }
            i += len;
        }
        self.0.drain(..i);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_events_stay_between_bytes() {
        let bytes = [42, ESC, 16, 0, 194, 1, 0, ESC, 17, 8, 0, 0, ESC, 3, 0x30, ESC, 2, 0x10, ESC, 0, 99];
        let mut d = Decoder::default();
        let mut events = Vec::new();
        for b in bytes {
            d.feed(&[b], |e| {
                events.push(format!("{e:?}"));
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(events, ["Data([42])", "Baud(115200)", "Format(8, 0, 0)", "Pins(48)", "Break(true)", "Data([255])", "Data([99])"]);
    }
}
