//! 资源管理器的多选、复制 / 剪切 / 粘贴、拖动移动，以及和系统剪贴板、系统文件管理器之间的文件互通。
//!
//! - Ctrl / Shift 单击多选；Ctrl+C / Ctrl+X / Ctrl+V（树获得焦点时）
//! - 复制的文件同时放进系统剪贴板（Windows 资源管理器里可以直接粘贴），系统里复制的文件也能粘进来
//! - 树里拖动 = 移动，按住 Ctrl 拖 = 复制；外部文件拖到某个文件夹上 = 复制进来（按住 Shift = 移动）
//! - 同名时自动起名「xxx - 副本.c」，从不覆盖；移动前打一个本地历史存档点

use super::*;

/// 剪贴板里的文件：路径 + 是不是剪切
#[derive(Default)]
pub(super) struct FileClip {
    pub paths: Vec<PathBuf>,
    pub cut: bool,
}

/// 树的选择、拖动状态
#[derive(Default)]
pub(super) struct TreeSel {
    pub sel: Vec<PathBuf>,
    pub anchor: Option<PathBuf>,
    /// 这一帧画出来的行顺序（Shift 范围选择用）
    pub order: Vec<PathBuf>,
    pub last_order: Vec<PathBuf>,
    /// 最近一次点在树上（Ctrl+C/V 归树处理）
    pub focus: bool,
    /// 正在拖动的路径
    pub drag: Option<Vec<PathBuf>>,
    /// 拖动时指针下面的目标文件夹
    pub hover_dir: Option<PathBuf>,
    /// 这一帧每个可放置的行：(区域, 目标文件夹)
    pub rects: Vec<(Rect, PathBuf)>,
    pub last_rects: Vec<(Rect, PathBuf)>,
    /// 树所在区域（外部拖放判断用）
    pub area: Option<Rect>,
    pub clip: FileClip,
    /// 最近一次看到 Ctrl / Cmd 按着的时间
    pub cmd_at: f64,
}

impl CodeView {
    /// 单击一行：Ctrl 切换选中、Shift 范围选择；返回 true 表示照常打开 / 展开
    pub(super) fn tree_click(&mut self, path: &Path, mods: egui::Modifiers) -> bool {
        self.ts.focus = true;
        if mods.command || mods.ctrl {
            if let Some(i) = self.ts.sel.iter().position(|p| p == path) {
                self.ts.sel.remove(i);
            } else {
                self.ts.sel.push(path.to_path_buf());
            }
            self.ts.anchor = Some(path.to_path_buf());
            return false;
        }
        if mods.shift {
            if let Some(a) = self.ts.anchor.clone() {
                let ord = &self.ts.last_order;
                if let (Some(i), Some(j)) = (ord.iter().position(|p| *p == a), ord.iter().position(|p| p == path)) {
                    let (i, j) = (i.min(j), i.max(j));
                    self.ts.sel = ord[i..=j].to_vec();
                    return false;
                }
            }
        }
        self.ts.sel = vec![path.to_path_buf()];
        self.ts.anchor = Some(path.to_path_buf());
        true
    }

    pub(super) fn tree_selected(&self, p: &Path) -> bool {
        self.ts.sel.iter().any(|x| x == p)
    }

    /// 右键 / 快捷键操作的对象：点在选中的行上 = 所有选中的；否则就是这一行
    pub(super) fn targets_for(&self, p: &Path) -> Vec<PathBuf> {
        if self.tree_selected(p) && self.ts.sel.len() > 1 {
            self.ts.sel.clone()
        } else {
            vec![p.to_path_buf()]
        }
    }

    /// 复制 / 剪切：自己记一份（剪切要靠它），同时放进系统剪贴板
    pub(super) fn tree_copy(&mut self, paths: Vec<PathBuf>, cut: bool) {
        if paths.is_empty() {
            return;
        }
        let n = paths.len();
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set().file_list(&paths);
        }
        self.ts.clip = FileClip { paths, cut };
        self.toast(
            if cut {
                tf("已剪切 {} 项，到目标文件夹按 Ctrl+V", &[&n])
            } else {
                tf("已复制 {} 项，可以在这里或系统文件管理器里粘贴", &[&n])
            },
            ACCENT(),
        );
    }

    /// 粘贴到某个文件夹：系统剪贴板里的文件优先（可能是在资源管理器里刚复制的）
    pub(super) fn tree_paste(&mut self, dir: &Path) {
        let sys: Vec<PathBuf> = arboard::Clipboard::new().ok().and_then(|mut c| c.get().file_list().ok()).unwrap_or_default();
        let ours = &self.ts.clip;
        // 系统剪贴板里就是我们刚放的那些：按我们的（才知道是不是剪切）
        let same = !ours.paths.is_empty() && (sys.is_empty() || same_set(&sys, &ours.paths));
        let (paths, cut) = if same { (ours.paths.clone(), ours.cut) } else { (sys, false) };
        if paths.is_empty() {
            self.toast(t("剪贴板里没有文件"), WARN());
            return;
        }
        self.transfer(&paths, dir, cut);
        if cut && same {
            self.ts.clip = FileClip::default();
        }
    }

    /// 把一批文件 / 文件夹复制或移动到 dir。同名自动加「 - 副本」，不会覆盖
    pub(crate) fn transfer(&mut self, srcs: &[PathBuf], dir: &Path, mv: bool) {
        if !dir.is_dir() {
            return;
        }
        let (mut ok, mut skipped, mut failed) = (0usize, 0usize, Vec::<String>::new());
        if mv {
            if let Some(h) = &self.hist.h {
                h.checkpoint(&tf("移动 {} 项到 {} 之前", &[&srcs.len(), &self.rel_of(dir)]), false);
            }
        }
        let mut last: Option<PathBuf> = None;
        for src in srcs {
            let name = src.file_name().map(|n| n.to_os_string()).unwrap_or_default();
            if name.is_empty() || !src.exists() {
                continue;
            }
            // 不能放进自己或自己的子目录
            if src.is_dir() && dir.starts_with(src) {
                skipped += 1;
                continue;
            }
            let mut dest = dir.join(&name);
            if mv && src.parent() == Some(dir) {
                continue; // 原地移动：什么都不做
            }
            if dest.exists() {
                if mv {
                    skipped += 1;
                    continue;
                }
                dest = unique_name(&dest);
            }
            let r = if mv { move_path(src, &dest) } else { copy_path(src, &dest) };
            match r {
                Ok(()) => {
                    ok += 1;
                    // 自己复制 / 移动的文件告诉本地历史，不算「外部修改」
                    mark_saved(&dest);
                    if mv {
                        crate::history::removed(src);
                        self.retarget_tabs(src, &dest);
                    }
                    last = Some(dest);
                }
                Err(e) => failed.push(format!("{}: {e}", name.to_string_lossy())),
            }
        }
        self.reload_tree_for(dir);
        for s in srcs.iter().filter_map(|s| s.parent()) {
            self.reload_tree_for(s);
        }
        self.expanded.insert(dir.to_path_buf());
        if let Some(p) = last {
            self.ts.sel = vec![p.clone()];
            self.ts.anchor = Some(p);
        }
        self.reindex();
        self.refresh_git();
        let verb = if mv { t("已移动") } else { t("已复制") };
        if !failed.is_empty() {
            self.toast(tf("{} {} 项，{} 项失败：{}", &[&verb, &ok, &failed.len(), &failed[0]]), ERR());
        } else if skipped > 0 {
            self.toast(tf("{} {} 项；{} 项因同名或位置不对跳过", &[&verb, &ok, &skipped]), WARN());
        } else if ok > 0 {
            self.toast(tf("{} {} 项到 {}", &[&verb, &ok, &self.rel_of(dir)]), OK());
        }
    }

    fn rel_of(&self, p: &Path) -> String {
        let root = self.root.clone().unwrap_or_default();
        let r = p.strip_prefix(&root).unwrap_or(p).to_string_lossy().replace('\\', "/");
        if r.is_empty() {
            root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "/".into())
        } else {
            format!("{r}/")
        }
    }

    /// 文件移动了：打开着的标签跟着换路径
    fn retarget_tabs(&mut self, from: &Path, to: &Path) {
        let root = self.root.clone().unwrap_or_default();
        for d in &mut self.tabs {
            if let Ok(rest) = d.path.strip_prefix(from) {
                d.path = if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) };
                d.rel = d.path.strip_prefix(&root).unwrap_or(&d.path).to_string_lossy().replace('\\', "/");
                d.lang = Lang::from_path(&d.path);
            }
        }
        self.sync_tabs();
    }

    /// 树获得焦点时的 Ctrl+C / Ctrl+X / Ctrl+V / Delete / F2
    pub(super) fn tree_keys(&mut self, ctx: &egui::Context) {
        if !self.ts.focus || self.side != Side::Files {
            return;
        }
        // 最后点的是树：从树里打开文件时编辑器会要焦点，这里让出来，键盘归树
        ctx.memory_mut(|m| {
            for d in &self.tabs {
                m.surrender_focus(d.ed.id);
            }
        });
        if ctx.wants_keyboard_input() {
            return;
        }
        let now = ctx.input(|i| i.time);
        if ctx.input(|i| i.modifiers.command) {
            self.ts.cmd_at = now;
        }
        let recent_cmd = now - self.ts.cmd_at < 0.6;
        let (copy, cut, paste) = ctx.input(|i| {
            let mut c = (false, false, false);
            for e in &i.events {
                match e {
                    egui::Event::Copy => c.0 = true,
                    egui::Event::Cut => c.1 = true,
                    // 剪贴板里没有文字时 egui 不发 Paste，按下的 V 也被吞了，只能看 V 松开；
                    // 松开时 Ctrl 可能已经先松了，所以看「刚才按着 Ctrl」（有文字时也只走这里，免得粘两次）
                    egui::Event::Key {
                        key: Key::V,
                        pressed: false,
                        modifiers,
                        ..
                    } if modifiers.command || recent_cmd => c.2 = true,
                    _ => {}
                }
            }
            c
        });
        if copy || cut {
            let sel = self.ts.sel.clone();
            self.tree_copy(sel, cut);
        }
        if paste {
            let dir = self.paste_dir();
            self.tree_paste(&dir);
        }
    }

    /// 粘贴目标：选中的是文件夹就放进去，是文件就放到它所在的文件夹
    fn paste_dir(&self) -> PathBuf {
        let root = self.root.clone().unwrap_or_default();
        match self.ts.sel.last() {
            Some(p) if p.is_dir() => p.clone(),
            Some(p) => p.parent().map(Path::to_path_buf).unwrap_or(root),
            None => root,
        }
    }

    /// 每一行画完后：选中高亮、拖动开始、拖动经过时记录目标
    pub(super) fn tree_row_after(&mut self, ui: &Ui, r: &egui::Response, path: &Path, is_dir: bool, parent: &Path) {
        let target = if is_dir { path.to_path_buf() } else { parent.to_path_buf() };
        self.ts.order.push(path.to_path_buf());
        self.ts.rects.push((r.rect, target.clone()));
        if self.tree_selected(path) && self.ts.sel.len() > 1 {
            ui.painter().rect_filled(r.rect, 0.0, ACCENT().gamma_multiply(0.10));
        }
        if r.drag_started() {
            let paths = if self.tree_selected(path) { self.ts.sel.clone() } else { vec![path.to_path_buf()] };
            self.ts.drag = Some(paths);
            self.ts.focus = true;
        }
        if self.ts.drag.is_some() && ui.rect_contains_pointer(r.rect) {
            self.ts.hover_dir = Some(target);
        }
    }

    /// 树画完后：处理拖动的提示和松手
    pub(super) fn tree_drag_end(&mut self, ui: &Ui, area: Rect) {
        self.ts.area = Some(area);
        self.ts.last_order = std::mem::take(&mut self.ts.order);
        self.ts.last_rects = std::mem::take(&mut self.ts.rects);
        let Some(paths) = self.ts.drag.clone() else { return };
        let (pos, released, copy) = ui.input(|i| (i.pointer.hover_pos(), i.pointer.any_released(), i.modifiers.ctrl || i.modifiers.command));
        let inside = pos.is_some_and(|p| area.contains(p));
        let target = self.ts.hover_dir.take().filter(|_| inside);
        // 目标文件夹整块高亮
        if let Some(t_) = &target {
            let rows: Vec<Rect> = self.ts.last_rects.iter().filter(|(_, d)| d == t_).map(|(r, _)| *r).collect();
            if let (Some(a), Some(b)) = (rows.first(), rows.last()) {
                let r = a.union(*b);
                ui.painter()
                    .rect(r, 2.0, ACCENT().gamma_multiply(0.08), st(1.0, ACCENT().gamma_multiply(0.7)), egui::StrokeKind::Inside);
            } else if Some(t_) == self.root.as_ref() {
                ui.painter().rect_stroke(area, 2.0, st(1.0, ACCENT().gamma_multiply(0.7)), egui::StrokeKind::Inside);
            }
        }
        if let Some(p) = pos {
            let n = paths.len();
            let what = if n == 1 {
                paths[0].file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
            } else {
                tf("{} 项", &[&n])
            };
            let msg = match &target {
                Some(d) if copy => tf("复制 {} 到 {}", &[&what, &self.rel_of(d)]),
                Some(d) => tf("移动 {} 到 {}（按住 Ctrl 复制）", &[&what, &self.rel_of(d)]),
                None => what,
            };
            egui::Area::new(egui::Id::new("tree-drag-tip"))
                .order(egui::Order::Tooltip)
                .fixed_pos(p + vec2(14.0, 10.0))
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(msg).size(12.0).color(TEXT())).extend());
                    });
                });
            ui.ctx().set_cursor_icon(if copy { egui::CursorIcon::Copy } else { egui::CursorIcon::Grabbing });
        }
        if released {
            self.ts.drag = None;
            if let Some(d) = target {
                self.transfer(&paths, &d, !copy);
            }
        }
    }

    /// 外部文件拖进来时，指针下面的文件夹（不在树上返回 None）
    pub fn drop_dir_at(&self, pos: egui::Pos2) -> Option<PathBuf> {
        if self.side != Side::Files || !self.ts.area.is_some_and(|a| a.contains(pos)) {
            return None;
        }
        self.ts.last_rects.iter().find(|(r, _)| r.contains(pos)).map(|(_, d)| d.clone()).or_else(|| self.root.clone())
    }

    /// 外部文件拖到树上：复制进去（Shift = 移动）
    pub fn drop_into(&mut self, paths: &[PathBuf], dir: &Path, mv: bool) {
        // 拖进来的本来就在工程里：默认移动更符合直觉
        let inside = self.root.as_ref().is_some_and(|r| paths.iter().all(|p| p.starts_with(r)));
        self.transfer(paths, dir, mv || inside);
    }
}

fn same_set(a: &[PathBuf], b: &[PathBuf]) -> bool {
    a.len() == b.len() && a.iter().all(|x| b.contains(x))
}

/// a.c → a - 副本.c → a - 副本 (2).c …
pub(crate) fn unique_name(p: &Path) -> PathBuf {
    let dir = p.parent().unwrap_or(Path::new("."));
    let is_dir = p.is_dir();
    let (stem, ext) = if is_dir {
        (p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(), String::new())
    } else {
        (
            p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
            p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default(),
        )
    };
    let copy = t("副本");
    for k in 1..1000 {
        let name = if k == 1 { format!("{stem} - {copy}{ext}") } else { format!("{stem} - {copy} ({k}){ext}") };
        let c = dir.join(name);
        if !c.exists() {
            return c;
        }
    }
    p.to_path_buf()
}

fn mark_saved(p: &Path) {
    if p.is_dir() {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                mark_saved(&e.path());
            }
        }
    } else if std::fs::metadata(p).is_ok_and(|m| m.len() <= 20 * 1024 * 1024) {
        if let Ok(b) = std::fs::read(p) {
            crate::history::saved(p, &b);
        }
    }
}

fn copy_path(src: &Path, dest: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dest)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_path(&e.path(), &dest.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dest).map(|_| ())
    }
}

/// 同一个盘直接改名；跨盘（rename 失败）就复制后删除
fn move_path(src: &Path, dest: &Path) -> std::io::Result<()> {
    if std::fs::rename(src, dest).is_ok() {
        return Ok(());
    }
    copy_path(src, dest)?;
    if src.is_dir() {
        std::fs::remove_dir_all(src)
    } else {
        std::fs::remove_file(src)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_names() {
        let d = std::env::temp_dir().join(format!("qccom-uniq-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a.c"), "x").unwrap();
        let u = unique_name(&d.join("a.c"));
        assert_eq!(u.file_name().unwrap().to_string_lossy(), format!("a - {}.c", t("副本")));
        std::fs::write(&u, "y").unwrap();
        let u2 = unique_name(&d.join("a.c"));
        assert!(u2.to_string_lossy().ends_with("(2).c"));
        std::fs::create_dir_all(d.join("sub/inner")).unwrap();
        std::fs::write(d.join("sub/inner/f.h"), "z").unwrap();
        copy_path(&d.join("sub"), &d.join("sub2")).unwrap();
        assert!(d.join("sub2/inner/f.h").is_file());
        move_path(&d.join("sub2"), &d.join("sub3")).unwrap();
        assert!(!d.join("sub2").exists() && d.join("sub3/inner/f.h").is_file());
        let _ = std::fs::remove_dir_all(&d);
    }
}
