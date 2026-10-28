#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod app;
mod atcmds;
mod binview;
mod ble;
mod bridge;
mod checksum;
mod cindex;
mod cmds;
mod codegen;
mod codeview;
mod cpp;
mod editor;
mod git;
mod gitview;
mod highlight;
mod i18n;
mod keymap;
mod logbuf;
mod lsp;
mod modbus;
mod net;
mod pattern;
mod script;
mod serial;
mod shell;
mod snippets;
mod term;
mod termview;
mod theme;
mod toolbox;
mod tools;
mod update;
mod vim;
mod xfer;

static DATA_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// 程序数据目录（设置、崩溃恢复）：便携模式下是 exe 旁边的 qccom-data，否则是系统的应用数据目录
pub fn data_dir() -> std::path::PathBuf {
    DATA_DIR
        .get_or_init(|| portable_dir().or_else(|| eframe::storage_dir("qccom")).unwrap_or_else(|| std::path::PathBuf::from(".")))
        .clone()
}

/// exe 旁边有 qccom-data 文件夹就是便携模式（放 U 盘里带着走，设置跟着走）
pub fn portable_dir() -> Option<std::path::PathBuf> {
    let d = std::env::current_exe().ok()?.parent()?.join("qccom-data");
    d.is_dir().then_some(d)
}

static INSTANCE: std::sync::OnceLock<(u16, Option<std::net::TcpListener>)> = std::sync::OnceLock::new();

/// 第几个窗口（从 1 开始）。每个窗口占住一个本机端口来确定编号，窗口关掉编号自动释放；
/// 第 2 个起的窗口各用一份设置，互不覆盖。
pub fn instance() -> u16 {
    INSTANCE
        .get_or_init(|| {
            for n in 1..=9u16 {
                if let Ok(l) = std::net::TcpListener::bind(("127.0.0.1", 29170 + n)) {
                    return (n, Some(l));
                }
            }
            (10, None)
        })
        .0
}

fn persistence_path() -> Option<std::path::PathBuf> {
    let n = instance();
    if n == 1 {
        return portable_dir().map(|d| d.join("app.ron"));
    }
    let dir = data_dir();
    let p = dir.join(format!("app-{n}.ron"));
    if !p.exists() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::copy(dir.join("app.ron"), &p);
    }
    Some(p)
}

fn main() -> eframe::Result {
    let n = instance();
    let title = if n == 1 { "QCCOM".to_string() } else { format!("QCCOM ({n})") };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([1300.0, 803.0]) // 1300 × 0.618，首次启动时再按屏幕大小调整
            .with_min_inner_size([900.0, 520.0])
            .with_icon(icon()),
        vsync: true,
        persistence_path: persistence_path(),
        ..Default::default()
    };
    migrate_storage();
    eframe::run_native("qccom", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}

/// 窗口图标（任务栏、标题栏）；exe 文件图标由 build.rs 写入
fn icon() -> eframe::egui::IconData {
    eframe::egui::IconData {
        rgba: include_bytes!("../assets/icon64.rgba").to_vec(),
        width: 64,
        height: 64,
    }
}

/// 0.4 及以前叫 uartlog，设置存在 uartlog 目录；改名后第一次启动把旧设置搬过来
fn migrate_storage() {
    if let (Some(old), Some(new)) = (eframe::storage_dir("uartlog"), eframe::storage_dir("qccom")) {
        if old.exists() && !new.exists() {
            let _ = std::fs::create_dir_all(&new);
            if let Ok(rd) = std::fs::read_dir(&old) {
                for e in rd.flatten() {
                    let _ = std::fs::copy(e.path(), new.join(e.file_name()));
                }
            }
        }
    }
}
