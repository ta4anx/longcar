// SPDX-License-Identifier: GPL-3.0-or-later

//! Presets page: start a preset, create, edit, delete and set the default.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use longcar_core::config::{Preset, slugify};
use longcar_core::state::{PresetEntry, StateDoc};
use serde_json::json;

use crate::client::Ctx;
use crate::util;

#[derive(Clone)]
pub struct PresetsPage {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    root: gtk::Box,
    list: gtk::ListBox,
    signature: RefCell<String>,
    rows: RefCell<Vec<(String, adw::ActionRow)>>,
}

impl PresetsPage {
    pub fn new(ctx: Ctx) -> Rc<Self> {
        let builder = gtk::Builder::from_resource("/io/github/ta4anx/Longcar/ui/presets_page.ui");
        let root: gtk::Box = builder.object("presets_page").expect("presets_page");
        let list: gtk::ListBox = builder.object("presets_list").expect("presets list");
        list.add_css_class("boxed-list");
        let new_button: gtk::Button = builder
            .object("new_preset_button")
            .expect("new preset button");

        let page = Rc::new(Self {
            inner: Rc::new(Inner {
                ctx,
                root,
                list,
                signature: RefCell::new(String::new()),
                rows: RefCell::new(Vec::new()),
            }),
        });

        let inner = page.inner.clone();
        new_button.connect_clicked(move |_| {
            let dialog = PresetDialog::new(inner.ctx.clone(), None);
            dialog.present(&inner.ctx.window);
        });

        page
    }

    pub fn root(&self) -> &gtk::Widget {
        self.inner.root.upcast_ref()
    }

    pub fn update(&self, state: &StateDoc) {
        let signature = state
            .presets
            .iter()
            .map(|preset| format!("{}|{}|{}", preset.id, preset.name, preset.screens.join(",")))
            .chain(std::iter::once(format!(
                "default={}",
                state.settings.default_preset
            )))
            .collect::<Vec<_>>()
            .join("\u{1f}");
        if *self.inner.signature.borrow() == signature {
            return;
        }
        *self.inner.signature.borrow_mut() = signature;
        self.rebuild(state);
    }

    fn rebuild(&self, state: &StateDoc) {
        while let Some(child) = self.inner.list.first_child() {
            self.inner.list.remove(&child);
        }
        self.inner.rows.borrow_mut().clear();

        if state.presets.is_empty() {
            self.inner
                .list
                .append(&adw::ActionRow::builder().title("No presets").build());
            return;
        }

        for preset in &state.presets {
            let row = adw::ActionRow::builder()
                .title(&preset.name)
                .subtitle(screen_names(state, preset))
                .build();
            let is_default = preset.id == state.settings.default_preset;
            if is_default {
                row.set_title(&format!("{} · default", preset.name));
            }

            let start = util::icon_button(
                "media-playback-start-symbolic",
                &format!("Start preset {}", preset.name),
            );
            {
                let inner = self.inner.clone();
                let id = preset.id.clone();
                start.connect_clicked(move |_| {
                    let inner = inner.clone();
                    let id = id.clone();
                    let Some(client) = inner.ctx.client() else {
                        inner.ctx.toast("The Longcar daemon is unavailable");
                        return;
                    };
                    let name = inner
                        .ctx
                        .state()
                        .and_then(|state| {
                            state
                                .presets
                                .into_iter()
                                .find(|preset| preset.id == id)
                                .map(|preset| preset.name)
                        })
                        .unwrap_or_else(|| id.clone());
                    glib::spawn_future_local(async move {
                        match client.apply_preset(&id).await {
                            Ok(()) => inner.ctx.toast(&format!("Starting {name}")),
                            Err(err) => inner.ctx.toast_err("Could not start preset", &err),
                        }
                    });
                });
            }

            let default = util::icon_button(
                "starred-symbolic",
                &format!("Set {} as the default preset", preset.name),
            );
            default.set_sensitive(!is_default);
            {
                let inner = self.inner.clone();
                let id = preset.id.clone();
                default.connect_clicked(move |_| {
                    let inner = inner.clone();
                    let id = id.clone();
                    let Some(client) = inner.ctx.client() else {
                        inner.ctx.toast("The Longcar daemon is unavailable");
                        return;
                    };
                    let payload = json!({ "defaultPreset": id }).to_string();
                    glib::spawn_future_local(async move {
                        match client.update_settings(&payload).await {
                            Ok(()) => inner.ctx.toast("Default preset updated"),
                            Err(err) => inner.ctx.toast_err("Could not update default", &err),
                        }
                    });
                });
            }

            let edit = util::icon_button(
                "document-edit-symbolic",
                &format!("Edit preset {}", preset.name),
            );
            {
                let inner = self.inner.clone();
                let preset = preset.clone();
                edit.connect_clicked(move |_| {
                    let screen_ids = inner
                        .ctx
                        .state()
                        .and_then(|state| {
                            state
                                .presets
                                .into_iter()
                                .find(|entry| entry.id == preset.id)
                        })
                        .map(|entry| entry.screens);
                    let Some(screen_ids) = screen_ids else {
                        inner.ctx.toast("That preset no longer exists");
                        return;
                    };
                    let editable = Preset {
                        id: preset.id.clone(),
                        name: preset.name.clone(),
                        screens: screen_ids.clone(),
                    };
                    let dialog = PresetDialog::new(inner.ctx.clone(), Some((editable, screen_ids)));
                    dialog.present(&inner.ctx.window);
                });
            }

            let delete = util::icon_button(
                "user-trash-symbolic",
                &format!("Delete preset {}", preset.name),
            );
            {
                let inner = self.inner.clone();
                let preset = preset.clone();
                delete.connect_clicked(move |_| {
                    let alert = adw::AlertDialog::new(
                        Some(&format!("Delete {}?", preset.name)),
                        Some("Screens stay configured; only the preset grouping is removed."),
                    );
                    alert.add_response("cancel", "Cancel");
                    alert.add_response("delete", "Delete");
                    alert.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                    let closure_inner = inner.clone();
                    let preset = preset.clone();
                    alert.connect_response(None, move |_, response| {
                        if response != "delete" {
                            return;
                        }
                        let state = closure_inner.ctx.state();
                        let Some(state) = state else {
                            closure_inner.ctx.toast("The Longcar daemon is unavailable");
                            return;
                        };
                        let remaining: Vec<PresetEntry> = state
                            .presets
                            .iter()
                            .filter(|entry| entry.id != preset.id)
                            .cloned()
                            .collect();
                        let threading = if state.settings.default_preset == preset.id {
                            remaining
                                .first()
                                .map(|entry| entry.id.clone())
                                .unwrap_or_default()
                        } else {
                            state.settings.default_preset.clone()
                        };
                        let Some(client) = closure_inner.ctx.client() else {
                            closure_inner.ctx.toast("The Longcar daemon is unavailable");
                            return;
                        };
                        let inner = closure_inner.clone();
                        glib::spawn_future_local(async move {
                            let result = save_presets(&client, &state, remaining, &threading).await;
                            match result {
                                Ok(()) => inner.ctx.toast("Preset deleted"),
                                Err(err) => inner.ctx.toast_err("Could not delete preset", &err),
                            }
                        });
                    });
                    alert.present(Some(&inner.ctx.window));
                });
            }

            let chips = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            chips.set_valign(gtk::Align::Center);
            for screen in &preset.screens {
                let chip = gtk::Label::new(Some(
                    state
                        .screens
                        .iter()
                        .find(|entry| &entry.id == screen)
                        .map(|entry| entry.name.as_str())
                        .unwrap_or(screen.as_str()),
                ));
                chip.add_css_class("pill");
                chip.add_css_class("caption");
                chips.append(&chip);
            }
            row.add_suffix(&chips);
            row.add_suffix(&start);
            row.add_suffix(&default);
            row.add_suffix(&edit);
            row.add_suffix(&delete);
            self.inner.list.append(&row);
            self.inner.rows.borrow_mut().push((preset.id.clone(), row));
        }
    }
}

fn screen_names(state: &StateDoc, preset: &PresetEntry) -> String {
    if preset.screens.is_empty() {
        return "No screens".to_string();
    }
    preset
        .screens
        .iter()
        .map(|id| {
            state
                .screens
                .iter()
                .find(|entry| &entry.id == id)
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| id.clone())
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Send a new preset array and, when necessary, a new default preset first.
async fn save_presets(
    client: &crate::client::Client,
    state: &StateDoc,
    presets: Vec<PresetEntry>,
    default_preset: &str,
) -> Result<(), String> {
    if default_preset != state.settings.default_preset {
        let payload = json!({ "defaultPreset": default_preset }).to_string();
        client.update_settings(&payload).await?;
    }
    let json = serde_json::to_string(&presets).map_err(|err| err.to_string())?;
    client.set_presets(&json).await
}

// ---------------------------------------------------------------------------
// Preset dialog
// ---------------------------------------------------------------------------

struct PresetDialog {
    inner: Rc<PresetDialogInner>,
}

struct PresetDialogInner {
    ctx: Ctx,
    dialog: adw::Dialog,
    existing: Option<Preset>,
    name: adw::EntryRow,
    switches: RefCell<Vec<(String, adw::SwitchRow)>>,
    error: gtk::Label,
}

impl PresetDialog {
    fn new(ctx: Ctx, existing: Option<(Preset, Vec<String>)>) -> Self {
        let dialog = adw::Dialog::builder()
            .content_width(460)
            .content_height(560)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let title = gtk::Label::new(Some(if existing.is_some() {
            "Edit preset"
        } else {
            "New preset"
        }));
        title.add_css_class("title-4");
        header.set_title_widget(Some(&title));
        let cancel = gtk::Button::with_label("Cancel");
        header.pack_start(&cancel);
        let save = gtk::Button::with_label("Save");
        save.add_css_class("suggested-action");
        header.pack_end(&save);
        toolbar.add_top_bar(&header);

        let page = adw::PreferencesPage::new();
        let details = adw::PreferencesGroup::new();
        details.set_title("Preset");
        let name = adw::EntryRow::new();
        name.set_title("Name");
        details.add(&name);
        page.add(&details);

        let screens_group = adw::PreferencesGroup::new();
        screens_group.set_title("Screens");
        screens_group.set_description(Some("Screens started together by this preset"));

        let state = ctx.state();
        let screens = state.map(|state| state.screens).unwrap_or_default();
        if screens.is_empty() {
            screens_group.set_description(Some("No screens are configured yet"));
        }
        let selected: Vec<String> = existing
            .as_ref()
            .map(|(_, ids)| ids.clone())
            .unwrap_or_default();
        let mut switches = Vec::new();
        for screen in &screens {
            let row = adw::SwitchRow::new();
            row.set_title(&screen.name);
            row.set_subtitle(&format!("{} × {}", screen.width, screen.height));
            row.set_active(selected.contains(&screen.id));
            screens_group.add(&row);
            switches.push((screen.id.clone(), row));
        }
        page.add(&screens_group);

        let error_group = adw::PreferencesGroup::new();
        let error = gtk::Label::new(None);
        error.add_css_class("error");
        error.set_wrap(true);
        error.set_xalign(0.0);
        error.set_visible(false);
        error_group.add(&error);
        page.add(&error_group);

        toolbar.set_content(Some(&page));
        dialog.set_child(Some(&toolbar));

        let inner = Rc::new(PresetDialogInner {
            ctx: ctx.clone(),
            dialog: dialog.clone(),
            existing: existing.map(|(preset, _)| preset),
            name,
            switches: RefCell::new(switches),
            error,
        });

        if let Some(preset) = &inner.existing {
            inner.name.set_text(&preset.name);
        }

        {
            let inner = inner.clone();
            cancel.connect_clicked(move |_| {
                inner.dialog.close();
            });
        }
        {
            let inner = inner.clone();
            let save_weak = save.downgrade();
            save.connect_clicked(move |_| {
                let Some(client) = inner.ctx.client() else {
                    inner.show_error("The Longcar daemon is unavailable");
                    return;
                };
                let Some(state) = inner.ctx.state() else {
                    inner.show_error("The Longcar daemon is unavailable");
                    return;
                };
                let preset = match inner.collect(&state) {
                    Ok(preset) => preset,
                    Err(message) => {
                        inner.show_error(&message);
                        return;
                    }
                };
                if let Some(save) = save_weak.upgrade() {
                    save.set_sensitive(false);
                }
                let mut presets: Vec<PresetEntry> = state
                    .presets
                    .iter()
                    .map(|entry| PresetEntry {
                        id: entry.id.clone(),
                        name: entry.name.clone(),
                        screens: entry.screens.clone(),
                    })
                    .collect();
                let new_entry = PresetEntry {
                    id: preset.id.clone(),
                    name: preset.name.clone(),
                    screens: preset.screens.clone(),
                };
                match presets.iter_mut().find(|entry| entry.id == preset.id) {
                    Some(entry) => *entry = new_entry,
                    None => presets.push(new_entry),
                }
                let default_preset = state.settings.default_preset.clone();
                let inner = inner.clone();
                let save_weak = save_weak.clone();
                glib::spawn_future_local(async move {
                    match save_presets(&client, &state, presets, &default_preset).await {
                        Ok(()) => {
                            inner.dialog.close();
                            inner.ctx.toast("Presets updated");
                        }
                        Err(err) => {
                            if let Some(save) = save_weak.upgrade() {
                                save.set_sensitive(true);
                            }
                            inner.show_error(&err);
                            inner.ctx.toast_err("Could not save preset", &err);
                        }
                    }
                });
            });
        }

        Self { inner }
    }

    fn present(&self, parent: &adw::ApplicationWindow) {
        self.inner.error.set_visible(false);
        self.inner.dialog.present(Some(parent));
    }
}

impl PresetDialogInner {
    fn show_error(&self, message: &str) {
        self.error.set_text(message);
        self.error.set_visible(true);
    }

    fn collect(&self, state: &StateDoc) -> Result<Preset, String> {
        let name = self.name.text().trim().to_string();
        if name.is_empty() {
            return Err("Enter a preset name".to_string());
        }
        if name.len() > 64 || name.contains('\n') || name.contains('\r') || name.contains('#') {
            return Err("Names must be 1–64 characters without newlines or '#'".to_string());
        }
        let screens: Vec<String> = self
            .switches
            .borrow()
            .iter()
            .filter(|(_, row)| row.is_active())
            .map(|(id, _)| id.clone())
            .collect();
        if screens.is_empty() {
            return Err("Select at least one screen".to_string());
        }
        let id = match &self.existing {
            Some(preset) => preset.id.clone(),
            None => {
                let base = slugify(&name);
                if base.len() < 2 {
                    return Err(
                        "The name must start with a letter and be at least two characters long"
                            .to_string(),
                    );
                }
                let mut candidate = base.clone();
                let mut counter = 2;
                while state.presets.iter().any(|entry| entry.id == candidate) {
                    candidate = format!("{base}-{counter}");
                    counter += 1;
                }
                candidate
            }
        };
        if state
            .presets
            .iter()
            .any(|entry| entry.id != id && entry.name.trim().eq_ignore_ascii_case(name.trim()))
        {
            return Err("Another preset already uses that name".to_string());
        }
        Ok(Preset { id, name, screens })
    }
}
