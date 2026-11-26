//! 多个串口和网络连接可同时在线，各自保留日志、终端、统计和录制。
//!
//! 当前显示的会话的状态直接放在 App 的字段里（界面代码不用改），其余会话停在 `sessions` 里。
//! 切换时整体交换；后台会话每帧临时换进来处理一次收到的数据再换回去，所以后台连接照常收日志、录制、自动重连。

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use super::logx::tool_window;
use super::work::{DockPos, SerialTarget};
use super::*;

/// 固定工具槽位和快捷命令使用的编号。额外串口标签分配独立槽位。
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

pub(super) fn serial_name_eq(a: &str, b: &str) -> bool {
    a == b || (cfg!(windows) && a.eq_ignore_ascii_case(b))
}

struct SerialState {
    port: String,
    opened: bool,
    cfg: PortConfig,
    baud_text: String,
    dtr: bool,
    rts: bool,
    sniff_port: String,
    bridge_port: u16,
    vcom_port: String,
    sniff: serialx::Sniff,
    bridge: Option<crate::bridge::Bridge>,
    vcom: Option<crate::vcom::Vcom>,
    ui: serialx::SerialUi,
    lent: Option<serialx::Lent>,
}

impl Default for SerialState {
    fn default() -> Self {
        let cfg = PortConfig::default();
        Self {
            port: String::new(),
            opened: false,
            cfg,
            baud_text: cfg.baud.to_string(),
            dtr: true,
            rts: false,
            sniff_port: String::new(),
            bridge_port: 7000,
            vcom_port: String::new(),
            sniff: Default::default(),
            bridge: None,
            vcom: None,
            ui: Default::default(),
            lent: None,
        }
    }
}

/// 一个会话自己的状态。字段名和 App 里的一一对应
pub(super) struct Sess {
    transport: Transport,
    serial: SerialState,
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
    auto_scroll: bool,
    log_load: logx::LogLoader,
    replay: Option<logx::Replay>,
    search: String,
    filter_only: bool,
    view_ids: VecDeque<u64>,
    view_checked: u64,
    view_dirty: bool,
    search_hit: Option<u64>,
    scroll_to: Option<u64>,
    focus_search: bool,
    focus_send: bool,
    sel: Option<(u64, u64)>,
    tsel: Option<TSel>,
    drag_scroll: f32,
    log_scroll_y: f32,
    log_top: Option<(u64, f32)>,
    at_bottom: bool,
    jump_bottom: bool,
    ov: (u64, Instant, Vec<u8>),
    auto_anchor: Option<egui::Pos2>,
    bottom_total: Option<u64>,
    goto_time: Option<String>,
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
    seq: Option<(usize, VecDeque<Vec<u8>>, Instant)>,
    last_rx_at: Option<Instant>,
    last_tx_at: Option<Instant>,
    tv: TermView,
    peers: Option<usize>,
    net_quiet: bool,
    stats: logx::Stats,
    signals: Option<[bool; 4]>,
    mb: dbgtools::Mb,
    mb_cfg: dbgtools::MbCfg,
    xf: dbgtools::Xf,
    script: Option<crate::script::ScriptRun>,
    pending_replies: VecDeque<(Instant, Vec<u8>)>,
    plot: plot::Plot,
    scan_upto: u64,
    /// 新数据到达后还没看过（状态栏的小点）
    pub unread: bool,
}

impl Sess {
    pub(super) fn recording(&self) -> bool {
        self.buf.recording_path().is_some()
    }

    pub fn new(slot: usize, transport: Transport, max_lines: usize, frame_gap_ms: u32) -> Self {
        let (evt_tx, evt_rx) = mpsc::channel();
        let mut buf = LogBuf::new(max_lines);
        buf.frame_gap_ms = frame_gap_ms;
        let tv = TermView::new(("term-view", slot), 100, 30);
        Self {
            transport,
            serial: SerialState::default(),
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
            auto_scroll: true,
            log_load: Default::default(),
            replay: None,
            search: String::new(),
            filter_only: false,
            view_ids: VecDeque::new(),
            view_checked: 0,
            view_dirty: true,
            search_hit: None,
            scroll_to: None,
            focus_search: false,
            focus_send: false,
            sel: None,
            tsel: None,
            drag_scroll: 0.0,
            log_scroll_y: 0.0,
            log_top: None,
            at_bottom: true,
            jump_bottom: false,
            ov: (0, Instant::now(), vec![]),
            auto_anchor: None,
            bottom_total: None,
            goto_time: None,
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
            seq: None,
            last_rx_at: None,
            last_tx_at: None,
            tv,
            peers: None,
            net_quiet: false,
            stats: Default::default(),
            signals: None,
            mb: Default::default(),
            mb_cfg: Default::default(),
            xf: Default::default(),
            script: None,
            pending_replies: VecDeque::new(),
            plot: Default::default(),
            scan_upto: 0,
            unread: false,
        }
    }

    /// 连着、正在连、或在等自动重连
    pub fn online(&self) -> bool {
        self.conn.is_some() || self.reconnect.is_some()
    }

    fn finish(mut self, encoding: Encoding) -> std::io::Result<()> {
        for connection in [self.conn.take(), self.serial.sniff.conn.take()].into_iter().flatten() {
            let _ = connection.cmd.send(Cmd::Close);
        }
        let pending = std::mem::take(&mut self.serial.sniff.pending);
        if !pending.is_empty() {
            self.buf.push_line(Dir::Rx2, pending, encoding);
        }
        self.buf.stop_recording(encoding).map(|_| ())
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
        if self.s.transport == Transport::Serial || s.transport == Transport::Serial {
            let serial = &mut s.serial;
            swap(&mut self.s.port, &mut serial.port);
            swap(&mut self.serial_opened, &mut serial.opened);
            swap(&mut self.s.cfg, &mut serial.cfg);
            swap(&mut self.baud_text, &mut serial.baud_text);
            swap(&mut self.s.dtr, &mut serial.dtr);
            swap(&mut self.s.rts, &mut serial.rts);
            swap(&mut self.s.sniff_port, &mut serial.sniff_port);
            swap(&mut self.s.bridge_port, &mut serial.bridge_port);
            swap(&mut self.s.vcom_port, &mut serial.vcom_port);
            swap(&mut self.sniff, &mut serial.sniff);
            swap(&mut self.bridge, &mut serial.bridge);
            swap(&mut self.vcom, &mut serial.vcom);
            swap(&mut self.serial_ui, &mut serial.ui);
            swap(&mut self.lent, &mut serial.lent);
        }
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
        swap(&mut self.auto_scroll, &mut s.auto_scroll);
        swap(&mut self.log_load, &mut s.log_load);
        swap(&mut self.replay, &mut s.replay);
        swap(&mut self.search, &mut s.search);
        swap(&mut self.filter_only, &mut s.filter_only);
        swap(&mut self.view_ids, &mut s.view_ids);
        swap(&mut self.view_checked, &mut s.view_checked);
        swap(&mut self.view_dirty, &mut s.view_dirty);
        swap(&mut self.search_hit, &mut s.search_hit);
        swap(&mut self.scroll_to, &mut s.scroll_to);
        swap(&mut self.focus_search, &mut s.focus_search);
        swap(&mut self.focus_send, &mut s.focus_send);
        swap(&mut self.sel, &mut s.sel);
        swap(&mut self.tsel, &mut s.tsel);
        swap(&mut self.drag_scroll, &mut s.drag_scroll);
        swap(&mut self.log_scroll_y, &mut s.log_scroll_y);
        swap(&mut self.log_top, &mut s.log_top);
        swap(&mut self.at_bottom, &mut s.at_bottom);
        swap(&mut self.jump_bottom, &mut s.jump_bottom);
        swap(&mut self.ov, &mut s.ov);
        swap(&mut self.auto_anchor, &mut s.auto_anchor);
        swap(&mut self.bottom_total, &mut s.bottom_total);
        swap(&mut self.goto_time, &mut s.goto_time);
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
        swap(&mut self.seq, &mut s.seq);
        swap(&mut self.last_rx_at, &mut s.last_rx_at);
        swap(&mut self.last_tx_at, &mut s.last_tx_at);
        swap(&mut self.tv, &mut s.tv);
        swap(&mut self.peers, &mut s.peers);
        swap(&mut self.net_quiet, &mut s.net_quiet);
        swap(&mut self.stats, &mut s.stats);
        swap(&mut self.signals, &mut s.signals);
        swap(&mut self.mb, &mut s.mb);
        swap(&mut self.s.mb, &mut s.mb_cfg);
        swap(&mut self.xf, &mut s.xf);
        swap(&mut self.dock.script, &mut s.script);
        swap(&mut self.dock.pending, &mut s.pending_replies);
        swap(&mut self.plot, &mut s.plot);
        swap(&mut self.work.scan_upto, &mut s.scan_upto);
        swap(&mut self.session_unread, &mut s.unread);
        self.configure_recording();
    }

    /// Shared filters affect the visible-line cache of every log session.
    pub(super) fn invalidate_log_views(&mut self) {
        self.view_dirty = true;
        self.ov.2.clear();
        for session in self.sessions.iter_mut().flatten() {
            session.view_dirty = true;
            session.ov.2.clear();
        }
    }

    pub(super) fn cur_slot(&self) -> usize {
        slot_of(self.s.transport)
    }

    pub(super) fn target_slot(&self, k: usize) -> usize {
        if k == 0 {
            self.selected_serial
        } else {
            k
        }
    }

    pub(super) fn serial_port_for(&self, k: usize) -> &str {
        if k == self.session_slot {
            &self.s.port
        } else {
            self.sessions[k].as_ref().map_or("", |s| s.serial.port.as_str())
        }
    }

    pub(super) fn serial_sniff_port_for(&self, slot: usize) -> &str {
        if slot == self.session_slot {
            &self.s.sniff_port
        } else {
            self.sessions[slot].as_ref().map_or("", |session| session.serial.sniff_port.as_str())
        }
    }

    fn local_vcom_for_slot(&self, slot: usize) -> Option<&crate::vcom::Vcom> {
        if slot == self.session_slot {
            return self.vcom.as_ref();
        }
        self.sessions.get(slot)?.as_ref()?.serial.vcom.as_ref()
    }

    pub(super) fn local_vcom_owner(&self, name: &str) -> Option<usize> {
        self.serial_tabs.iter().copied().find(|&slot| self.local_vcom_for_slot(slot).is_some_and(|vcom| vcom.owns_port(name)))
    }

    pub(super) fn local_vcom_ports(&self) -> Vec<(usize, String)> {
        self.serial_tabs
            .iter()
            .copied()
            .filter_map(|slot| self.local_vcom_for_slot(slot).map(|vcom| (slot, vcom)))
            .flat_map(|(slot, vcom)| vcom.external_ports().iter().cloned().map(move |name| (slot, name)))
            .collect()
    }

    pub(super) fn serial_port_present(&self, name: &str) -> bool {
        !self.ports_seen
            || name.is_empty()
            || self.ports.iter().any(|port| serial_name_eq(&port.name, name))
            || self.local_vcom_owner(name).is_some()
            || self.is_mapping_monitor(name)
    }

    pub(super) fn prune_missing_serial_ports(&mut self) {
        let missing: Vec<_> = self.serial_tabs.iter().copied().filter(|&slot| !self.serial_port_present(self.serial_port_for(slot))).collect();
        if missing.is_empty() {
            return;
        }
        for &slot in &missing {
            self.remove_serial_pane(slot);
        }
        if missing.contains(&self.selected_serial) {
            self.selected_serial = self.serial_tabs.iter().copied()
                .find(|slot| !missing.contains(slot) && !self.serial_tab_docked(*slot))
                .unwrap_or_else(|| self.new_serial_log_slot());
        }
        if missing.contains(&self.session_slot) {
            self.use_session(self.selected_serial);
        }
        for slot in missing {
            let waiting = self.with_session(slot, |app| {
                if app.conn.is_none() && app.reconnect.is_none() && app.autobaud.is_none() && app.lent.is_none() {
                    return false;
                }
                // Keep an in-flight connection or reconnect in the background, with no
                // device tab or old log visible while the port is absent.
                app.clear();
                app.tv.reset();
                app.paused = false;
                app.pause_buf.clear();
                app.session_unread = false;
                true
            });
            if !waiting {
                self.close_serial_tab(slot);
            }
        }
    }

    pub(super) fn serial_port_busy(&self, name: &str) -> bool {
        if self.local_vcom_owner(name).is_some() {
            return true;
        }
        let uses = |port: &str, busy: bool, side: &str, listening: bool| (busy && port == name) || (listening && side == name);
        self.serial_tabs.iter().any(|&k| {
            if k == self.session_slot {
                uses(&self.s.port, self.conn.is_some() || self.autobaud.is_some(), &self.s.sniff_port, self.sniff.conn.is_some())
            } else {
                self.sessions[k]
                    .as_ref()
                    .is_some_and(|s| uses(&s.serial.port, s.conn.is_some() || s.autobaud.is_some(), &s.serial.sniff_port, s.serial.sniff.conn.is_some()))
            }
        })
    }

    pub(super) fn serial_port_connected(&self, name: &str) -> bool {
        #[cfg(windows)]
        if self.mappings.serial_online(name) {
            return true;
        }
        self.serial_tabs.iter().any(|&slot| {
            let connected = if slot == self.session_slot {
                self.connected.as_deref()
            } else {
                self.sessions[slot].as_ref().and_then(|session| session.connected.as_deref())
            };
            connected == Some(name)
        })
    }

    pub(super) fn is_mapping_monitor(&self, name: &str) -> bool {
        #[cfg(windows)]
        {
            self.mappings.serial_view_port(name).is_some()
        }
        #[cfg(not(windows))]
        {
            let _ = name;
            false
        }
    }

    fn stop_serial_tab_mapping(&mut self, slot: usize) -> bool {
        #[cfg(windows)]
        {
            let port = self.serial_port_for(slot).to_owned();
            self.mappings.stop_serial_view(&port)
        }
        #[cfg(not(windows))]
        {
            let _ = slot;
            false
        }
    }

    pub(super) fn choose_serial_port(&mut self, name: &str, connect: bool) {
        self.choose_serial(name, connect, None);
    }

    pub(super) fn choose_serial_port_in(&mut self, name: &str, connect: bool, target: SerialTarget) {
        self.choose_serial(name, connect, Some(target));
    }

    fn choose_serial(&mut self, name: &str, connect: bool, target: Option<SerialTarget>) {
        if name.is_empty() || target == Some(SerialTarget::Side) && !self.serial_split_active() {
            return;
        }
        let owner = self.local_vcom_owner(name);
        #[cfg(windows)]
        let mapped = self.mappings.serial_view_port(name);
        #[cfg(not(windows))]
        let mapped: Option<&str> = None;
        let name = mapped.unwrap_or(name);
        if !self.serial_port_present(name) {
            return;
        }
        let slot = owner.or_else(|| self.serial_tabs.iter().copied().find(|&slot| serial_name_eq(self.serial_port_for(slot), name)));
        let connect = connect && owner.is_none() && mapped.is_none();
        let name = name.to_owned();
        let slot = slot.unwrap_or_else(|| {
            let reference = match target {
                Some(SerialTarget::Side) => self.side_pane().and_then(|pane| pane.serial_slot(self.selected_serial)).unwrap_or(self.selected_serial),
                Some(SerialTarget::Main) => self.selected_serial,
                None if self.s.transport == Transport::Serial => self.session_slot,
                None => self.selected_serial,
            };
            self.with_session(reference, |app| {
                if app.conn.is_none() && app.autobaud.is_none() && app.reconnect.is_none() && !app.serial_opened && app.buf.total() == 0 {
                    app.s.port = name;
                    return reference;
                }
                let slot = app.new_serial_log_slot();
                app.sessions[slot].as_mut().expect("new serial session exists").serial.port = name;
                slot
            })
        });
        if let Some(target) = target {
            if !self.route_serial_selection(slot, target) {
                return;
            }
        } else if !self.focus_docked_serial(slot) {
            self.use_session(slot);
            if self.page == Page::Debug {
                self.work.focus_side = false;
                self.work.serial_devices_side = false;
            }
        }
        self.with_session(slot, |app| {
            app.tv.focus_req = app.s.terminal;
            app.focus_send = true;
            if connect {
                app.connect_chosen_serial();
            }
        });
    }

    fn connect_chosen_serial(&mut self) {
        if self.conn.is_none() && self.autobaud.is_none() && self.reconnect.is_none() {
            self.connect();
        }
    }

    pub(super) fn new_serial_log_slot(&mut self) -> usize {
        let slot = self.sessions.len();
        let mut session = Sess::new(slot, Transport::Serial, self.s.max_lines, self.s.frame_gap_ms);
        session.serial.cfg = self.s.cfg;
        session.serial.baud_text = self.s.cfg.baud.to_string();
        session.serial.dtr = self.s.dtr;
        session.serial.rts = self.s.rts;
        self.sessions.push(Some(session));
        self.serial_tabs.push(slot);
        slot
    }

    pub(super) fn close_serial_tab(&mut self, slot: usize) {
        if self.serial_tabs.len() <= 1 {
            return;
        }
        let Some(index) = self.serial_tabs.iter().position(|&k| k == slot) else { return };
        self.stop_serial_tab_mapping(slot);
        self.remove_serial_pane(slot);
        self.serial_tabs.remove(index);
        if slot == self.session_slot {
            let opened: Vec<_> = self.serial_log_slots().into_iter().filter(|&slot| !self.serial_tab_docked(slot)).collect();
            let next = opened
                .get(index.min(opened.len().saturating_sub(1)))
                .copied()
                .or_else(|| self.serial_tabs.iter().copied().find(|&slot| !self.serial_tab_docked(slot)))
                .unwrap_or_else(|| self.new_serial_log_slot());
            self.use_session(next);
        }
        if let Some(closed) = self.sessions[slot].take() {
            if let Err(error) = closed.finish(self.s.encoding) {
                self.recording_failed(error);
            }
        }
    }

    pub(super) fn serial_log_slots(&self) -> Vec<usize> {
        self.serial_tabs
            .iter()
            .copied()
            .filter(|&k| {
                if !self.serial_port_present(self.serial_port_for(k)) {
                    return false;
                }
                if k == self.session_slot {
                    self.serial_opened
                } else {
                    self.sessions[k].as_ref().is_some_and(|s| s.serial.opened)
                }
            })
            .collect()
    }

    pub(super) fn serial_tab_bar(&mut self, ui: &mut egui::Ui) {
        let tabs: Vec<_> = self.serial_log_slots().into_iter().filter(|&slot| !self.serial_tab_docked(slot)).collect();
        if tabs.len() < 2 {
            return;
        }
        let mut action = None;
        let mut rects = Vec::with_capacity(tabs.len());
        let scroll = egui::ScrollArea::horizontal()
            .id_salt("serial-log-tabs")
            .auto_shrink([false, true])
            .drag_to_scroll(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for &slot in &tabs {
                        ui.push_id(slot, |ui| {
                            let active = slot == self.session_slot;
                            let port = self.serial_port_for(slot).to_owned();
                            let connected = self.serial_port_connected(&port);
                            let mapped = self.is_mapping_monitor(&port);
                            let (rect, tab, close) = serial_log_tab(ui, &port, active, connected);
                            rects.push((slot, rect));
                            if tab.drag_started_by(egui::PointerButton::Primary) {
                                self.drag_source(&tab, Pane::Serial(slot), None);
                            }
                            if close.clicked() || tab.middle_clicked() {
                                action = Some((slot, SerialTabAction::Close));
                            } else if tab.clicked() {
                                action = Some((slot, SerialTabAction::Activate));
                            }
                            tab.context_menu(|ui| {
                                if let Some(hit) = serial_tab_menu(ui, &port, connected, mapped, false, false) {
                                    action = Some((slot, hit));
                                    ui.close_menu();
                                }
                            });
                        });
                    }
                });
            });
        ui.add_space(4.0);
        let row = rects[0].1;
        let target = Rect::from_min_max(pos2(scroll.inner_rect.left(), row.top()), pos2(scroll.inner_rect.right(), row.bottom()))
            .intersect(scroll.inner_rect)
            .intersect(ui.clip_rect());
        self.work.serial_tab_rect = Some(target);
        self.serial_tab_reorder(ui, target, &rects);
        if let Some((slot, action)) = action {
            self.apply_serial_tab_action(slot, action);
        }
    }

    pub(super) fn serial_pane_menu(&mut self, ui: &mut egui::Ui, slot: usize, detached: bool) {
        let port = self.serial_port_for(slot).to_owned();
        if let Some(action) = serial_tab_menu(ui, &port, self.serial_port_connected(&port), self.is_mapping_monitor(&port), true, detached) {
            self.apply_serial_tab_action(slot, action);
            ui.close_menu();
        }
    }

    fn apply_serial_tab_action(&mut self, slot: usize, action: SerialTabAction) {
        match action {
            SerialTabAction::Activate => {
                self.use_session(slot);
                self.work.serial_devices_side = false;
            }
            SerialTabAction::Disconnect => {
                if !self.stop_serial_tab_mapping(slot) {
                    self.with_session(slot, |app| app.disconnect());
                }
            }
            SerialTabAction::Reconnect => self.with_session(slot, |app| {
                if app.is_mapping_monitor(&app.s.port) {
                    return;
                }
                if app.conn.is_some() {
                    app.disconnect();
                    app.reconnect = Some((app.s.port.clone(), Instant::now()));
                    app.ctx.request_repaint();
                } else {
                    app.connect_chosen_serial();
                }
            }),
            SerialTabAction::Save => self.with_session(slot, |app| app.save()),
            SerialTabAction::Dock(pos, detached) => self.open_serial_side(slot, pos, detached),
            SerialTabAction::Restore => {
                self.remove_serial_pane(slot);
                self.goto_pane(Pane::Serial(slot));
            }
            SerialTabAction::Close => self.close_serial_tab(slot),
            SerialTabAction::CloseOthers => {
                self.use_session(slot);
                for other in self.serial_log_slots() {
                    if other != slot && !self.serial_tab_docked(other) {
                        self.close_serial_tab(other);
                    }
                }
            }
        }
        self.update_view();
    }

    fn serial_tab_reorder(&mut self, ui: &egui::Ui, bar: Rect, tabs: &[(usize, Rect)]) {
        if bar.width() < 2.0 {
            return;
        }
        let Some(drag) = &self.work.drag else { return };
        let Pane::Serial(slot) = drag.pane else { return };
        let (pointer, released, cancelled) = ui.input(|input| (input.pointer.hover_pos(), input.pointer.primary_released(), input.key_pressed(Key::Escape)));
        let Some(pointer) = pointer.filter(|&p| bar.contains(p) && (drag.live || p.distance(drag.from) > 8.0)) else {
            return;
        };
        if cancelled {
            return;
        }
        let Some(&(last, last_rect)) = tabs.last() else { return };
        let before = tabs.iter().find(|(_, rect)| pointer.x < rect.center().x);
        let x = before.map_or(last_rect.right(), |(_, rect)| rect.left()).clamp(bar.left() + 1.0, bar.right() - 1.0);
        ui.painter_at(bar).line_segment([pos2(x, bar.top() + 2.0), pos2(x, bar.bottom() - 2.0)], st(2.0, ACCENT()));
        if released {
            self.work.drag = None;
            if self.serial_tab_docked(slot) {
                self.remove_serial_pane(slot);
                self.use_session(slot);
            }
            move_serial_tab(&mut self.serial_tabs, slot, before.map(|(slot, _)| *slot), last);
        }
    }

    pub(super) fn use_slot(&mut self, k: usize) {
        self.use_session(self.target_slot(k));
    }

    pub(super) fn use_session(&mut self, k: usize) {
        if k == self.session_slot {
            return;
        }
        self.quiet_session(k);
        self.session_unread = false;
        if self.s.transport == Transport::Serial {
            self.selected_serial = k;
        }
        if let Some(replay) = self.replay.as_mut() {
            replay.reset_clock();
        }
        // 日志相关的设置可能在别的会话里改过
        self.buf.max_lines = self.s.max_lines;
        self.buf.frame_gap_ms = self.s.frame_gap_ms;
        self.apply_frame();
        self.view_dirty = true;
        self.tv.focus_req = self.s.terminal;
        self.focus_send = true;
        self.drag_scroll = 0.0;
        self.auto_anchor = None;
        if let Some(selection) = &mut self.tsel {
            selection.drag = false;
        }
    }

    /// 临时换进另一个会话。调用方可以在画完副页面后按实际槽位还原。
    pub(super) fn quiet_session(&mut self, k: usize) {
        let cur = self.session_slot;
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
                Sess::new(k, t, self.s.max_lines, self.s.frame_gap_ms)
            }
        };
        self.swap_session(&mut s);
        if k == 1 {
            self.s.tcp_mode = self.s.transport;
        } else if cur == 1 {
            self.s.tcp_mode = s.transport;
        }
        self.sessions[cur] = Some(s);
        self.session_slot = k;
    }

    /// 后台会话：换进来处理收到的数据、重连、录制，再换回去
    pub(super) fn tick_background(&mut self) {
        let current = self.session_slot;
        for k in 0..self.sessions.len() {
            if k == current || self.sessions[k].is_none() {
                continue;
            }
            self.quiet_session(k);
            let before = self.buf.total();
            self.bg = true;
            self.handle_events();
            self.tick_dock();
            self.tick_tools();
            if self.s.transport == Transport::Serial {
                self.tick_sniff();
                self.poll_bridge();
                self.poll_vcom();
                self.tick_lent();
            }
            self.tick_conn();
            self.bg = false;
            self.session_unread |= self.buf.total() != before;
            self.quiet_session(current);
        }
    }

    /// 发给某个后台连接（快捷命令指定了目标连接时）
    pub(super) fn send_to_slot(&mut self, k: usize, data: Vec<u8>) {
        let target = self.target_slot(k);
        if target == self.session_slot {
            return self.send_bytes(data);
        }
        if !self.sessions.get(target).and_then(Option::as_ref).is_some_and(|s| s.conn.is_some()) {
            self.toast(tf("「{}」没有连接", &[&crate::cmds::target_name(Some(k as u8))]), WARN());
            return;
        }
        let mut s = self.sessions[target].take().unwrap();
        self.swap_session(&mut s);
        self.send_bytes(data);
        self.swap_session(&mut s);
        self.sessions[target] = Some(s);
    }

    /// 合并日志窗口：所有连接的日志按时间排在一起，每行标出来源
    pub(super) fn merged_window(&mut self, ctx: &egui::Context) {
        if !self.wins.merged {
            return;
        }
        const PER: usize = 3000;
        let cur = self.session_slot;
        let mut rows: Vec<(chrono::DateTime<chrono::Local>, usize, Dir, Level, String)> = vec![];
        for k in 0..self.sessions.len() {
            if (k == 0 || k >= SLOTS) && !self.serial_port_present(self.serial_port_for(k)) {
                continue;
            }
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
                        let src = format!("{:<9} ", self.session_label(*k));
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
                    .map(|(ts, k, dir, _, s)| format!("{} [{}] {}{s}\n", ts.format("%H:%M:%S%.3f"), self.session_label(*k), if *dir == Dir::Tx { "> " } else { "" }))
                    .collect();
                ui.ctx().copy_text(text);
            }
        });
        if !open {
            self.wins.merged = false;
        }
    }

    fn session_label(&self, k: usize) -> &str {
        if self.serial_tabs.contains(&k) {
            let port = self.serial_port_for(k);
            if !port.is_empty() {
                return port;
            }
            return t("串口");
        }
        crate::cmds::target_name(Some(k as u8))
    }

    /// 状态栏用：所有在线的连接（含当前的）
    pub(super) fn live_conns(&self) -> Vec<LiveConn> {
        let cur = self.session_slot;
        let mut v = vec![];
        for k in 0..self.sessions.len() {
            if (k == 0 || k >= SLOTS) && !self.serial_port_present(self.serial_port_for(k)) {
                continue;
            }
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
            let name = name.unwrap_or_else(|| if t == Transport::Serial { self.serial_port_for(k).to_owned() } else { self.s.net.label(t) });
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
        let k = self.target_slot(k);
        if k == self.session_slot {
            self.conn.is_some()
        } else {
            self.sessions[k].as_ref().is_some_and(|s| s.conn.is_some())
        }
    }

    /// 后台会话是否在一秒内收到过数据（状态点闪一下）
    pub(super) fn slot_active(&self, k: usize) -> bool {
        let k = self.target_slot(k);
        let at = if k == self.session_slot {
            self.last_rx_at
        } else {
            self.sessions[k].as_ref().and_then(|s| s.last_rx_at)
        };
        at.is_some_and(|t| t.elapsed().as_millis() < 400)
    }

    /// 关闭全部连接（退出时）
    pub(super) fn close_all_sessions(&mut self) {
        for s in self.sessions.iter_mut().filter_map(Option::take) {
            if let Err(e) = s.finish(self.s.encoding) {
                eprintln!("Recording write failed on exit: {e}");
            }
        }
    }
}

enum SerialTabAction {
    Activate,
    Disconnect,
    Reconnect,
    Save,
    Dock(DockPos, bool),
    Restore,
    Close,
    CloseOthers,
}

fn serial_log_tab(ui: &mut egui::Ui, port: &str, active: bool, connected: bool) -> (Rect, egui::Response, egui::Response) {
    let name = port.strip_prefix("/dev/").unwrap_or(port);
    let font = FontId::new(12.5, mono_bold());
    let text_width = ui.fonts(|fonts| fonts.layout_no_wrap(name.to_owned(), font.clone(), TEXT()).size().x);
    let (rect, _) = ui.allocate_exact_size(vec2((text_width + 48.0).clamp(72.0, 185.0), 28.0), Sense::hover());
    let close_rect = Rect::from_center_size(pos2(rect.right() - 13.0, rect.center().y), vec2(20.0, 22.0));
    let title_rect = Rect::from_min_max(rect.min, pos2(close_rect.left(), rect.bottom()));
    let tab = ui
        .interact(title_rect, ui.id().with("tab"), Sense::click_and_drag())
        .on_hover_cursor(egui::CursorIcon::Grab)
        .on_hover_text(port);
    tab.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, active, port));
    let close = ui.interact(close_rect, ui.id().with("close"), Sense::click()).on_hover_text(t("关闭串口标签"));
    let hovered = tab.hovered() || close.hovered();
    let painter = ui.painter_at(rect);
    if active || hovered {
        painter.rect_filled(rect, R_WIDGET, if active { BG3() } else { BG2() });
    }
    if active {
        painter.hline(rect.x_range().shrink(6.0), rect.bottom() - 1.0, st(1.5, ACCENT()));
    }
    let color = if active { TEXT() } else { TEXT2() };
    let dot = pos2(rect.left() + 11.0, rect.center().y);
    painter.circle_filled(dot, 3.0, if connected { OK() } else { TEXT3() });
    let mut job = egui::text::LayoutJob::simple(name.to_owned(), font, color, (title_rect.width() - 23.0).max(0.0));
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let text = ui.fonts(|fonts| fonts.layout_job(job));
    painter.galley(pos2(rect.left() + 19.0, rect.center().y - text.size().y * 0.5), text, color);
    if active || hovered {
        draw_icon(
            &painter,
            Rect::from_center_size(close_rect.center(), vec2(12.0, 12.0)),
            Icon::Close,
            if close.hovered() { TEXT() } else { TEXT3() },
        );
    }
    (rect, tab, close)
}

fn serial_tab_menu(ui: &mut egui::Ui, port: &str, connected: bool, mapped: bool, in_side: bool, detached: bool) -> Option<SerialTabAction> {
    if in_side {
        if menu_item(ui, t("移回串口标签栏"), "") {
            return Some(SerialTabAction::Restore);
        }
        ui.separator();
    }
    if (connected || mapped) && menu_item(ui, t("断开连接"), "") {
        return Some(SerialTabAction::Disconnect);
    }
    if !mapped && menu_item(ui, t("重新连接"), "") {
        return Some(SerialTabAction::Reconnect);
    }
    if menu_item(ui, t("保存日志"), "") {
        return Some(SerialTabAction::Save);
    }
    if menu_item(ui, t("复制端口名"), "") {
        ui.ctx().copy_text(port.to_owned());
        ui.close_menu();
    }
    ui.separator();
    for (label, position, pop_out) in [
        (t("在右边打开"), DockPos::Right, false),
        (t("在下方打开"), DockPos::Bottom, false),
        (t("弹出成独立窗口（放到第二块屏幕）"), DockPos::Right, true),
    ] {
        if !(detached && pop_out) && menu_item(ui, label, "") {
            return Some(SerialTabAction::Dock(position, pop_out));
        }
    }
    ui.separator();
    if menu_item(ui, t("关闭串口标签"), "") {
        return Some(SerialTabAction::Close);
    }
    if !in_side && menu_item(ui, t("关闭其他"), "") {
        return Some(SerialTabAction::CloseOthers);
    }
    None
}

fn move_serial_tab(slots: &mut Vec<usize>, slot: usize, before: Option<usize>, last: usize) {
    let target = before.unwrap_or(last);
    if target == slot {
        return;
    }
    let (Some(from), Some(to)) = (slots.iter().position(|&id| id == slot), slots.iter().position(|&id| id == target)) else {
        return;
    };
    slots.remove(from);
    slots.insert(to - usize::from(from < to) + usize::from(before.is_none()), slot);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_serial_views_update_font_and_highlight_in_place() {
        use std::{cell::RefCell, rc::Rc};

        let settings = Settings {
            telemetry: false,
            auto_update: false,
            font_size: 11.5,
            show_ts: false,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let creation = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::with_settings(&creation, settings, Vec::new(), Vec::new());
        app.set_page(Page::Debug);
        app.s.port = "COM1".into();
        app.serial_opened = true;
        app.buf.push_line(Dir::Rx, b"primary 1234".to_vec(), app.s.encoding);
        app.choose_serial_port("COM2", false);
        app.serial_opened = true;
        app.buf.push_line(Dir::Rx, b"secondary 1234".to_vec(), app.s.encoding);
        let side = app.session_slot;
        app.open_serial_side(side, DockPos::Right, false);
        app.work.opened_at = None;

        let raw = |viewport_id| egui::RawInput {
            viewport_id,
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 720.0))),
            ..Default::default()
        };
        let detached = Rc::new(RefCell::new(Vec::new()));
        let capture = detached.clone();
        egui::Context::set_immediate_viewport_renderer(move |ctx, mut viewport| {
            let mut input = raw(viewport.ids.this);
            input.viewports.insert(
                viewport.ids.this,
                egui::ViewportInfo {
                    parent: Some(viewport.ids.parent),
                    ..Default::default()
                },
            );
            *capture.borrow_mut() = ctx.run(input, |ctx| (viewport.viewport_ui_cb)(ctx)).shapes;
        });
        let frame = |app: &mut App| {
            ctx.run(raw(egui::ViewportId::ROOT), |ctx| {
                app.dock_panel(ctx, &[]);
                app.debug_page(ctx);
            })
            .shapes
        };
        let verify_line = |shapes: &[egui::epaint::ClippedShape], name: &str, size, highlighted| {
            let job = shapes
                .iter()
                .find_map(|entry| match &entry.shape {
                    Shape::Text(text) if text.galley.job.text.starts_with(name) => Some(&text.galley.job),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing serial output for {name}"));
            let number = job.text.find("1234").unwrap();
            let format = &job.sections.iter().find(|section| section.byte_range.contains(&number)).unwrap().format;
            assert_eq!(format.font_id, FontId::monospace(size));
            assert_eq!(format.color != job.sections[0].format.color, highlighted);
        };

        let split = frame(&mut app);
        verify_line(&split, "primary", 11.5, false);
        verify_line(&split, "secondary", 11.5, false);

        app.work.detached = true;
        let _ = frame(&mut app);
        verify_line(&detached.borrow(), "secondary", 11.5, false);
        app.s.font_size = 19.5;
        app.refresh_log_font_size(&ctx);
        app.s.log_preset = LogPreset::DeviceLog;
        app.repaint_log_windows(&ctx);
        verify_line(&frame(&mut app), "primary", 19.5, true);
        verify_line(&detached.borrow(), "secondary", 19.5, true);
    }

    #[test]
    fn choosing_a_device_in_a_split_preserves_the_log_and_uses_the_target_settings() {
        let settings = Settings {
            telemetry: false,
            auto_update: false,
            ..Default::default()
        };
        let ctx = eframe::CreationContext::_new_kittest(egui::Context::default());
        let mut app = App::with_settings(&ctx, settings, Vec::new(), Vec::new());
        app.set_page(Page::Debug);
        app.s.port = "COM1".into();
        app.s.cfg.baud = 9_600;
        app.serial_opened = true;
        let main = app.session_slot;
        app.choose_serial_port("COM2", false);
        app.s.cfg.baud = 115_200;
        app.serial_opened = true;
        let old_side = app.session_slot;
        app.open_serial_side(old_side, DockPos::Right, false);

        app.choose_serial_port("COM2", false);
        assert!(app.work.focus_side);
        assert_eq!(app.selected_serial, main);
        assert_eq!(app.side_pane(), Some(Pane::Serial(old_side)));

        app.choose_serial_port_in("COM3", false, SerialTarget::Side);
        let Some(Pane::Serial(side)) = app.side_pane() else { panic!("serial split pane is missing") };
        assert_eq!(app.selected_serial, main);
        assert_eq!(app.serial_port_for(side), "COM3");
        assert_eq!(app.with_session(side, |app| app.s.cfg.baud), 115_200);
        assert_eq!(app.serial_port_for(old_side), "COM2");
        assert!(!app.serial_tab_docked(old_side));
        assert!(app.work.focus_side);
    }

    #[test]
    fn serial_tab_reordering_uses_stable_slot_ids() {
        let mut slots = vec![0, 6, 7, 8];
        move_serial_tab(&mut slots, 7, Some(0), 8);
        assert_eq!(slots, [7, 0, 6, 8]);
        move_serial_tab(&mut slots, 0, None, 8);
        assert_eq!(slots, [7, 6, 8, 0]);
        move_serial_tab(&mut slots, 0, None, 0);
        assert_eq!(slots, [7, 6, 8, 0]);
    }

    #[test]
    fn serial_tab_pointer_drag_reorders_on_release() {
        let settings = Settings {
            telemetry: false,
            auto_update: false,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let creation = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::with_settings(&creation, settings, Vec::new(), Vec::new());
        app.s.port = "COM1".into();
        app.serial_opened = true;
        let first = app.session_slot;
        app.choose_serial_port("COM2", false);
        app.serial_opened = true;
        let second = app.session_slot;

        let frame = |app: &mut App, events| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(640.0, 360.0))),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.serial_tab_bar(ui));
            });
        };
        frame(&mut app, vec![]);
        let bar = app.work.serial_tab_rect.unwrap();
        assert!(bar.height() <= 28.0);
        let from = pos2(bar.left() + 22.0, bar.center().y);
        let to = pos2(bar.left() + 220.0, bar.center().y);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        frame(&mut app, vec![egui::Event::PointerMoved(from), button(from, true)]);
        frame(&mut app, vec![egui::Event::PointerMoved(to)]);
        assert_eq!(app.serial_log_slots(), [first, second]);
        frame(&mut app, vec![button(to, false)]);
        assert_eq!(app.serial_log_slots(), [second, first]);
        assert_eq!(app.session_slot, second);
    }

    #[test]
    fn serial_tabs_isolate_background_input_output_and_close() {
        let settings = Settings {
            telemetry: false,
            auto_update: false,
            ..Default::default()
        };
        let ctx = eframe::CreationContext::_new_kittest(egui::Context::default());
        let mut app = App::with_settings(&ctx, settings, Vec::new(), Vec::new());

        app.s.port = "COM1".into();
        app.s.cfg.baud = 9_600;
        let (a_cmd, a_sent) = mpsc::channel();
        app.conn = Some(Connection { cmd: a_cmd });
        app.serial_opened = true;
        app.connected = Some(app.s.port.clone());
        let a_input = app.evt_tx.clone();
        let a = app.session_slot;
        assert_eq!(app.serial_log_slots(), [a]);

        app.choose_serial_port("COM2", false);
        app.s.cfg.baud = 115_200;
        let b = app.session_slot;
        assert_ne!(a, b);
        assert_eq!(app.serial_log_slots(), [a]);
        let (b_cmd, b_sent) = mpsc::channel();
        app.conn = Some(Connection { cmd: b_cmd });
        app.serial_opened = true;
        app.connected = Some(app.s.port.clone());
        assert_eq!(app.serial_log_slots(), [a, b]);
        app.evt_tx.send(Evt::Data(b"from B\n".to_vec())).unwrap();
        a_input.send(Evt::Data(b"from A\n".to_vec())).unwrap();
        app.tick_background();
        app.handle_events();

        assert_eq!(app.buf.get(0).unwrap().plain, "from B");
        app.send_bytes(b"to B".to_vec());
        assert!(matches!(b_sent.try_recv(), Ok(Cmd::Send(d)) if d == b"to B"));
        assert!(a_sent.try_recv().is_err());

        app.search = "from B".into();
        app.filter_only = true;
        app.update_view();
        app.quiet_session(a);
        assert!(app.search.is_empty() && !app.filter_only);
        app.auto_scroll = false;
        app.scroll_to = Some(0);
        app.bottom_total = Some(1);
        app.goto_time = Some("08:30".into());
        app.focus_search = true;
        app.focus_send = false;
        app.ov.2 = vec![1];
        app.quiet_session(b);
        assert_eq!(app.search, "from B");
        assert!(app.filter_only && app.auto_scroll);
        assert!(!app.focus_search);
        assert!(app.scroll_to.is_none() && app.goto_time.is_none());
        app.invalidate_log_views();
        app.quiet_session(a);
        assert!(!app.auto_scroll);
        assert_eq!((app.scroll_to, app.bottom_total), (Some(0), Some(1)));
        assert_eq!(app.goto_time.as_deref(), Some("08:30"));
        assert!(app.focus_search && !app.focus_send);
        assert!(app.view_dirty && app.ov.2.is_empty());
        app.quiet_session(b);

        app.use_slot(3);
        assert!(app.s.transport == Transport::Mqtt);
        app.use_slot(0);
        assert_eq!(app.session_slot, b);
        assert_eq!(app.s.cfg.baud, 115_200);

        app.use_session(a);
        assert_eq!(app.s.port, "COM1");
        assert_eq!(app.s.cfg.baud, 9_600);
        assert_eq!(app.buf.get(0).unwrap().plain, "from A");
        app.send_bytes(b"to A".to_vec());
        assert!(matches!(a_sent.try_recv(), Ok(Cmd::Send(d)) if d == b"to A"));
        assert!(b_sent.try_recv().is_err());

        app.close_serial_tab(b);
        assert_eq!(app.serial_log_slots(), [a]);
        assert!(matches!(b_sent.try_recv(), Ok(Cmd::Close)));
        assert!(app.conn.is_some());
        assert!(a_sent.try_recv().is_err());
        app.disconnect();
    }

    #[cfg(unix)]
    #[test]
    fn local_virtual_port_opens_the_existing_physical_log() {
        let settings = Settings {
            telemetry: false,
            auto_update: false,
            ..Default::default()
        };
        let ctx = eframe::CreationContext::_new_kittest(egui::Context::default());
        let mut app = App::with_settings(&ctx, settings, Vec::new(), Vec::new());
        let owner = app.session_slot;
        app.s.port = "physical fixture".into();
        let (cmd, sent) = mpsc::channel();
        app.conn = Some(Connection { cmd });
        app.serial_opened = true;
        app.connected = Some(app.s.port.clone());
        let vcom = crate::vcom::Vcom::start_pty(1, owner, app.ctx.clone()).unwrap();
        let virtual_port = vcom.external_ports()[0].clone();
        app.vcom = Some(vcom);
        app.buf.push_line(Dir::Tx, b"canonical log".to_vec(), app.s.encoding);

        let background = app.new_serial_log_slot();
        app.use_session(background);
        assert_eq!(app.local_vcom_ports(), [(owner, virtual_port.clone())]);
        assert_eq!(app.local_vcom_owner(&virtual_port), Some(owner));
        app.choose_serial_port(&virtual_port, true);
        assert_eq!(app.session_slot, owner);
        assert_eq!(app.serial_tabs, [owner, background]);
        assert_eq!(app.buf.get(0).unwrap().plain, "canonical log");
        assert!(sent.try_recv().is_err());
    }
}
