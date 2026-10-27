//! 低功耗蓝牙（BLE）调试：扫描周围设备、连接、列出服务和特征，订阅通知、读、写。
//!
//! btleplug 是异步库，这里起一个常驻线程跑单线程 tokio 运行时（「hub」）：扫描和连接都在里面做，
//! 界面通过 [`Hub::send`] 发命令，从 [`Hub::st`] 读设备列表和特征表。连上以后对外表现得和其它连接方式一样：
//! 通知数据走 `Evt::Packet`，发送区的数据写到选中的特征。

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use btleplug::api::{Central, CentralEvent, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter, ValueNotification, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use eframe::egui;
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
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

#[derive(Clone)]
pub struct Chr {
    pub service: Uuid,
    pub uuid: Uuid,
    pub props: CharPropFlags,
    /// 已订阅通知
    pub on: bool,
    /// 最后一次读到 / 收到的值
    pub last: Option<Vec<u8>>,
}

impl Chr {
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
    pub target: Option<Uuid>,
    /// 正在做的事：连接中 / 发现服务中
    pub busy: Option<String>,
    pub mtu: u16,
}

pub enum HubCmd {
    Scan(bool),
    Clear,
    Connect { gen: u64, id: String, label: String, tx: Sender<Evt>, with_resp: bool },
    Conn(u64, Cmd),
    Target(Uuid),
    Notify(Uuid, bool),
    Read(Uuid),
    WithResp(bool),
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
        let _ = self.tx.send(c);
    }

    /// 连接设备。返回的 Connection 和串口 / 网络的用法一样：发 Cmd::Send 写数据，Cmd::Close 断开
    pub fn open(&self, gen: u64, cfg: &BleConfig, tx: Sender<Evt>) -> Connection {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Cmd>();
        self.send(HubCmd::Connect {
            gen,
            id: cfg.id.clone(),
            label: cfg.label(),
            tx,
            with_resp: cfg.with_resp,
        });
        // Connection 的命令是同步通道，转一手送进 hub
        let hub = self.tx.clone();
        let _ = std::thread::Builder::new().name("ble-cmd".into()).spawn(move || {
            while let Ok(c) = cmd_rx.recv() {
                let close = matches!(c, Cmd::Close);
                if hub.send(HubCmd::Conn(gen, c)).is_err() || close {
                    return;
                }
            }
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
}

enum Ev {
    Cmd(Option<HubCmd>),
    Central(Option<CentralEvent>),
    Note(Option<ValueNotification>),
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
    let mut notes: Option<Notes> = None;
    let mut live: Option<Live> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(800));
    loop {
        let ev = tokio::select! {
            c = rx.recv() => Ev::Cmd(c),
            e = async { match events.as_mut() { Some(s) => s.next().await, None => std::future::pending().await } } => Ev::Central(e),
            n = async { match notes.as_mut() { Some(s) => s.next().await, None => std::future::pending().await } } => Ev::Note(n),
            _ = tick.tick() => Ev::Tick,
        };
        match ev {
            Ev::Cmd(None) => break,
            Ev::Cmd(Some(c)) => handle(c, &central, &st, &mut live, &mut notes, &ctx).await,
            Ev::Central(None) => events = None,
            Ev::Central(Some(CentralEvent::DeviceDisconnected(id))) => {
                if live.as_ref().is_some_and(|l| l.p.id() == id) {
                    let l = live.take().unwrap();
                    notes = None;
                    st.lock().unwrap().chars.clear();
                    let _ = l.tx.send(Evt::Closed(l.gen, Some(t("设备断开了连接").into())));
                    ctx.request_repaint();
                }
            }
            Ev::Central(Some(_)) => {}
            Ev::Note(None) => notes = None,
            Ev::Note(Some(n)) => {
                if let Some(l) = &live {
                    let _ = l.tx.send(Evt::Packet(char_name(&n.uuid), n.value.clone()));
                    let mut s = st.lock().unwrap();
                    if let Some(c) = s.chars.iter_mut().find(|c| c.uuid == n.uuid) {
                        c.last = Some(n.value);
                    }
                    ctx.request_repaint_after(Duration::from_millis(33));
                }
            }
            Ev::Tick => {
                if st.lock().unwrap().scanning {
                    refresh(&central, &st).await;
                    ctx.request_repaint();
                }
            }
        }
    }
    if let Some(l) = live {
        let _ = l.p.disconnect().await;
    }
}

/// 扫描中：把适配器知道的设备同步到列表里（保持原来的顺序，免得列表跳来跳去）
async fn refresh(central: &Adapter, st: &Arc<Mutex<Shared>>) {
    let Ok(list) = central.peripherals().await else { return };
    let mut found = Vec::new();
    for p in list {
        let Ok(Some(pr)) = p.properties().await else { continue };
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
    let mut s = st.lock().unwrap();
    for d in found {
        match s.devices.iter_mut().find(|x| x.id == d.id) {
            Some(x) => {
                // 只有信号值变了才算「又看到了」（BlueZ 会一直报缓存里的设备）
                if d.rssi.is_some() && (d.rssi != x.rssi || x.seen.elapsed() < Duration::from_secs(5)) {
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
            None => s.devices.push(d),
        }
    }
}

async fn find(central: &Adapter, id: &str) -> Option<Peripheral> {
    central.peripherals().await.ok()?.into_iter().find(|p| p.id().to_string() == id)
}

fn fail(tx: &Sender<Evt>, gen: u64, e: impl std::fmt::Display, ctx: &egui::Context) {
    let _ = tx.send(Evt::Closed(gen, Some(e.to_string())));
    ctx.request_repaint();
}

async fn handle(c: HubCmd, central: &Adapter, st: &Arc<Mutex<Shared>>, live: &mut Option<Live>, notes: &mut Option<Notes>, ctx: &egui::Context) {
    let busy = |m: Option<String>| {
        st.lock().unwrap().busy = m;
        ctx.request_repaint();
    };
    match c {
        HubCmd::Scan(on) => {
            let r = if on { central.start_scan(ScanFilter::default()).await } else { central.stop_scan().await };
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
        HubCmd::Connect { gen, id, label, tx, with_resp } => {
            if let Some(l) = live.take() {
                let _ = l.p.disconnect().await;
                *notes = None;
            }
            let was_scanning = st.lock().unwrap().scanning;
            if was_scanning {
                // 很多适配器扫描时连不上
                let _ = central.stop_scan().await;
                st.lock().unwrap().scanning = false;
            }
            busy(Some(t("正在连接…").into()));
            let Some(p) = find(central, &id).await else {
                busy(None);
                return fail(&tx, gen, t("找不到这个设备，请先扫描"), ctx);
            };
            match tokio::time::timeout(Duration::from_secs(15), p.connect()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    busy(None);
                    return fail(&tx, gen, tf("连接失败：{}", &[&e]), ctx);
                }
                Err(_) => {
                    busy(None);
                    let _ = p.disconnect().await;
                    return fail(&tx, gen, t("连接超时（15 秒）"), ctx);
                }
            }
            busy(Some(t("正在读取服务…").into()));
            match tokio::time::timeout(Duration::from_secs(20), p.discover_services()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    busy(None);
                    let _ = p.disconnect().await;
                    return fail(&tx, gen, tf("读取服务失败：{}", &[&e]), ctx);
                }
                Err(_) => {
                    busy(None);
                    let _ = p.disconnect().await;
                    return fail(&tx, gen, t("读取服务超时"), ctx);
                }
            }
            let chars: Vec<Characteristic> = p.characteristics().into_iter().collect();
            let target = pick_target(&chars);
            *notes = p.notifications().await.ok();
            // 自动订阅能通知的特征（标准服务里的除外）
            let mut table = Vec::new();
            for c in &chars {
                let mut on = false;
                if c.properties.intersects(CharPropFlags::NOTIFY | CharPropFlags::INDICATE) && !is_generic(&c.service_uuid) {
                    on = p.subscribe(c).await.is_ok();
                }
                table.push(Chr {
                    service: c.service_uuid,
                    uuid: c.uuid,
                    props: c.properties,
                    on,
                    last: None,
                });
            }
            {
                let mut s = st.lock().unwrap();
                s.chars = table;
                s.target = target;
                s.busy = None;
                s.mtu = p.mtu();
            }
            let _ = tx.send(Evt::Opened(gen, label));
            match target {
                Some(u) => {
                    let _ = tx.send(Evt::Sys(tf("发送数据会写到特征 {}", &[&char_name(&u)])));
                }
                None => {
                    let _ = tx.send(Evt::Sys(t("这个设备没有可写的特征，只能接收").into()));
                }
            }
            ctx.request_repaint();
            *live = Some(Live { gen, p, tx, with_resp });
        }
        HubCmd::Conn(gen, cmd) => {
            let Some(l) = live.as_ref().filter(|l| l.gen == gen) else { return };
            match cmd {
                Cmd::Send(d) => {
                    let target = st.lock().unwrap().target;
                    let Some(c) = target.and_then(|u| l.p.characteristics().into_iter().find(|c| c.uuid == u)) else {
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
                    let l = live.take().unwrap();
                    *notes = None;
                    let _ = l.p.disconnect().await;
                    st.lock().unwrap().chars.clear();
                    let _ = l.tx.send(Evt::Closed(l.gen, None));
                    ctx.request_repaint();
                }
                _ => {}
            }
        }
        HubCmd::Target(u) => st.lock().unwrap().target = Some(u),
        HubCmd::WithResp(on) => {
            if let Some(l) = live.as_mut() {
                l.with_resp = on;
            }
        }
        HubCmd::Notify(u, on) => {
            let Some(l) = live.as_ref() else { return };
            let Some(c) = l.p.characteristics().into_iter().find(|c| c.uuid == u) else { return };
            let r = if on { l.p.subscribe(&c).await } else { l.p.unsubscribe(&c).await };
            match r {
                Ok(()) => {
                    if let Some(x) = st.lock().unwrap().chars.iter_mut().find(|x| x.uuid == u) {
                        x.on = on;
                    }
                }
                Err(e) => {
                    let _ = l.tx.send(Evt::Sys(tf("✗ 订阅失败：{}", &[&e])));
                }
            }
            ctx.request_repaint();
        }
        HubCmd::Read(u) => {
            let Some(l) = live.as_ref() else { return };
            let Some(c) = l.p.characteristics().into_iter().find(|c| c.uuid == u) else { return };
            match l.p.read(&c).await {
                Ok(v) => {
                    let _ = l.tx.send(Evt::Packet(format!("{} R", char_name(&u)), read_text(&v)));
                    if let Some(x) = st.lock().unwrap().chars.iter_mut().find(|x| x.uuid == u) {
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
fn pick_target(chars: &[Characteristic]) -> Option<Uuid> {
    let writable = |c: &&Characteristic| c.properties.intersects(CharPropFlags::WRITE | CharPropFlags::WRITE_WITHOUT_RESPONSE);
    let prefer = [
        Uuid::from_u128(NUS_RX),
        Uuid::from_u128((0xFFE1u128 << 96) | BASE_TAIL),
        Uuid::from_u128((0xFFF2u128 << 96) | BASE_TAIL),
    ];
    for p in prefer {
        if let Some(c) = chars.iter().filter(writable).find(|c| c.uuid == p) {
            return Some(c.uuid);
        }
    }
    chars
        .iter()
        .filter(writable)
        .find(|c| !matches!(short(&c.service_uuid), Some(0x1800..=0x18FF)))
        .or_else(|| chars.iter().find(|c| writable(c)))
        .map(|c| c.uuid)
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

    fn std16(x: u16) -> Uuid {
        Uuid::from_u128(((x as u128) << 96) | BASE_TAIL)
    }

    fn chars() -> Vec<Chr> {
        let c = |service: Uuid, uuid: Uuid, props: CharPropFlags| Chr {
            service,
            uuid,
            props,
            on: false,
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
                    if let Some((_, tx)) = &live {
                        let on = s.chars.iter().any(|c| c.uuid == Uuid::from_u128(NUS_TX) && c.on);
                        if on {
                            let v = format!("temp={:.1} hum={:.0}\n", 24.0 + (n as f32 * 0.3).sin() * 1.5, 48.0 + (n as f32 * 0.2).cos() * 4.0);
                            let _ = tx.send(Evt::Packet("NUS TX".into(), v.clone().into_bytes()));
                            if let Some(c) = s.chars.iter_mut().find(|c| c.uuid == Uuid::from_u128(NUS_TX)) {
                                c.last = Some(v.into_bytes());
                            }
                        }
                    }
                }
                Some(HubCmd::Scan(on)) => s.scanning = on,
                Some(HubCmd::Clear) => s.devices.clear(),
                Some(HubCmd::Connect { gen, id, label, tx, .. }) => {
                    s.scanning = false;
                    if !id.starts_with("C4:4F") {
                        let _ = tx.send(Evt::Closed(gen, Some(tf("连接失败：{}", &[&"Timed out"]))));
                    } else {
                        let mut ch = chars();
                        for c in ch.iter_mut().filter(|c| c.can_notify() && !is_generic(&c.service)) {
                            c.on = true;
                        }
                        s.chars = ch;
                        s.target = Some(Uuid::from_u128(NUS_RX));
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
                            if s.target == Some(Uuid::from_u128(NUS_RX)) {
                                let _ = tx.send(Evt::Packet("NUS TX".into(), d));
                            }
                        }
                        Cmd::Close => {
                            let _ = tx.send(Evt::Closed(gen, None));
                            s.chars.clear();
                            live = None;
                        }
                        _ => {}
                    }
                }
                Some(HubCmd::Target(u)) => s.target = Some(u),
                Some(HubCmd::Notify(u, on)) => {
                    if let Some(c) = s.chars.iter_mut().find(|c| c.uuid == u) {
                        c.on = on;
                    }
                }
                Some(HubCmd::Read(u)) => {
                    let v: Vec<u8> = match short(&u) {
                        Some(0x2A00) => b"QC-Sensor".to_vec(),
                        Some(0x2A29) => b"Espressif".to_vec(),
                        Some(0x2A26) => b"v2.3.1".to_vec(),
                        Some(0x2A19) => vec![87],
                        _ => vec![],
                    };
                    if let Some((_, tx)) = &live {
                        let _ = tx.send(Evt::Packet(format!("{} R", char_name(&u)), read_text(&v)));
                    }
                    if let Some(c) = s.chars.iter_mut().find(|c| c.uuid == u) {
                        c.last = Some(v);
                    }
                }
                Some(HubCmd::WithResp(_)) => {}
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
        assert_eq!(pick_target(&list), Some(std16(0xFFE1)));
        assert_eq!(pick_target(&list[..2]), Some(std16(0xAB01)));
        assert_eq!(pick_target(&list[..1]), Some(std16(0x2A00)));
    }
}
