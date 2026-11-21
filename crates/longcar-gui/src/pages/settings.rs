// SPDX-License-Identifier: GPL-3.0-or-later

//! Settings page: daemon settings, extension install/update and maintenance.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use longcar_core::model::ScreenState;
use longcar_core::state::StateDoc;
use serde_json::json;

use crate::client::Ctx;
use crate::pages::doctor::DoctorPage;
use crate::util;

#[derive(Clone)]
pub struct SettingsPage {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    root: adw::PreferencesPage,
    syncing: Cell<bool>,
    autostart: adw::SwitchRow,
    notifications: adw::SwitchRow,
    idle: adw::SpinRow,
    restore_layout: adw::SwitchRow,
    preset_dropdown: gtk::DropDown,
    preset_ids: RefCell<Vec<String>>,
    audio_master: adw::SwitchRow,
    audio_list: gtk::ListBox,
    audio_signature: RefCell<String>,
    doctor: Rc<DoctorPage>,
    doctor_loaded: Cell<bool>,
    default_preset_row: adw::ActionRow,
    extension_row: adw::ActionRow,
    daemon_row: adw::ActionRow,
    logs_row: adw::ActionRow,
}

impl SettingsPage {
    pub fn new(ctx: Ctx) -> Rc<Self> {
        let builder = gtk::Builder::from_resource("/io/github/ta4anx/Longcar/ui/settings_page.ui");
        let root: adw::PreferencesPage = builder.object("settings_page").expect("settings_page");
        let autostart: adw::SwitchRow = builder
            .object("autostart_switch")
            .expect("autostart switch");
        let notifications: adw::SwitchRow = builder
            .object("notifications_switch")
            .expect("notifications switch");
        let idle: adw::SpinRow = builder.object("idle_spin").expect("idle spin");
        let restore_layout: adw::SwitchRow = builder
            .object("restore_layout_switch")
            .expect("restore layout switch");
        let default_preset_row: adw::ActionRow = builder
            .object("default_preset_row")
            .expect("default preset row");
        let audio_master: adw::SwitchRow = builder
            .object("audio_master_switch")
            .expect("audio master switch");
        let audio_list: gtk::ListBox = builder
            .object("audio_screens_list")
            .expect("audio screens list");
        audio_list.add_css_class("boxed-list");
        let extension_row: adw::ActionRow = builder.object("extension_row").expect("extension row");
        let daemon_row: adw::ActionRow = builder.object("daemon_row").expect("daemon row");
        let logs_row: adw::ActionRow = builder.object("logs_row").expect("logs row");

        let preset_dropdown = gtk::DropDown::from_strings(&[]);
        preset_dropdown.set_valign(gtk::Align::Center);
        preset_dropdown.set_tooltip_text(Some("Preset started by StartDefault"));
        default_preset_row.add_suffix(&preset_dropdown);
        default_preset_row.set_activatable_widget(Some(&preset_dropdown));

        let doctor = DoctorPage::new(ctx.clone());
        let diagnostics = adw::PreferencesGroup::builder()
            .title("Diagnostics")
            .description("Environment checks, fixes and the display self-test")
            .build();
        diagnostics.add(doctor.root());
        root.add(&diagnostics);

        let page = Rc::new(Self {
            inner: Rc::new(Inner {
                ctx,
                root,
                syncing: Cell::new(false),
                autostart,
                notifications,
                idle,
                restore_layout,
                preset_dropdown,
                preset_ids: RefCell::new(Vec::new()),
                audio_master,
                audio_list,
                audio_signature: RefCell::new(String::new()),
                doctor,
                doctor_loaded: Cell::new(false),
                default_preset_row,
                extension_row,
                daemon_row,
                logs_row,
            }),
        });

        page.connect();
        page
    }

    pub fn root(&self) -> &gtk::Widget {
        self.inner.root.upcast_ref()
    }

    /// The embedded diagnostics widget, used by the screenshot hook.
    pub fn diagnostics_widget(&self) -> &gtk::Widget {
        self.inner.doctor.root()
    }

    fn connect(&self) {
        {
            let inner = self.inner.clone();
            self.inner.autostart.connect_active_notify(move |row| {
                if inner.syncing.get() {
                    return;
                }
                send_setting(
                    &inner.ctx,
                    json!({ "autostartScreens": row.is_active() }),
                    "Autostart updated",
                );
            });
        }
        {
            let inner = self.inner.clone();
            self.inner.notifications.connect_active_notify(move |row| {
                if inner.syncing.get() {
                    return;
                }
                send_setting(
                    &inner.ctx,
                    json!({ "notifications": row.is_active() }),
                    "Notifications updated",
                );
            });
        }
        {
            let inner = self.inner.clone();
            self.inner.idle.connect_value_notify(move |row| {
                if inner.syncing.get() {
                    return;
                }
                send_setting(
                    &inner.ctx,
                    json!({ "stopWhenIdleMinutes": row.value() as u32 }),
                    "Idle timeout updated",
                );
            });
        }
        {
            let inner = self.inner.clone();
            self.inner.restore_layout.connect_active_notify(move |row| {
                if inner.syncing.get() {
                    return;
                }
                send_setting(
                    &inner.ctx,
                    json!({ "restoreLayoutOnStop": row.is_active() }),
                    "Layout restore preference updated",
                );
            });
        }
        {
            let inner = self.inner.clone();
            self.inner
                .preset_dropdown
                .connect_selected_notify(move |dropdown| {
                    if inner.syncing.get() {
                        return;
                    }
                    let id = inner
                        .preset_ids
                        .borrow()
                        .get(dropdown.selected() as usize)
                        .cloned();
                    if let Some(id) = id.filter(|id| !id.is_empty()) {
                        send_setting(
                            &inner.ctx,
                            json!({ "defaultPreset": id }),
                            "Default preset updated",
                        );
                    }
                });
        }
        {
            let inner = self.inner.clone();
            self.inner.audio_master.connect_active_notify(move |row| {
                if inner.syncing.get() {
                    return;
                }
                let enabled = row.is_active();
                let Some(state) = inner.ctx.state() else {
                    return;
                };
                let targets: Vec<String> = state
                    .screens
                    .iter()
                    .filter(|screen| screen.audio != enabled)
                    .map(|screen| screen.id.clone())
                    .collect();
                if targets.is_empty() {
                    return;
                }
                let Some(client) = inner.ctx.client() else {
                    inner.ctx.toast("The Longcar daemon is unavailable");
                    revert_master(&inner);
                    return;
                };
                let inner = inner.clone();
                glib::spawn_future_local(async move {
                    if !enabled {
                        match client.set_audio_screen("").await {
                            Ok(()) => {
                                inner.ctx.toast("Audio disabled for every screen");
                            }
                            Err(err) => {
                                inner.ctx.toast_err("Could not update audio routing", &err);
                                revert_master(&inner);
                            }
                        }
                        return;
                    }
                    for id in targets {
                        if let Err(err) = client.set_screen_audio(&id, enabled).await {
                            inner.ctx.toast_err("Could not update audio routing", &err);
                            revert_master(&inner);
                            return;
                        }
                    }
                    inner.ctx.toast("Audio enabled for every screen");
                });
            });
        }
        {
            let ctx = self.inner.ctx.clone();
            let install =
                util::text_button("Install or update", "Install the GNOME Shell extension");
            install.connect_clicked(move |button| {
                let Some(client) = ctx.client() else {
                    ctx.toast("The Longcar daemon is unavailable");
                    return;
                };
                button.set_sensitive(false);
                let ctx = ctx.clone();
                let button = button.clone();
                glib::spawn_future_local(async move {
                    match client.run_doctor_fix("install-extension").await {
                        Ok(detail) => ctx.toast(&detail),
                        Err(err) => ctx.toast_err("Extension install failed", &err),
                    }
                    button.set_sensitive(true);
                });
            });
            self.inner.extension_row.add_suffix(&install);
        }
        {
            let ctx = self.inner.ctx.clone();
            let restart = util::text_button("Restart", "Restart the Longcar daemon");
            restart.connect_clicked(move |_| {
                let argv = ["systemctl", "--user", "restart", "longcar-daemon.service"];
                let args: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
                match gio::Subprocess::newv(&args, gio::SubprocessFlags::NONE) {
                    Ok(_) => ctx.toast("Restarting the Longcar daemon…"),
                    Err(err) => ctx.toast_err("Could not restart the daemon", &err.to_string()),
                }
            });
            self.inner.daemon_row.add_suffix(&restart);
        }
        {
            let ctx = self.inner.ctx.clone();
            let open = util::text_button("Show", "Show recent daemon and screen logs");
            open.connect_clicked(move |_| (ctx.open_logs)());
            self.inner.logs_row.add_suffix(&open);
        }
    }

    pub fn update(&self, state: &StateDoc) {
        self.inner.syncing.set(true);
        self.inner
            .autostart
            .set_active(state.settings.autostart_screens);
        self.inner
            .notifications
            .set_active(state.settings.notifications);
        self.inner
            .idle
            .set_value(f64::from(state.settings.stop_when_idle_minutes));
        self.inner
            .restore_layout
            .set_active(state.settings.restore_layout_on_stop);

        // Default preset dropdown.
        let mut preset_ids = vec![String::new()];
        let mut preset_labels = vec!["No default preset".to_string()];
        for preset in &state.presets {
            preset_ids.push(preset.id.clone());
            preset_labels.push(preset.name.clone());
        }
        let labels: Vec<&str> = preset_labels.iter().map(String::as_str).collect();
        self.inner
            .preset_dropdown
            .set_model(Some(&gtk::StringList::new(&labels)));
        let preset_index = preset_ids
            .iter()
            .position(|id| *id == state.settings.default_preset)
            .unwrap_or(0);
        self.inner.preset_dropdown.set_selected(preset_index as u32);
        *self.inner.preset_ids.borrow_mut() = preset_ids;

        // Audio master switch and per-screen routing.
        let streaming_any = state
            .screens
            .iter()
            .any(|screen| screen.state == ScreenState::Streaming);
        self.inner.audio_master.set_active(
            !state.screens.is_empty() && state.screens.iter().all(|screen| screen.audio),
        );
        self.inner.audio_master.set_sensitive(!streaming_any);
        self.inner
            .audio_master
            .set_tooltip_text(Some(if streaming_any {
                "Unavailable while a screen is streaming"
            } else {
                "Turns audio on or off for every screen"
            }));
        rebuild_audio_rows(&self.inner, state);
        self.inner
            .default_preset_row
            .set_sensitive(!state.presets.is_empty());

        self.inner.syncing.set(false);

        if !self.inner.doctor_loaded.get() {
            self.inner.doctor_loaded.set(true);
            self.inner.doctor.activate();
        }
    }
}

fn revert_master(inner: &Rc<Inner>) {
    let active = inner.ctx.state().is_some_and(|state| {
        !state.screens.is_empty() && state.screens.iter().all(|screen| screen.audio)
    });
    inner.syncing.set(true);
    inner.audio_master.set_active(active);
    inner.syncing.set(false);
}

fn rebuild_audio_rows(inner: &Rc<Inner>, state: &StateDoc) {
    let signature = state
        .screens
        .iter()
        .map(|screen| {
            format!(
                "{}|{}|{}|{}",
                screen.id,
                screen.name,
                screen.audio,
                screen.state == ScreenState::Streaming
            )
        })
        .collect::<Vec<_>>()
        .join("\u{1f}");
    if *inner.audio_signature.borrow() == signature {
        return;
    }
    *inner.audio_signature.borrow_mut() = signature;

    while let Some(child) = inner.audio_list.first_child() {
        inner.audio_list.remove(&child);
    }
    if state.screens.is_empty() {
        inner
            .audio_list
            .append(&util::dim_label("No screens are configured"));
        return;
    }
    for screen in &state.screens {
        let streaming = screen.state == ScreenState::Streaming;
        let row = adw::SwitchRow::builder()
            .title(format!("{} audio", screen.name))
            .subtitle(if streaming {
                "Unavailable while this screen is streaming"
            } else {
                "Stream this screen's audio to the client"
            })
            .active(screen.audio)
            .sensitive(!streaming)
            .build();
        {
            let inner = inner.clone();
            let id = screen.id.clone();
            let switch_row = row.clone();
            row.connect_active_notify(move |row| {
                if inner.syncing.get() {
                    return;
                }
                let enabled = row.is_active();
                let Some(client) = inner.ctx.client() else {
                    inner.ctx.toast("The Longcar daemon is unavailable");
                    revert_screen_audio(&inner, &switch_row, &id);
                    return;
                };
                let inner = inner.clone();
                let switch_row = switch_row.clone();
                let id = id.clone();
                glib::spawn_future_local(async move {
                    if let Err(err) = client.set_screen_audio(&id, enabled).await {
                        inner.ctx.toast_err("Could not change audio routing", &err);
                        revert_screen_audio(&inner, &switch_row, &id);
                    }
                });
            });
        }
        inner.audio_list.append(&row);
    }
}

fn revert_screen_audio(inner: &Rc<Inner>, row: &adw::SwitchRow, id: &str) {
    let active = inner
        .ctx
        .state()
        .and_then(|state| state.screens.into_iter().find(|screen| screen.id == id))
        .map(|screen| screen.audio)
        .unwrap_or(false);
    inner.syncing.set(true);
    row.set_active(active);
    inner.syncing.set(false);
}

fn send_setting(ctx: &Ctx, payload: serde_json::Value, success: &str) {
    let Some(client) = ctx.client() else {
        ctx.toast("The Longcar daemon is unavailable");
        return;
    };
    let payload = payload.to_string();
    let ctx = ctx.clone();
    let success = success.to_string();
    glib::spawn_future_local(async move {
        match client.update_settings(&payload).await {
            Ok(()) => ctx.toast(&success),
            Err(err) => ctx.toast_err("Could not update settings", &err),
        }
    });
}
