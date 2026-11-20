//! Publish an allow-listed client report through the existing community Help board.
//! Never register a new device, log in, attach logs, or retry a possibly delivered POST.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::diagnostics::{Consent, Report, Store};
use crate::hub::{self, Body, Creds, HubErr};
use crate::i18n::{t, tf};

pub struct Account {
    pub consent: Consent,
    pub label: String,
    creds: Creds,
}

impl Account {
    pub fn current() -> Option<Self> {
        let creds = hub::creds();
        let user = creds.user.as_ref()?;
        if creds.device_id.is_empty() || creds.device_token.is_empty() || creds.session.is_empty() {
            return None;
        }
        let consent = Consent::new(user["id"].as_i64()?, hub::base_url())?;
        let label = user["nickname"].as_str().filter(|s| !s.is_empty()).or_else(|| user["username"].as_str())?.to_string();
        Some(Self { consent, label, creds })
    }

    fn still_signed_in(&self) -> bool {
        let current = hub::creds();
        current.session == self.creds.session && current.device_id == self.creds.device_id && current.device_token == self.creds.device_token
    }
}

pub fn title(report: &Report) -> String {
    let version = report.version_string();
    format!("{} v{version}", t("QCCOM 故障诊断"))
}

pub fn body(report: &Report) -> String {
    format!(
        "- {}: `{}`\n- {}: `{}`\n- {}: `{}`\n- {}: `{}`",
        t("故障类型"),
        report.event.code(),
        t("QCCOM 版本"),
        report.version_string(),
        t("系统类型"),
        report.os,
        t("处理器架构"),
        report.arch,
    )
}

pub fn visibility(account: &Account) -> String {
    tf("将用社区账号「{}」向 {} 的求助区发布公开帖子。", &[&account.label, &account.consent.server])
}

#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    Submitted { moderated: bool },
    Deferred,
    CoolingDown,
    LocalError,
    Uncertain,
}

pub fn publish(store: &Store, report: &Report, account: Account, automatic: bool) -> Delivery {
    if automatic && !store.auto_ready(SystemTime::now()) {
        return Delivery::CoolingDown;
    }
    // No redirects: account/device authentication must stay at the named destination.
    let Ok(builder) = crate::update::agent_builder() else {
        return Delivery::Deferred;
    };
    let server = &account.consent.server;
    let client = builder.redirects(0).timeout(Duration::from_secs(15)).max_idle_connections(0).build();
    publish_using(
        report,
        account.consent.account,
        || account.still_signed_in() && (!automatic || store.consent().as_ref() == Some(&account.consent)),
        || {
            if store.claim(report).is_err() {
                return false;
            }
            if automatic && store.mark_auto_attempt().is_err() {
                let _ = store.release_claim(report);
                return false;
            }
            true
        },
        || store.release_claim(report).is_ok(),
        |method, path, request_body| hub::request_as(&client, method, path, request_body, &account.creds, server),
    )
}

fn publish_using(
    report: &Report,
    account: i64,
    allowed: impl Fn() -> bool,
    claim: impl FnOnce() -> bool,
    release_claim: impl FnOnce() -> bool,
    mut request: impl FnMut(&str, &str, Body) -> Result<Value, HubErr>,
) -> Delivery {
    // Verify the existing session independently. A cached user is not proof that
    // the authenticated session remains valid, and an anonymous response is not enough.
    if !allowed() {
        return Delivery::Deferred;
    }
    let Ok(me) = request("GET", "/me", Body::None) else {
        return Delivery::Deferred;
    };
    if me["user"]["id"].as_i64() != Some(account) || me["banned"].as_bool() == Some(true) || !allowed() {
        return Delivery::Deferred;
    }
    let post = json!({
        "board": "help",
        "title": title(report),
        "body": body(report),
        "anonymous": false,
    });
    if !claim() {
        return Delivery::LocalError;
    }
    if !allowed() {
        // Turning off consent or signing out can happen during the disk sync.
        return if release_claim() { Delivery::Deferred } else { Delivery::LocalError };
    }
    match request("POST", "/posts", Body::Json(post)) {
        Ok(v) if v["id"].as_i64().is_some_and(|id| id > 0) => Delivery::Submitted {
            moderated: v["pending"].as_bool().unwrap_or(false),
        },
        // Even an HTTP error can follow a successful write. Do not resend.
        _ => Delivery::Uncertain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Event;
    use std::cell::Cell;

    fn report() -> Report {
        Report {
            event: Event::Panic,
            version: [1, 9, 8],
            os: "linux",
            arch: "x86_64",
        }
    }

    #[test]
    fn disabled_or_mismatched_consent_has_no_post() {
        assert_eq!(
            publish_using(
                &report(),
                42,
                || false,
                || panic!("claimed without consent"),
                || panic!("released without claiming"),
                |_, _, _| panic!("accessed network without consent"),
            ),
            Delivery::Deferred
        );
        assert_eq!(
            publish_using(
                &report(),
                42,
                || true,
                || panic!("claimed without matching auth"),
                || panic!("released without claiming"),
                |method, path, _| {
                    assert_eq!((method, path), ("GET", "/me"));
                    Ok(json!({"user":{"id":17}}))
                }
            ),
            Delivery::Deferred
        );
    }

    #[test]
    fn revoked_while_claiming_releases_without_posting() {
        let allowed = Cell::new(true);
        let released = Cell::new(false);
        let d = publish_using(
            &report(),
            42,
            || allowed.get(),
            || {
                allowed.set(false);
                true
            },
            || {
                released.set(true);
                true
            },
            |method, path, _| {
                assert_eq!((method, path), ("GET", "/me"));
                Ok(json!({"user":{"id":42}}))
            },
        );
        assert_eq!(d, Delivery::Deferred);
        assert!(released.get());
    }

    #[test]
    fn accepts_existing_moderation_response() {
        assert_eq!(
            publish_using(
                &report(),
                42,
                || true,
                || true,
                || panic!("released after valid claim"),
                |method, _, _| { Ok(if method == "GET" { json!({"user":{"id":42}}) } else { json!({"id":12,"pending":true}) }) },
            ),
            Delivery::Submitted { moderated: true }
        );
    }

    #[test]
    fn payload_contains_only_bounded_report_and_never_retries() {
        let claimed = Cell::new(false);
        let count = Cell::new(0);
        let r = report();
        let d = publish_using(
            &r,
            42,
            || true,
            || {
                claimed.set(true);
                true
            },
            || panic!("released despite attempting POST"),
            |method, path, data| {
                count.set(count.get() + 1);
                if method == "GET" {
                    assert_eq!(path, "/me");
                    assert!(!claimed.get());
                    return Ok(json!({"user":{"id":42,"username":"private-account-name"},"device":{"id":"private-device-id"}}));
                }
                assert!(claimed.get());
                assert_eq!((method, path), ("POST", "/posts"));
                let Body::Json(v) = data else { panic!("expected JSON") };
                assert_eq!(v.as_object().unwrap().len(), 4);
                assert_eq!(v["board"], "help");
                assert_eq!(v["anonymous"], false);
                assert!(v["title"].as_str().unwrap().contains("v1.9.8"));
                let body = v["body"].as_str().unwrap();
                assert_eq!(body.lines().count(), 4);
                for field in ["`rust-panic`", "`1.9.8`", "`linux`", "`x86_64`"] {
                    assert!(body.contains(field));
                }
                assert!(v.to_string().len() < 512);
                assert!(!v.to_string().contains("private-"));
                Err(HubErr {
                    code: "network".into(),
                    msg: "network unavailable".into(),
                })
            },
        );
        assert_eq!(count.get(), 2);
        assert_eq!(d, Delivery::Uncertain);
    }
}
