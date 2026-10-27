//! 设置中心：集中管理所有设置，可搜索；快捷键也在这里改。

use super::*;
use crate::keymap::{chord_str, def, Chord, Combo, ACTIONS};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cat {
    General,
    Appearance,
    Log,
    Connection,
    Editor,
    Keys,
    About,
}

impl Cat {
    pub const ALL: [Cat; 7] = [Cat::General, Cat::Appearance, Cat::Log, Cat::Connection, Cat::Editor, Cat::Keys, Cat::About];

    pub fn label(self) -> &'static str {
        match self {
            Cat::General => t("常规"),
            Cat::Appearance => t("外观"),
            Cat::Log => t("日志"),
            Cat::Connection => t("连接"),
            Cat::Editor => t("代码编辑器"),
            Cat::Keys => t("快捷键"),
            Cat::About => t("关于"),
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Cat::General => "general",
            Cat::Appearance => "appearance",
            Cat::Log => "log",
            Cat::Connection => "connection",
            Cat::Editor => "editor",
            Cat::Keys => "keys",
            Cat::About => "about",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Cat::General => Icon::Gear,
            Cat::Appearance => Icon::Palette,
            Cat::Log => Icon::Lines,
            Cat::Connection => Icon::Plug,
            Cat::Editor => Icon::Code,
            Cat::Keys => Icon::Keyboard,
            Cat::About => Icon::Info,
        }
    }
}

/// 正在录制的快捷键：(命令, 替换第几个绑定（None = 新增）, 已按下的组合, 最后一次按键时间)
pub struct Recording {
    pub act: Act,
    pub slot: Option<usize>,
    pub combos: Vec<Combo>,
    pub at: Instant,
}

pub struct Prefs {
    pub cat: Cat,
    pub search: String,
    pub recording: Option<Recording>,
    key_search: String,
    /// 刚改过、有冲突的命令，行上标红提示
    conflict: Option<(Act, Vec<Act>)>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            cat: Cat::General,
            search: String::new(),
            recording: None,
            key_search: String::new(),
            conflict: None,
        }
    }
}

/// 一行设置：左边标题和说明，右边控件。搜索时不匹配就不画
fn row(ui: &mut egui::Ui, q: &str, title: &str, desc: &str, add: impl FnOnce(&mut egui::Ui)) {
    if !q.is_empty() && find_ci(title, q).is_none() && find_ci(desc, q).is_none() {
        return;
    }
    let w = ui.available_width();
    egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 10)).show(ui, |ui| {
        ui.set_width(w);
        ui.horizontal(|ui| {
            let text_w = (w - 260.0).max(220.0);
            ui.vertical(|ui| {
                ui.set_width(text_w);
                ui.label(RichText::new(title).size(13.5).color(TEXT()));
                if !desc.is_empty() {
                    ui.add_space(2.0);
                    ui.label(RichText::new(desc).size(12.0).color(TEXT3()));
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().slider_width = 170.0;
                add(ui)
            });
        });
    });
    let y = ui.cursor().top();
    ui.painter().hline(ui.max_rect().x_range(), y, st(1.0, LINE()));
}

/// 这一类里有没有设置项匹配搜索（决定搜索时显示不显示分类标题）
fn cat_matches(cat: Cat, q: &str) -> bool {
    let items: &[&str] = match cat {
        Cat::General => &["界面语言", "启动时自动检查更新", "检查更新", "便携模式", "命令面板"],
        Cat::Appearance => &["界面主题", "深色", "浅色", "跟随系统", "界面缩放", "日志字号"],
        Cat::Log => &["时间戳", "行号", "发送回显", "ANSI 颜色", "级别着色", "复制时带时间戳", "保留行数", "空闲分帧"],
        Cat::Connection => &["拔出后自动重连", "自动选中新设备", "DTR", "RTS"],
        Cat::Editor => &["代码字号", "缩略图", "使用 clangd", "clangd 路径", "代码片段", "Vim 键位", "自动保存", "行内 Blame"],
        Cat::Keys => &["快捷键"],
        Cat::About => &["关于", "版本", "许可证"],
    };
    q.is_empty() || find_ci(cat.label(), q).is_some() || items.iter().any(|s| find_ci(t(s), q).is_some())
}

impl App {
    pub(super) fn settings_page(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.key_pressed(Key::Escape)) && self.prefs.recording.is_none() && !ctx.wants_keyboard_input() {
            self.set_page(self.prev_page);
            return;
        }
        egui::SidePanel::left("prefs-nav")
            .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                left: 12,
                right: 12,
                top: 16,
                bottom: 12,
            }))
            .exact_width(220.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(RichText::new(t("设置")).font(FontId::new(18.0, bold())).color(TEXT()));
                ui.add_space(12.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.prefs.search)
                        .hint_text(t("搜索设置"))
                        .desired_width(f32::INFINITY)
                        .margin(vec2(8.0, 6.0)),
                );
                if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::F)) {
                    r.request_focus();
                }
                ui.add_space(10.0);
                for c in Cat::ALL {
                    let on = self.prefs.cat == c && self.prefs.search.is_empty();
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
                    let p = ui.painter();
                    if on {
                        p.rect_filled(rect, CornerRadius::same(6), BG3());
                        p.rect_filled(Rect::from_min_size(rect.min + vec2(0.0, 8.0), vec2(2.5, 16.0)), 1.0, ACCENT());
                    } else if resp.hovered() {
                        p.rect_filled(rect, CornerRadius::same(6), tint(5));
                    }
                    let col = if on { TEXT() } else { TEXT2() };
                    draw_icon(
                        p,
                        Rect::from_center_size(pos2(rect.left() + 18.0, rect.center().y), vec2(15.0, 15.0)),
                        c.icon(),
                        if on { ACCENT() } else { TEXT3() },
                    );
                    p.text(pos2(rect.left() + 36.0, rect.center().y), Align2::LEFT_CENTER, c.label(), FontId::proportional(13.5), col);
                    if resp.clicked() {
                        self.prefs.cat = c;
                        self.prefs.search.clear();
                    }
                }
            });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG0())).show(ctx, |ui| {
            // 右上角关闭
            let close = Rect::from_center_size(pos2(ui.max_rect().right() - 28.0, ui.max_rect().top() + 26.0), vec2(28.0, 28.0));
            let cr = ui.interact(close, ui.id().with("close-prefs"), Sense::click()).on_hover_text(t("关闭设置（Esc）"));
            if cr.hovered() {
                ui.painter().rect_filled(close, CornerRadius::same(6), BG3());
            }
            draw_icon(ui.painter(), close.shrink(8.0), Icon::Close, TEXT2());
            if cr.clicked() {
                self.set_page(self.prev_page);
                return;
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let w = (ui.available_width() - 80.0).clamp(420.0, 760.0);
                ui.horizontal(|ui| {
                    ui.add_space(40.0);
                    ui.vertical(|ui| {
                        ui.set_width(w);
                        ui.add_space(24.0);
                        let q = self.prefs.search.trim().to_string();
                        if q.is_empty() {
                            self.prefs_cat(ui, self.prefs.cat, "");
                        } else {
                            let mut any = false;
                            for c in Cat::ALL {
                                if c != Cat::Keys && c != Cat::About && cat_matches(c, &q) {
                                    any = true;
                                    self.prefs_cat(ui, c, &q);
                                    ui.add_space(20.0);
                                }
                            }
                            if !any {
                                ui.label(RichText::new(t("没有匹配的设置")).color(TEXT3()));
                            }
                        }
                        ui.add_space(40.0);
                    });
                });
            });
        });
    }

    fn prefs_cat(&mut self, ui: &mut egui::Ui, cat: Cat, q: &str) {
        ui.label(RichText::new(cat.label()).font(FontId::new(20.0, bold())).color(TEXT()));
        ui.add_space(8.0);
        match cat {
            Cat::General => self.prefs_general(ui, q),
            Cat::Appearance => self.prefs_appearance(ui, q),
            Cat::Log => self.prefs_log(ui, q),
            Cat::Connection => self.prefs_conn(ui, q),
            Cat::Editor => self.prefs_editor(ui, q),
            Cat::Keys => self.prefs_keys(ui),
            Cat::About => self.prefs_about(ui),
        }
    }

    fn prefs_general(&mut self, ui: &mut egui::Ui, q: &str) {
        let det = i18n::LANGS[i18n::detect()].1;
        row(ui, q, t("界面语言"), t("默认跟随系统语言，切换立即生效"), |ui| {
            let cur = if self.s.lang == "auto" {
                format!("{}  ({det})", t("跟随系统"))
            } else {
                i18n::LANGS[i18n::current()].1.to_string()
            };
            egui::ComboBox::from_id_salt("pref-lang").width(200.0).selected_text(cur).show_ui(ui, |ui| {
                if ui.selectable_label(self.s.lang == "auto", format!("{}  ({det})", t("跟随系统"))).clicked() {
                    self.s.lang = "auto".into();
                    i18n::apply("auto");
                }
                for (code, name) in i18n::LANGS {
                    if ui.selectable_label(self.s.lang == *code, *name).clicked() {
                        self.s.lang = code.to_string();
                        i18n::apply(code);
                    }
                }
            });
        });
        row(ui, q, t("启动时自动检查更新"), t("有新版本时在右下角提示，不会自动安装"), |ui| {
            switch(ui, &mut self.s.auto_update);
        });
        row(ui, q, t("检查更新"), &tf("当前版本 v{}", &[&env!("CARGO_PKG_VERSION")]), |ui| {
            if ui.button(t("立即检查")).clicked() {
                self.check_update(true);
            }
        });
        let portable = crate::portable_dir();
        let desc = match &portable {
            Some(d) => tf("已启用：设置和恢复数据保存在 {}", &[&d.display()]),
            None => t("把设置保存在程序旁边的 qccom-data 文件夹里，整个文件夹拷到 U 盘就能带着走").to_string(),
        };
        row(ui, q, t("便携模式"), &desc, |ui| {
            if portable.is_none() && ui.button(t("启用")).clicked() {
                match enable_portable() {
                    Ok(()) => self.toast(t("已启用便携模式，重启后生效"), OK()),
                    Err(e) => self.toast(e.to_string(), ERR()),
                }
            }
        });
        let pk = self.km.label(Act::Palette);
        row(ui, q, t("命令面板"), &tf("按 {} 可以搜索并执行任何功能", &[&pk]), |ui| {
            if ui.button(t("打开")).clicked() {
                self.open_palette();
            }
        });
    }

    fn prefs_appearance(&mut self, ui: &mut egui::Ui, q: &str) {
        if q.is_empty() || ["界面主题", "深色", "浅色", "跟随系统"].iter().any(|s| find_ci(t(s), q).is_some()) {
            ui.label(RichText::new(t("界面主题")).size(13.5).color(TEXT()));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (mode, name) in [("dark", t("深色")), ("light", t("浅色")), ("system", t("跟随系统"))] {
                    if theme_card(ui, mode, name, self.s.theme == mode).clicked() {
                        let ctx = ui.ctx().clone();
                        self.set_theme(&ctx, mode);
                    }
                    ui.add_space(8.0);
                }
            });
            ui.add_space(10.0);
            let y = ui.cursor().top();
            ui.painter().hline(ui.max_rect().x_range(), y, st(1.0, LINE()));
        }
        row(ui, q, t("界面缩放"), t("高分屏上觉得字小可以放大"), |ui| {
            let mut pct = (self.s.ui_scale * 100.0).round() as i32;
            let r = ui.add(egui::Slider::new(&mut pct, 80..=200).step_by(10.0).suffix("%"));
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.s.ui_scale = pct as f32 / 100.0;
                ui.ctx().set_zoom_factor(self.s.ui_scale);
            }
        });
        row(ui, q, t("日志字号"), t("日志和终端使用的等宽字体大小"), |ui| {
            if ui.add(egui::Slider::new(&mut self.s.font_size, 10.0..=22.0).step_by(0.5)).changed() {
                let fs = self.s.font_size;
                ui.ctx().all_styles_mut(|s| {
                    s.text_styles.insert(TextStyle::Monospace, FontId::new(fs, FontFamily::Monospace));
                });
            }
        });
    }

    fn prefs_log(&mut self, ui: &mut egui::Ui, q: &str) {
        row(ui, q, t("时间戳"), t("每行前显示接收时间（毫秒）"), |ui| {
            switch(ui, &mut self.s.show_ts);
        });
        row(ui, q, t("行号"), t("左侧显示行号"), |ui| {
            switch(ui, &mut self.s.show_ln);
        });
        row(ui, q, t("发送回显"), t("在日志里显示自己发送的数据"), |ui| {
            if switch(ui, &mut self.s.show_tx).changed() {
                self.view_dirty = true;
            }
        });
        row(ui, q, t("ANSI 颜色"), t("解析终端颜色转义码（\\x1b[31m 等）"), |ui| {
            switch(ui, &mut self.s.ansi);
        });
        row(ui, q, t("级别着色"), t("ERROR / WARN 整行上色，级别标记高亮"), |ui| {
            switch(ui, &mut self.s.level_color);
        });
        row(
            ui,
            q,
            t("复制时带时间戳"),
            t("Ctrl+C 复制选中内容时，每行带上时间戳；右键菜单里两种都有"),
            |ui| {
                switch(ui, &mut self.s.copy_ts);
            },
        );
        row(ui, q, t("保留行数"), t("超过后丢弃最早的日志，避免占用太多内存"), |ui| {
            if ui.add(egui::DragValue::new(&mut self.s.max_lines).range(1000..=2_000_000).speed(1000).suffix(t(" 行"))).changed() {
                self.buf.max_lines = self.s.max_lines;
            }
        });
        row(
            ui,
            q,
            t("空闲分帧"),
            t("两次数据间隔超过这个时间就另起一行（0 = 只按换行符分行）。\n看二进制协议时很好用。"),
            |ui| {
                if ui.add(egui::DragValue::new(&mut self.s.frame_gap_ms).range(0..=5000).suffix(" ms")).changed() {
                    self.buf.frame_gap_ms = self.s.frame_gap_ms;
                }
            },
        );
    }

    fn prefs_conn(&mut self, ui: &mut egui::Ui, q: &str) {
        row(ui, q, t("拔出后自动重连"), t("设备断开后等它重新出现，自动连回去"), |ui| {
            switch(ui, &mut self.s.auto_reconnect);
        });
        row(ui, q, t("自动选中新设备"), t("插入新串口时自动切换过去"), |ui| {
            switch(ui, &mut self.s.follow_new_port);
        });
        row(ui, q, "DTR", t("部分开发板（如 ESP32）用 DTR/RTS 控制复位和下载模式"), |ui| {
            if switch(ui, &mut self.s.dtr).changed() {
                if let Some(c) = &self.conn {
                    let _ = c.cmd.send(Cmd::SetDtr(self.s.dtr));
                }
            }
        });
        row(ui, q, "RTS", t("部分开发板（如 ESP32）用 DTR/RTS 控制复位和下载模式"), |ui| {
            if switch(ui, &mut self.s.rts).changed() {
                if let Some(c) = &self.conn {
                    let _ = c.cmd.send(Cmd::SetRts(self.s.rts));
                }
            }
        });
    }

    fn prefs_editor(&mut self, ui: &mut egui::Ui, q: &str) {
        row(ui, q, t("代码字号"), t("代码浏览器的字体大小（Ctrl+= / Ctrl+- 也可以调）"), |ui| {
            ui.add(egui::Slider::new(&mut self.code.s.font_size, 9.0..=28.0).step_by(0.5));
        });
        row(ui, q, t("缩略图"), t("编辑器右侧显示整个文件的缩略图，可拖动定位"), |ui| {
            switch(ui, &mut self.code.s.minimap);
        });
        row(
            ui,
            q,
            t("使用 clangd"),
            t("装了 clangd 且工程里有 compile_commands.json 时，跳转定义交给 clangd，更准确；否则用内置索引"),
            |ui| {
                switch(ui, &mut self.code.s.clangd);
            },
        );
        let (status, ok) = self.code.clangd_status();
        if q.is_empty() || find_ci("clangd", q).is_some() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(if ok { "●" } else { "○" }).size(10.0).color(if ok { ACCENT() } else { TEXT3() }));
                ui.label(RichText::new(status).size(12.0).color(TEXT2()));
            });
        }
        row(ui, q, t("clangd 路径"), t("留空会自动查找（PATH、LLVM 默认安装目录）"), |ui| {
            ui.add(egui::TextEdit::singleline(&mut self.code.s.clangd_path).desired_width(240.0).hint_text(t("自动")));
        });
        row(
            ui,
            q,
            t("代码片段"),
            t("输入前缀后按 Tab 展开，或按 Ctrl+Alt+J 搜索；片段保存在一个 JSON 文件里"),
            |ui| {
                if ui.button(t("编辑…")).clicked() {
                    self.code.edit_snippets();
                    self.set_page(Page::Code);
                }
            },
        );
        row(ui, q, t("Vim 键位"), t("普通 / 插入 / 可视模式、hjkl、dw、ciw、. 重复、:w 等常用操作"), |ui| {
            switch(ui, &mut self.code.s.vim);
        });
        row(ui, q, t("自动保存"), t("停止输入 1 秒后自动保存修改过的文件"), |ui| {
            switch(ui, &mut self.code.s.autosave);
        });
        row(ui, q, t("行内 Blame"), t("在当前行末尾淡淡显示最后修改它的人、时间和提交说明"), |ui| {
            switch(ui, &mut self.code.s.inline_blame);
        });
    }

    fn prefs_about(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
            ui.painter().image(self.logo.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), logo_tint());
            ui.vertical(|ui| {
                ui.label(RichText::new(format!("QCCOM  v{}", env!("CARGO_PKG_VERSION"))).font(FontId::new(17.0, bold())).color(TEXT()));
                ui.label(RichText::new(t("串口 / 网络调试助手")).color(TEXT2()));
                ui.label(RichText::new("© 2026 zenghi  ·  Apache License 2.0").size(12.0).color(TEXT3()));
            });
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            if ui.button("GitHub").clicked() {
                open_url("https://github.com/zenghi/QCCOM");
            }
            if ui.button(t("使用手册")).clicked() {
                open_url("https://github.com/zenghi/QCCOM/blob/main/docs/USER_GUIDE.md");
            }
            if ui.button(t("问题反馈")).clicked() {
                open_url("https://github.com/zenghi/QCCOM/issues");
            }
        });
    }

    // ------------------------------------------------------------ 快捷键

    /// 保存某个命令当前的绑定；和默认一样就删掉覆盖项
    fn store_keys(&mut self, a: Act, list: Vec<String>) {
        let d = def(a);
        let defaults: Vec<String> = d.keys.iter().filter_map(|k| crate::keymap::parse_chord(k)).map(|c| chord_str(&c)).collect();
        if list == defaults {
            self.s.keys.remove(d.id);
        } else {
            self.s.keys.insert(d.id.to_string(), list);
        }
        self.km.rebuild(&self.s.keys);
    }

    fn record_keys(&mut self, ctx: &egui::Context) {
        let Some(rec) = self.prefs.recording.as_mut() else { return };
        let mut cancel = false;
        ctx.input_mut(|i| {
            i.events.retain(|e| {
                let egui::Event::Key { key, pressed: true, modifiers, .. } = e else {
                    return !matches!(e, egui::Event::Text(_));
                };
                if *key == Key::Escape && !modifiers.any() {
                    cancel = true;
                    return false;
                }
                let c = Combo::from_event(*key, *modifiers);
                if c.usable() && rec.combos.len() < 2 {
                    rec.combos.push(c);
                    rec.at = Instant::now();
                }
                false
            });
        });
        if cancel {
            self.prefs.recording = None;
            return;
        }
        let rec = self.prefs.recording.as_ref().unwrap();
        // 按完第一组后等一下，再按一组就是序列（如 Ctrl+K Ctrl+S）
        let done = rec.combos.len() == 2 || (!rec.combos.is_empty() && rec.at.elapsed() > Duration::from_millis(900));
        if !done {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        let rec = self.prefs.recording.take().unwrap();
        let chord: Chord = rec.combos;
        let mut list: Vec<String> = self.km.bindings(rec.act).iter().map(|c| chord_str(c)).collect();
        let s = chord_str(&chord);
        match rec.slot {
            Some(i) if i < list.len() => list[i] = s,
            _ => {
                if !list.contains(&s) {
                    list.push(s)
                }
            }
        }
        self.store_keys(rec.act, list);
        let clash = self.km.conflicts(rec.act, &chord);
        self.prefs.conflict = (!clash.is_empty()).then_some((rec.act, clash));
    }

    fn prefs_keys(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.record_keys(&ctx);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.prefs.key_search)
                    .hint_text(t("按命令名称或按键搜索，如「清屏」「Ctrl+L」"))
                    .desired_width(320.0)
                    .margin(vec2(8.0, 6.0)),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(t("恢复全部默认")).clicked() {
                    self.s.keys.clear();
                    self.km.rebuild(&self.s.keys);
                    self.prefs.conflict = None;
                }
                if ui.button(t("导出")).clicked() {
                    if let Some(p) = rfd::FileDialog::new().set_file_name("qccom-keybindings.json").save_file() {
                        let txt = serde_json::to_string_pretty(&self.s.keys).unwrap_or_default();
                        match std::fs::write(&p, txt) {
                            Ok(()) => self.toast(t("已导出"), OK()),
                            Err(e) => self.toast(e.to_string(), ERR()),
                        }
                    }
                }
                if ui.button(t("导入")).clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                        match std::fs::read_to_string(&p)
                            .map_err(|e| e.to_string())
                            .and_then(|s| serde_json::from_str::<Overrides>(&s).map_err(|e| e.to_string()))
                        {
                            Ok(ov) => {
                                self.s.keys = ov;
                                self.km.rebuild(&self.s.keys);
                                self.toast(t("已导入快捷键"), OK());
                            }
                            Err(e) => self.toast(tf("导入失败：{}", &[&e]), ERR()),
                        }
                    }
                }
            });
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new(t("点击按键可以修改，按 Esc 取消；连按两组组合键可以设成序列（如 Ctrl+K Ctrl+S）。右键可恢复默认或删除。"))
                .size(12.0)
                .color(TEXT3()),
        );
        ui.add_space(10.0);
        let q = self.prefs.key_search.trim().to_string();
        let w = ui.available_width();
        let mut last_scope = None;
        let mut action: Option<(Act, KeyOp)> = None;
        // 按范围分组显示，组内保持表里的顺序
        let mut defs: Vec<_> = ACTIONS.iter().collect();
        defs.sort_by_key(|d| d.scope as u8);
        for d in defs {
            let binds: Vec<String> = self.km.bindings(d.act).iter().map(|c| chord_str(c)).collect();
            let label = t(d.label);
            if !q.is_empty() && find_ci(label, &q).is_none() && find_ci(d.id, &q).is_none() && !binds.iter().any(|b| find_ci(b, &q).is_some()) {
                continue;
            }
            if last_scope != Some(d.scope) {
                last_scope = Some(d.scope);
                ui.add_space(12.0);
                ui.label(RichText::new(d.scope.label()).font(FontId::new(11.5, bold())).color(TEXT3()));
                ui.add_space(4.0);
            }
            let modified = self.s.keys.contains_key(d.id);
            let clash = self.prefs.conflict.as_ref().filter(|(a, _)| *a == d.act).map(|(_, v)| v.clone());
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 36.0), Sense::click());
            let p = ui.painter().clone();
            if resp.hovered() {
                p.rect_filled(rect, CornerRadius::same(6), tint(4));
            }
            if modified {
                p.circle_filled(pos2(rect.left() + 6.0, rect.center().y), 3.0, ACCENT());
            }
            p.text(pos2(rect.left() + 16.0, rect.center().y), Align2::LEFT_CENTER, label, FontId::proportional(13.5), TEXT());
            // 绑定：每个一个按键帽组，可点
            let mut x = rect.left() + (w * 0.45).max(240.0);
            let recording_here = self.prefs.recording.as_ref().is_some_and(|r| r.act == d.act);
            for (i, b) in binds.iter().enumerate() {
                let br = Rect::from_min_size(pos2(x, rect.top() + 6.0), vec2(0.0, 24.0));
                let width = palette::key_caps(&p, pos2(x + 400.0, rect.center().y), b);
                let br = Rect::from_min_size(br.min, vec2(width + 8.0, 24.0));
                // key_caps 右对齐画的，挪到左边：重画一次
                p.rect_filled(
                    Rect::from_min_size(pos2(x + 400.0 - width - 2.0, rect.top() + 4.0), vec2(width + 4.0, 28.0)),
                    0.0,
                    if resp.hovered() { blend_bg() } else { BG0() },
                );
                let hit = ui.interact(br, ui.id().with((d.id, i)), Sense::click());
                if hit.hovered() {
                    p.rect(
                        br.expand(2.0),
                        CornerRadius::same(6),
                        Color32::TRANSPARENT,
                        st(1.0, ACCENT().gamma_multiply(0.6)),
                        egui::StrokeKind::Inside,
                    );
                }
                palette::key_caps(&p, pos2(x + width, rect.center().y), b);
                if hit.clicked() {
                    action = Some((d.act, KeyOp::Record(Some(i))));
                }
                hit.context_menu(|ui| {
                    if menu_item(ui, t("修改"), "") {
                        action = Some((d.act, KeyOp::Record(Some(i))));
                        ui.close_menu();
                    }
                    if menu_item(ui, t("删除这个快捷键"), "") {
                        action = Some((d.act, KeyOp::Remove(i)));
                        ui.close_menu();
                    }
                });
                x += width + 14.0;
            }
            if recording_here {
                let txt = match &self.prefs.recording {
                    Some(r) if !r.combos.is_empty() => format!("{} …", chord_str(&r.combos)),
                    _ => t("请按下新的快捷键…").to_string(),
                };
                let g = p.layout_no_wrap(txt, FontId::proportional(12.5), ACCENT());
                let rr = Rect::from_min_size(pos2(x, rect.center().y - 12.0), vec2(g.size().x + 16.0, 24.0));
                p.rect(rr, CornerRadius::same(5), ACCENT().gamma_multiply(0.1), st(1.0, ACCENT()), egui::StrokeKind::Inside);
                p.galley(pos2(rr.left() + 8.0, rr.center().y - g.size().y / 2.0), g, ACCENT());
            } else if resp.hovered() || binds.is_empty() {
                // 添加
                let ar = Rect::from_min_size(pos2(x, rect.center().y - 12.0), vec2(24.0, 24.0));
                let hit = ui.interact(ar, ui.id().with((d.id, "add")), Sense::click()).on_hover_text(t("添加快捷键"));
                p.rect(
                    ar,
                    CornerRadius::same(5),
                    if hit.hovered() { BG3() } else { Color32::TRANSPARENT },
                    st(1.0, LINE_HI()),
                    egui::StrokeKind::Inside,
                );
                draw_icon(&p, ar.shrink(6.0), Icon::Plus, TEXT2());
                if hit.clicked() {
                    action = Some((d.act, KeyOp::Record(None)));
                }
            }
            // 右侧：范围 / 恢复默认
            if modified && resp.hovered() {
                let rr = Rect::from_min_size(pos2(rect.right() - 86.0, rect.center().y - 12.0), vec2(80.0, 24.0));
                let hit = ui.interact(rr, ui.id().with((d.id, "reset")), Sense::click());
                p.text(
                    rr.center(),
                    Align2::CENTER_CENTER,
                    t("恢复默认"),
                    FontId::proportional(12.0),
                    if hit.hovered() { ACCENT() } else { TEXT3() },
                );
                if hit.clicked() {
                    action = Some((d.act, KeyOp::Reset));
                }
            }
            resp.context_menu(|ui| {
                if menu_item(ui, t("添加快捷键"), "") {
                    action = Some((d.act, KeyOp::Record(None)));
                    ui.close_menu();
                }
                if menu_item(ui, t("恢复默认"), "") {
                    action = Some((d.act, KeyOp::Reset));
                    ui.close_menu();
                }
                if menu_item(ui, t("删除全部快捷键"), "") {
                    action = Some((d.act, KeyOp::Clear));
                    ui.close_menu();
                }
            });
            if let Some(cl) = clash {
                let names: Vec<&str> = cl.iter().map(|a| t(def(*a).label)).collect();
                ui.label(RichText::new(tf("⚠ 和「{}」冲突，同一页面里按下时只会执行其中一个", &[&names.join("」「")])).size(12.0).color(ERR()));
            }
            let y = ui.cursor().top();
            ui.painter().hline(rect.x_range(), y, st(1.0, LINE().gamma_multiply(0.6)));
        }
        if let Some((a, op)) = action {
            self.prefs.conflict = None;
            match op {
                KeyOp::Record(slot) => {
                    self.prefs.recording = Some(Recording {
                        act: a,
                        slot,
                        combos: vec![],
                        at: Instant::now(),
                    });
                }
                KeyOp::Remove(i) => {
                    let mut list: Vec<String> = self.km.bindings(a).iter().map(|c| chord_str(c)).collect();
                    if i < list.len() {
                        list.remove(i);
                    }
                    self.store_keys(a, list);
                }
                KeyOp::Reset => {
                    self.s.keys.remove(def(a).id);
                    self.km.rebuild(&self.s.keys);
                }
                KeyOp::Clear => self.store_keys(a, vec![]),
            }
        }
    }
}

enum KeyOp {
    Record(Option<usize>),
    Remove(usize),
    Reset,
    Clear,
}

fn blend_bg() -> Color32 {
    // 悬停行的底色（BG0 上叠一层 tint）
    let b = BG0();
    let o = tint(4);
    let a = o.a() as f32 / 255.0;
    let mix = |x: u8, y: u8| (x as f32 * (1.0 - a) + y as f32) as u8;
    Color32::from_rgb(mix(b.r(), o.r()), mix(b.g(), o.g()), mix(b.b(), o.b()))
}

/// 主题选择卡片：画一个小小的界面预览
fn theme_card(ui: &mut egui::Ui, mode: &str, name: &str, on: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(150.0, 112.0), Sense::click());
    let p = ui.painter();
    let preview = Rect::from_min_size(rect.min, vec2(150.0, 84.0));
    let draw = |r: Rect, pal: &crate::theme::Palette| {
        p.rect_filled(r, CornerRadius::same(6), pal.bg1);
        let side = Rect::from_min_size(r.min, vec2(12.0, r.height()));
        p.rect_filled(side, CornerRadius { nw: 6, sw: 6, ne: 0, se: 0 }, pal.bg0);
        let body = Rect::from_min_max(pos2(r.left() + 20.0, r.top() + 10.0), pos2(r.right() - 8.0, r.bottom() - 8.0));
        p.rect(body, CornerRadius::same(4), pal.bg0, st(1.0, pal.line), egui::StrokeKind::Inside);
        for (i, c) in [pal.log_text, pal.ok, pal.warn, pal.log_text, pal.err].iter().enumerate() {
            let y = body.top() + 9.0 + i as f32 * 10.0;
            let w = [60.0, 44.0, 70.0, 52.0, 38.0][i];
            p.line_segment([pos2(body.left() + 8.0, y), pos2(body.left() + 8.0 + w, y)], st(3.0, *c));
        }
        p.rect_filled(Rect::from_min_size(pos2(r.left() + 3.0, r.top() + 8.0), vec2(6.0, 6.0)), 2.0, pal.accent);
    };
    match mode {
        "dark" => draw(preview, &crate::theme::DARK),
        "light" => draw(preview, &crate::theme::LIGHT),
        _ => {
            // 跟随系统：左半深右半浅
            let left = Rect::from_min_max(preview.min, pos2(preview.center().x, preview.bottom()));
            let right = Rect::from_min_max(pos2(preview.center().x, preview.top()), preview.max);
            p.with_clip_rect(left).add(egui::Shape::Noop);
            let pl = p.with_clip_rect(left);
            let pr = p.with_clip_rect(right);
            for (pp, pal) in [(pl, &crate::theme::DARK), (pr, &crate::theme::LIGHT)] {
                pp.rect_filled(preview, CornerRadius::same(6), pal.bg1);
                let body = Rect::from_min_max(pos2(preview.left() + 20.0, preview.top() + 10.0), pos2(preview.right() - 8.0, preview.bottom() - 8.0));
                pp.rect(body, CornerRadius::same(4), pal.bg0, st(1.0, pal.line), egui::StrokeKind::Inside);
                for (i, c) in [pal.log_text, pal.ok, pal.warn, pal.log_text, pal.err].iter().enumerate() {
                    let y = body.top() + 9.0 + i as f32 * 10.0;
                    let w = [60.0, 44.0, 70.0, 52.0, 38.0][i];
                    pp.line_segment([pos2(body.left() + 8.0, y), pos2(body.left() + 8.0 + w, y)], st(3.0, *c));
                }
            }
        }
    }
    let border = if on {
        st(2.0, ACCENT())
    } else if resp.hovered() {
        st(1.0, LINE_HI())
    } else {
        st(1.0, LINE())
    };
    p.rect(preview, CornerRadius::same(6), Color32::TRANSPARENT, border, egui::StrokeKind::Outside);
    p.text(
        pos2(rect.left() + 2.0, rect.bottom() - 10.0),
        Align2::LEFT_CENTER,
        name,
        FontId::proportional(13.0),
        if on { TEXT() } else { TEXT2() },
    );
    if on {
        p.circle_filled(pos2(rect.right() - 8.0, rect.bottom() - 10.0), 4.0, ACCENT());
    }
    resp
}

/// 在 exe 旁边建 qccom-data，并把现有设置拷过去（下次启动就从那里读）
fn enable_portable() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| std::io::Error::other("exe dir"))?.join("qccom-data");
    std::fs::create_dir_all(&dir)?;
    if let Some(old) = eframe::storage_dir("qccom") {
        let _ = std::fs::copy(old.join("app.ron"), dir.join("app.ron"));
    }
    Ok(())
}
