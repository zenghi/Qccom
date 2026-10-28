//! 多个连接同时在线：串口、TCP、UDP、MQTT、BLE、WebSocket 各有一个「会话」，各自一份日志、终端、统计和录制。
//!
//! 当前显示的会话的状态直接放在 App 的字段里（界面代码不用改），其余会话停在 `sessions` 里。
//! 切换时整体交换；后台会话每帧临时换进来处理一次收到的数据再换回去，所以后台连接照常收日志、录制、自动重连。

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use super::logx::tool_window;
use super::*;

/// 会话槽位：0 串口、1 TCP（客户端 / 服务器）、2 UDP、3 MQTT、4 BLE、5 WebSocket
pub(super) const SLOTS: usize = 6;

pub(super) fn slot_of(t: Transport) -> usize {
    match t {
        Transport::Serial => 0,
        Transport::TcpClient | Transport::TcpServer => 1,
        Transport::Udp => 2,
        Transport::Mqtt => 3,
        Transport::Ble => 4,
        Transport::Ws => 5,
    }
}

/// 一个会话自己的状态。字段名和 App 里的一一对应
pub(super) struct Sess {
    transport: Transport,
    buf: LogBuf,
    evt_tx: Sender<Evt>,
    evt_rx: Receiver<Evt>,
    conn: Option<Connection>,
    conn_gen: u64,
    connected: Option<String>,
    connected_at: Option<Instant>,
    autobaud: Option<u32>,
    reconnect: Option<(String, Instant)>,
    last_error: Option<String>,
    paused: bool,
    pause_buf: Vec<u8>,
    view_ids: VecDeque<u64>,
    view_checked: u64,
    view_dirty: bool,
    search_hit: Option<u64>,
    sel: Option<(u64, u64)>,
    tsel: Option<TSel>,
    log_scroll_y: f32,
    at_bottom: bool,
    send_text: String,
    hist_pos: Option<usize>,
    repeat: bool,
    last_repeat: Instant,
    rx_bytes: u64,
    tx_bytes: u64,
    rate: f64,
    rate_acc: u64,
    rate_t: Instant,
    rate_hist: VecDeque<f32>,
    record: Option<Rec>,
    seq: Option<(usize, VecDeque<Vec<u8>>, Instant)>,
    last_rx_at: Option<Instant>,
    last_tx_at: Option<Instant>,
    tv: TermView,
    peers: Option<usize>,
    net_quiet: bool,
    stats: logx::Stats,
    signals: Option<[bool; 4]>,
    /// 新数据到达后还没看过（状态栏的小点）
    pub unread: bool,
}

impl Sess {
    pub(super) fn recording(&self) -> bool {
        self.record.is_some()
    }

    pub fn new(transport: Transport, max_lines: usize, frame_gap_ms: u32) -> Self {
        let (evt_tx, evt_rx) = mpsc::channel();
        let mut buf = LogBuf::new(max_lines);
        buf.frame_gap_ms = frame_gap_ms;
        let mut tv = TermView::new("term-view", 100, 30);
        tv.focus_req = true;
        Self {
            transport,
            buf,
            evt_tx,
            evt_rx,
            conn: None,
            conn_gen: 0,
            connected: None,
            connected_at: None,
            autobaud: None,
            reconnect: None,
            last_error: None,
            paused: false,
            pause_buf: vec![],
            view_ids: VecDeque::new(),
            view_checked: 0,
            view_dirty: true,
            search_hit: None,
            sel: None,
            tsel: None,
            log_scroll_y: 0.0,
            at_bottom: true,
            send_text: String::new(),
            hist_pos: None,
            repeat: false,
            last_repeat: Instant::now(),
            rx_bytes: 0,
            tx_bytes: 0,
            rate: 0.0,
            rate_acc: 0,
            rate_t: Instant::now(),
            rate_hist: VecDeque::from(vec![0.0; RATE_HISTORY]),
            record: None,
            seq: None,
            last_rx_at: None,
            last_tx_at: None,
            tv,
            peers: None,
            net_quiet: false,
            stats: Default::default(),
            signals: None,
            unread: false,
        }
    }

    /// 连着、正在连、或在等自动重连
    pub fn online(&self) -> bool {
        self.conn.is_some() || self.reconnect.is_some()
    }
}

/// 状态栏里列出的一个在线连接
pub(super) struct LiveConn {
    pub slot: usize,
    pub transport: Transport,
    pub name: String,
    pub up: bool,
    pub unread: bool,
    pub current: bool,
}

impl App {
    /// 把 App 里当前会话的状态和 s 交换
    fn swap_session(&mut self, s: &mut Sess) {
        use std::mem::swap;
        swap(&mut self.s.transport, &mut s.transport);
        swap(&mut self.buf, &mut s.buf);
        swap(&mut self.evt_tx, &mut s.evt_tx);
        swap(&mut self.evt_rx, &mut s.evt_rx);
        swap(&mut self.conn, &mut s.conn);
        swap(&mut self.conn_gen, &mut s.conn_gen);
        swap(&mut self.connected, &mut s.connected);
        swap(&mut self.connected_at, &mut s.connected_at);
        swap(&mut self.autobaud, &mut s.autobaud);
        swap(&mut self.reconnect, &mut s.reconnect);
        swap(&mut self.last_error, &mut s.last_error);
        swap(&mut self.paused, &mut s.paused);
        swap(&mut self.pause_buf, &mut s.pause_buf);
        swap(&mut self.view_ids, &mut s.view_ids);
        swap(&mut self.view_checked, &mut s.view_checked);
        swap(&mut self.view_dirty, &mut s.view_dirty);
        swap(&mut self.search_hit, &mut s.search_hit);
        swap(&mut self.sel, &mut s.sel);
        swap(&mut self.tsel, &mut s.tsel);
        swap(&mut self.log_scroll_y, &mut s.log_scroll_y);
        swap(&mut self.at_bottom, &mut s.at_bottom);
        swap(&mut self.send_text, &mut s.send_text);
        swap(&mut self.hist_pos, &mut s.hist_pos);
        swap(&mut self.repeat, &mut s.repeat);
        swap(&mut self.last_repeat, &mut s.last_repeat);
        swap(&mut self.rx_bytes, &mut s.rx_bytes);
        swap(&mut self.tx_bytes, &mut s.tx_bytes);
        swap(&mut self.rate, &mut s.rate);
        swap(&mut self.rate_acc, &mut s.rate_acc);
        swap(&mut self.rate_t, &mut s.rate_t);
        swap(&mut self.rate_hist, &mut s.rate_hist);
        swap(&mut self.record, &mut s.record);
        swap(&mut self.seq, &mut s.seq);
        swap(&mut self.last_rx_at, &mut s.last_rx_at);
        swap(&mut self.last_tx_at, &mut s.last_tx_at);
        swap(&mut self.tv, &mut s.tv);
        swap(&mut self.peers, &mut s.peers);
        swap(&mut self.net_quiet, &mut s.net_quiet);
        swap(&mut self.stats, &mut s.stats);
        swap(&mut self.signals, &mut s.signals);
    }

    pub(super) fn cur_slot(&self) -> usize {
        slot_of(self.s.transport)
    }

    /// 切到某个会话显示。第一次用到时新建
    pub(super) fn use_slot(&mut self, k: usize) {
        let cur = self.cur_slot();
        if k == cur {
            return;
        }
        let mut s = match self.sessions[k].take() {
            Some(s) => s,
            None => {
                let t = match k {
                    0 => Transport::Serial,
                    1 => self.s.tcp_mode,
                    2 => Transport::Udp,
                    3 => Transport::Mqtt,
                    4 => Transport::Ble,
                    _ => Transport::Ws,
                };
                Sess::new(t, self.s.max_lines, self.s.frame_gap_ms)
            }
        };
        s.unread = false;
        self.swap_session(&mut s);
        if cur == 1 {
            self.s.tcp_mode = s.transport;
        }
        self.sessions[cur] = Some(s);
        // 日志相关的设置可能在别的会话里改过
        self.buf.max_lines = self.s.max_lines;
        self.buf.frame_gap_ms = self.s.frame_gap_ms;
        self.apply_frame();
        self.view_dirty = true;
        self.tv.focus_req = self.s.terminal;
        self.focus_send = true;
    }

    /// 后台会话：换进来处理收到的数据、重连、录制，再换回去
    pub(super) fn tick_background(&mut self) {
        for k in 0..SLOTS {
            let Some(mut s) = self.sessions[k].take() else { continue };
            let before = s.buf.total();
            self.swap_session(&mut s);
            self.bg = true;
            self.handle_events();
            if self.s.transport == Transport::Serial {
                self.tick_sniff();
                self.poll_bridge();
            }
            self.tick_conn();
            self.bg = false;
            self.swap_session(&mut s);
            if s.buf.total() != before {
                s.unread = true;
            }
            self.sessions[k] = Some(s);
        }
    }

    /// 发给某个后台连接（快捷命令指定了目标连接时）
    pub(super) fn send_to_slot(&mut self, k: usize, data: Vec<u8>) {
        if k == self.cur_slot() {
            return self.send_bytes(data);
        }
        let Some(mut s) = self.sessions.get_mut(k).and_then(|s| s.take()).filter(|s| s.conn.is_some()) else {
            self.toast(tf("「{}」没有连接", &[&crate::cmds::target_name(Some(k as u8))]), WARN());
            return;
        };
        self.swap_session(&mut s);
        self.send_bytes(data);
        self.swap_session(&mut s);
        self.sessions[k] = Some(s);
    }

    /// 合并日志窗口：所有连接的日志按时间排在一起，每行标出来源
    pub(super) fn merged_window(&mut self, ctx: &egui::Context) {
        if !self.wins.merged {
            return;
        }
        const PER: usize = 3000;
        let cur = self.cur_slot();
        let mut rows: Vec<(chrono::DateTime<chrono::Local>, usize, Dir, Level, String)> = vec![];
        for k in 0..SLOTS {
            let buf = if k == cur {
                &self.buf
            } else if let Some(s) = &self.sessions[k] {
                &s.buf
            } else {
                continue;
            };
            let total = buf.total();
            let from = total.saturating_sub(PER as u64).max(buf.dropped);
            for id in from..total {
                if let Some(l) = buf.get(id) {
                    if l.dir == Dir::Tx && !self.s.show_tx {
                        continue;
                    }
                    rows.push((l.ts, k, l.dir, l.level, l.plain.clone()));
                }
            }
        }
        rows.sort_by_key(|r| r.0);
        let slot_col = |k: usize| [ACCENT(), VIOLET(), WARN(), OK(), ink(Color32::from_rgb(0x60, 0xa5, 0xfa)), ink(Color32::from_rgb(0xf4, 0x72, 0xb6))][k % 6];
        let mut open = true;
        let n_src = rows.iter().map(|r| r.1).collect::<std::collections::BTreeSet<_>>().len();
        tool_window(ctx, t("合并日志（所有连接）"), "merged", &mut open, 760.0, |ui| {
            ui.label(RichText::new(tf("{} 个连接的最近日志按时间排在一起；每个连接最多取最近 {} 行", &[&n_src, &PER])).small().color(TEXT3()));
            ui.add_space(4.0);
            let font = FontId::monospace(12.0);
            let row_h = ui.fonts(|f| f.row_height(&font)) + 3.0;
            egui::ScrollArea::both()
                .max_height(460.0)
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show_rows(ui, row_h, rows.len(), |ui, range| {
                    for (ts, k, dir, level, text) in &rows[range] {
                        let mut job = LayoutJob::default();
                        let fmt = |c: Color32| TextFormat {
                            font_id: font.clone(),
                            color: c,
                            ..Default::default()
                        };
                        job.append(&ts.format("%H:%M:%S%.3f ").to_string(), 0.0, fmt(TEXT3()));
                        let src = format!("{:<9} ", crate::cmds::target_name(Some(*k as u8)));
                        job.append(&src, 0.0, fmt(slot_col(*k)));
                        job.append(if *dir == Dir::Tx { "→ " } else { "  " }, 0.0, fmt(TEXT3()));
                        let c = match level {
                            Level::Error => ERR(),
                            Level::Warn => WARN(),
                            _ if *dir == Dir::Tx => ACCENT(),
                            _ => LOG_TEXT(),
                        };
                        job.append(text, 0.0, fmt(c));
                        ui.add(egui::Label::new(job).extend());
                    }
                });
            ui.add_space(4.0);
            if ui.button(t("复制全部")).clicked() {
                let text: String = rows
                    .iter()
                    .map(|(ts, k, dir, _, s)| {
                        format!(
                            "{} [{}] {}{s}\n",
                            ts.format("%H:%M:%S%.3f"),
                            crate::cmds::target_name(Some(*k as u8)),
                            if *dir == Dir::Tx { "> " } else { "" }
                        )
                    })
                    .collect();
                ui.ctx().copy_text(text);
            }
        });
        if !open {
            self.wins.merged = false;
        }
    }

    /// 状态栏用：所有在线的连接（含当前的）
    pub(super) fn live_conns(&self) -> Vec<LiveConn> {
        let cur = self.cur_slot();
        let mut v = vec![];
        for k in 0..SLOTS {
            let (t, name, up, online, unread) = if k == cur {
                (
                    self.s.transport,
                    self.connected.clone(),
                    self.connected.is_some(),
                    self.conn.is_some() || self.reconnect.is_some(),
                    false,
                )
            } else if let Some(s) = &self.sessions[k] {
                (s.transport, s.connected.clone(), s.connected.is_some(), s.online(), s.unread)
            } else {
                continue;
            };
            if !online {
                continue;
            }
            let name = name.unwrap_or_else(|| if t == Transport::Serial { self.s.port.clone() } else { self.s.net.label(t) });
            v.push(LiveConn {
                slot: k,
                transport: t,
                name,
                up,
                unread,
                current: k == cur,
            });
        }
        v
    }

    /// 某个槽位的连接是否在线（工具箱列表上的状态点）
    pub(super) fn slot_online(&self, k: usize) -> bool {
        if k == self.cur_slot() {
            self.conn.is_some()
        } else {
            self.sessions[k].as_ref().is_some_and(|s| s.conn.is_some())
        }
    }

    /// 后台会话是否在一秒内收到过数据（状态点闪一下）
    pub(super) fn slot_active(&self, k: usize) -> bool {
        let at = if k == self.cur_slot() {
            self.last_rx_at
        } else {
            self.sessions[k].as_ref().and_then(|s| s.last_rx_at)
        };
        at.is_some_and(|t| t.elapsed().as_millis() < 400)
    }

    /// 关闭全部连接（退出时）
    pub(super) fn close_all_sessions(&mut self) {
        for s in self.sessions.iter_mut().flatten() {
            if let Some(c) = s.conn.take() {
                let _ = c.cmd.send(Cmd::Close);
            }
        }
    }
}
