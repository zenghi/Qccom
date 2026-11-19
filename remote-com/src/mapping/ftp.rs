//! Keep passive FTP clients on loopback, including clients that honor the address in 227.
use anyhow::{bail, ensure, Result};
use iroh::endpoint::{RecvStream, SendStream};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

async fn line(reader: &mut (impl AsyncBufRead + Unpin), out: &mut Vec<u8>) -> Result<bool> {
    out.clear();
    let n = reader.take(8193).read_until(b'\n', out).await?;
    ensure!(n <= 8192, "FTP control line too long");
    Ok(n > 0)
}
fn passive_reply(bytes: &[u8], range: (u16, u16)) -> Result<Option<Vec<u8>>> {
    if !bytes.starts_with(b"227 ") && !bytes.starts_with(b"229 ") {
        return Ok(None);
    }
    let text = std::str::from_utf8(bytes)?;
    let (_, body) = text.split_once('(').ok_or_else(|| anyhow::anyhow!("invalid passive reply"))?;
    let (tuple, _) = body.split_once(')').ok_or_else(|| anyhow::anyhow!("invalid passive reply"))?;
    let port = if bytes.starts_with(b"227 ") {
        let ns: Vec<u8> = tuple.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>()?;
        ensure!(ns.len() == 6, "invalid PASV endpoint");
        u16::from_be_bytes([ns[4], ns[5]])
    } else {
        let delim = tuple.chars().next().ok_or_else(|| anyhow::anyhow!("invalid EPSV endpoint"))?;
        let fields: Vec<_> = tuple.split(delim).collect();
        ensure!(fields.len() == 5 && fields[..3] == ["", "", ""] && fields[4].is_empty(), "invalid EPSV endpoint");
        fields[3].parse()?
    };
    if !(range.0..=range.1).contains(&port) {
        return Ok(Some(b"425 Data port outside configured QCCOM passive range\r\n".to_vec()));
    }
    if bytes.starts_with(b"227 ") {
        let [hi, lo] = port.to_be_bytes();
        return Ok(Some(format!("227 Entering Passive Mode (127,0,0,1,{hi},{lo})\r\n").into_bytes()));
    }
    Ok(None)
}
pub(super) async fn control(tcp: TcpStream, mut tx: SendStream, rx: RecvStream, range: (u16, u16)) -> Result<()> {
    let (read, mut write) = tcp.into_split();
    let commands = async {
        let mut reader = BufReader::new(rx);
        let mut buf = Vec::new();
        while line(&mut reader, &mut buf).await? {
            let verb = buf.split(|b| b.is_ascii_whitespace()).next().unwrap_or_default();
            if [b"PORT".as_slice(), b"EPRT", b"AUTH"].iter().any(|v| verb.eq_ignore_ascii_case(v)) {
                bail!("use passive FTP");
            }
            write.write_all(&buf).await?;
        }
        write.shutdown().await?;
        Ok::<_, anyhow::Error>(())
    };
    let replies = async {
        let mut reader = BufReader::new(read);
        let mut buf = Vec::new();
        while line(&mut reader, &mut buf).await? {
            let replacement = passive_reply(&buf, range)?;
            tx.write_all(replacement.as_deref().unwrap_or(&buf)).await?;
        }
        tx.finish()?;
        Ok::<_, anyhow::Error>(())
    };
    tokio::try_join!(commands, replies)?;
    Ok(())
}
