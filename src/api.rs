//! API 调试（类似 Postman）：拼请求、变量替换、发送、cURL 导入导出、JSON 美化。
//! 界面在 app/apiview.rs；这里不碰 egui，方便测试。

use std::io::Read;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::i18n::{t, tf};

pub const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// 响应体最多读这么多，再大的只保留开头
const MAX_BODY: u64 = 32 * 1024 * 1024;

#[derive(Clone, Default, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Kv {
    pub on: bool,
    pub k: String,
    pub v: String,
}

impl Kv {
    pub fn new(k: &str, v: &str) -> Kv {
        Kv { on: true, k: k.into(), v: v.into() }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize, Debug)]
pub enum BodyKind {
    #[default]
    None,
    Json,
    Form,
    Text,
}

#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize, Debug)]
pub enum AuthKind {
    #[default]
    None,
    Bearer,
    Basic,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Request {
    pub name: String,
    pub method: String,
    pub url: String,
    pub params: Vec<Kv>,
    pub headers: Vec<Kv>,
    pub body_kind: BodyKind,
    pub body: String,
    pub form: Vec<Kv>,
    pub auth: AuthKind,
    pub token: String,
    pub user: String,
    pub pass: String,
}

impl Default for Request {
    fn default() -> Self {
        Request {
            name: String::new(),
            method: "GET".into(),
            url: String::new(),
            params: vec![],
            headers: vec![],
            body_kind: BodyKind::None,
            body: String::new(),
            form: vec![],
            auth: AuthKind::None,
            token: String::new(),
            user: String::new(),
            pass: String::new(),
        }
    }
}

impl Request {
    /// 列表里显示的名字：起了名用名字，否则用去掉协议的 URL
    pub fn title(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        let u = self.url.trim();
        let u = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://")).unwrap_or(u);
        if u.is_empty() {
            t("未命名请求").to_string()
        } else {
            u.to_string()
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct HistItem {
    pub req: Request,
    pub status: u16,
    pub ms: u64,
    /// 本地时间 "MM-DD HH:MM:SS"
    pub at: String,
}

impl Default for HistItem {
    fn default() -> Self {
        HistItem {
            req: Request::default(),
            status: 0,
            ms: 0,
            at: String::new(),
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct ApiSettings {
    pub current: Request,
    pub saved: Vec<Request>,
    pub history: Vec<HistItem>,
    /// 环境变量：URL、请求头、请求体里的 {{名字}} 会被替换
    pub env: Vec<Kv>,
    pub timeout_s: u32,
    pub follow_redirects: bool,
}

impl Default for ApiSettings {
    fn default() -> Self {
        ApiSettings {
            current: Request {
                url: "{{base}}/get".into(),
                params: vec![Kv::new("device", "qccom")],
                ..Default::default()
            },
            saved: vec![],
            history: vec![],
            env: vec![Kv::new("base", "https://httpbin.org")],
            timeout_s: 30,
            follow_redirects: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub truncated: bool,
    pub ms: u64,
    pub url: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn is_json(&self) -> bool {
        self.header("content-type").is_some_and(|c| c.contains("json")) || {
            let b = self.body.iter().find(|b| !b.is_ascii_whitespace());
            matches!(b, Some(b'{') | Some(b'['))
        }
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// 把 {{名字}} 换成环境变量的值；找不到的保持原样
pub fn subst(s: &str, env: &[Kv]) -> String {
    if !s.contains("{{") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        match after.find("}}") {
            Some(j) => {
                let name = after[..j].trim();
                match env.iter().find(|e| e.on && e.k == name) {
                    Some(e) => out.push_str(&e.v),
                    None => out.push_str(&rest[i..i + 4 + j]),
                }
                rest = &after[j + 2..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// URL 里用到了但没有定义的变量
pub fn missing_vars(req: &Request, env: &[Kv]) -> Vec<String> {
    let mut texts = vec![req.url.as_str(), req.body.as_str(), req.token.as_str()];
    for kv in req.params.iter().chain(&req.headers).chain(&req.form).filter(|k| k.on) {
        texts.push(&kv.k);
        texts.push(&kv.v);
    }
    let mut out: Vec<String> = vec![];
    for s in texts {
        let mut rest = s;
        while let Some(i) = rest.find("{{") {
            let after = &rest[i + 2..];
            let Some(j) = after.find("}}") else { break };
            let name = after[..j].trim().to_string();
            if !env.iter().any(|e| e.on && e.k == name) && !out.contains(&name) {
                out.push(name);
            }
            rest = &after[j + 2..];
        }
    }
    out
}

/// 百分号编码（查询参数、表单用）
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 地址栏里手写的 ?k=v 挪进参数表：同名参数改值，没有的追加（不然会和参数表重复拼两次）
pub fn absorb_query(req: &mut Request) {
    let Some((base, q)) = req.url.split_once('?') else { return };
    let (base, q) = (base.to_string(), q.to_string());
    for p in q.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = p.split_once('=').unwrap_or((p, ""));
        let (k, v) = (decode(k), decode(v));
        match req.params.iter_mut().find(|x| x.k == k) {
            Some(x) => {
                x.v = v;
                x.on = true;
            }
            None => req.params.push(Kv::new(&k, &v)),
        }
    }
    req.url = base;
}

pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(v) => {
                    out.push(v);
                    i += 3;
                    continue;
                }
                None => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 完整的请求地址：变量替换 + 查询参数；没写协议的补上 http://
pub fn full_url(req: &Request, env: &[Kv]) -> String {
    let mut url = subst(req.url.trim(), env);
    if !url.is_empty() && !url.contains("://") {
        url = format!("http://{url}");
    }
    let q: Vec<String> = req
        .params
        .iter()
        .filter(|p| p.on && !p.k.is_empty())
        .map(|p| format!("{}={}", encode(&subst(&p.k, env)), encode(&subst(&p.v, env))))
        .collect();
    if !q.is_empty() {
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(&q.join("&"));
    }
    url
}

/// 实际要发的请求头（含自动加的 Content-Type 和认证）
pub fn effective_headers(req: &Request, env: &[Kv]) -> Vec<(String, String)> {
    let mut h: Vec<(String, String)> = req.headers.iter().filter(|h| h.on && !h.k.is_empty()).map(|h| (subst(&h.k, env), subst(&h.v, env))).collect();
    let has = |h: &[(String, String)], n: &str| h.iter().any(|(k, _)| k.eq_ignore_ascii_case(n));
    let ct = match req.body_kind {
        BodyKind::Json => Some("application/json"),
        BodyKind::Form => Some("application/x-www-form-urlencoded"),
        BodyKind::Text => Some("text/plain; charset=utf-8"),
        BodyKind::None => None,
    };
    if let Some(ct) = ct {
        if !has(&h, "content-type") {
            h.push(("Content-Type".into(), ct.into()));
        }
    }
    if !has(&h, "authorization") {
        match req.auth {
            AuthKind::Bearer if !req.token.is_empty() => h.push(("Authorization".into(), format!("Bearer {}", subst(&req.token, env)))),
            AuthKind::Basic => {
                let raw = format!("{}:{}", subst(&req.user, env), subst(&req.pass, env));
                h.push(("Authorization".into(), format!("Basic {}", base64(raw.as_bytes()))));
            }
            _ => {}
        }
    }
    h
}

pub fn body_bytes(req: &Request, env: &[Kv]) -> Option<Vec<u8>> {
    match req.body_kind {
        BodyKind::None => None,
        BodyKind::Json | BodyKind::Text => Some(subst(&req.body, env).into_bytes()),
        BodyKind::Form => Some(
            req.form
                .iter()
                .filter(|f| f.on && !f.k.is_empty())
                .map(|f| format!("{}={}", encode(&subst(&f.k, env)), encode(&subst(&f.v, env))))
                .collect::<Vec<_>>()
                .join("&")
                .into_bytes(),
        ),
    }
}

pub fn base64(d: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(d.len().div_ceil(3) * 4);
    for c in d.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// 本机、局域网地址（开发板通常在这里）和 NO_PROXY 里列出的主机不走环境变量里的代理
fn use_proxy(url: &str) -> bool {
    let host = url.split("://").nth(1).unwrap_or(url);
    let host = host.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = if host.starts_with('[') {
        host.split(']').next().unwrap_or("").trim_start_matches('[')
    } else {
        host.split(':').next().unwrap_or("")
    }
    .to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".local") || host == "::1" {
        return false;
    }
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        if ip.is_loopback() || ip.is_private() || ip.is_link_local() {
            return false;
        }
    }
    let no = std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")).unwrap_or_default();
    !no.split(',')
        .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .any(|n| n == "*" || host == n || host.ends_with(&format!(".{n}")))
}

/// 发送请求（阻塞，在后台线程里调用）。4xx / 5xx 也算拿到了响应。
pub fn send(req: &Request, env: &[Kv], timeout_s: u32, follow: bool) -> Result<Response, String> {
    let url = full_url(req, env);
    if url.is_empty() {
        return Err(t("请输入地址").into());
    }
    #[allow(unused_mut)]
    let mut b = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(timeout_s.max(1) as u64))
        .redirects(if follow { 8 } else { 0 })
        .try_proxy_from_env(use_proxy(&url))
        .user_agent(concat!("QCCOM/", env!("CARGO_PKG_VERSION")));
    #[cfg(windows)]
    {
        b = b.tls_connector(std::sync::Arc::new(native_tls::TlsConnector::new().map_err(|e| e.to_string())?));
    }
    let agent = b.build();
    let mut r = agent.request(&req.method, &url);
    for (k, v) in effective_headers(req, env) {
        r = r.set(&k, &v);
    }
    let start = Instant::now();
    let res = match body_bytes(req, env) {
        Some(body) => r.send_bytes(&body),
        None => r.call(),
    };
    let resp = match res {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(ureq::Error::Transport(e)) => return Err(e.to_string()),
    };
    let status = resp.status();
    let status_text = resp.status_text().to_string();
    let final_url = resp.get_url().to_string();
    let headers: Vec<(String, String)> = resp.headers_names().iter().filter_map(|n| Some((n.clone(), resp.header(n)?.to_string()))).collect();
    let mut body = vec![];
    resp.into_reader().take(MAX_BODY + 1).read_to_end(&mut body).map_err(|e| e.to_string())?;
    let truncated = body.len() as u64 > MAX_BODY;
    body.truncate(MAX_BODY as usize);
    Ok(Response {
        status,
        status_text,
        headers,
        body,
        truncated,
        ms: start.elapsed().as_millis() as u64,
        url: final_url,
    })
}

// ---------------------------------------------------------------- cURL

fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// 导出成 cURL 命令（变量已替换）
pub fn to_curl(req: &Request, env: &[Kv]) -> String {
    let mut parts = vec!["curl".to_string()];
    if req.method != "GET" || req.body_kind != BodyKind::None {
        if req.method == "HEAD" {
            parts.push("-I".into());
        } else {
            parts.push(format!("-X {}", req.method));
        }
    }
    parts.push(shell_quote(&full_url(req, env)));
    for (k, v) in effective_headers(req, env) {
        parts.push(format!("-H {}", shell_quote(&format!("{k}: {v}"))));
    }
    if let Some(b) = body_bytes(req, env) {
        parts.push(format!("--data-raw {}", shell_quote(&String::from_utf8_lossy(&b))));
    }
    parts.join(" \\\n  ")
}

/// 按 shell 规则切分（单双引号、反斜杠、续行）
fn shell_split(s: &str) -> Result<Vec<String>, String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut has = false;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\'' => {
                has = true;
                loop {
                    match it.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return Err(t("引号没有闭合").into()),
                    }
                }
            }
            '"' => {
                has = true;
                loop {
                    match it.next() {
                        Some('"') => break,
                        Some('\\') => match it.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => cur.push(c),
                            Some('\n') => {}
                            Some(c) => {
                                cur.push('\\');
                                cur.push(c);
                            }
                            None => return Err(t("引号没有闭合").into()),
                        },
                        Some(c) => cur.push(c),
                        None => return Err(t("引号没有闭合").into()),
                    }
                }
            }
            '\\' => match it.next() {
                Some('\n') | Some('\r') => {}
                Some(c) => {
                    has = true;
                    cur.push(c);
                }
                None => {}
            },
            // Windows cmd 的续行符 ^
            '^' if it.peek().is_some_and(|c| *c == '\n' || *c == '\r') => {}
            c if c.is_whitespace() => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                has = true;
                cur.push(c);
            }
        }
    }
    if has {
        out.push(cur);
    }
    Ok(out)
}

/// 从 cURL 命令解析出请求（浏览器开发者工具「复制为 cURL」的结果可以直接粘贴）
pub fn from_curl(cmd: &str) -> Result<Request, String> {
    let args = shell_split(cmd.trim())?;
    let mut it = args.into_iter().peekable();
    match it.next() {
        Some(c) if c == "curl" || c.ends_with("/curl") || c.eq_ignore_ascii_case("curl.exe") => {}
        _ => return Err(t("不是 curl 命令").into()),
    }
    let mut req = Request::default();
    let mut method: Option<String> = None;
    let mut data: Vec<String> = vec![];
    let mut json = false;
    let mut get = false;
    while let Some(a) = it.next() {
        let val = |it: &mut std::iter::Peekable<std::vec::IntoIter<String>>| it.next().unwrap_or_default();
        match a.as_str() {
            "-X" | "--request" => method = Some(val(&mut it).to_uppercase()),
            "-H" | "--header" => {
                let h = val(&mut it);
                if let Some((k, v)) = h.split_once(':') {
                    req.headers.push(Kv::new(k.trim(), v.trim()));
                }
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-ascii" => data.push(val(&mut it)),
            "--data-urlencode" => {
                let d = val(&mut it);
                data.push(match d.split_once('=') {
                    Some((k, v)) => format!("{k}={}", encode(v)),
                    None => encode(&d),
                });
            }
            "--json" => {
                data.push(val(&mut it));
                json = true;
            }
            "-u" | "--user" => {
                let u = val(&mut it);
                let (user, pass) = u.split_once(':').unwrap_or((&u, ""));
                req.auth = AuthKind::Basic;
                req.user = user.into();
                req.pass = pass.into();
            }
            "-G" | "--get" => get = true,
            "-I" | "--head" => method = Some("HEAD".into()),
            "--url" => req.url = val(&mut it),
            "-A" | "--user-agent" => req.headers.push(Kv::new("User-Agent", &val(&mut it))),
            "-e" | "--referer" => req.headers.push(Kv::new("Referer", &val(&mut it))),
            "-b" | "--cookie" => req.headers.push(Kv::new("Cookie", &val(&mut it))),
            // 带参数但和请求内容无关的选项
            "-o" | "--output" | "-m" | "--max-time" | "--connect-timeout" | "-x" | "--proxy" | "-w" | "--write-out" | "--retry" | "-c" | "--cookie-jar" => {
                val(&mut it);
            }
            s if s.starts_with('-') => {}
            s => {
                if req.url.is_empty() {
                    req.url = s.to_string();
                }
            }
        }
    }
    if req.url.is_empty() {
        return Err(t("没有找到地址").into());
    }
    // 地址里的查询参数拆到参数表里，方便改
    if let Some((base, q)) = req.url.clone().split_once('?') {
        req.url = base.to_string();
        for p in q.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            req.params.push(Kv::new(&decode(k), &decode(v)));
        }
    }
    let ct = req.headers.iter().find(|h| h.k.eq_ignore_ascii_case("content-type")).map(|h| h.v.to_ascii_lowercase());
    if !data.is_empty() {
        let body = data.join("&");
        if get {
            for p in body.split('&').filter(|p| !p.is_empty()) {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                req.params.push(Kv::new(&decode(k), &decode(v)));
            }
        } else if json || ct.as_deref().is_some_and(|c| c.contains("json")) || body.trim_start().starts_with(['{', '[']) {
            req.body_kind = BodyKind::Json;
            req.body = pretty_json(&body).unwrap_or(body);
        } else if ct.as_deref().is_none_or(|c| c.contains("x-www-form-urlencoded")) && body.contains('=') && !body.contains('\n') {
            req.body_kind = BodyKind::Form;
            for p in body.split('&').filter(|p| !p.is_empty()) {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                req.form.push(Kv::new(&decode(k), &decode(v)));
            }
        } else {
            req.body_kind = BodyKind::Text;
            req.body = body;
        }
        // 自动推断出来的 Content-Type 不必重复留在请求头里
        if matches!(req.body_kind, BodyKind::Json | BodyKind::Form) {
            req.headers.retain(|h| {
                let v = h.v.to_ascii_lowercase();
                !(h.k.eq_ignore_ascii_case("content-type") && (v.starts_with("application/json") || v.starts_with("application/x-www-form-urlencoded")))
            });
        }
    }
    req.method = method.unwrap_or_else(|| if !data.is_empty() && !get { "POST".into() } else { "GET".into() });
    // Authorization: Bearer 也拆到认证页
    if let Some(i) = req.headers.iter().position(|h| h.k.eq_ignore_ascii_case("authorization") && h.v.starts_with("Bearer ")) {
        req.token = req.headers[i].v["Bearer ".len()..].to_string();
        req.auth = AuthKind::Bearer;
        req.headers.remove(i);
    }
    Ok(req)
}

// ---------------------------------------------------------------- JSON

/// 重新缩进 JSON（不解析成对象，所以键的顺序、数字写法都原样保留）。不是合法 JSON 返回 None。
pub fn pretty_json(s: &str) -> Option<String> {
    let s = s.trim();
    if !s.starts_with(['{', '[']) || serde_json::from_str::<serde::de::IgnoredAny>(s).is_err() {
        return None;
    }
    let mut out = String::with_capacity(s.len() * 2);
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    let chars: Vec<char> = s.chars().collect();
    let nl = |out: &mut String, d: usize| {
        out.push('\n');
        for _ in 0..d {
            out.push_str("  ");
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '{' | '[' => {
                // 空对象 / 空数组写在一行
                let close = if c == '{' { '}' } else { ']' };
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if chars.get(j) == Some(&close) {
                    out.push(c);
                    out.push(close);
                    i = j + 1;
                    continue;
                }
                out.push(c);
                depth += 1;
                nl(&mut out, depth);
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                nl(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(c);
                nl(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
        i += 1;
    }
    Some(out)
}

/// JSON 文本里每段的种类，用于上色：(起, 止, 种类)
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tok {
    Key,
    Str,
    Num,
    Lit,
    Punct,
}

pub fn json_tokens(s: &str) -> Vec<(usize, usize, Tok)> {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'"' => {
                let st = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(b.len());
                // 后面紧跟冒号的是键
                let mut j = i;
                while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                    j += 1;
                }
                out.push((st, i, if b.get(j) == Some(&b':') { Tok::Key } else { Tok::Str }));
                continue;
            }
            b'-' | b'0'..=b'9' => {
                let st = i;
                while i < b.len() && (b[i].is_ascii_digit() || matches!(b[i], b'-' | b'+' | b'.' | b'e' | b'E')) {
                    i += 1;
                }
                out.push((st, i, Tok::Num));
                continue;
            }
            b't' | b'f' | b'n' => {
                let st = i;
                while i < b.len() && b[i].is_ascii_alphabetic() {
                    i += 1;
                }
                out.push((st, i, Tok::Lit));
                continue;
            }
            b'{' | b'}' | b'[' | b']' | b',' | b':' => out.push((i, i + 1, Tok::Punct)),
            _ => {}
        }
        i += 1;
    }
    out
}

pub fn status_name(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

pub fn human_size(n: usize) -> String {
    if n < 1024 {
        tf("{} 字节", &[&n])
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vars_and_url() {
        let env = vec![Kv::new("host", "http://10.0.0.2"), Kv::new("id", "7")];
        let mut r = Request {
            url: "{{host}}/dev/{{id}}".into(),
            ..Default::default()
        };
        r.params.push(Kv::new("q", "a b&c"));
        r.params.push(Kv { on: false, ..Kv::new("x", "1") });
        assert_eq!(full_url(&r, &env), "http://10.0.0.2/dev/7?q=a%20b%26c");
        r.url = "{{nope}}/x".into();
        assert_eq!(missing_vars(&r, &env), vec!["nope".to_string()]);
        assert_eq!(
            full_url(
                &Request {
                    url: "example.com".into(),
                    ..Default::default()
                },
                &[]
            ),
            "http://example.com"
        );
    }

    #[test]
    fn auth_and_body() {
        let r = Request {
            auth: AuthKind::Basic,
            user: "Aladdin".into(),
            pass: "open sesame".into(),
            body_kind: BodyKind::Form,
            form: vec![Kv::new("a", "1"), Kv::new("b", "x y")],
            ..Default::default()
        };
        let h = effective_headers(&r, &[]);
        assert!(h.contains(&("Authorization".into(), "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==".into())));
        assert!(h.iter().any(|(k, v)| k == "Content-Type" && v.contains("urlencoded")));
        assert_eq!(body_bytes(&r, &[]).unwrap(), b"a=1&b=x%20y");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(decode("a%20b+c"), "a b c");
    }

    #[test]
    fn curl_roundtrip() {
        let c = r#"curl 'https://api.example.com/v1/items?page=2&q=hi%20there' \
  -H 'Authorization: Bearer abc123' \
  -H "Accept: application/json" \
  --data-raw '{"name":"x","n":[1,2]}'"#;
        let r = from_curl(c).unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.url, "https://api.example.com/v1/items");
        assert_eq!(r.params, vec![Kv::new("page", "2"), Kv::new("q", "hi there")]);
        assert_eq!(r.auth, AuthKind::Bearer);
        assert_eq!(r.token, "abc123");
        assert_eq!(r.body_kind, BodyKind::Json);
        assert!(r.body.contains("\"name\": \"x\""));
        let back = to_curl(&r, &[]);
        let r2 = from_curl(&back).unwrap();
        assert_eq!(r2.url, r.url);
        assert_eq!(r2.params, r.params);
        let mut q = Request {
            url: "http://h/a?device=x&n=1".into(),
            params: vec![Kv::new("device", "qccom")],
            ..Default::default()
        };
        absorb_query(&mut q);
        assert_eq!(q.url, "http://h/a");
        assert_eq!(q.params, vec![Kv::new("device", "x"), Kv::new("n", "1")]);
        assert_eq!(r2.token, r.token);
        assert!(from_curl("wget x").is_err());
        let f = from_curl("curl -u admin:pw -d a=1 -d b=2 http://h/form").unwrap();
        assert_eq!((f.method.as_str(), f.body_kind, f.form.len(), f.user.as_str()), ("POST", BodyKind::Form, 2, "admin"));
    }

    #[test]
    fn json_pretty() {
        let p = pretty_json(r#"{"b":1,"a":[1,2,{}],"s":"x,{y}:\"z\"","e":[]}"#).unwrap();
        assert_eq!(p, "{\n  \"b\": 1,\n  \"a\": [\n    1,\n    2,\n    {}\n  ],\n  \"s\": \"x,{y}:\\\"z\\\"\",\n  \"e\": []\n}");
        assert!(pretty_json("{bad").is_none());
        let toks = json_tokens("{\"k\": \"v\", \"n\": -1.5e3, \"t\": true}");
        let kinds: Vec<Tok> = toks.iter().map(|t| t.2).collect();
        assert_eq!(
            kinds,
            vec![
                Tok::Punct,
                Tok::Key,
                Tok::Punct,
                Tok::Str,
                Tok::Punct,
                Tok::Key,
                Tok::Punct,
                Tok::Num,
                Tok::Punct,
                Tok::Key,
                Tok::Punct,
                Tok::Lit,
                Tok::Punct
            ]
        );
    }

    #[test]
    fn local_server() {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut first = String::new();
            r.read_line(&mut first).unwrap();
            let mut len = 0;
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            std::io::Read::read_exact(&mut r, &mut body).unwrap();
            let out = format!("{{\"line\":\"{}\",\"body\":{}}}", first.trim(), String::from_utf8_lossy(&body));
            let mut s = s;
            write!(s, "HTTP/1.1 418 I'm a teapot\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", out.len(), out).unwrap();
        });
        let req = Request {
            method: "POST".into(),
            url: format!("127.0.0.1:{port}/x"),
            body_kind: BodyKind::Json,
            body: "{\"a\":1}".into(),
            ..Default::default()
        };
        let r = send(&req, &[], 5, true).unwrap();
        assert_eq!(r.status, 418);
        assert!(r.is_json());
        assert!(!use_proxy("http://192.168.1.5:8080/x") && !use_proxy("localhost/x"));
        assert_eq!(r.text(), "{\"line\":\"POST /x HTTP/1.1\",\"body\":{\"a\":1}}");
    }
}
