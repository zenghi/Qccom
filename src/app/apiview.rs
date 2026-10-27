//! 侧边栏「API」标签：类似 Postman 的 HTTP 接口调试。
//! 左边是收藏和历史，中间上半部分编辑请求，下半部分看响应。串口 / 网络连接在后台照常收日志。

use crate::api::{self, AuthKind, BodyKind, HistItem, Kv, Request, Response, Tok};

use super::logx::tool_window;
use super::*;

/// 请求区里的分页
#[derive(Clone, Copy, PartialEq)]
pub(super) enum ReqTab {
    Params,
    Headers,
    Body,
    Auth,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum RespTab {
    Body,
    Headers,
}

pub(super) struct ApiUi {
    pub req_tab: ReqTab,
    pub resp_tab: RespTab,
    pub pretty: bool,
    gen: u64,
    pub busy: Option<Instant>,
    rx: Option<Receiver<(u64, Result<Response, String>)>>,
    pub resp: Option<Result<Response, String>>,
    /// 响应体排好版、上好色的结果（大响应每帧重排太慢）
    body_job: Option<(bool, LayoutJob)>,
    pub env_open: bool,
    pub curl_open: Option<String>,
    pub save_name: Option<String>,
    renaming: Option<(usize, String)>,
    pub req_h: f32,
}

impl Default for ApiUi {
    fn default() -> Self {
        ApiUi {
            req_tab: ReqTab::Params,
            resp_tab: RespTab::Body,
            pretty: true,
            gen: 0,
            busy: None,
            rx: None,
            resp: None,
            body_job: None,
            env_open: false,
            curl_open: None,
            save_name: None,
            renaming: None,
            req_h: 230.0,
        }
    }
}

pub(super) fn method_color(m: &str) -> Color32 {
    match m {
        "GET" => OK(),
        "POST" => WARN(),
        "PUT" => INFO(),
        "PATCH" => VIOLET(),
        "DELETE" => ERR(),
        _ => TEXT2(),
    }
}

fn status_color(code: u16) -> Color32 {
    match code {
        200..=299 => OK(),
        300..=399 => INFO(),
        400..=499 => WARN(),
        _ => ERR(),
    }
}

impl App {
    // ------------------------------------------------------------ 发送

    pub(super) fn api_send(&mut self) {
        if self.api.busy.is_some() {
            return;
        }
        if !self.s.api.current.url.trim_start().starts_with("curl ") {
            api::absorb_query(&mut self.s.api.current);
        }
        let missing = api::missing_vars(&self.s.api.current, &self.s.api.env);
        if !missing.is_empty() {
            self.toast(tf("没有定义的变量：{}", &[&missing.join(", ")]), WARN());
        }
        let a = &self.s.api;
        let req = clean(&a.current);
        let env = a.env.clone();
        let (timeout, follow) = (a.timeout_s, a.follow_redirects);
        self.api.gen += 1;
        let gen = self.api.gen;
        let (tx, rx) = mpsc::channel();
        self.api.rx = Some(rx);
        self.api.busy = Some(Instant::now());
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let r = api::send(&req, &env, timeout, follow);
            let _ = tx.send((gen, r));
            ctx.request_repaint();
        });
    }

    pub(super) fn api_poll(&mut self) {
        let Some(rx) = &self.api.rx else { return };
        let Ok((gen, r)) = rx.try_recv() else { return };
        self.api.rx = None;
        if gen != self.api.gen {
            return;
        }
        self.api.busy = None;
        let (status, ms) = match &r {
            Ok(resp) => (resp.status, resp.ms),
            Err(_) => (0, 0),
        };
        let h = &mut self.s.api.history;
        h.insert(
            0,
            HistItem {
                req: clean(&self.s.api.current),
                status,
                ms,
                at: chrono::Local::now().format("%m-%d %H:%M:%S").to_string(),
            },
        );
        h.truncate(100);
        self.api.resp = Some(r);
        self.api.body_job = None;
    }

    fn api_cancel(&mut self) {
        self.api.gen += 1;
        self.api.busy = None;
        self.api.rx = None;
    }

    fn api_load(&mut self, r: Request) {
        self.s.api.current = r;
        self.api.resp = None;
        self.api.body_job = None;
    }

    fn api_save(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let mut r = clean(&self.s.api.current);
        r.name = name.to_string();
        self.s.api.current.name = r.name.clone();
        match self.s.api.saved.iter_mut().find(|s| s.name == r.name) {
            Some(s) => *s = r,
            None => self.s.api.saved.push(r),
        }
        self.toast(tf("已保存「{}」", &[&name]), OK());
    }

    // ------------------------------------------------------------ 侧边栏

    pub(super) fn api_sidebar(&mut self, ui: &mut egui::Ui) {
        if self.conn.is_some() {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(OK()));
                    ui.label(
                        RichText::new(tf("{} 仍在连接，日志在后台照常接收", &[&self.connected.clone().unwrap_or_default()]))
                            .small()
                            .color(TEXT2()),
                    );
                });
            });
            ui.add_space(10.0);
        }
        let n_env = self.s.api.env.iter().filter(|e| e.on && !e.k.is_empty()).count();
        eyebrow(ui, t("环境变量"), &n_env.to_string());
        let r = ui
            .add(
                egui::Button::new(
                    RichText::new(if n_env == 0 { t("添加变量…").to_string() } else { self.env_summary() })
                        .font(FontId::new(12.0, FontFamily::Monospace))
                        .color(TEXT2()),
                )
                .fill(BG2())
                .stroke(st(1.0, LINE()))
                .min_size(vec2(ui.available_width(), 28.0)),
            )
            .on_hover_text(t("在地址、请求头、请求体里写 {{名字}}，发送时换成这里的值"));
        if r.clicked() {
            self.api.env_open = !self.api.env_open;
        }
        ui.add_space(14.0);

        // ---- 收藏
        eyebrow(ui, t("已保存"), &self.s.api.saved.len().to_string());
        if self.s.api.saved.is_empty() {
            ui.label(RichText::new(t("编辑好请求后点「保存」，就会出现在这里")).small().color(TEXT3()));
        }
        let mut load = None;
        let mut del = None;
        let mut dup = None;
        let mut rename = None;
        let renaming = self.api.renaming.clone();
        for (i, r) in self.s.api.saved.iter().enumerate() {
            if let Some((ri, name)) = &renaming {
                if *ri == i {
                    let mut n = name.clone();
                    let resp = ui.add(egui::TextEdit::singleline(&mut n).desired_width(f32::INFINITY));
                    let fid = egui::Id::new("api_rename_focus");
                    if ui.memory_mut(|m| std::mem::take(m.data.get_temp_mut_or_default::<bool>(fid))) {
                        resp.request_focus();
                    }
                    if resp.lost_focus() {
                        if ui.input(|i| i.key_pressed(Key::Enter)) && !n.trim().is_empty() {
                            rename = Some((i, n.trim().to_string()));
                        }
                        self.api.renaming = None;
                    } else {
                        self.api.renaming = Some((i, n));
                    }
                    continue;
                }
            }
            let active = self.s.api.current.name == r.name;
            let resp = req_row(ui, &r.method, &r.title(), "", active);
            if resp.clicked() {
                load = Some(r.clone());
            }
            resp.context_menu(|ui| {
                if menu_item(ui, t("重命名…"), "") {
                    self.api.renaming = Some((i, r.name.clone()));
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("api_rename_focus"), true));
                    ui.close_menu();
                }
                if menu_item(ui, t("复制一份"), "") {
                    dup = Some(i);
                    ui.close_menu();
                }
                if menu_item(ui, t("删除"), "") {
                    del = Some(i);
                    ui.close_menu();
                }
            });
        }
        // 重命名确认（放在循环外改，避免借用冲突）
        if let Some((i, name)) = rename {
            if let Some(r) = self.s.api.saved.get_mut(i) {
                if self.s.api.current.name == r.name {
                    self.s.api.current.name = name.clone();
                }
                r.name = name;
            }
        }
        if let Some(i) = dup {
            let mut r = self.s.api.saved[i].clone();
            r.name = format!("{} 2", r.name);
            self.s.api.saved.insert(i + 1, r);
        }
        if let Some(i) = del {
            self.s.api.saved.remove(i);
        }
        if let Some(r) = load {
            self.api_load(r);
        }
        ui.add_space(14.0);

        // ---- 历史
        let n = self.s.api.history.len();
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("历史")).font(FontId::new(11.0, bold())).color(TEXT3()).extra_letter_spacing(1.5));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if n > 0 && ui.add(egui::Label::new(RichText::new(t("清空")).small().color(TEXT3())).sense(Sense::click())).clicked() {
                    self.s.api.history.clear();
                }
            });
        });
        ui.add_space(2.0);
        if n == 0 {
            ui.label(RichText::new(t("发送过的请求会记在这里")).small().color(TEXT3()));
        }
        let mut load = None;
        for h in self.s.api.history.iter().take(50) {
            let note = if h.status == 0 { "✗".to_string() } else { h.status.to_string() };
            let resp = req_row(ui, &h.req.method, &h.req.title(), &note, false).on_hover_text(format!("{}  ·  {} ms", h.at, h.ms));
            if resp.clicked() {
                load = Some(h.req.clone());
            }
        }
        if let Some(r) = load {
            self.api_load(r);
        }
    }

    fn env_summary(&self) -> String {
        let v: Vec<&str> = self.s.api.env.iter().filter(|e| e.on && !e.k.is_empty()).map(|e| e.k.as_str()).collect();
        truncate(&v.join("  "), 30)
    }

    // ------------------------------------------------------------ 主区域

    pub(super) fn api_page(&mut self, ui: &mut egui::Ui) {
        self.api_poll();
        let full = ui.available_rect_before_wrap();
        // Ctrl+Enter 发送
        if ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter)) {
            self.api_send();
        }
        self.api_url_bar(ui);
        ui.add_space(10.0);
        // 请求区高度可拖动
        let top = ui.cursor().top();
        let max_h = (full.bottom() - top - 160.0).max(120.0);
        let h = self.api.req_h.clamp(120.0, max_h);
        let req_rect = Rect::from_min_size(pos2(full.left(), top), vec2(full.width(), h));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(req_rect));
        self.api_request_tabs(&mut child);
        let split = Rect::from_min_size(pos2(full.left(), req_rect.bottom()), vec2(full.width(), 12.0));
        let sr = ui.interact(split, egui::Id::new("api_split"), Sense::drag());
        let lc = if sr.hovered() || sr.dragged() { ACCENT().gamma_multiply(0.6) } else { LINE() };
        ui.painter().hline(split.x_range(), split.center().y, st(1.0, lc));
        if sr.hovered() || sr.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if sr.dragged() {
            self.api.req_h = h + sr.drag_delta().y;
        }
        let resp_rect = Rect::from_min_max(pos2(full.left(), split.bottom()), full.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(resp_rect));
        self.api_response(&mut child);
        ui.advance_cursor_after_rect(full);
    }

    fn api_url_bar(&mut self, ui: &mut egui::Ui) {
        let busy = self.api.busy.is_some();
        ui.horizontal(|ui| {
            let cur = &mut self.s.api.current;
            let mc = method_color(&cur.method);
            egui::ComboBox::from_id_salt("api_method")
                .width(86.0)
                .selected_text(RichText::new(&cur.method).font(FontId::new(13.0, mono_bold())).color(mc))
                .show_ui(ui, |ui| {
                    for m in api::METHODS {
                        ui.selectable_value(&mut cur.method, m.to_string(), RichText::new(m).font(FontId::new(13.0, mono_bold())).color(method_color(m)));
                    }
                });
            let send_w = 84.0;
            let save_w = 70.0;
            let w = ui.available_width() - send_w - save_w - 16.0;
            let r = ui.add(
                egui::TextEdit::singleline(&mut cur.url)
                    .id(egui::Id::new("api_url"))
                    .font(FontId::new(13.5, FontFamily::Monospace))
                    .hint_text(t("地址，如 http://192.168.4.1/api/status 或 {{base}}/get"))
                    .desired_width(w)
                    .margin(vec2(8.0, 6.0)),
            );
            // 粘贴整条 curl 命令到地址栏：直接导入
            // 只在粘贴时导入（一个字一个字地打时会在半截时就被解析）；手打完按回车也会导入
            let pasted = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let is_curl = cur.url.trim_start().starts_with("curl ");
            if is_curl && ((r.changed() && pasted) || enter) {
                match api::from_curl(&cur.url) {
                    Ok(req) => {
                        // 导入的是一个新请求：不带原来的名字，免得保存时覆盖收藏
                        *cur = req;
                        self.toast(t("已从 cURL 导入"), OK());
                    }
                    Err(e) => self.toast(e, ERR()),
                }
            }
            if r.lost_focus() && !is_curl {
                api::absorb_query(&mut self.s.api.current);
            }
            if enter && !is_curl {
                self.api_send();
            }
            let label = if busy { t("取消") } else { t("发送") };
            let b = egui::Button::new(RichText::new(label).font(FontId::new(13.0, bold())).color(if busy { TEXT() } else { ON_ACCENT() }))
                .fill(if busy { BG3() } else { ACCENT() })
                .corner_radius(R_WIDGET)
                .min_size(vec2(send_w, 32.0));
            if ui.add(b).on_hover_text("Ctrl+Enter").clicked() {
                if busy {
                    self.api_cancel();
                } else {
                    self.api_send();
                }
            }
            let sb = egui::Button::new(RichText::new(t("保存")).size(12.5).color(TEXT2()))
                .fill(BG2())
                .stroke(st(1.0, LINE()))
                .min_size(vec2(save_w - 8.0, 32.0));
            let sr = ui.add(sb).on_hover_text(t("保存到左边的列表；已保存的请求直接覆盖"));
            let pop = egui::Id::new("api_save_pop");
            if sr.clicked() {
                let name = self.s.api.current.name.clone();
                if !name.is_empty() && self.s.api.saved.iter().any(|s| s.name == name) {
                    self.api_save(&name);
                } else {
                    self.api.save_name = Some(String::new());
                    ui.memory_mut(|m| {
                        m.open_popup(pop);
                        m.data.insert_temp(pop, true);
                    });
                }
            }
            egui::popup::popup_below_widget(ui, pop, &sr, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                ui.set_min_width(220.0);
                ui.label(RichText::new(t("名称")).small().color(TEXT3()));
                if let Some(n) = &mut self.api.save_name {
                    let r = ui.add(egui::TextEdit::singleline(n).hint_text(t("如：读取设备状态")).desired_width(f32::INFINITY));
                    // 只在刚打开时抢焦点（每帧都抢的话回车让出的焦点会被抢回来，保存不了）
                    if ui.memory_mut(|m| std::mem::take(m.data.get_temp_mut_or_default::<bool>(pop))) {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        let n = n.clone();
                        self.api.save_name = None;
                        self.api_save(&n);
                        ui.memory_mut(|m| m.close_popup());
                    }
                }
            });
        });
    }

    fn api_request_tabs(&mut self, ui: &mut egui::Ui) {
        let cur = &self.s.api.current;
        let np = cur.params.iter().filter(|p| p.on && !p.k.is_empty()).count();
        let nh = cur.headers.iter().filter(|p| p.on && !p.k.is_empty()).count();
        let lp = if np > 0 { format!("{} {np}", t("参数")) } else { t("参数").to_string() };
        let lh = if nh > 0 { format!("{} {nh}", t("请求头")) } else { t("请求头").to_string() };
        let lb = if cur.body_kind != BodyKind::None {
            format!("{} ●", t("请求体"))
        } else {
            t("请求体").to_string()
        };
        let la = if cur.auth != AuthKind::None { format!("{} ●", t("认证")) } else { t("认证").to_string() };
        ui.horizontal(|ui| {
            let w = ui.available_width();
            ui.allocate_ui(vec2((w - 170.0).min(420.0), 28.0), |ui| {
                let mut tab = self.api.req_tab;
                tab_strip(
                    ui,
                    &mut tab,
                    &[(ReqTab::Params, lp.as_str()), (ReqTab::Headers, lh.as_str()), (ReqTab::Body, lb.as_str()), (ReqTab::Auth, la.as_str())],
                    28.0,
                );
                self.api.req_tab = tab;
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let r = ui.add(
                    egui::Button::new(RichText::new("cURL ▾").size(12.0).color(TEXT2()))
                        .fill(Color32::TRANSPARENT)
                        .stroke(st(1.0, LINE()))
                        .min_size(vec2(0.0, 26.0)),
                );
                let pop = egui::Id::new("api_curl_pop");
                if r.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(pop));
                }
                egui::popup::popup_below_widget(ui, pop, &r, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                    ui.set_min_width(180.0);
                    if menu_item(ui, t("复制为 cURL"), "") {
                        let c = api::to_curl(&clean(&self.s.api.current), &self.s.api.env);
                        ui.ctx().copy_text(c);
                        self.toast(t("已复制到剪贴板"), OK());
                    }
                    if menu_item(ui, t("从 cURL 导入…"), "") {
                        self.api.curl_open = Some(String::new());
                    }
                    ui.separator();
                    if menu_item(ui, t("新建请求"), "") {
                        self.api_load(Request::default());
                    }
                });
            });
        });
        ui.add_space(8.0);
        egui::ScrollArea::vertical().id_salt("api_req").auto_shrink([false, false]).show(ui, |ui| {
            let env = self.s.api.env.clone();
            let cur = &mut self.s.api.current;
            match self.api.req_tab {
                ReqTab::Params => {
                    kv_table(ui, &mut cur.params, t("参数名"), t("值"));
                    let url = api::full_url(&clean(cur), &env);
                    if !url.is_empty() {
                        ui.add_space(6.0);
                        ui.add(egui::Label::new(RichText::new(url).font(FontId::new(11.5, FontFamily::Monospace)).color(TEXT3())).wrap());
                    }
                }
                ReqTab::Headers => {
                    kv_table(ui, &mut cur.headers, t("名称"), t("值"));
                    let auto: Vec<(String, String)> = api::effective_headers(&clean(cur), &env)
                        .into_iter()
                        .filter(|(k, _)| !cur.headers.iter().any(|h| h.on && h.k.eq_ignore_ascii_case(k)))
                        .collect();
                    if !auto.is_empty() {
                        ui.add_space(6.0);
                        ui.label(RichText::new(t("自动添加")).small().color(TEXT3()));
                        for (k, v) in auto {
                            ui.label(
                                RichText::new(format!("{k}: {}", if k.eq_ignore_ascii_case("authorization") { "••••••".to_string() } else { v }))
                                    .font(FontId::new(11.5, FontFamily::Monospace))
                                    .color(TEXT3()),
                            );
                        }
                    }
                }
                ReqTab::Body => {
                    ui.horizontal(|ui| {
                        for (k, name) in [(BodyKind::None, t("无")), (BodyKind::Json, "JSON"), (BodyKind::Form, t("表单")), (BodyKind::Text, t("文本"))] {
                            ui.radio_value(&mut cur.body_kind, k, name);
                        }
                        if cur.body_kind == BodyKind::Json {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(egui::Button::new(RichText::new(t("格式化")).size(12.0)).min_size(vec2(0.0, 24.0))).clicked() {
                                    match api::pretty_json(&cur.body) {
                                        Some(p) => cur.body = p,
                                        None => self.toast = Some((t("不是合法的 JSON").to_string(), ERR(), Instant::now())),
                                    }
                                }
                            });
                        }
                    });
                    ui.add_space(6.0);
                    match cur.body_kind {
                        BodyKind::None => {
                            ui.label(RichText::new(t("这个请求不带请求体")).small().color(TEXT3()));
                        }
                        BodyKind::Form => kv_table(ui, &mut cur.form, t("字段"), t("值")),
                        BodyKind::Json | BodyKind::Text => {
                            let json = cur.body_kind == BodyKind::Json;
                            let mut layouter = |ui: &egui::Ui, text: &str, wrap: f32| {
                                let mut job = if json {
                                    json_job(text, 13.0)
                                } else {
                                    LayoutJob::simple(text.to_owned(), FontId::new(13.0, FontFamily::Monospace), TEXT(), wrap)
                                };
                                job.wrap.max_width = wrap;
                                ui.fonts(|f| f.layout_job(job))
                            };
                            egui::Frame::new().fill(BG0()).stroke(st(1.0, LINE())).corner_radius(R_WIDGET).show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut cur.body)
                                        .font(FontId::new(13.0, FontFamily::Monospace))
                                        .code_editor()
                                        .frame(false)
                                        .layouter(&mut layouter)
                                        .desired_rows(6)
                                        .desired_width(f32::INFINITY)
                                        .margin(vec2(8.0, 6.0))
                                        .hint_text(if json { "{\n  \"key\": \"value\"\n}" } else { "" }),
                                );
                            });
                        }
                    }
                }
                ReqTab::Auth => {
                    ui.horizontal(|ui| {
                        for (k, name) in [(AuthKind::None, t("无")), (AuthKind::Bearer, "Bearer Token"), (AuthKind::Basic, "Basic Auth")] {
                            ui.radio_value(&mut cur.auth, k, name);
                        }
                    });
                    ui.add_space(8.0);
                    let field = |ui: &mut egui::Ui, name: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
                        ui.horizontal(|ui| {
                            ui.add_sized(vec2(64.0, 24.0), egui::Label::new(RichText::new(name).color(TEXT2())));
                            add(ui);
                        });
                    };
                    match cur.auth {
                        AuthKind::None => {
                            ui.label(RichText::new(t("不发送认证信息")).small().color(TEXT3()));
                        }
                        AuthKind::Bearer => field(ui, "Token", &mut |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut cur.token)
                                    .font(FontId::new(13.0, FontFamily::Monospace))
                                    .desired_width(420.0)
                                    .hint_text("{{token}}"),
                            );
                        }),
                        AuthKind::Basic => {
                            field(ui, t("用户名"), &mut |ui| {
                                ui.add(egui::TextEdit::singleline(&mut cur.user).desired_width(260.0));
                            });
                            ui.add_space(4.0);
                            field(ui, t("密码"), &mut |ui| {
                                ui.add(egui::TextEdit::singleline(&mut cur.pass).password(true).desired_width(260.0));
                            });
                        }
                    }
                    if cur.auth != AuthKind::None {
                        ui.add_space(6.0);
                        ui.label(RichText::new(t("保存在本机的设置文件里；敏感的值建议写成 {{变量}}")).small().color(TEXT3()));
                    }
                }
            }
        });
    }

    fn api_response(&mut self, ui: &mut egui::Ui) {
        if let Some(at) = self.api.busy {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.add(egui::Spinner::new().size(22.0).color(ACCENT()));
                ui.add_space(6.0);
                ui.label(RichText::new(tf("正在请求… {} s", &[&format!("{:.1}", at.elapsed().as_secs_f32())])).color(TEXT2()));
            });
            ui.ctx().request_repaint_after(Duration::from_millis(100));
            return;
        }
        let Some(res) = &self.api.resp else {
            let r = ui.available_rect_before_wrap();
            ui.painter().text(
                r.center_top() + vec2(0.0, 60.0),
                Align2::CENTER_CENTER,
                t("填好地址，点「发送」或按 Ctrl+Enter"),
                FontId::proportional(13.0),
                TEXT3(),
            );
            ui.painter().text(
                r.center_top() + vec2(0.0, 84.0),
                Align2::CENTER_CENTER,
                t("也可以把浏览器里「复制为 cURL」的命令直接粘贴到地址栏"),
                FontId::proportional(12.0),
                TEXT3(),
            );
            return;
        };
        let resp = match res {
            Err(e) => {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("请求失败")).font(FontId::new(13.0, bold())).color(ERR()));
                });
                ui.add(egui::Label::new(RichText::new(e).font(FontId::new(12.5, FontFamily::Monospace)).color(TEXT2())).wrap());
                return;
            }
            Ok(r) => r.clone(),
        };
        // ---- 状态行
        ui.horizontal(|ui| {
            let c = status_color(resp.status);
            let text = format!("{} {}", resp.status, if resp.status_text.is_empty() { api::status_name(resp.status) } else { &resp.status_text });
            egui::Frame::new()
                .fill(c.gamma_multiply(0.14))
                .stroke(st(1.0, c.gamma_multiply(0.45)))
                .corner_radius(R_WIDGET)
                .inner_margin(egui::Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.label(RichText::new(text).font(FontId::new(12.5, mono_bold())).color(c));
                });
            ui.label(RichText::new(format!("{} ms", resp.ms)).font(FontId::new(12.0, FontFamily::Monospace)).color(TEXT2()));
            ui.label(RichText::new(api::human_size(resp.body.len())).font(FontId::new(12.0, FontFamily::Monospace)).color(TEXT2()));
            if resp.truncated {
                ui.label(RichText::new(t("（太大，只保留了前 32 MB）")).small().color(WARN()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, Icon::Save, t("把响应体存成文件"), false).clicked() {
                    if let Some(p) = rfd::FileDialog::new().set_file_name(if resp.is_json() { "response.json" } else { "response.txt" }).save_file() {
                        match std::fs::write(&p, &resp.body) {
                            Ok(()) => self.toast(t("已保存"), OK()),
                            Err(e) => self.toast(e.to_string(), ERR()),
                        }
                    }
                }
                if icon_button(ui, Icon::Copy, t("复制响应体"), false).clicked() {
                    ui.ctx().copy_text(resp.text());
                    self.toast(t("已复制到剪贴板"), OK());
                }
                if self.api.resp_tab == RespTab::Body && resp.is_json() {
                    let was = self.api.pretty;
                    toggle_chip(ui, &mut self.api.pretty, t("格式化"), t("JSON 缩进显示"));
                    if was != self.api.pretty {
                        self.api.body_job = None;
                    }
                }
            });
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.allocate_ui(vec2(220.0, 26.0), |ui| {
                let lh = format!("{} {}", t("响应头"), resp.headers.len());
                tab_strip(ui, &mut self.api.resp_tab, &[(RespTab::Body, t("响应体")), (RespTab::Headers, lh.as_str())], 26.0);
            });
            if resp.url != api::full_url(&self.s.api.current, &self.s.api.env) {
                ui.label(RichText::new(format!("→ {}", resp.url)).small().color(TEXT3())).on_hover_text(t("跟随跳转后的最终地址"));
            }
        });
        ui.add_space(6.0);
        egui::Frame::new()
            .fill(BG0())
            .stroke(st(1.0, LINE()))
            .corner_radius(R_CARD)
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                egui::ScrollArea::both().id_salt("api_resp").auto_shrink([false, false]).show(ui, |ui| match self.api.resp_tab {
                    RespTab::Headers => {
                        egui::Grid::new("api_rh").num_columns(2).striped(true).spacing([18.0, 4.0]).show(ui, |ui| {
                            for (k, v) in &resp.headers {
                                ui.label(RichText::new(k).font(FontId::new(12.5, mono_bold())).color(ACCENT()));
                                ui.add(egui::Label::new(RichText::new(v).font(FontId::new(12.5, FontFamily::Monospace)).color(TEXT())).selectable(true));
                                ui.end_row();
                            }
                        });
                    }
                    RespTab::Body => {
                        if resp.body.is_empty() {
                            ui.label(RichText::new(t("（响应体为空）")).color(TEXT3()));
                            return;
                        }
                        let pretty = self.api.pretty;
                        if self.api.body_job.as_ref().is_none_or(|(p, _)| *p != pretty) {
                            self.api.body_job = Some((pretty, body_job(&resp, pretty)));
                        }
                        if let Some((_, job)) = &self.api.body_job {
                            ui.add(egui::Label::new(job.clone()).selectable(true).extend());
                        }
                    }
                });
            });
    }

    // ------------------------------------------------------------ 窗口

    pub(super) fn api_windows(&mut self, ctx: &egui::Context) {
        if self.api.env_open {
            let mut open = true;
            let env = &mut self.s.api.env;
            let (timeout, follow) = (&mut self.s.api.timeout_s, &mut self.s.api.follow_redirects);
            tool_window(ctx, t("环境变量"), "api_env", &mut open, 460.0, |ui| {
                ui.label(
                    RichText::new(t("在地址、参数、请求头、请求体、Token 里写 {{名字}}，发送时换成这里的值。比如开发板 IP 变了只改一处。"))
                        .small()
                        .color(TEXT3()),
                );
                ui.add_space(4.0);
                kv_table(ui, env, t("变量名"), t("值"));
                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("超时")).color(TEXT2()));
                    ui.add(egui::DragValue::new(timeout).range(1..=600).suffix(" s"));
                    ui.add_space(12.0);
                    ui.checkbox(follow, t("跟随跳转"));
                });
            });
            self.api.env_open = open;
        }
        if let Some(mut text) = self.api.curl_open.clone() {
            let mut open = true;
            let mut import = false;
            tool_window(ctx, t("从 cURL 导入"), "api_curl", &mut open, 520.0, |ui| {
                ui.label(RichText::new(t("粘贴 curl 命令（浏览器开发者工具里右键请求 →「复制为 cURL」）")).small().color(TEXT3()));
                ui.add(
                    egui::TextEdit::multiline(&mut text)
                        .font(FontId::new(12.5, FontFamily::Monospace))
                        .desired_rows(8)
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal(|ui| {
                    if ui.add_enabled(!text.trim().is_empty(), egui::Button::new(t("导入"))).clicked() {
                        import = true;
                    }
                });
            });
            if import {
                match api::from_curl(&text) {
                    Ok(r) => {
                        self.api_load(r);
                        self.toast(t("已从 cURL 导入"), OK());
                        open = false;
                    }
                    Err(e) => self.toast(e, ERR()),
                }
            }
            self.api.curl_open = if open { Some(text) } else { None };
        }
    }
}

/// 去掉表格末尾的空行（编辑用的占位行）
fn clean(r: &Request) -> Request {
    let mut r = r.clone();
    for v in [&mut r.params, &mut r.headers, &mut r.form] {
        v.retain(|kv| !kv.k.is_empty() || !kv.v.is_empty());
    }
    r
}

/// 侧边栏里一行请求：方法 + 名称 + 右侧小字
fn req_row(ui: &mut egui::Ui, method: &str, title: &str, note: &str, active: bool) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 28.0), Sense::click());
    let p = ui.painter();
    if active {
        p.rect_filled(rect, R_WIDGET, BG3());
    } else if resp.hovered() {
        p.rect_filled(rect, R_WIDGET, BG2());
    }
    let mfont = FontId::new(10.5, mono_bold());
    let short = match method {
        "DELETE" => "DEL",
        "OPTIONS" => "OPT",
        "PATCH" => "PATCH",
        m => m,
    };
    p.text(pos2(rect.left() + 6.0, rect.center().y), Align2::LEFT_CENTER, short, mfont, method_color(method));
    let x = rect.left() + 50.0;
    let right = if note.is_empty() { rect.right() - 6.0 } else { rect.right() - 36.0 };
    let max_chars = ((right - x) / 7.3).max(4.0) as usize;
    p.text(
        pos2(x, rect.center().y),
        Align2::LEFT_CENTER,
        truncate(title, max_chars),
        FontId::new(12.0, FontFamily::Monospace),
        if active { TEXT() } else { TEXT2() },
    );
    if !note.is_empty() {
        let c = note.parse::<u16>().map_or(ERR(), status_color);
        p.text(pos2(rect.right() - 6.0, rect.center().y), Align2::RIGHT_CENTER, note, FontId::new(10.5, FontFamily::Monospace), c);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text(title)
}

/// 键值表格：勾选、键、值、删除。最后总有一行空行，往里输入就自动加一行。
pub(super) fn kv_table(ui: &mut egui::Ui, rows: &mut Vec<Kv>, kh: &str, vh: &str) {
    if rows.last().is_none_or(|r| !r.k.is_empty() || !r.v.is_empty()) {
        rows.push(Kv { on: true, ..Default::default() });
    }
    let w = ui.available_width();
    let kw = ((w - 70.0) * 0.38).max(80.0);
    let vw = (w - 70.0 - kw).max(80.0);
    let mut del = None;
    // 不用 Grid：Grid 的单元格宽度取上一帧的列宽，输入框会一直缩在最小宽度
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        for (text, w) in [(kh, kw), (vh, vw)] {
            ui.allocate_ui_with_layout(vec2(w, 16.0), Layout::left_to_right(Align::Center), |ui| {
                ui.set_min_width(w);
                ui.add_space(4.0);
                ui.label(RichText::new(text).small().color(TEXT3()));
            });
        }
    });
    let n = rows.len();
    for (i, r) in rows.iter_mut().enumerate() {
        let last = i + 1 == n;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.add_enabled_ui(!last, |ui| ui.checkbox(&mut r.on, ""));
            let font = FontId::new(12.5, FontFamily::Monospace);
            ui.add(egui::TextEdit::singleline(&mut r.k).font(font.clone()).desired_width(kw).hint_text(if last { t("添加…") } else { "" }));
            ui.add(egui::TextEdit::singleline(&mut r.v).font(font).desired_width(vw).text_color(if r.on { TEXT() } else { TEXT3() }));
            if !last {
                let (rect, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                draw_icon(ui.painter(), rect, Icon::Close, if resp.hovered() { ERR() } else { TEXT3() });
                if resp.on_hover_text(t("删除")).clicked() {
                    del = Some(i);
                }
            }
        });
        ui.add_space(2.0);
    }
    if let Some(i) = del {
        rows.remove(i);
    }
}

pub(super) fn json_job(text: &str, size: f32) -> LayoutJob {
    let font = FontId::new(size, FontFamily::Monospace);
    let mut job = LayoutJob::default();
    let mut pos = 0;
    let fmt = |c: Color32| TextFormat {
        font_id: font.clone(),
        color: c,
        ..Default::default()
    };
    for (a, b, k) in api::json_tokens(text) {
        if a > pos {
            job.append(&text[pos..a], 0.0, fmt(TEXT()));
        }
        let c = match k {
            Tok::Key => ACCENT(),
            Tok::Str => ink(Color32::from_rgb(0x86, 0xef, 0xac)),
            Tok::Num => ink(Color32::from_rgb(0xfb, 0xbf, 0x24)),
            Tok::Lit => VIOLET(),
            Tok::Punct => TEXT3(),
        };
        job.append(&text[a..b], 0.0, fmt(c));
        pos = b;
    }
    if pos < text.len() {
        job.append(&text[pos..], 0.0, fmt(TEXT()));
    }
    job
}

/// 显示用的响应体：JSON 上色，太大的只显示开头
fn body_job(resp: &Response, pretty: bool) -> LayoutJob {
    const SHOW: usize = 2 * 1024 * 1024;
    let text = resp.text();
    let mut job = if resp.is_json() {
        let shown = if pretty { api::pretty_json(&text).unwrap_or(text) } else { text };
        let cut = cut_at(&shown, SHOW);
        let mut j = json_job(cut, 12.5);
        if cut.len() < shown.len() {
            j.append(
                &format!("\n\n{}", t("…… 只显示了前 2 MB，完整内容请复制或保存")),
                0.0,
                TextFormat { color: WARN(), ..Default::default() },
            );
        }
        j
    } else {
        let cut = cut_at(&text, SHOW);
        let mut j = LayoutJob::simple(cut.to_string(), FontId::new(12.5, FontFamily::Monospace), TEXT(), f32::INFINITY);
        if cut.len() < text.len() {
            j.append(
                &format!("\n\n{}", t("…… 只显示了前 2 MB，完整内容请复制或保存")),
                0.0,
                TextFormat { color: WARN(), ..Default::default() },
            );
        }
        j
    };
    job.wrap.max_width = f32::INFINITY;
    job
}

fn cut_at(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut i = n;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}
