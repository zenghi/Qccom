#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cindex;
mod cmds;
mod codeview;
mod git;
mod gitview;
mod highlight;
mod i18n;
mod logbuf;
mod net;
mod serial;
mod term;
mod theme;
mod update;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("QCCOM")
            .with_inner_size([1300.0, 803.0]) // 1300 × 0.618，首次启动时再按屏幕大小调整
            .with_min_inner_size([900.0, 520.0])
            .with_icon(icon()),
        vsync: true,
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
