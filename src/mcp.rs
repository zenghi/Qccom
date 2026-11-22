//! On-demand stdio MCP bridge. Only the selected application instance owns the device.
use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

const LIMIT: usize = 1 << 20;
pub type Answer = Result<Value, String>;
pub struct Call {
    pub name: String,
    pub args: Value,
    pub reply: mpsc::SyncSender<Answer>,
    pub deadline: std::time::Instant,
}
pub struct Bridge {
    pub address: SocketAddr,
    pub token: String,
    pub calls: mpsc::Receiver<Call>,
    alive: Arc<AtomicBool>,
}
fn message(r: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut line = Vec::new();
    let n = r.take(LIMIT as u64 + 1).read_until(b'\n', &mut line)?;
    if n == 0 {
        return Ok(None);
    }
    if n > LIMIT {
        return Err(io::Error::other("Message too large"));
    }
    serde_json::from_slice(&line).map(Some).map_err(io::Error::other)
}
fn send(w: &mut impl Write, v: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *w, v)?;
    w.write_all(b"\n")?;
    w.flush()
}
impl Bridge {
    pub fn open(ctx: eframe::egui::Context) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let token = uuid::Uuid::new_v4().to_string();
        let secret = token.clone();
        let alive = Arc::new(AtomicBool::new(true));
        let running = alive.clone();
        let (tx, calls) = mpsc::sync_channel(8);
        std::thread::Builder::new().name("qccom-mcp".into()).spawn(move || {
            for stream in listener.incoming() {
                if !running.load(Ordering::Acquire) {
                    break;
                }
                let Ok(mut stream) = stream else { break };
                let timeout = Some(Duration::from_secs(3));
                let _ = stream.set_read_timeout(timeout);
                let _ = stream.set_write_timeout(timeout);
                let request = message(&mut BufReader::new(&stream));
                let result: Answer = (|| {
                    let v = request.map_err(|e| e.to_string())?.ok_or("Missing request")?;
                    if v["token"].as_str() != Some(&secret) {
                        return Err("Invalid session".into());
                    }
                    let (reply, rx) = mpsc::sync_channel(1);
                    let duration = Duration::from_secs(3);
                    tx.try_send(Call {
                        name: v["name"].as_str().unwrap_or("").into(),
                        args: v["args"].clone(),
                        reply,
                        deadline: std::time::Instant::now() + duration,
                    })
                    .map_err(|_| "Session busy")?;
                    ctx.request_repaint();
                    rx.recv_timeout(duration).map_err(|_| "Session unavailable".to_string())?
                })();
                let _ = send(
                    &mut stream,
                    &match result {
                        Ok(value) => json!({"value":value}),
                        Err(e) => json!({"error":e}),
                    },
                );
            }
        })?;
        Ok(Self { address, token, calls, alive })
    }
    pub fn config(&self) -> Value {
        json!({"mcpServers":{"qccom":{"command":std::env::current_exe().unwrap_or_default(),"args":["--mcp",self.address.to_string()],"env":{"QCCOM_MCP_SESSION":self.token}}}})
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(50));
    }
}
fn tools() -> Value {
    let descriptors = [
        ("session_info", "Project, linked port, MCU tools, saved commands and build diagnostics", json!({})),
        (
            "read_events",
            "Latest serial/CAN/parameter events in a time range, up to 200 entries",
            json!({"from":{"type":"number"},"to":{"type":"number"},"limit":{"type":"integer","minimum":1,"maximum":200}}),
        ),
        (
            "read_signal",
            "Channel statistics, bounded samples and optional FFT peak; channel starts at 1",
            json!({"channel":{"type":"integer","minimum":1},"from":{"type":"number"},"to":{"type":"number"},"fft_hz":{"type":"number","exclusiveMinimum":0},"target":{"type":"number"},"tolerance":{"type":"number","exclusiveMinimum":0}}),
        ),
        (
            "compare_signal",
            "Compare a channel against the loaded reference capture",
            json!({"channel":{"type":"integer","minimum":1}}),
        ),
        (
            "capture_control",
            "Start or finish an in-memory capture in the shared session",
            json!({"action":{"type":"string","enum":["start","stop"]}}),
        ),
        (
            "set_parameter",
            "Send the configured tuning parameter value (within its configured limits)",
            json!({"value":{"type":"number"}}),
        ),
        ("saved_command", "Run a named serial command saved for this connection", json!({"name":{"type":"string"}})),
        (
            "mcu_task",
            "Use the project's selected MCU terminal to build or flash",
            json!({"action":{"type":"string","enum":["build","flash"]}}),
        ),
    ];
    json!(descriptors
        .into_iter()
        .map(|(name, description, properties)| {
            let mut schema = json!({"type":"object","properties":properties,"additionalProperties":false});
            if let Some(key) = match name {
                "capture_control" | "mcu_task" => Some("action"),
                "set_parameter" => Some("value"),
                "saved_command" => Some("name"),
                _ => None,
            } {
                schema["required"] = json!([key]);
            }
            json!({"name":name,"description":description,"inputSchema":schema})
        })
        .collect::<Vec<_>>())
}
pub fn stdio(args: &[String]) -> i32 {
    let Some(addr) = args.first().and_then(|s| s.parse::<SocketAddr>().ok()).filter(|a| a.ip().is_loopback()) else {
        return 2;
    };
    let Ok(token) = std::env::var("QCCOM_MCP_SESSION") else { return 2 };
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    loop {
        let request = match message(&mut input) {
            Ok(Some(v)) => v,
            Ok(None) => return 0,
            Err(_) => return 1,
        };
        let Some(id) = request.get("id") else { continue };
        let params = &request["params"];
        let result = match request["method"].as_str() {
            Some("initialize") => {
                let requested = params["protocolVersion"].as_str().unwrap_or("");
                let version = match requested {
                    "2025-03-26" | "2025-06-18" | "2025-11-25" => requested,
                    _ => "2025-11-25",
                };
                Ok(json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"QCCOM","version":env!("CARGO_PKG_VERSION")}}))
            }
            Some("ping") => Ok(json!({})),
            Some("tools/list") => Ok(json!({"tools":tools()})),
            Some("tools/call") => {
                let call = (|| -> io::Result<Value> {
                    let mut socket = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
                    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
                    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
                    send(&mut socket, &json!({"token":token,"name":params["name"],"args":params["arguments"]}))?;
                    message(&mut BufReader::new(socket))?.ok_or_else(|| io::Error::other("Session closed"))
                })();
                let v = call.unwrap_or_else(|e| json!({"error":e.to_string()}));
                let failed = v.get("error").is_some();
                let body = if failed { v["error"].as_str().unwrap_or("Error").into() } else { v["value"].to_string() };
                Ok(json!({"content":[{"type":"text","text":body}],"isError":failed}))
            }
            _ => Err(json!({"code":-32601,"message":"Method not found"})),
        };
        let response = match result {
            Ok(v) => json!({"jsonrpc":"2.0","id":id,"result":v}),
            Err(e) => json!({"jsonrpc":"2.0","id":id,"error":e}),
        };
        if send(&mut output, &response).is_err() {
            return 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authenticated_bridge_only_dispatches_the_shared_session() {
        let bridge = Bridge::open(eframe::egui::Context::default()).unwrap();
        let address = bridge.address;
        let request = |token: String| {
            std::thread::spawn(move || {
                let mut socket = TcpStream::connect(address).unwrap();
                send(&mut socket, &json!({"name":"session_info","args":{},"token":token})).unwrap();
                message(&mut BufReader::new(socket)).unwrap().unwrap()
            })
        };
        assert!(request("expired".into()).join().unwrap().get("error").is_some());
        assert!(bridge.calls.try_recv().is_err());
        let client = request(bridge.token.clone());
        let call = bridge.calls.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(call.name, "session_info");
        call.reply.send(Ok(json!({"port":"COM30"}))).unwrap();
        assert_eq!(client.join().unwrap()["value"]["port"], "COM30");
    }
}
