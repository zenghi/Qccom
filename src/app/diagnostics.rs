use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui;

use crate::community::diagnostics::{self as community, Account, Delivery};
use crate::diagnostics::{self as local, Consent, Report, Store};
use crate::i18n::t;

const FIELDS: &str = "报告只含故障类型、QCCOM 版本、系统类型和处理器架构。不含日志、串口数据、文件路径、崩溃堆栈或密钥。";
const AUTH: &str = "服务器按普通社区发帖验证已有登录；账号令牌和设备标识不会写入帖子内容。";

pub(super) struct DiagnosticsUi {
    store: Option<Store>,
    consent: Option<Consent>,
    report: Option<(Report, bool)>,
    delivery: Option<Receiver<Delivery>>,
    auto_eligible: bool,
    auto_tried: bool,
    review: bool,
    hidden: bool,
    status: Option<&'static str>,
}

impl DiagnosticsUi {
    pub(super) fn new() -> Self {
        let store = local::store();
        let consent = store.as_ref().and_then(Store::consent);
        Self {
            auto_eligible: consent.is_some(),
            consent,
            report: store.as_ref().and_then(Store::previous),
            store,
            delivery: None,
            auto_tried: false,
            review: false,
            hidden: false,
            status: None,
        }
    }

    pub(super) fn available(&self) -> bool {
        self.store.is_some()
    }

    pub(super) fn has_report(&self) -> bool {
        self.report.is_some()
    }

    pub(super) fn review(&mut self) {
        self.review = true;
    }

    pub(super) fn automatic(&self) -> bool {
        self.consent.is_some()
    }

    pub(super) fn setting_description(&self, account: Option<&Account>) -> String {
        let paused = self.consent.is_some() && self.consent.as_ref() != account.map(|a| &a.consent);
        let audience = if paused {
            t("当前账号或服务器与保存的授权不符，不会自动发布。使用当前账号启用时，请先关闭再重新开启。").into()
        } else {
            account.map(community::visibility).unwrap_or_else(|| t("开启前请在社区页登录已有账号。").into())
        };
        format!(
            "{}
{}
{}
{}",
            t("默认关闭。开启后，下次启动时自动提交本机的故障报告。授权只适用于此窗口、此账号和此服务器。"),
            audience,
            t(FIELDS),
            t(AUTH),
        )
    }

    pub(super) fn set_automatic(&mut self, v: bool, account: Option<Account>) -> bool {
        let Some(store) = &self.store else { return false };
        let new = if v {
            let Some(account) = account else { return false };
            Some(account.consent)
        } else {
            None
        };
        if store.set_consent(new.as_ref()).is_err() {
            return false;
        }
        self.consent = new;
        // A new opt-in applies to subsequent launches only.
        self.auto_eligible = false;
        true
    }

    fn start(&mut self, ctx: &egui::Context, account: Account, automatic: bool) {
        if self.delivery.is_some() {
            return;
        }
        let Some(store) = self.store.clone() else { return };
        let Some((report, false)) = self.report.clone() else { return };
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.auto_tried = true;
        self.status = None;
        match std::thread::Builder::new().name("qccom-diagnostic-report".into()).spawn(move || {
            let delivery = community::publish(&store, &report, account, automatic);
            if matches!(delivery, Delivery::Submitted { .. }) {
                let _ = store.discard(true);
            }
            let _ = tx.send(delivery);
            ctx.request_repaint();
        }) {
            Ok(_) => self.delivery = Some(rx),
            Err(_) => self.status = Some(t("无法启动故障报告；本地记录仍保留。")),
        }
    }

    pub(super) fn tick(&mut self, ctx: &egui::Context) -> Option<(&'static str, bool)> {
        if let Some(delivery) = &self.delivery {
            let outcome = match delivery.try_recv() {
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => Delivery::LocalError,
                Ok(delivery) => delivery,
            };
            self.delivery = None;
            match outcome {
                Delivery::Submitted { moderated } => {
                    self.report = None;
                    self.review = false;
                    return Some((
                        if moderated {
                            t("故障报告已提交，审核后公开。")
                        } else {
                            t("故障报告已公开发布。")
                        },
                        false,
                    ));
                }
                Delivery::Deferred => {
                    self.status = Some(t("无法验证当前社区账号或服务器，报告仍仅保存在本机。"));
                }
                Delivery::CoolingDown => {
                    self.status = Some(t("每个窗口 24 小时内最多尝试自动发布一次，你仍可查看并手动发布。"));
                }
                Delivery::LocalError => {
                    self.report = self.store.as_ref().and_then(Store::previous);
                    self.status = Some(t("无法准备本地故障报告，没有发布。"));
                }
                Delivery::Uncertain => {
                    if let Some((_, attempted)) = &mut self.report {
                        *attempted = true;
                    }
                }
            }
            self.hidden = false;
        }
        if self.auto_eligible && !self.auto_tried && self.consent.is_some() && matches!(self.report, Some((_, false))) {
            if let Some(account) = Account::current().filter(|a| self.consent.as_ref() == Some(&a.consent)) {
                self.start(ctx, account, true);
            }
        }
        None
    }

    pub(super) fn banner(&mut self, ctx: &egui::Context) {
        let Some((_, attempted)) = &self.report else { return };
        if self.hidden {
            return;
        }
        let title = if self.delivery.is_some() {
            t("正在提交故障报告…")
        } else if *attempted {
            t("上次故障报告的发布状态未知，不会重试。")
        } else {
            t("发现上次运行的故障诊断，报告仍保存在本机。")
        };
        egui::TopBottomPanel::bottom("diagnostic-banner").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(title);
                if ui.button(t("查看故障报告")).clicked() {
                    self.review = true;
                }
                if ui.button(t("稍后")).clicked() {
                    self.hidden = true;
                }
            });
        });
    }

    pub(super) fn window(&mut self, ctx: &egui::Context) {
        if !self.review {
            return;
        }
        let Some((report, attempted)) = &self.report else { return };
        let account = Account::current();
        let busy = self.delivery.is_some();
        #[derive(Clone, Copy)]
        enum Action {
            Later,
            Publish,
            Discard,
        }
        let dialog = egui::Modal::new(egui::Id::new("diagnostic-review")).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.heading(t("查看故障报告"));
            ui.label(t(FIELDS));
            ui.label(t(AUTH));
            ui.separator();
            ui.strong(community::title(report));
            // Match the exact plain Markdown text sent to the existing forum API.
            egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                ui.monospace(community::body(report));
            });
            ui.separator();
            if *attempted {
                ui.label(t("上次故障报告的发布状态未知，不会重试。"));
            } else if let Some(account) = &account {
                ui.label(community::visibility(account));
            } else {
                ui.label(t("发布前请在社区页登录已有账号。"));
            }
            if busy {
                ui.label(t("正在提交故障报告…"));
            }
            if let Some(status) = self.status {
                ui.label(status);
            }
            let mut action = None;
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(!busy && !attempted && account.is_some(), egui::Button::new(t("确认公开发布"))).clicked() {
                    action = Some(Action::Publish);
                }
                if ui.add_enabled(!busy, egui::Button::new(t("丢弃本地报告"))).clicked() {
                    action = Some(Action::Discard);
                }
                if ui.button(t("稍后")).clicked() {
                    action = Some(Action::Later);
                }
            });
            action
        });
        let close = dialog.should_close();
        match dialog.inner {
            Some(Action::Publish) => {
                if let Some(account) = account {
                    self.start(ctx, account, false);
                }
            }
            Some(Action::Discard) => {
                if self.store.as_ref().is_some_and(|s| s.discard(*attempted).is_ok()) {
                    self.report = None;
                    self.status = None;
                    self.review = false;
                } else {
                    self.status = Some(t("无法删除本地故障报告。"));
                }
            }
            Some(Action::Later) => self.review = false,
            None if close => self.review = false,
            None => {}
        }
    }
}
