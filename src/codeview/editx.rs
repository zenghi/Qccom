//! 编辑器增强（v1.8）：自动换行、空白显示和清理、编码和换行符转换（单个文件 / 整个工程转 UTF-8）、
//! clang-format 格式化、外部修改和未保存内容冲突时三方合并。
//! 界面上只多了状态行里编码、换行符可以点；其它都在命令面板和右键里。

use super::*;

/// 整个工程转 UTF-8 的扫描结果
#[derive(Default)]
pub(super) struct EncScan {
    /// GBK 编码的文件（相对路径）
    pub gbk: Vec<String>,
    /// 含中文等非 ASCII 字符的 UTF-8 文件个数
    pub utf8: usize,
}

#[derive(Default)]
pub(super) struct EditX {
    pub scan: Option<EncScan>,
    pub scan_rx: Option<Receiver<EncScan>>,
    /// 整个工程转 UTF-8 的窗口：(勾选, 加 BOM)
    pub conv_win: Option<(Vec<bool>, bool)>,
    /// 编码混用提示已经关掉（本次运行）
    pub mixed_off: bool,
}

/// 会转换编码的文本文件
const TEXT_EXTS: &[&str] = &[
    "c", "h", "cc", "cpp", "cxx", "hpp", "hxx", "s", "asm", "inc", "txt", "md", "ini", "cfg", "conf", "ld", "lds", "sct", "icf", "py", "cmake", "mk", "json", "yaml", "yml", "xml", "csv", "bat", "sh",
];

fn is_text_name(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    TEXT_EXTS.contains(&ext.as_str()) || matches!(name, "Makefile" | "makefile" | "Kconfig" | "CMakeLists.txt")
}

pub(super) fn scan_encodings(root: &Path) -> EncScan {
    let mut s = EncScan::default();
    for rel in crate::history::project_files(root, 4 << 20) {
        if !is_text_name(&rel) {
            continue;
        }
        let Ok(b) = std::fs::read(root.join(&rel)) else { continue };
        if b.is_ascii() {
            continue;
        }
        match crate::history::review::detect(&b) {
            crate::history::review::Enc::Gbk => s.gbk.push(rel),
            crate::history::review::Enc::Utf8 | crate::history::review::Enc::Utf8Bom => s.utf8 += 1,
            _ => {}
        }
    }
    s.gbk.sort();
    s
}

/// 找 clang-format：PATH → clang-format-NN → 和设置里的 clangd 同目录 → Windows 下 LLVM 默认目录
fn find_clang_format(clangd: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) { "clang-format.exe" } else { "clang-format" };
    let dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(p) = dirs.iter().map(|d| d.join(exe)).find(|p| p.is_file()) {
        return Some(p);
    }
    if let Some(p) = Path::new(clangd.trim()).parent().map(|d| d.join(exe)).filter(|p| p.is_file()) {
        return Some(p);
    }
    if cfg!(windows) {
        ["C:\\Program Files\\LLVM\\bin", "C:\\Program Files (x86)\\LLVM\\bin"]
            .iter()
            .map(|b| Path::new(b).join(exe))
            .find(|p| p.is_file())
    } else {
        (10..=30).rev().flat_map(|v| dirs.iter().map(move |d| d.join(format!("clang-format-{v}")))).find(|p| p.is_file())
    }
}

/// 文件所在目录往上找 .clang-format
fn has_style(path: &Path) -> bool {
    path.ancestors().skip(1).any(|d| d.join(".clang-format").is_file() || d.join("_clang-format").is_file())
}

/// 调 clang-format 格式化（lines：只格式化这些行，从 1 开始，含首尾）
fn clang_format(exe: &Path, path: &Path, text: &str, lines: Option<(usize, usize)>) -> Result<String, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut c = Command::new(exe);
    c.arg("-style=file").arg(format!("--assume-filename={}", path.display()));
    if let Some((a, b)) = lines {
        c.arg(format!("--lines={a}:{b}"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let mut ch = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let input = text.to_string();
    let mut stdin = ch.stdin.take().ok_or("stdin")?;
    let w = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = ch.wait_with_output().map_err(|e| e.to_string())?;
    let _ = w.join();
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("clang-format").to_string());
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

/// 新旧文字只差中间一段：返回要替换的范围和新内容（光标、书签不会整体跳）
pub(super) fn middle_diff(old: &str, new: &str) -> Option<(std::ops::Range<usize>, String)> {
    if old == new {
        return None;
    }
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let mut pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while !old.is_char_boundary(pre) || !new.is_char_boundary(pre) {
        pre -= 1;
    }
    let max_suf = a.len().min(b.len()) - pre;
    let mut suf = a.iter().rev().zip(b.iter().rev()).take(max_suf).take_while(|(x, y)| x == y).count();
    while !old.is_char_boundary(a.len() - suf) || !new.is_char_boundary(b.len() - suf) {
        suf -= 1;
    }
    Some((pre..a.len() - suf, new[pre..b.len() - suf].to_string()))
}

impl CodeView {
    pub(super) fn editx_start(&mut self) {
        self.ex.scan = None;
        self.ex.conv_win = None;
        let Some(root) = self.root.clone() else { return };
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("enc-scan".into())
            .spawn(move || {
                let _ = tx.send(scan_encodings(&root));
            })
            .ok();
        self.ex.scan_rx = Some(rx);
    }

    pub(super) fn editx_poll(&mut self) {
        if let Some(rx) = &self.ex.scan_rx {
            if let Ok(s) = rx.try_recv() {
                self.ex.scan = Some(s);
                self.ex.scan_rx = None;
            }
        }
    }

    /// 工程里 GBK 和 UTF-8 混用时提示一次
    pub(super) fn editx_notice(&mut self, ui: &mut Ui) {
        if self.ex.mixed_off {
            return;
        }
        let Some(s) = &self.ex.scan else { return };
        if s.gbk.is_empty() || s.utf8 == 0 {
            return;
        }
        let msg = tf("工程里的中文文件编码不统一：{} 个 GBK，{} 个 UTF-8", &[&s.gbk.len(), &s.utf8]);
        match notice_bar(ui, &msg, &[t("统一成 UTF-8…"), t("不再提示")]) {
            Some(0) => self.open_conv_win(),
            Some(1) => self.ex.mixed_off = true,
            _ => {}
        }
    }

    fn open_conv_win(&mut self) {
        match &self.ex.scan {
            Some(s) if !s.gbk.is_empty() => self.ex.conv_win = Some((vec![true; s.gbk.len()], true)),
            Some(_) => self.toast(t("工程里没有 GBK 编码的文件"), OK()),
            None => self.toast(t("正在检查文件编码，稍等再试"), TEXT3()),
        }
    }

    /// 整个工程 GBK → UTF-8：先列出来勾选，转换前自动打存档点
    pub(super) fn conv_window(&mut self, ctx: &egui::Context) {
        let Some((mut pick, mut bom)) = self.ex.conv_win.take() else { return };
        let files = self.ex.scan.as_ref().map(|s| s.gbk.clone()).unwrap_or_default();
        let mut open = true;
        let mut go = false;
        egui::Window::new(t("工程转成 UTF-8"))
            .id(egui::Id::new("enc-conv"))
            .open(&mut open)
            .collapsible(false)
            .default_width(460.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(t("下面这些文件是 GBK 编码。转换只改编码不改内容，转换前会自动新建存档点，随时可以回去。"))
                        .size(12.0)
                        .color(TEXT2()),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let all = pick.iter().all(|x| *x);
                    if ui.small_button(if all { t("全不选") } else { t("全选") }).clicked() {
                        pick.iter_mut().for_each(|x| *x = !all);
                    }
                    ui.label(RichText::new(tf("已选 {} / {}", &[&pick.iter().filter(|x| **x).count(), &files.len()])).size(11.5).color(TEXT3()));
                });
                egui::ScrollArea::vertical().max_height(300.0).auto_shrink([false, true]).show(ui, |ui| {
                    for (i, f) in files.iter().enumerate() {
                        if let Some(p) = pick.get_mut(i) {
                            ui.checkbox(p, RichText::new(f).font(FontId::new(12.0, FontFamily::Monospace)));
                        }
                    }
                });
                ui.add_space(6.0);
                ui.checkbox(&mut bom, t("加 BOM（Keil 等老工具靠它识别 UTF-8，推荐）"));
                ui.add_space(6.0);
                let n = pick.iter().filter(|x| **x).count();
                if ui
                    .add_enabled(n > 0, egui::Button::new(RichText::new(tf("转换 {} 个文件", &[&n])).color(ON_ACCENT()).strong()).fill(ACCENT()))
                    .clicked()
                {
                    go = true;
                }
            });
        if go {
            let chosen: Vec<String> = files.iter().zip(&pick).filter(|(_, p)| **p).map(|(f, _)| f.clone()).collect();
            self.convert_project(&chosen, bom);
            return;
        }
        if open {
            self.ex.conv_win = Some((pick, bom));
        }
    }

    fn convert_project(&mut self, files: &[String], bom: bool) {
        let Some(root) = self.root.clone() else { return };
        let cp = self.checkpoint_now(&tf("{} 个文件 GBK 转 UTF-8 之前", &[&files.len()]));
        let (mut ok, mut failed) = (0, vec![]);
        for rel in files {
            let p = root.join(rel);
            // 已经打开的文件：改标签页的编码标记再保存，未保存的修改一起写进去
            if let Some(d) = self.tabs.iter_mut().find(|d| d.path == p) {
                d.gbk = false;
                d.bom = bom;
                match d.save() {
                    Ok(()) => ok += 1,
                    Err(_) => failed.push(rel.clone()),
                }
                continue;
            }
            let Ok(b) = std::fs::read(&p) else {
                failed.push(rel.clone());
                continue;
            };
            let text = encoding_rs::GBK.decode(&b).0.into_owned();
            let mut data = text.into_bytes();
            if bom {
                data.splice(0..0, *b"\xef\xbb\xbf");
            }
            match crate::history::save_tracked(&p, &data) {
                Ok(()) => ok += 1,
                Err(_) => failed.push(rel.clone()),
            }
        }
        if let Some(s) = self.ex.scan.as_mut() {
            s.gbk.retain(|f| failed.contains(f) || !files.contains(f));
            s.utf8 += ok;
        }
        let undo = cp.map(|id| tf("（存档点 #{} 可以回去）", &[&id])).unwrap_or_default();
        if failed.is_empty() {
            self.toast(tf("已把 {} 个文件转成 UTF-8{}", &[&ok, &undo]), OK());
        } else {
            self.toast(tf("{} 个文件已转换，{} 个失败（可能被占用）：{}", &[&ok, &failed.len(), &failed.join("、")]), WARN());
        }
    }

    /// 当前文件换编码：改完立即保存（旧版本在本地历史里）
    fn set_encoding(&mut self, gbk: bool, bom: bool) {
        let Some(d) = self.tabs.get_mut(self.active) else { return };
        if gbk && !crate::history::review::gbk_lossless(&d.buf.text) {
            self.toast(t("文件里有 GBK 表示不了的字符，不能转成 GBK"), WARN());
            return;
        }
        d.gbk = gbk;
        d.bom = bom && !gbk;
        let label = if gbk {
            "GBK"
        } else if bom {
            "UTF-8 BOM"
        } else {
            "UTF-8"
        };
        let r = d.save();
        let rel = d.rel.clone();
        match r {
            Ok(()) => {
                if let Some(s) = self.ex.scan.as_mut() {
                    s.gbk.retain(|f| f != &rel);
                }
                self.toast(tf("已转成 {} 并保存", &[&label]), OK());
            }
            Err(e) => self.toast(e, ERR()),
        }
    }

    fn set_eol(&mut self, crlf: bool) {
        let Some(d) = self.tabs.get_mut(self.active) else { return };
        if d.crlf == crlf {
            return;
        }
        d.crlf = crlf;
        let r = d.save();
        match r {
            Ok(()) => self.toast(tf("换行符已改成 {} 并保存", &[&if crlf { "CRLF" } else { "LF" }]), OK()),
            Err(e) => self.toast(e, ERR()),
        }
    }

    /// 状态行里的编码、换行符：点击弹出转换菜单
    pub(super) fn status_menus(&mut self, ui: &mut Ui, enc: Rect, eol: Rect) {
        let Some(d) = self.tabs.get(self.active) else { return };
        let (gbk, bom, crlf) = (d.gbk, d.bom, d.crlf);
        let mut act: Option<u8> = None;
        let r = ui.interact(enc.expand(3.0), egui::Id::new("st-enc"), Sense::click()).on_hover_text(t("点击转换编码"));
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let id = egui::Id::new("st-enc-pop");
        if r.clicked() {
            ui.memory_mut(|m| m.toggle_popup(id));
        }
        egui::popup_above_or_below_widget(ui, id, &r, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(200.0);
            ui.label(RichText::new(t("转换并保存为")).size(11.0).color(TEXT3()));
            if ui.add_enabled(gbk || bom, egui::Button::new("UTF-8")).clicked() {
                act = Some(0);
            }
            if ui.add_enabled(gbk || !bom, egui::Button::new("UTF-8 BOM")).clicked() {
                act = Some(1);
            }
            if ui.add_enabled(!gbk, egui::Button::new("GBK")).clicked() {
                act = Some(2);
            }
            ui.separator();
            if ui.button(t("整个工程转成 UTF-8…")).clicked() {
                act = Some(3);
            }
        });
        let r = ui.interact(eol.expand(3.0), egui::Id::new("st-eol"), Sense::click()).on_hover_text(t("点击转换换行符"));
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let id = egui::Id::new("st-eol-pop");
        if r.clicked() {
            ui.memory_mut(|m| m.toggle_popup(id));
        }
        egui::popup_above_or_below_widget(ui, id, &r, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(160.0);
            if ui.add_enabled(crlf, egui::Button::new(t("LF（Linux / macOS）"))).clicked() {
                act = Some(4);
            }
            if ui.add_enabled(!crlf, egui::Button::new(t("CRLF（Windows）"))).clicked() {
                act = Some(5);
            }
        });
        match act {
            Some(0) => self.set_encoding(false, false),
            Some(1) => self.set_encoding(false, true),
            Some(2) => self.set_encoding(true, false),
            Some(3) => self.open_conv_win(),
            Some(4) => self.set_eol(false),
            Some(5) => self.set_eol(true),
            _ => {}
        }
    }

    /// 格式化当前文件（有选区时只格式化选中的行）
    pub(super) fn format_doc(&mut self, now: f64, quiet: bool) -> bool {
        let Some(d) = self.tabs.get(self.active) else { return false };
        if !(d.lang == Lang::C || d.lang == Lang::Asm) || d.virt {
            if !quiet {
                self.toast(t("格式化目前支持 C / C++ 文件"), TEXT3());
            }
            return false;
        }
        if !has_style(&d.path) {
            if !quiet {
                self.toast(t("工程里没有 .clang-format 文件，不知道要什么风格，先不格式化"), WARN());
            }
            return false;
        }
        let Some(exe) = find_clang_format(&self.s.clangd_path) else {
            if !quiet {
                self.toast(t("没有找到 clang-format，请安装 LLVM（clangd 同一个包里就有）"), WARN());
            }
            return false;
        };
        let sel = d.ed.primary_sel();
        let lines = (!sel.empty()).then(|| (d.buf.line_of(sel.min()) + 1, d.buf.line_of(sel.max()) + 1));
        match clang_format(&exe, &d.path, &d.buf.text, lines) {
            Ok(out) => {
                let d = &mut self.tabs[self.active];
                match middle_diff(&d.buf.text, &out) {
                    Some(item) => {
                        d.ed.replace_keep(&mut d.buf, vec![item], now);
                        d.syms_stale = Some(now);
                        d.edited_at = Some(now);
                        true
                    }
                    None => {
                        if !quiet {
                            self.toast(t("已经是格式化好的"), TEXT3());
                        }
                        false
                    }
                }
            }
            Err(e) => {
                if !quiet {
                    self.toast(tf("格式化失败：{}", &[&e]), ERR());
                }
                false
            }
        }
    }

    /// 文件被外部改了、编辑器里又有没保存的修改：三方合并，两边都保住
    pub(super) fn merge_external(&mut self, now: f64) {
        let Some(d) = self.tabs.get_mut(self.active) else { return };
        let name = d.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let base = crate::compareview::temp_file(&format!("base-{name}"), d.saved.as_bytes());
        let mine = crate::compareview::temp_file(&format!("mine-{name}"), d.buf.text.as_bytes());
        let out = d.path.clone();
        // 编辑器先换成磁盘上的内容（Ctrl+Z 还能回到自己的版本），合并结果保存后会自动重新载入
        if let Err(e) = d.reload(now) {
            self.toast(e, ERR());
            return;
        }
        self.events.push(CodeEvt::Merge(base, mine, out));
    }

    pub(super) fn editx_acts(&mut self, has: &dyn Fn(Act) -> bool, now: f64) {
        if has(Act::ToggleWrap) {
            self.s.wrap = !self.s.wrap;
            self.toast(if self.s.wrap { t("自动换行：开") } else { t("自动换行：关") }, TEXT3());
        }
        if has(Act::ToggleWhitespace) {
            self.s.show_ws = !self.s.show_ws;
        }
        if has(Act::TrimWhitespace) {
            if let Some(d) = self.tabs.get_mut(self.active) {
                let n = d.ed.trim_trailing(&mut d.buf, now);
                if n > 0 {
                    d.syms_stale = Some(now);
                    d.edited_at = Some(now);
                }
                self.toast(
                    if n == 0 {
                        t("没有行尾空格").to_string()
                    } else {
                        tf("已清理 {} 行的行尾空格", &[&n])
                    },
                    TEXT3(),
                );
            }
        }
        if has(Act::FormatDoc) {
            self.format_doc(now, false);
        }
        if has(Act::ProjectUtf8) {
            self.open_conv_win();
        }
        if has(Act::RegPanel) {
            self.open_regs();
        }
    }
}
