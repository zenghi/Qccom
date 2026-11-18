//! 低功耗蓝牙（BLE）调试：扫描周围设备、连接、列出服务和特征，订阅通知、读、写。
//!
//! btleplug 是异步库，这里起一个常驻线程跑单线程 tokio 运行时（「hub」）：扫描和连接都在里面做，
//! 界面通过 [`Hub::send`] 发命令，从 [`Hub::st`] 读设备列表和特征表。连上以后对外表现得和其它连接方式一样：
//! 通知数据走 `Evt::Packet`，发送区的数据写到选中的特征。

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use btleplug::api::{Central, CentralEvent, CharPropFlags, Characteristic, Manager as _, Peripheral as _, RetrievePeripheralsOptions, ScanFilter, ValueNotification, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral, PeripheralId};
use eframe::egui;
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::i18n::{t, tf};
use crate::serial::{Cmd, Connection, Evt};

/// 记在设置里的「上次连的设备」
#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BleConfig {
    pub id: String,
    pub name: String,
    /// 写特征时要求设备应答（慢但可靠）；默认不要应答
    pub with_resp: bool,
    /// 只显示有名字的设备
    pub named_only: bool,
}

impl BleConfig {
    pub fn label(&self) -> String {
        if self.name.is_empty() {
            format!("ble://{}", self.id)
        } else {
            format!("ble://{}", self.name)
        }
    }
}

pub struct Dev {
    pub id: String,
    pub name: String,
    pub addr: String,
    pub rssi: Option<i16>,
    pub services: Vec<Uuid>,
    /// 厂商 ID（广播里的 Manufacturer Data）
    pub company: Option<u16>,
    pub seen: Instant,
}

/// 保持首见顺序；过滤只影响哪些设备可见。
pub fn visible_devices<'a>(devices: &'a [Dev], query: &'a str, named_only: bool) -> impl Iterator<Item = &'a Dev> {
    devices
        .iter()
        .filter(move |d| !named_only || !d.name.is_empty())
        .filter(move |d| query.is_empty() || d.name.to_lowercase().contains(query) || d.addr.to_lowercase().contains(query))
}

#[derive(Clone)]
pub struct Chr {
    pub service: Uuid,
    pub uuid: Uuid,
    pub props: CharPropFlags,
    /// 已订阅通知
    pub on: bool,
    /// 等待订阅或取消订阅的应答
    pub notify_pending: Option<bool>,
    /// 最后一次读到 / 收到的值
    pub last: Option<Vec<u8>>,
}

impl Chr {
    pub fn id(&self) -> CharId {
        CharId {
            service: self.service,
            uuid: self.uuid,
        }
    }

    pub fn can_write(&self) -> bool {
        self.props.intersects(CharPropFlags::WRITE | CharPropFlags::WRITE_WITHOUT_RESPONSE)
    }
    pub fn can_notify(&self) -> bool {
        self.props.intersects(CharPropFlags::NOTIFY | CharPropFlags::INDICATE)
    }
    pub fn can_read(&self) -> bool {
        self.props.contains(CharPropFlags::READ)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CharId {
    service: Uuid,
    uuid: Uuid,
}

impl From<&Characteristic> for CharId {
    fn from(c: &Characteristic) -> Self {
        Self {
            service: c.service_uuid,
            uuid: c.uuid,
        }
    }
}

/// 界面和 hub 线程共享的状态
#[derive(Default)]
pub struct Shared {
    /// 蓝牙不可用的原因（没有适配器、没开蓝牙、没权限）
    pub err: Option<String>,
    pub scanning: bool,
    pub devices: Vec<Dev>,
    /// 当前连接设备的特征表
    pub chars: Vec<Chr>,
    /// 发送区的数据写到这个特征
    pub target: Option<CharId>,
    /// 正在做的事：连接中 / 发现服务中
    pub busy: Option<String>,
    pub mtu: u16,
    gen: Option<u64>,
}

impl Shared {
    fn clear_link(&mut self) {
        self.gen = None;
        self.chars.clear();
        self.target = None;
        self.mtu = 0;
    }

    fn char_mut(&mut self, gen: u64, id: CharId) -> Option<&mut Chr> {
        if self.gen != Some(gen) {
            return None;
        }
        self.chars.iter_mut().find(|c| c.id() == id)
    }

    fn begin_notify(&mut self, gen: u64, id: CharId, on: bool) -> bool {
        let Some(c) = self.char_mut(gen, id) else { return false };
        if !c.can_notify() || c.notify_pending.is_some() || c.on == on {
            return false;
        }
        c.notify_pending = Some(on);
        true
    }

    fn finish_notify(&mut self, gen: u64, id: CharId, on: bool, ok: bool) {
        if let Some(c) = self.char_mut(gen, id) {
            c.notify_pending = None;
            if ok {
                c.on = on;
            }
        }
    }
}

pub enum HubCmd {
    Scan(bool),
    Clear,
    Connect {
        gen: u64,
        id: String,
        label: String,
        tx: Sender<Evt>,
        with_resp: bool,
        cancel: oneshot::Receiver<()>,
    },
    Conn(u64, Cmd),
    Target(u64, CharId),
    Notify(u64, CharId, bool),
    Read(u64, CharId),
    WithResp(u64, bool),
}

pub struct Hub {
    tx: UnboundedSender<HubCmd>,
    pub st: Arc<Mutex<Shared>>,
}

impl Hub {
    pub fn start(ctx: egui::Context) -> Hub {
        let (tx, rx) = unbounded_channel();
        let st = Arc::new(Mutex::new(Shared::default()));
        let st2 = st.clone();
        let spawned = std::thread::Builder::new().name("ble".into()).spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    st2.lock().unwrap().err = Some(e.to_string());
                    return;
                }
            };
            if std::env::var_os("QCCOM_BLE_DEMO").is_some() {
                rt.block_on(demo::run(rx, st2, ctx));
            } else {
                rt.block_on(run(rx, st2, ctx));
            }
        });
        if let Err(e) = spawned {
            st.lock().unwrap().err = Some(e.to_string());
        }
        Hub { tx, st }
    }

    pub fn send(&self, c: HubCmd) {
        if let HubCmd::Notify(gen, id, on) = c {
            let mut st = self.st.lock().unwrap();
            if st.begin_notify(gen, id, on) && self.tx.send(c).is_err() {
                st.finish_notify(gen, id, on, false);
            }
            return;
        }
        let _ = self.tx.send(c);
    }

    /// 连接设备。返回的 Connection 和串口 / 网络的用法一样：发 Cmd::Send 写数据，Cmd::Close 断开
    pub fn open(&self, gen: u64, cfg: &BleConfig, tx: Sender<Evt>) -> Connection {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Cmd>();
        let (cancel_tx, cancel) = oneshot::channel();
        self.send(HubCmd::Connect {
            gen,
            id: cfg.id.clone(),
            label: cfg.label(),
            tx,
            with_resp: cfg.with_resp,
            cancel,
        });
        // Connection 的命令是同步通道，转一手送进 hub
        let hub = self.tx.clone();
        let _ = std::thread::Builder::new().name("ble-cmd".into()).spawn(move || {
            while let Ok(c) = cmd_rx.recv() {
                let close = matches!(c, Cmd::Close);
                if close {
                    let _ = cancel_tx.send(());
                    let _ = hub.send(HubCmd::Conn(gen, c));
                    return;
                }
                if hub.send(HubCmd::Conn(gen, c)).is_err() {
                    return;
                }
            }
            let _ = cancel_tx.send(());
            let _ = hub.send(HubCmd::Conn(gen, Cmd::Close));
        });
        Connection { cmd: cmd_tx }
    }
}

type Notes = std::pin::Pin<Box<dyn Stream<Item = ValueNotification> + Send>>;
type Events = std::pin::Pin<Box<dyn Stream<Item = CentralEvent> + Send>>;

struct Live {
    gen: u64,
    p: Peripheral,
    tx: Sender<Evt>,
    with_resp: bool,
    listener: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.stop_listener();
    }
}

impl Live {
    fn stop_listener(&mut self) {
        if let Some(listener) = self.listener.take() {
            listener.abort();
        }
    }
}

enum Ev {
    Cmd(Option<HubCmd>),
    Central(Option<CentralEvent>),
    Tick,
}

async fn adapter() -> Result<Adapter, String> {
    let m = Manager::new().await.map_err(|e| tf("蓝牙不可用：{}", &[&e]))?;
    let list = m.adapters().await.map_err(|e| tf("蓝牙不可用：{}", &[&e]))?;
    list.into_iter().next().ok_or_else(|| t("没有找到蓝牙适配器").to_string())
}

async fn run(mut rx: UnboundedReceiver<HubCmd>, st: Arc<Mutex<Shared>>, ctx: egui::Context) {
    let central = match adapter().await {
        Ok(a) => a,
        Err(e) => {
            st.lock().unwrap().err = Some(e.clone());
            ctx.request_repaint();
            // 蓝牙用不了：连接请求直接回失败，其它命令忽略
            while let Some(c) = rx.recv().await {
                if let HubCmd::Connect { gen, tx, .. } = c {
                    let _ = tx.send(Evt::Closed(gen, Some(e.clone())));
                    ctx.request_repaint();
                }
            }
            return;
        }
    };
    let mut events: Option<Events> = central.events().await.ok();
    let mut live: Option<Live> = None;
    // 适配器在设备断开时可能清除扫描记录；自己保留最近连接的 ID。
    let mut known = None;
    let mut tick = tokio::time::interval(Duration::from_millis(800));
    loop {
        let ev = tokio::select! {
            c = rx.recv() => Ev::Cmd(c),
            e = async { match events.as_mut() { Some(s) => s.next().await, None => std::future::pending().await } } => Ev::Central(e),
            _ = tick.tick() => Ev::Tick,
        };
        match ev {
            Ev::Cmd(None) => break,
            Ev::Cmd(Some(c)) => handle(c, &central, &st, &mut live, &mut known, &ctx).await,
            Ev::Central(None) => events = None,
            Ev::Central(Some(CentralEvent::DeviceDisconnected(id))) => {
                if let Some(l) = live.as_ref().filter(|l| l.p.id() == id) {
                    // 上一条连接延迟到达的断开事件，不能关闭刚建立的新连接。
                    if matches!(tokio::time::timeout(Duration::from_secs(2), l.p.is_connected()).await, Ok(Ok(true))) {
                        continue;
                    }
                    let l = live.take().unwrap();
                    st.lock().unwrap().clear_link();
                    let _ = l.tx.send(Evt::Closed(l.gen, Some(t("设备断开了连接").into())));
                    ctx.request_repaint();
                }
            }
            Ev::Central(Some(_)) => {}
            Ev::Tick => {
                if st.lock().unwrap().scanning {
                    refresh(&central, &st).await;
                    ctx.request_repaint();
                }
            }
        }
    }
    if let Some(mut l) = live {
        st.lock().unwrap().clear_link();
        l.stop_listener();
        disconnect_peripheral(&l.p).await;
    }
}

/// 扫描中：把适配器知道的设备同步到列表里（保持原来的顺序，免得列表跳来跳去）
async fn refresh(central: &Adapter, st: &Arc<Mutex<Shared>>) {
    let Ok(list) = central.peripherals().await else { return };
    let mut found = Vec::new();
    for p in list {
        let Ok(Some(pr)) = p.properties().await else { continue };
        // WinRT 允许按缓存地址创建对象；没收到广播的对象不属于附近设备列表。
        #[cfg(windows)]
        if pr.rssi.is_none() {
            continue;
        }
        let name = pr.local_name.or(pr.advertisement_name).unwrap_or_default();
        found.push(Dev {
            id: p.id().to_string(),
            name: name.trim_matches('\0').trim().to_string(),
            addr: pr.address.to_string(),
            rssi: pr.rssi,
            services: pr.services,
            company: pr.manufacturer_data.keys().next().copied(),
            seen: Instant::now(),
        });
    }
    update_devices(&mut st.lock().unwrap().devices, found);
}

fn update_devices(existing: &mut Vec<Dev>, found: Vec<Dev>) {
    for d in found {
        match existing.iter_mut().find(|x| x.id == d.id) {
            Some(x) => {
                // 只有信号值变了才算「又看到了」（BlueZ 会一直报缓存里的设备）
                if d.rssi.is_some() && (d.rssi != x.rssi || d.seen.saturating_duration_since(x.seen) < Duration::from_secs(5)) {
                    x.seen = d.seen;
                }
                if !d.name.is_empty() {
                    x.name = d.name;
                }
                x.rssi = d.rssi.or(x.rssi);
                if !d.services.is_empty() {
                    x.services = d.services;
                }
                x.company = d.company.or(x.company);
            }
            None => existing.push(d),
        }
    }
}

async fn find(central: &Adapter, id: &str) -> Option<Peripheral> {
    let p = central.peripherals().await.ok()?.into_iter().find(|p| p.id().to_string() == id)?;
    #[cfg(windows)]
    if p.properties().await.ok().flatten()?.rssi.is_none() {
        return None;
    }
    Some(p)
}

async fn find_cached(central: &Adapter, id: &str, known: Option<&PeripheralId>) -> Option<Peripheral> {
    // Windows/macOS 的设置也能跨应用重启直接从地址/UUID 查找。
    let key = known.filter(|key| key.to_string() == id).cloned().or_else(|| {
        #[cfg(windows)]
        return id.parse::<btleplug::api::BDAddr>().ok().map(PeripheralId::from);
        #[cfg(target_os = "macos")]
        return id.parse::<Uuid>().ok().map(PeripheralId::from);
        #[cfg(not(any(windows, target_os = "macos")))]
        None
    })?;
    if let Ok(p) = central.peripheral(&key).await {
        return Some(p);
    }
    // WinRT 的 retrieve_peripherals 只返回已连接设备；add_peripheral 可用已知地址建立连接。
    if cfg!(windows) {
        return central.add_peripheral(&key).await.ok();
    }
    central
        .retrieve_peripherals(RetrievePeripheralsOptions {
            identifiers: Some(vec![key.clone()]),
            services: None,
        })
        .await
        .ok()?
        .into_iter()
        .find(|p| p.id() == key)
}

async fn find_scanned(central: &Adapter, id: &str, scanning: bool) -> Option<Peripheral> {
    // 系统可能也没有缓存（例如未配对的设备）：先等它广播，再停止扫描连接。
    if !scanning && set_scan(central, true).await.is_err() {
        return None;
    }
    let found = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(p) = find(central, id).await {
                return p;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .ok();
    if !scanning {
        let _ = set_scan(central, false).await;
    }
    found
}

async fn find_for_connect(central: &Adapter, id: &str, known: Option<&PeripheralId>, scanning: bool) -> Option<(Peripheral, bool)> {
    resolve_peripheral(
        async { tokio::time::timeout(Duration::from_secs(2), find(central, id)).await.ok().flatten() },
        async { tokio::time::timeout(Duration::from_secs(3), find_cached(central, id, known)).await.ok().flatten() },
        find_scanned(central, id, scanning),
    )
    .await
}

async fn resolve_peripheral<P>(
    nearby: impl std::future::Future<Output = Option<P>>,
    cached: impl std::future::Future<Output = Option<P>>,
    scan: impl std::future::Future<Output = Option<P>>,
) -> Option<(P, bool)> {
    if let Some(p) = nearby.await {
        return Some((p, false));
    }
    if let Some(p) = cached.await {
        return Some((p, true));
    }
    scan.await.map(|p| (p, false))
}

async fn set_scan(central: &Adapter, on: bool) -> Result<(), String> {
    let change = async {
        if on {
            central.start_scan(ScanFilter::default()).await
        } else {
            central.stop_scan().await
        }
    };
    match tokio::time::timeout(Duration::from_secs(2), change).await {
        Ok(r) => r.map_err(|e| e.to_string()),
        Err(_) => Err(t("超时").into()),
    }
}

async fn disconnect_peripheral(p: &Peripheral) {
    // CoreBluetooth/BlueZ 或驱动不回应时，也要让 hub 能继续处理命令。
    let _ = tokio::time::timeout(Duration::from_secs(2), p.disconnect()).await;
}

#[derive(PartialEq, Eq)]
enum SetupStage {
    Connect,
    Discover,
}

struct SetupError {
    stage: SetupStage,
    message: String,
}

impl SetupError {
    fn connect(e: impl std::fmt::Display) -> Self {
        Self {
            stage: SetupStage::Connect,
            message: tf("连接失败：{}", &[&e]),
        }
    }

    fn discover(e: impl std::fmt::Display) -> Self {
        Self {
            stage: SetupStage::Discover,
            message: tf("读取服务失败：{}", &[&e]),
        }
    }
}

fn needs_retry(windows: bool, attempt: usize, from_cache: bool, stage: &SetupStage) -> bool {
    attempt == 0 && (windows || from_cache && *stage == SetupStage::Connect)
}

fn check_characteristics(chars: Vec<Characteristic>) -> Result<Vec<Characteristic>, SetupError> {
    if chars.is_empty() {
        return Err(SetupError::discover(t("没有发现 GATT 特征")));
    }
    Ok(chars)
}

async fn setup_attempt(p: &Peripheral, busy: &impl Fn(Option<String>), attempt: usize) -> Result<Vec<Characteristic>, SetupError> {
    busy(Some(if attempt == 0 { t("正在连接…") } else { t("正在重试连接…") }.into()));
    // WinRT 的 connect 自身会发起一次 GATT 请求；不能让它独占整个重试预算。
    let connect_timeout = Duration::from_secs(if cfg!(windows) { 10 } else { 30 });
    match tokio::time::timeout(connect_timeout, p.connect()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(SetupError::connect(e)),
        Err(_) => return Err(SetupError::connect(t("超时"))),
    }
    busy(Some(t("正在读取服务…").into()));
    let budget = Duration::from_secs(if cfg!(windows) { 12 } else { 20 });
    let discover = async {
        p.discover_services().await.map_err(SetupError::discover)?;
        if !p.is_connected().await.map_err(SetupError::discover)? {
            return Err(SetupError::discover(t("设备断开了连接")));
        }
        // WinRT 可能吞掉非成功的 GATT 状态或枚举失败，返回 Ok 但没有特征。
        check_characteristics(p.characteristics().into_iter().collect())
    };
    match tokio::time::timeout(budget, discover).await {
        Ok(result) => result,
        Err(_) => Err(SetupError {
            stage: SetupStage::Discover,
            message: t("读取服务超时").into(),
        }),
    }
}

async fn setup_link(
    central: &Adapter,
    id: &str,
    known: Option<&PeripheralId>,
    st: &Mutex<Shared>,
    pending: &mut Option<Peripheral>,
    busy: &impl Fn(Option<String>),
) -> Result<Vec<Characteristic>, String> {
    let scanning = st.lock().unwrap().scanning;
    let Some((mut p, mut from_cache)) = find_for_connect(central, id, known, scanning).await else {
        return Err(t("找不到这个设备，请确认设备已开启并在广播").into());
    };
    if scanning {
        // 停止扫描再连接，避免适配器切换时干扰 GATT 操作。
        let _ = set_scan(central, false).await;
        st.lock().unwrap().scanning = false;
    }

    for attempt in 0..2 {
        *pending = Some(p);
        let p_ref = pending.as_ref().unwrap();
        let error = match setup_attempt(p_ref, busy, attempt).await {
            Ok(chars) => return Ok(chars),
            Err(error) => error,
        };
        if !needs_retry(cfg!(windows), attempt, from_cache, &error.stage) {
            return Err(error.message);
        }

        let needs_advertisement = from_cache && error.stage == SetupStage::Connect;
        let last_id = p_ref.id();
        busy(Some(t("正在重试连接…").into()));
        disconnect_peripheral(p_ref).await;
        *pending = None;
        tokio::time::sleep(Duration::from_millis(350)).await;
        // 重新获取对象。旧对象上 WinRT 可能缓存了空服务，重新 discover 会跳过它们。
        let candidate = if needs_advertisement {
            find_scanned(central, id, false).await.map(|p| (p, false))
        } else {
            find_for_connect(central, id, Some(&last_id), false).await
        };
        let Some((replacement, cached)) = candidate else { return Err(error.message) };
        p = replacement;
        from_cache = cached;
    }
    unreachable!("the second failed attempt does not retry")
}

fn fail(tx: &Sender<Evt>, gen: u64, e: impl std::fmt::Display, ctx: &egui::Context) {
    let _ = tx.send(Evt::Closed(gen, Some(e.to_string())));
    ctx.request_repaint();
}

const NOTIFY_TIMEOUT: Duration = Duration::from_secs(5);

async fn open_notifications(p: &Peripheral) -> Result<Notes, String> {
    match tokio::time::timeout(NOTIFY_TIMEOUT, p.notifications()).await {
        Ok(r) => r.map_err(|e| e.to_string()),
        Err(_) => Err(t("超时").into()),
    }
}

fn deliver_notification(st: &mut Shared, tx: &Sender<Evt>, gen: u64, note: ValueNotification) {
    let id = CharId {
        service: note.service_uuid,
        uuid: note.uuid,
    };
    let Some(c) = st.char_mut(gen, id).filter(|c| c.on || c.notify_pending == Some(true)) else {
        return;
    };
    let _ = tx.send(Evt::Packet(char_name(&id.uuid), note.value.clone()));
    c.last = Some(note.value);
}

async fn receive_notifications<F, Fut>(mut notes: Notes, mut reopen: F, retry: Duration, gen: u64, tx: Sender<Evt>, st: Arc<Mutex<Shared>>, ctx: egui::Context)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Notes, String>>,
{
    loop {
        while let Some(note) = notes.next().await {
            deliver_notification(&mut st.lock().unwrap(), &tx, gen, note);
            ctx.request_repaint_after(Duration::from_millis(33));
        }
        let _ = tx.send(Evt::Sys(t("通知接收已中断，正在重试…").into()));
        ctx.request_repaint();
        // 重新打开接收流不需要再次写入设备的订阅配置。
        loop {
            tokio::time::sleep(retry).await;
            if let Ok(stream) = reopen().await {
                notes = stream;
                break;
            }
        }
    }
}

async fn ensure_listener(l: &mut Live, st: &Arc<Mutex<Shared>>, ctx: &egui::Context) -> Result<(), String> {
    if l.listener.is_none() {
        let notes = open_notifications(&l.p).await?;
        let p = l.p.clone();
        let reopen = move || {
            let p = p.clone();
            async move { open_notifications(&p).await }
        };
        // 独立接收，避免订阅、读写或刷新列表等待时，上游通知缓冲区溢出。
        l.listener = Some(tokio::spawn(receive_notifications(
            notes,
            reopen,
            Duration::from_millis(800),
            l.gen,
            l.tx.clone(),
            st.clone(),
            ctx.clone(),
        )));
    }
    Ok(())
}

async fn change_notification(l: &mut Live, st: &Arc<Mutex<Shared>>, ctx: &egui::Context, c: &Characteristic, on: bool) {
    let result = async {
        if on {
            ensure_listener(l, st, ctx).await?;
        }
        let change = async {
            if on {
                l.p.subscribe(c).await
            } else {
                l.p.unsubscribe(c).await
            }
        };
        match tokio::time::timeout(NOTIFY_TIMEOUT, change).await {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(_) => Err(t("超时").into()),
        }
    }
    .await;
    st.lock().unwrap().finish_notify(l.gen, c.into(), on, result.is_ok());
    if let Err(e) = result {
        let _ = l.tx.send(Evt::Sys(tf("✗ 订阅失败：{}", &[&format!("{}: {e}", char_name(&c.uuid))])));
    }
    ctx.request_repaint();
}

async fn handle(c: HubCmd, central: &Adapter, st: &Arc<Mutex<Shared>>, live: &mut Option<Live>, known: &mut Option<PeripheralId>, ctx: &egui::Context) {
    let busy = |m: Option<String>| {
        st.lock().unwrap().busy = m;
        ctx.request_repaint();
    };
    match c {
        HubCmd::Scan(on) => {
            let r = set_scan(central, on).await;
            let mut s = st.lock().unwrap();
            match r {
                Ok(()) => {
                    s.scanning = on;
                    s.err = None;
                }
                Err(e) if on => s.err = Some(tf("无法开始扫描：{}", &[&e])),
                Err(_) => s.scanning = false,
            }
            ctx.request_repaint();
        }
        HubCmd::Clear => st.lock().unwrap().devices.clear(),
        HubCmd::Connect {
            gen,
            id,
            label,
            tx,
            with_resp,
            mut cancel,
        } => {
            st.lock().unwrap().clear_link();
            if let Some(mut l) = live.take() {
                l.stop_listener();
                disconnect_peripheral(&l.p).await;
            }
            busy(Some(t("正在连接…").into()));
            let mut pending = None;
            // 两次尝试共用一个截止时间，取消连接不必排队等 GATT 返回。
            let deadline = Duration::from_secs(if cfg!(windows) { 32 } else { 60 });
            let result = tokio::select! {
                biased;
                _ = &mut cancel => None,
                r = tokio::time::timeout(deadline, setup_link(central, &id, known.as_ref(), st, &mut pending, &busy)) => Some(r),
            };
            let chars = match result {
                Some(Ok(Ok(chars))) => chars,
                other => {
                    if let Some(p) = pending {
                        disconnect_peripheral(&p).await;
                    }
                    // 扫描若是连接过程临时开启的，超时/取消时也要收尾。
                    let scanning = st.lock().unwrap().scanning;
                    if !scanning && !matches!(other, Some(Ok(Err(_)))) {
                        let _ = set_scan(central, false).await;
                    }
                    busy(None);
                    match other {
                        Some(Ok(Err(e))) => fail(&tx, gen, e, ctx),
                        Some(Err(_)) => fail(&tx, gen, SetupError::connect(t("超时")).message, ctx),
                        None => {
                            let _ = tx.send(Evt::Closed(gen, None));
                        }
                        Some(Ok(Ok(_))) => unreachable!(),
                    }
                    return;
                }
            };
            let p = pending.expect("successful GATT setup has a peripheral");
            let target = pick_target(&chars);
            {
                let mut s = st.lock().unwrap();
                s.gen = Some(gen);
                s.chars = chars
                    .iter()
                    .map(|c| Chr {
                        service: c.service_uuid,
                        uuid: c.uuid,
                        props: c.properties,
                        on: false,
                        notify_pending: None,
                        last: None,
                    })
                    .collect();
                s.target = target;
                s.busy = None;
                s.mtu = p.mtu();
            }
            let _ = tx.send(Evt::Opened(gen, label));
            match target {
                Some(u) => {
                    let _ = tx.send(Evt::Sys(tf("发送数据会写到特征 {}", &[&char_name(&u.uuid)])));
                }
                None => {
                    let _ = tx.send(Evt::Sys(t("这个设备没有可写的特征，只能接收").into()));
                }
            }
            ctx.request_repaint();
            *known = Some(p.id());
            let mut l = Live {
                gen,
                p,
                tx,
                with_resp,
                listener: None,
            };
            // 自动订阅能通知的特征（标准服务里的除外）；发送已连接事件后再传递通知。
            let subscribe_automatically = async {
                let mut auto = chars
                    .iter()
                    .filter(|c| c.properties.intersects(CharPropFlags::NOTIFY | CharPropFlags::INDICATE) && !is_generic(&c.service_uuid))
                    .peekable();
                if auto.peek().is_some() {
                    match ensure_listener(&mut l, st, ctx).await {
                        Ok(()) => {
                            for c in auto {
                                let pending = st.lock().unwrap().begin_notify(gen, c.into(), true);
                                if pending {
                                    change_notification(&mut l, st, ctx, c, true).await;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = l.tx.send(Evt::Sys(tf("✗ 订阅失败：{}", &[&e])));
                            ctx.request_repaint();
                        }
                    }
                }
            };
            let canceled = tokio::select! {
                biased;
                _ = &mut cancel => true,
                _ = subscribe_automatically => false,
            };
            if canceled {
                st.lock().unwrap().clear_link();
                l.stop_listener();
                disconnect_peripheral(&l.p).await;
                let _ = l.tx.send(Evt::Closed(gen, None));
                ctx.request_repaint();
            } else {
                *live = Some(l);
            }
        }
        HubCmd::Conn(gen, cmd) => {
            let Some(l) = live.as_ref().filter(|l| l.gen == gen) else { return };
            match cmd {
                Cmd::Send(d) => {
                    let target = st.lock().unwrap().target;
                    let Some(c) = target.and_then(|id| l.p.characteristics().into_iter().find(|c| CharId::from(c) == id)) else {
                        let _ = l.tx.send(Evt::Sys(t("✗ 没有选择要写入的特征").into()));
                        return;
                    };
                    let wt = if l.with_resp && c.properties.contains(CharPropFlags::WRITE) || !c.properties.contains(CharPropFlags::WRITE_WITHOUT_RESPONSE) {
                        WriteType::WithResponse
                    } else {
                        WriteType::WithoutResponse
                    };
                    // 一次最多写 MTU-3 字节，长数据分包
                    let n = (l.p.mtu().max(23) - 3) as usize;
                    for part in d.chunks(n) {
                        if let Err(e) = l.p.write(&c, part, wt).await {
                            let _ = l.tx.send(Evt::Sys(tf("✗ 写入失败：{}", &[&e])));
                            break;
                        }
                    }
                }
                Cmd::Close => {
                    let mut l = live.take().unwrap();
                    st.lock().unwrap().clear_link();
                    l.stop_listener();
                    disconnect_peripheral(&l.p).await;
                    let _ = l.tx.send(Evt::Closed(l.gen, None));
                    ctx.request_repaint();
                }
                _ => {}
            }
        }
        HubCmd::Target(gen, id) => {
            let mut s = st.lock().unwrap();
            if s.char_mut(gen, id).is_some_and(|c| c.can_write()) {
                s.target = Some(id);
            }
        }
        HubCmd::WithResp(gen, on) => {
            if let Some(l) = live.as_mut().filter(|l| l.gen == gen) {
                l.with_resp = on;
            }
        }
        HubCmd::Notify(gen, id, on) => {
            let Some(l) = live.as_mut().filter(|l| l.gen == gen) else { return };
            if let Some(c) = l.p.characteristics().into_iter().find(|c| CharId::from(c) == id) {
                change_notification(l, st, ctx, &c, on).await;
            } else {
                st.lock().unwrap().finish_notify(gen, id, on, false);
                ctx.request_repaint();
            }
        }
        HubCmd::Read(gen, id) => {
            let Some(l) = live.as_ref().filter(|l| l.gen == gen) else { return };
            let Some(c) = l.p.characteristics().into_iter().find(|c| CharId::from(c) == id) else { return };
            match l.p.read(&c).await {
                Ok(v) => {
                    let _ = l.tx.send(Evt::Packet(format!("{} R", char_name(&id.uuid)), read_text(&v)));
                    if let Some(x) = st.lock().unwrap().char_mut(gen, id) {
                        x.last = Some(v);
                    }
                }
                Err(e) => {
                    let _ = l.tx.send(Evt::Sys(tf("✗ 读取失败：{}", &[&e])));
                }
            }
            ctx.request_repaint();
        }
    }
}

// ------------------------------------------------------------ UUID 和名字

const BASE_TAIL: u128 = 0x0000_1000_8000_0080_5f9b_34fb;

/// 16 位短 UUID（蓝牙联盟分配的标准 UUID）；不是标准格式返回 None
pub fn short(u: &Uuid) -> Option<u16> {
    let v = u.as_u128();
    if v & ((1u128 << 96) - 1) == BASE_TAIL && v >> 112 == 0 {
        Some((v >> 96) as u16)
    } else {
        None
    }
}

fn is_generic(u: &Uuid) -> bool {
    matches!(short(u), Some(0x1800 | 0x1801))
}

const NUS_SVC: u128 = 0x6e400001_b5a3_f393_e0a9_e50e24dcca9e;
const NUS_RX: u128 = 0x6e400002_b5a3_f393_e0a9_e50e24dcca9e;
const NUS_TX: u128 = 0x6e400003_b5a3_f393_e0a9_e50e24dcca9e;

/// 常见服务的名字
pub fn service_name(u: &Uuid) -> Option<&'static str> {
    if u.as_u128() == NUS_SVC {
        return Some("Nordic UART");
    }
    Some(match short(u)? {
        0x1800 => "Generic Access",
        0x1801 => "Generic Attribute",
        0x180A => "Device Information",
        0x180F => "Battery",
        0x180D => "Heart Rate",
        0x1809 => "Health Thermometer",
        0x181A => "Environmental Sensing",
        0x1812 => "HID",
        0x181C => "User Data",
        0xFE59 => "Nordic DFU",
        0xFFE0 => "HM-10 / CC254x UART",
        0xFFF0 => "Vendor UART",
        _ => return None,
    })
}

/// 常见特征的名字
pub fn known_char(u: &Uuid) -> Option<&'static str> {
    match u.as_u128() {
        NUS_RX => return Some("NUS RX"),
        NUS_TX => return Some("NUS TX"),
        _ => {}
    }
    Some(match short(u)? {
        0x2A00 => "Device Name",
        0x2A01 => "Appearance",
        0x2A04 => "Conn Params",
        0x2A05 => "Service Changed",
        0x2A19 => "Battery Level",
        0x2A23 => "System ID",
        0x2A24 => "Model Number",
        0x2A25 => "Serial Number",
        0x2A26 => "Firmware Rev",
        0x2A27 => "Hardware Rev",
        0x2A28 => "Software Rev",
        0x2A29 => "Manufacturer",
        0x2A37 => "Heart Rate",
        0x2A6E => "Temperature",
        0x2A6F => "Humidity",
        _ => return None,
    })
}

/// 日志里用的特征名：认识的写名字，标准 UUID 写 4 位，其它写前 8 位
pub fn char_name(u: &Uuid) -> String {
    if let Some(n) = known_char(u) {
        return n.to_string();
    }
    uuid_short_str(u)
}

pub fn uuid_short_str(u: &Uuid) -> String {
    match short(u) {
        Some(s) => format!("{s:04X}"),
        None => u.simple().to_string()[..8].to_uppercase(),
    }
}

/// 自动挑一个写特征：先找常见透传服务，再找第一个非标准服务里可写的
fn pick_target(chars: &[Characteristic]) -> Option<CharId> {
    let writable = |c: &&Characteristic| c.properties.intersects(CharPropFlags::WRITE | CharPropFlags::WRITE_WITHOUT_RESPONSE);
    let prefer = [
        Uuid::from_u128(NUS_RX),
        Uuid::from_u128((0xFFE1u128 << 96) | BASE_TAIL),
        Uuid::from_u128((0xFFF2u128 << 96) | BASE_TAIL),
    ];
    for p in prefer {
        if let Some(c) = chars.iter().filter(writable).find(|c| c.uuid == p) {
            return Some(c.into());
        }
    }
    chars
        .iter()
        .filter(writable)
        .find(|c| !matches!(short(&c.service_uuid), Some(0x1800..=0x18FF)))
        .or_else(|| chars.iter().find(|c| writable(c)))
        .map(CharId::from)
}

/// 读到的值写进日志：3 字节以上的可读文字原样，否则写成 HEX（1~4 字节再附上小端整数值）
pub fn read_text(v: &[u8]) -> Vec<u8> {
    if let Ok(s) = std::str::from_utf8(v) {
        if s.len() >= 3 && s.chars().all(|c| !c.is_control() || c == '\n' || c == '\r' || c == '\t') {
            return v.to_vec();
        }
    }
    let mut out: String = v.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ");
    if (1..=4).contains(&v.len()) {
        let n = v.iter().rev().fold(0u32, |a, &b| (a << 8) | b as u32);
        out.push_str(&format!("  ({n})"));
    }
    out.into_bytes()
}

/// 信号强度分 0~4 格
pub fn bars(rssi: Option<i16>) -> u8 {
    match rssi {
        None => 0,
        Some(r) if r >= -60 => 4,
        Some(r) if r >= -70 => 3,
        Some(r) if r >= -80 => 2,
        Some(_) => 1,
    }
}

/// 常见厂商 ID
pub fn company(id: u16) -> Option<&'static str> {
    Some(match id {
        0x004C => "Apple",
        0x0006 => "Microsoft",
        0x0075 => "Samsung",
        0x00E0 => "Google",
        0x0059 => "Nordic",
        0x000D => "TI",
        0x02E5 => "Espressif",
        0x0157 => "Huami",
        0x038F => "Xiaomi",
        0x027D => "Huawei",
        0x0131 => "Cypress",
        0x0030 => "ST",
        _ => return None,
    })
}

/// 演示模式（环境变量 QCCOM_BLE_DEMO=1）：没有蓝牙硬件时模拟几个设备，用来截图和测界面。
/// 连上「QC-Sensor」后写 NUS RX 的数据会从 NUS TX 原样回来，并且每秒推一条温度。
mod demo {
    use super::*;

    const TX: CharId = CharId {
        service: Uuid::from_u128(NUS_SVC),
        uuid: Uuid::from_u128(NUS_TX),
    };
    const RX: CharId = CharId {
        service: Uuid::from_u128(NUS_SVC),
        uuid: Uuid::from_u128(NUS_RX),
    };

    fn std16(x: u16) -> Uuid {
        Uuid::from_u128(((x as u128) << 96) | BASE_TAIL)
    }

    fn chars() -> Vec<Chr> {
        let c = |service: Uuid, uuid: Uuid, props: CharPropFlags| Chr {
            service,
            uuid,
            props,
            on: false,
            notify_pending: None,
            last: None,
        };
        let nus = Uuid::from_u128(NUS_SVC);
        vec![
            c(std16(0x1800), std16(0x2A00), CharPropFlags::READ),
            c(std16(0x180A), std16(0x2A29), CharPropFlags::READ),
            c(std16(0x180A), std16(0x2A26), CharPropFlags::READ),
            c(std16(0x180F), std16(0x2A19), CharPropFlags::READ | CharPropFlags::NOTIFY),
            c(nus, Uuid::from_u128(NUS_RX), CharPropFlags::WRITE | CharPropFlags::WRITE_WITHOUT_RESPONSE),
            c(nus, Uuid::from_u128(NUS_TX), CharPropFlags::NOTIFY),
        ]
    }

    pub async fn run(mut rx: UnboundedReceiver<HubCmd>, st: Arc<Mutex<Shared>>, ctx: egui::Context) {
        let devs: [(&str, &str, i16, Option<Uuid>, Option<u16>); 6] = [
            ("QC-Sensor", "C4:4F:33:7A:10:2E", -52, Some(Uuid::from_u128(NUS_SVC)), Some(0x02E5)),
            ("HM-10", "A4:C1:38:22:9B:01", -67, Some(std16(0xFFE0)), None),
            ("Mi Band 7", "E8:12:4D:90:3C:77", -74, None, Some(0x0157)),
            ("", "5D:21:A0:7F:E2:19", -81, None, Some(0x004C)),
            ("ESP32-C3", "84:F7:03:11:AB:6C", -59, None, Some(0x02E5)),
            ("", "72:9B:16:C4:08:D3", -90, None, Some(0x0006)),
        ];
        let mut live: Option<(u64, Sender<Evt>)> = None;
        let mut tick = tokio::time::interval(Duration::from_millis(1000));
        let mut n = 0u32;
        loop {
            let c = tokio::select! {
                c = rx.recv() => match c { Some(c) => Some(c), None => return },
                _ = tick.tick() => None,
            };
            n += 1;
            let mut s = st.lock().unwrap();
            match c {
                None => {
                    if s.scanning {
                        let k = (s.devices.len() + 1).min(devs.len());
                        for (i, d) in devs.iter().take(k).enumerate() {
                            let rssi = d.2 + ((n as i16 + i as i16 * 3) % 5) - 2;
                            match s.devices.iter_mut().find(|x| x.addr == d.1) {
                                Some(x) => {
                                    x.rssi = Some(rssi);
                                    x.seen = Instant::now();
                                }
                                None => s.devices.push(Dev {
                                    id: d.1.into(),
                                    name: d.0.into(),
                                    addr: d.1.into(),
                                    rssi: Some(rssi),
                                    services: d.3.into_iter().collect(),
                                    company: d.4,
                                    seen: Instant::now(),
                                }),
                            }
                        }
                    }
                    if let Some((gen, tx)) = &live {
                        let on = s.char_mut(*gen, TX).is_some_and(|c| c.on);
                        if on {
                            let v = format!("temp={:.1} hum={:.0}\n", 24.0 + (n as f32 * 0.3).sin() * 1.5, 48.0 + (n as f32 * 0.2).cos() * 4.0);
                            deliver_notification(
                                &mut s,
                                tx,
                                *gen,
                                ValueNotification {
                                    service_uuid: TX.service,
                                    uuid: TX.uuid,
                                    value: v.into_bytes(),
                                },
                            );
                        }
                    }
                }
                Some(HubCmd::Scan(on)) => s.scanning = on,
                Some(HubCmd::Clear) => s.devices.clear(),
                Some(HubCmd::Connect { gen, id, label, tx, .. }) => {
                    s.scanning = false;
                    s.clear_link();
                    live = None;
                    if id != devs[0].1 {
                        let _ = tx.send(Evt::Closed(gen, Some(tf("连接失败：{}", &[&"Timed out"]))));
                    } else {
                        let mut ch = chars();
                        for c in ch.iter_mut().filter(|c| c.can_notify() && !is_generic(&c.service)) {
                            c.on = true;
                        }
                        s.gen = Some(gen);
                        s.chars = ch;
                        s.target = Some(RX);
                        s.mtu = 247;
                        let _ = tx.send(Evt::Opened(gen, label));
                        let _ = tx.send(Evt::Sys(tf("发送数据会写到特征 {}", &[&"NUS RX"])));
                        live = Some((gen, tx));
                    }
                }
                Some(HubCmd::Conn(gen, cmd)) => {
                    let Some((g, tx)) = &live else { continue };
                    if *g != gen {
                        continue;
                    }
                    match cmd {
                        Cmd::Send(d) => {
                            if s.target == Some(RX) {
                                deliver_notification(
                                    &mut s,
                                    tx,
                                    gen,
                                    ValueNotification {
                                        service_uuid: TX.service,
                                        uuid: TX.uuid,
                                        value: d,
                                    },
                                );
                            }
                        }
                        Cmd::Close => {
                            let _ = tx.send(Evt::Closed(gen, None));
                            s.clear_link();
                            live = None;
                        }
                        _ => {}
                    }
                }
                Some(HubCmd::Target(gen, id)) => {
                    if s.char_mut(gen, id).is_some_and(|c| c.can_write()) {
                        s.target = Some(id);
                    }
                }
                Some(HubCmd::Notify(gen, id, on)) => s.finish_notify(gen, id, on, true),
                Some(HubCmd::Read(gen, id)) => {
                    let Some(c) = s.char_mut(gen, id) else { continue };
                    let v: Vec<u8> = match short(&id.uuid) {
                        Some(0x2A00) => b"QC-Sensor".to_vec(),
                        Some(0x2A29) => b"Espressif".to_vec(),
                        Some(0x2A26) => b"v2.3.1".to_vec(),
                        Some(0x2A19) => vec![87],
                        _ => vec![],
                    };
                    if let Some((_, tx)) = &live {
                        let _ = tx.send(Evt::Packet(format!("{} R", char_name(&id.uuid)), read_text(&v)));
                    }
                    c.last = Some(v);
                }
                Some(HubCmd::WithResp(..)) => {}
            }
            drop(s);
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn std16(x: u16) -> Uuid {
        Uuid::from_u128(((x as u128) << 96) | BASE_TAIL)
    }

    #[test]
    fn short_uuids() {
        assert_eq!(short(&std16(0x180F)), Some(0x180F));
        assert_eq!(short(&Uuid::from_u128(NUS_RX)), None);
        assert_eq!(char_name(&std16(0xFFE1)), "FFE1");
        assert_eq!(char_name(&std16(0x2A19)), "Battery Level");
        assert_eq!(char_name(&Uuid::from_u128(NUS_TX)), "NUS TX");
        assert_eq!(uuid_short_str(&Uuid::from_u128(0x12345678_0000_0000_0000_000000000000)), "12345678");
        assert_eq!(service_name(&std16(0xFFE0)), Some("HM-10 / CC254x UART"));
        assert_eq!(read_text(b"v2.3"), b"v2.3");
        assert_eq!(read_text(&[87]), b"57  (87)");
        assert_eq!(read_text(&[0x10, 0x27]), b"10 27  (10000)");
    }

    #[test]
    fn picks_uart_char() {
        let ch = |svc: Uuid, u: Uuid, p: CharPropFlags| Characteristic {
            uuid: u,
            service_uuid: svc,
            properties: p,
            descriptors: Default::default(),
        };
        let list = vec![
            ch(std16(0x1800), std16(0x2A00), CharPropFlags::READ | CharPropFlags::WRITE),
            ch(std16(0xAB00), std16(0xAB01), CharPropFlags::WRITE),
            ch(std16(0xFFE0), std16(0xFFE1), CharPropFlags::WRITE_WITHOUT_RESPONSE | CharPropFlags::NOTIFY),
        ];
        assert_eq!(pick_target(&list), Some(CharId::from(&list[2])));
        assert_eq!(pick_target(&list[..2]), Some(CharId::from(&list[1])));
        assert_eq!(pick_target(&list[..1]), Some(CharId::from(&list[0])));
    }

    #[tokio::test]
    async fn reconnect_with_no_scan_result_uses_cache_then_scan_if_needed() {
        let cached = resolve_peripheral(async { None }, async { Some("remembered") }, async { panic!("cached reconnect should not scan") }).await;
        assert_eq!(cached, Some(("remembered", true)));
        let found = resolve_peripheral(async { None }, async { None }, async { Some("advertised") }).await;
        assert_eq!(found, Some(("advertised", false)));
    }

    #[test]
    fn windows_retries_transient_or_empty_gatt_setup_only_once() {
        let result = check_characteristics(vec![]);
        assert!(result.is_err(), "backend success with an empty list is not a ready link");
        let err = result.err().unwrap();
        assert!(needs_retry(true, 0, false, &err.stage));
        assert!(!needs_retry(true, 1, true, &err.stage));
        assert!(needs_retry(true, 0, false, &SetupStage::Connect));
        assert!(!needs_retry(false, 0, false, &err.stage));
        assert!(needs_retry(false, 0, true, &SetupStage::Connect));
        let usable = Characteristic {
            service_uuid: std16(0x180F),
            uuid: std16(0x2A19),
            properties: CharPropFlags::READ,
            descriptors: Default::default(),
        };
        assert!(check_characteristics(vec![usable]).is_ok(), "read-only standard devices are supported");
    }

    #[test]
    fn discoveries_stay_in_first_seen_order_across_name_rssi_and_age_changes() {
        let now = Instant::now();
        let dev = |id: &str, name: &str, rssi: i16, seen: Instant| Dev {
            id: id.into(),
            name: name.into(),
            addr: id.into(),
            rssi: Some(rssi),
            services: vec![],
            company: None,
            seen,
        };
        let mut devices = vec![];
        update_devices(&mut devices, vec![dev("A", "", -91, now), dev("B", "Bob", -48, now)]);
        let visible = |devices: &[Dev], query, named_only| visible_devices(devices, query, named_only).map(|d| d.id.clone()).collect::<Vec<_>>();
        assert_eq!(visible(&devices, "", false), ["A", "B"]);
        assert_eq!(visible(&devices, "", true), ["B"]);

        let later = now + Duration::from_secs(35);
        update_devices(&mut devices, vec![dev("B", "Bob", -48, later), dev("C", "Cat", -40, later), dev("A", "Ada", -43, later)]);
        assert_eq!(visible(&devices, "", true), ["A", "B", "C"]);
        assert_eq!(visible(&devices, "a", false), ["A", "C"]);
        assert_eq!(devices[0].name, "Ada");
        assert_eq!(devices[0].rssi, Some(-43));
        assert_eq!(devices[0].seen, later);
        assert_eq!(devices[1].seen, now);
    }

    fn notify_char(svc: u16, chr: u16) -> Chr {
        Chr {
            service: std16(svc),
            uuid: std16(chr),
            props: CharPropFlags::NOTIFY,
            on: false,
            notify_pending: None,
            last: None,
        }
    }

    fn note(id: CharId, value: u8) -> ValueNotification {
        ValueNotification {
            service_uuid: id.service,
            uuid: id.uuid,
            value: vec![value],
        }
    }

    #[test]
    fn subscription_is_single_in_flight_and_service_qualified() {
        let first = notify_char(0xab00, 0xab01);
        let second = notify_char(0xcd00, 0xab01);
        let second_id = second.id();
        let mut st = Shared {
            gen: Some(7),
            chars: vec![first, second],
            ..Shared::default()
        };
        let (tx, rx) = std::sync::mpsc::channel();

        assert!(st.begin_notify(7, second_id, true));
        assert!(!st.begin_notify(7, second_id, true));
        assert!(!st.begin_notify(7, second_id, false));
        assert!(!st.begin_notify(6, second_id, true)); // a previous link
        deliver_notification(&mut st, &tx, 7, note(second_id, 1)); // device sends during subscribe
        st.finish_notify(7, second_id, true, true);
        assert!(!st.chars[0].on);
        assert_eq!(st.chars[0].last, None);
        assert!(st.chars[1].on);
        assert_eq!(st.chars[1].last, Some(vec![1]));
        assert!(matches!(rx.try_recv(), Ok(Evt::Packet(_, v)) if v == [1]));

        assert!(st.begin_notify(7, second_id, false));
        st.finish_notify(7, second_id, false, false); // backend failure preserves active state
        assert!(st.chars[1].on);
        assert!(st.begin_notify(7, second_id, false));
        st.finish_notify(7, second_id, false, true);
        deliver_notification(&mut st, &tx, 7, note(second_id, 2));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn notification_listener_recovers_after_stream_ends_and_a_retry_fails() {
        let mut c = notify_char(0xab00, 0xab01);
        c.on = true;
        let id = c.id();
        let st = Arc::new(Mutex::new(Shared {
            gen: Some(7),
            chars: vec![c],
            ..Shared::default()
        }));
        let (tx, rx) = std::sync::mpsc::channel();
        let mut attempts = 0;
        let reopen = move || {
            attempts += 1;
            std::future::ready(if attempts == 1 {
                Err("temporary stream failure".into())
            } else {
                let stream: Notes = Box::pin(futures::stream::iter([note(id, 2)]).chain(futures::stream::pending()));
                Ok(stream)
            })
        };
        let initial: Notes = Box::pin(futures::stream::iter([note(id, 1)]));
        let task = tokio::spawn(receive_notifications(initial, reopen, Duration::ZERO, 7, tx, st.clone(), egui::Context::default()));
        let packets = tokio::time::timeout(Duration::from_secs(1), async {
            let mut values = Vec::new();
            while values.len() < 2 {
                if let Ok(Evt::Packet(_, values_in_packet)) = rx.try_recv() {
                    values.extend(values_in_packet);
                }
                tokio::task::yield_now().await;
            }
            values
        })
        .await
        .expect("both packets arrive without re-subscribing");
        task.abort();
        assert_eq!(packets, vec![1, 2]);
        assert_eq!(st.lock().unwrap().chars[0].last, Some(vec![2]));
    }
}
