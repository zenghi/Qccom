#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cindex;
mod cmds;
mod codeview;
mod editor;
mod git;
mod gitview;
mod highlight;
mod i18n;
mod keymap;
mod logbuf;
mod lsp;
mod net;
mod pattern;
mod serial;
mod shell;
mod term;
mod termview;
mod theme;
mod update;

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

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("QCCOM")
            .with_inner_size([1300.0, 803.0]) // 1300 × 0.618，首次启动时再按屏幕大小调整
            .with_min_inner_size([900.0, 520.0])
            .with_icon(icon()),
        vsync: true,
        persistence_path: portable_dir().map(|d| d.join("app.ron")),
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
