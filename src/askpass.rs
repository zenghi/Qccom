//! OpenSSH passwords and host confirmations use a one-time local channel.
//! The SSH child still checks known_hosts and handles authentication.
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream as BlockingStream},
    time::Duration,
};

use anyhow::{ensure, Context, Result};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::oneshot,
    task::JoinHandle,
    time::timeout,
};

const SOCKET_ENV: &str = "QCCOM_SFTP_ASKPASS_SOCKET";
const TOKEN_ENV: &str = "QCCOM_SFTP_ASKPASS_TOKEN";
const MAX_PROMPT: usize = 8192;
const MAX_ANSWER: usize = 1000;
const AUTH_WAIT: Duration = Duration::from_secs(180);
const IO_WAIT: Duration = Duration::from_secs(3);
const CANCELLED: u16 = u16::MAX;

pub(crate) struct AuthRequest {
    pub prompt: String,
    pub echo: bool,
    reply: oneshot::Sender<Option<String>>,
}

impl AuthRequest {
    pub fn answer(self, answer: Option<String>) {
        let valid = answer.filter(|s| s.len() <= MAX_ANSWER && !s.contains(['\r', '\n', '\0']));
        let _ = self.reply.send(valid);
    }
}

pub(crate) struct Broker(JoinHandle<()>);

impl Broker {
    pub async fn attach(command: &mut Command, notify: impl Fn(AuthRequest) + Send + Sync + 'static) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.context("could not open the local SSH authentication channel")?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        command
            .env(SOCKET_ENV, listener.local_addr()?.to_string())
            .env(TOKEN_ENV, &token)
            .env("SSH_ASKPASS", std::env::current_exe()?)
            .env("SSH_ASKPASS_REQUIRE", "force");
        Ok(Self(tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let Ok(Ok((echo, prompt))) = timeout(IO_WAIT, read_request(&mut stream, &token)).await else {
                    continue;
                };
                let (reply, response) = oneshot::channel();
                notify(AuthRequest { prompt, echo, reply });
                let answer = timeout(AUTH_WAIT, response).await.ok().and_then(Result::ok).flatten();
                let _ = timeout(IO_WAIT, send_answer(&mut stream, answer)).await;
            }
        })))
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn read_request(stream: &mut TcpStream, token: &str) -> Result<(bool, String)> {
    let mut supplied = [0u8; 32];
    stream.read_exact(&mut supplied).await?;
    ensure!(supplied == token.as_bytes(), "unexpected SSH authentication client");
    let echo = stream.read_u8().await? == 1;
    let len = usize::from(stream.read_u16().await?);
    ensure!(len <= MAX_PROMPT, "SSH authentication prompt is too long");
    let mut prompt = vec![0; len];
    stream.read_exact(&mut prompt).await?;
    let prompt = String::from_utf8(prompt)?;
    Ok((echo, prompt))
}

async fn send_answer(stream: &mut TcpStream, answer: Option<String>) -> std::io::Result<()> {
    match answer {
        Some(text) => {
            stream.write_u16(text.len() as u16).await?;
            stream.write_all(text.as_bytes()).await
        }
        None => stream.write_u16(CANCELLED).await,
    }
}

/// Invoked as a private OpenSSH helper before the GUI or instance listener starts.
pub(crate) fn run_helper() -> Option<bool> {
    let socket = std::env::var_os(SOCKET_ENV)?;
    let token = std::env::var(TOKEN_ENV).ok();
    Some(reply_to_ssh(&socket.to_string_lossy(), token.as_deref().unwrap_or_default()).is_ok())
}

fn reply_to_ssh(socket: &str, token: &str) -> Result<()> {
    let address = socket.parse::<SocketAddr>()?;
    ensure!(
        address.ip().is_loopback() && token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid SSH authentication channel"
    );
    let mut args = std::env::args_os().skip(1);
    let prompt = args.next().context("SSH authentication prompt missing")?;
    let prompt = prompt.to_string_lossy();
    ensure!(args.next().is_none() && prompt.len() <= MAX_PROMPT, "invalid SSH authentication prompt");
    let echo = std::env::var_os("SSH_ASKPASS_PROMPT").is_some_and(|kind| kind == "confirm");
    let mut stream = BlockingStream::connect_timeout(&address, IO_WAIT)?;
    stream.set_read_timeout(Some(AUTH_WAIT + IO_WAIT))?;
    stream.set_write_timeout(Some(IO_WAIT))?;
    stream.write_all(token.as_bytes())?;
    stream.write_all(&[u8::from(echo)])?;
    stream.write_all(&(prompt.len() as u16).to_be_bytes())?;
    stream.write_all(prompt.as_bytes())?;
    let mut len = [0; 2];
    stream.read_exact(&mut len)?;
    let len = usize::from(u16::from_be_bytes(len));
    ensure!(len <= MAX_ANSWER, "SSH authentication cancelled");
    let mut reply = vec![0; len];
    stream.read_exact(&mut reply)?;
    let mut output = std::io::stdout().lock();
    output.write_all(&reply)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broker_accepts_only_its_helper_and_delivers_the_prompt_reply() {
        let mut command = Command::new("ssh");
        let (notify, mut prompts) = tokio::sync::mpsc::unbounded_channel();
        let _broker = Broker::attach(&mut command, move |prompt| {
            let _ = notify.send(prompt);
        })
        .await
        .unwrap();
        let value = |name| {
            command
                .as_std()
                .get_envs()
                .find_map(|(key, value)| (key == name).then(|| value.unwrap().to_str().unwrap().to_owned()))
                .unwrap()
        };
        let socket = value(SOCKET_ENV);
        let mut denied = TcpStream::connect(&socket).await.unwrap();
        denied.write_all(&[b'0'; 32]).await.unwrap();
        assert_eq!(timeout(IO_WAIT, denied.read(&mut [0; 1])).await.unwrap().unwrap(), 0);
        assert!(prompts.try_recv().is_err());

        let mut helper = TcpStream::connect(&socket).await.unwrap();
        helper.write_all(value(TOKEN_ENV).as_bytes()).await.unwrap();
        helper.write_u8(0).await.unwrap();
        helper.write_u16(9).await.unwrap();
        helper.write_all(b"Password:").await.unwrap();
        let prompt = timeout(IO_WAIT, prompts.recv()).await.unwrap().unwrap();
        assert_eq!(prompt.prompt, "Password:");
        assert!(!prompt.echo);
        prompt.answer(Some("fixture".into()));
        assert_eq!(helper.read_u16().await.unwrap(), 7);
        let mut reply = [0; 7];
        helper.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"fixture");
    }
}
