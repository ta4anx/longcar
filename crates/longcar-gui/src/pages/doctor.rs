// SPDX-License-Identifier: GPL-3.0-or-later

//! Doctor page: environment checks, fixes, display self-test and diagnostics.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use serde::Deserialize;

use crate::client::Ctx;
use crate::util;

#[derive(Debug, Clone, Deserialize)]
struct DoctorDoc {
    #[serde(default)]
    checks: Vec<DoctorCheck>,
}

#[derive(Debug, Clone, Deserialize)]
struct DoctorCheck {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    fix: Option<String>,
}

#[derive(Clone)]
pub struct DoctorPage {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    root: gtk::Box,
    checks: gtk::ListBox,
    detail: gtk::Label,
    refresh_button: gtk::Button,
    test: gtk::Button,
    copy: gtk::Button,
    doctor_json: RefCell<String>,
    signature: RefCell<String>,
}

impl DoctorPage {
    pub fn new(ctx: Ctx) -> Rc<Self> {
        let builder = gtk::Builder::from_resource("/io/github/ta4anx/Longcar/ui/doctor_page.ui");
        let root: gtk::Box = builder.object("doctor_page").expect("doctor_page");
        let checks: gtk::ListBox = builder.object("doctor_checks").expect("doctor checks");
        checks.add_css_class("boxed-list");
        let detail: gtk::Label = builder.object("doctor_detail").expect("doctor detail");
        let refresh_button: gtk::Button = builder.object("refresh_doctor_button").expect("refresh");
        let test: gtk::Button = builder.object("display_test_button").expect("display test");
        let copy: gtk::Button = builder.object("copy_doctor_button").expect("copy doctor");

        let page = Rc::new(Self {
            inner: Rc::new(Inner {
                ctx,
                root,
                checks,
                detail,
                refresh_button,
                test,
                copy,
                doctor_json: RefCell::new(String::new()),
                signature: RefCell::new(String::new()),
            }),
        });

        {
            let inner = page.inner.clone();
            page.inner.refresh_button.connect_clicked(move |_| {
                refresh_doctor(&inner);
            });
        }
        {
            let inner = page.inner.clone();
            page.inner.test.connect_clicked(move |button| {
                let Some(client) = inner.ctx.client() else {
                    inner.ctx.toast("The Longcar daemon is unavailable");
                    return;
                };
                button.set_sensitive(false);
                inner
                    .detail
                    .set_text("Creating a temporary virtual display…");
                let inner = inner.clone();
                let button = button.clone();
                glib::spawn_future_local(async move {
                    match client.doctor_display_test().await {
                        Ok((true, detail)) => {
                            inner.detail.set_text(&detail);
                            inner.ctx.toast("Display self-test passed");
                        }
                        Ok((false, detail)) => {
                            inner.detail.set_text(&detail);
                            inner.ctx.toast("Display self-test failed");
                        }
                        Err(err) => {
                            inner.detail.set_text(&format!("Self-test failed: {err}"));
                            inner.ctx.toast_err("Display self-test failed", &err);
                        }
                    }
                    button.set_sensitive(true);
                });
            });
        }
        {
            let inner = page.inner.clone();
            page.inner.copy.connect_clicked(move |_| {
                let json = inner.doctor_json.borrow().clone();
                if json.is_empty() {
                    inner.ctx.toast("Run the checks first");
                } else {
                    inner.ctx.copy(&util::redact(&json), "Diagnostics");
                }
            });
        }

        page
    }

    pub fn root(&self) -> &gtk::Widget {
        self.inner.root.upcast_ref()
    }

    /// Doctor data is only fetched on demand: gathering it runs subprocess checks.
    pub fn activate(&self) {
        refresh_doctor(&self.inner);
    }
}

fn refresh_doctor(inner: &Rc<Inner>) {
    let Some(client) = inner.ctx.client() else {
        inner.detail.set_text("The Longcar daemon is unavailable");
        return;
    };
    inner.refresh_button.set_sensitive(false);
    let weak = Rc::downgrade(inner);
    glib::spawn_future_local(async move {
        let result = client.get_doctor().await;
        let Some(this) = weak.upgrade() else {
            return;
        };
        this.refresh_button.set_sensitive(true);
        match result {
            Ok(json) => {
                *this.doctor_json.borrow_mut() = json.clone();
                match serde_json::from_str::<DoctorDoc>(&json) {
                    Ok(doc) => render_doctor(&this, &doc),
                    Err(err) => this
                        .detail
                        .set_text(&format!("Malformed doctor output: {err}")),
                }
            }
            Err(err) => this
                .detail
                .set_text(&format!("Could not read doctor checks: {err}")),
        }
    });
}

fn render_doctor(inner: &Rc<Inner>, doc: &DoctorDoc) {
    let signature = doc
        .checks
        .iter()
        .map(|check| {
            format!(
                "{}|{}|{}|{}",
                check.id,
                check.title,
                check.status,
                check.fix.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\u{1f}");
    if *inner.signature.borrow() == signature {
        return;
    }
    *inner.signature.borrow_mut() = signature;
    while let Some(child) = inner.checks.first_child() {
        inner.checks.remove(&child);
    }
    if doc.checks.is_empty() {
        inner
            .checks
            .append(&util::dim_label("The daemon reported no checks"));
        return;
    }
    let (ok, warnings, errors) =
        doc.checks
            .iter()
            .fold((0, 0, 0), |counts, check| match check.status.as_str() {
                "ok" => (counts.0 + 1, counts.1, counts.2),
                "error" => (counts.0, counts.1, counts.2 + 1),
                _ => (counts.0, counts.1 + 1, counts.2),
            });
    inner
        .detail
        .set_text(&format!("{ok} ok · {warnings} warnings · {errors} errors"));

    for check in &doc.checks {
        let row = adw::ActionRow::builder()
            .title(&check.title)
            .subtitle(&check.detail)
            .build();
        let (icon_name, css_class, tooltip) = match check.status.as_str() {
            "ok" => ("emblem-ok-symbolic", "success", "Passing check"),
            "error" => ("dialog-error-symbolic", "error", "Failing check"),
            _ => ("dialog-warning-symbolic", "warning", "Warning"),
        };
        let icon = gtk::Image::from_icon_name(icon_name);
        icon.add_css_class(css_class);
        icon.set_tooltip_text(Some(tooltip));
        row.add_prefix(&icon);

        if let Some(fix) = &check.fix {
            let fix_button = util::text_button("Fix", "Run the suggested repair");
            let ctx = inner.ctx.clone();
            let weak = Rc::downgrade(inner);
            let fix_id = fix.clone();
            fix_button.connect_clicked(move |button| {
                let destructive =
                    fix_id.starts_with("reset-instance") || fix_id.starts_with("reauthorize");
                if destructive {
                    let alert = adw::AlertDialog::new(
                        Some("Run this fix?"),
                        Some(
                            "This resets the screen's Sunshine instance or portal authorization. Pairing data may be lost.",
                        ),
                    );
                    alert.add_response("cancel", "Cancel");
                    alert.add_response("run", "Run fix");
                    alert.set_response_appearance("run", adw::ResponseAppearance::Destructive);
                    let alert_ctx = ctx.clone();
                    let weak = weak.clone();
                    let fix_id = fix_id.clone();
                    let button = button.clone();
                    alert.connect_response(None, move |_, response| {
                        if response == "run" {
                            run_fix(&alert_ctx, &weak, &button, &fix_id);
                        }
                    });
                    alert.present(Some(&ctx.window));
                } else {
                    run_fix(&ctx, &weak, button, &fix_id);
                }
            });
            row.add_suffix(&fix_button);
        }
        inner.checks.append(&row);
    }
}

fn run_fix(ctx: &Ctx, weak: &std::rc::Weak<Inner>, button: &gtk::Button, fix_id: &str) {
    let Some(client) = ctx.client() else {
        ctx.toast("The Longcar daemon is unavailable");
        return;
    };
    button.set_sensitive(false);
    let ctx = ctx.clone();
    let weak = weak.clone();
    let button = button.clone();
    let fix_id = fix_id.to_string();
    glib::spawn_future_local(async move {
        match client.run_doctor_fix(&fix_id).await {
            Ok(detail) => {
                ctx.toast(&detail);
                if let Some(inner) = weak.upgrade() {
                    refresh_doctor(&inner);
                }
            }
            Err(err) => {
                ctx.toast_err("Fix failed", &err);
                button.set_sensitive(true);
            }
        }
    });
}
