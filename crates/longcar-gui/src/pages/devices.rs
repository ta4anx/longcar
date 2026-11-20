// SPDX-License-Identifier: GPL-3.0-or-later

//! Devices page: pending pairings, paired clients and connection information.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use longcar_core::state::{DeviceEntry, ScreenEntry, StateDoc};

use crate::client::Ctx;
use crate::util;

#[derive(Clone, Debug)]
struct Pairing {
    id: String,
    name: String,
    address: String,
}

#[derive(Clone)]
pub struct DevicesPage {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    root: gtk::Box,
    groups: gtk::Box,
    polled: RefCell<HashMap<String, Vec<Pairing>>>,
    timer: RefCell<Option<glib::SourceId>>,
    signature: RefCell<String>,
}

impl DevicesPage {
    pub fn new(ctx: Ctx) -> Rc<Self> {
        let builder = gtk::Builder::from_resource("/io/github/ta4anx/Longcar/ui/devices_page.ui");
        let root: gtk::Box = builder.object("devices_page").expect("devices_page");
        let groups: gtk::Box = builder.object("devices_groups").expect("devices groups");

        Rc::new(Self {
            inner: Rc::new(Inner {
                ctx,
                root,
                groups,
                polled: RefCell::new(HashMap::new()),
                timer: RefCell::new(None),
                signature: RefCell::new(String::new()),
            }),
        })
    }

    pub fn root(&self) -> &gtk::Widget {
        self.inner.root.upcast_ref()
    }

    pub fn update(&self, state: &StateDoc) {
        let signature = self.inner.signature(state);
        if *self.inner.signature.borrow() == signature {
            return;
        }
        *self.inner.signature.borrow_mut() = signature;
        self.inner.render(state);
    }

    /// Poll `ListPairings` only while the page is visible.
    pub fn set_active(self: &Rc<Self>, active: bool) {
        if active {
            self.start_polling();
            if let Some(state) = self.inner.ctx.state() {
                self.inner.render(&state);
            }
        } else if let Some(timer) = self.inner.timer.borrow_mut().take() {
            timer.remove();
        }
    }

    fn start_polling(self: &Rc<Self>) {
        if self.inner.timer.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(&self.inner);
        let id = glib::timeout_add_seconds_local(2, move || {
            if let Some(inner) = weak.upgrade() {
                poll_pairings(&inner);
            }
            glib::ControlFlow::Continue
        });
        *self.inner.timer.borrow_mut() = Some(id);
    }
}

fn poll_pairings(inner: &Rc<Inner>) {
    let Some(client) = inner.ctx.client() else {
        return;
    };
    let Some(state) = inner.ctx.state() else {
        return;
    };
    let screens: Vec<String> = state
        .screens
        .iter()
        .filter(|screen| {
            matches!(
                screen.state,
                longcar_core::model::ScreenState::Ready
                    | longcar_core::model::ScreenState::Streaming
            )
        })
        .map(|screen| screen.id.clone())
        .collect();
    if screens.is_empty() {
        return;
    }
    let weak = Rc::downgrade(inner);
    glib::spawn_future_local(async move {
        let mut polled = HashMap::new();
        for id in screens {
            if let Ok(json) = client.list_pairings(&id).await {
                match parse_pairings(&json) {
                    Ok(pairings) => {
                        polled.insert(id, pairings);
                    }
                    Err(err) => eprintln!("longcar: bad ListPairings payload: {err}"),
                }
            }
        }
        let Some(inner) = weak.upgrade() else {
            return;
        };
        *inner.polled.borrow_mut() = polled;
        if let Some(state) = inner.ctx.state() {
            let signature = inner.signature(&state);
            *inner.signature.borrow_mut() = signature;
            inner.render(&state);
        }
    });
}

fn parse_pairings(json: &str) -> Result<Vec<Pairing>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|err| format!("invalid JSON: {err}"))?;
    let array = value
        .as_array()
        .ok_or_else(|| "expected an array".to_string())?;
    let mut out = Vec::new();
    for item in array {
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        out.push(Pairing {
            id: id.to_string(),
            name: item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown device")
                .to_string(),
            address: item
                .get("address")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        });
    }
    Ok(out)
}

impl Inner {
    fn signature(&self, state: &StateDoc) -> String {
        let polled = self.polled.borrow();
        let mut parts = Vec::new();
        for screen in &state.screens {
            parts.push(format!(
                "screen:{}:{}:{}",
                screen.id,
                screen.name,
                screen.state.as_str()
            ));
            if let Some(pairings) = polled.get(&screen.id) {
                for pairing in pairings {
                    parts.push(format!("pending:{}:{}", pairing.id, pairing.name));
                }
            }
            for device in state
                .devices
                .iter()
                .filter(|device| device.screen_id == screen.id)
            {
                parts.push(format!(
                    "device:{}:{}:{}",
                    device.uuid, device.name, device.enabled
                ));
            }
        }
        for pairing in &state.pending_pairings {
            parts.push(format!(
                "state-pending:{}:{}:{}",
                pairing.screen_id, pairing.id, pairing.name
            ));
        }
        parts.join("\u{1f}")
    }

    fn render(&self, state: &StateDoc) {
        while let Some(child) = self.groups.first_child() {
            self.groups.remove(&child);
        }

        if state.screens.is_empty() {
            let status = adw::StatusPage::new();
            status.set_icon_name(Some("video-display-symbolic"));
            status.set_title("No screens configured");
            status.set_description(Some("Add a screen first; pairing happens per screen."));
            self.groups.append(&status);
            return;
        }

        for screen in &state.screens {
            let group = self.screen_group(state, screen);
            self.groups.append(&group);
        }

        let note = gtk::Label::new(Some(
            "Idle Moonlight clients do not advertise themselves. A device appears here while it is waiting for a PIN and after it is paired.",
        ));
        note.add_css_class("dim-label");
        note.set_wrap(true);
        note.set_xalign(0.0);
        self.groups.append(&note);
    }

    fn screen_group(&self, state: &StateDoc, screen: &ScreenEntry) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::new();
        group.set_title(&screen.name);
        group.set_description(Some(&format!(
            "Host port {} · PIN requests appear while the screen is ready",
            screen.base_port
        )));

        let devices: Vec<&DeviceEntry> = state
            .devices
            .iter()
            .filter(|device| device.screen_id == screen.id)
            .collect();
        let pending: Vec<Pairing> = match self.polled.borrow().get(&screen.id) {
            Some(polled) => polled.clone(),
            None => state
                .pending_pairings
                .iter()
                .filter(|pairing| pairing.screen_id == screen.id)
                .map(|pairing| Pairing {
                    id: pairing.id.clone(),
                    name: pairing.name.clone(),
                    address: pairing.address.clone(),
                })
                .collect(),
        };

        for pairing in pending {
            let row = adw::ActionRow::builder()
                .title(&pairing.name)
                .subtitle(format!("Waiting for a PIN · {}", pairing.address))
                .build();
            let pair = util::text_button("Pair", "Enter the 4-digit PIN shown by the client");
            {
                let ctx = self.ctx.clone();
                let screen_id = screen.id.clone();
                let pairing = pairing.clone();
                pair.connect_clicked(move |_| {
                    let dialog = PairDialog::new(
                        ctx.clone(),
                        screen_id.clone(),
                        pairing.id.clone(),
                        pairing.name.clone(),
                        pairing.address.clone(),
                    );
                    dialog.present(&ctx.window);
                });
            }
            let cancel = util::icon_button("window-close-symbolic", "Reject this pairing request");
            {
                let ctx = self.ctx.clone();
                let screen_id = screen.id.clone();
                let pairing_id = pairing.id.clone();
                cancel.connect_clicked(move |_| {
                    let Some(client) = ctx.client() else {
                        ctx.toast("The Longcar daemon is unavailable");
                        return;
                    };
                    let ctx = ctx.clone();
                    let screen_id = screen_id.clone();
                    let pairing_id = pairing_id.clone();
                    glib::spawn_future_local(async move {
                        match client.cancel_pairing(&screen_id, &pairing_id).await {
                            Ok(()) => ctx.toast("Pairing request rejected"),
                            Err(err) => ctx.toast_err("Could not reject pairing", &err),
                        }
                    });
                });
            }
            row.add_suffix(&pair);
            row.add_suffix(&cancel);
            group.add(&row);
        }

        if devices.is_empty() && pending_row_count(&self.polled, state, &screen.id) == 0 {
            let row = adw::ActionRow::builder()
                .title("No paired clients")
                .subtitle("Pair from Moonlight or Artemis to see a device here")
                .build();
            group.add(&row);
        }

        for device in devices {
            let row = adw::ActionRow::builder()
                .title(&device.name)
                .subtitle(&device.uuid)
                .build();
            let enabled = adw::SwitchRow::new();
            enabled.set_active(device.enabled);
            enabled.set_tooltip_text(Some("Allow this device to connect"));
            {
                let ctx = self.ctx.clone();
                let screen_id = screen.id.clone();
                let uuid = device.uuid.clone();
                enabled.connect_active_notify(move |switch| {
                    let requested = switch.is_active();
                    let Some(client) = ctx.client() else {
                        ctx.toast("The Longcar daemon is unavailable");
                        return;
                    };
                    let screen_id = screen_id.clone();
                    let uuid = uuid.clone();
                    let ctx = ctx.clone();
                    glib::spawn_future_local(async move {
                        if let Err(err) = client
                            .set_client_enabled(&screen_id, &uuid, requested)
                            .await
                        {
                            ctx.toast_err("Could not update device", &err);
                        }
                    });
                });
            }
            let unpair = util::icon_button("user-trash-symbolic", "Unpair this device");
            {
                let ctx = self.ctx.clone();
                let screen_id = screen.id.clone();
                let uuid = device.uuid.clone();
                let name = device.name.clone();
                unpair.connect_clicked(move |_| {
                    let alert = adw::AlertDialog::new(
                        Some(&format!("Unpair {name}?")),
                        Some("The client must pair again before it can connect."),
                    );
                    alert.add_response("cancel", "Cancel");
                    alert.add_response("unpair", "Unpair");
                    alert.set_response_appearance("unpair", adw::ResponseAppearance::Destructive);
                    let alert_ctx = ctx.clone();
                    let screen_id = screen_id.clone();
                    let uuid = uuid.clone();
                    alert.connect_response(None, move |_, response| {
                        if response != "unpair" {
                            return;
                        }
                        let Some(client) = alert_ctx.client() else {
                            alert_ctx.toast("The Longcar daemon is unavailable");
                            return;
                        };
                        let screen_id = screen_id.clone();
                        let uuid = uuid.clone();
                        let ctx = alert_ctx.clone();
                        glib::spawn_future_local(async move {
                            match client.unpair(&screen_id, &uuid).await {
                                Ok(()) => ctx.toast("Device unpaired"),
                                Err(err) => ctx.toast_err("Could not unpair", &err),
                            }
                        });
                    });
                    alert.present(Some(&ctx.window));
                });
            }
            let switch_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            switch_box.set_valign(gtk::Align::Center);
            switch_box.append(&enabled);
            switch_box.append(&unpair);
            row.add_suffix(&switch_box);
            group.add(&row);
        }

        let info = adw::ActionRow::builder()
            .title("Connection info")
            .subtitle("Host address, port and recommended client settings")
            .build();
        let show = util::text_button("Show", "Show how to connect this screen");
        {
            let ctx = self.ctx.clone();
            let screen = screen.clone();
            show.connect_clicked(move |_| {
                let dialog = ConnectionDialog::new(ctx.clone(), screen.clone());
                dialog.present(&ctx.window);
            });
        }
        info.add_suffix(&show);
        group.add(&info);

        group
    }
}

fn pending_row_count(
    polled: &RefCell<HashMap<String, Vec<Pairing>>>,
    state: &StateDoc,
    screen_id: &str,
) -> usize {
    match polled.borrow().get(screen_id) {
        Some(pairings) => pairings.len(),
        None => state
            .pending_pairings
            .iter()
            .filter(|pairing| pairing.screen_id == screen_id)
            .count(),
    }
}

// ---------------------------------------------------------------------------
// Pair dialog
// ---------------------------------------------------------------------------

struct PairDialog {
    ctx: Ctx,
    dialog: adw::Dialog,
    pin: gtk::Entry,
    error: gtk::Label,
}

impl PairDialog {
    fn new(ctx: Ctx, screen_id: String, pairing_id: String, name: String, address: String) -> Self {
        let builder = gtk::Builder::from_resource("/io/github/ta4anx/Longcar/ui/pair_dialog.ui");
        let dialog: adw::Dialog = builder.object("pair_dialog").expect("pair dialog");
        let pin: gtk::Entry = builder.object("pin_entry").expect("pin entry");
        let error: gtk::Label = builder.object("pair_error").expect("pair error");
        let pair: gtk::Button = builder.object("pair_button").expect("pair button");
        let cancel: gtk::Button = builder.object("cancel_button").expect("cancel button");
        let title: gtk::Label = builder.object("pair_title").expect("pair title");
        let subtitle: gtk::Label = builder.object("pair_subtitle").expect("pair subtitle");
        title.set_text(&format!("Pair {name}"));
        subtitle.set_text(&format!("Client {address} · screen {screen_id}"));
        pin.set_max_length(4);
        pin.set_input_purpose(gtk::InputPurpose::Digits);

        let this = Self {
            ctx: ctx.clone(),
            dialog: dialog.clone(),
            pin: pin.clone(),
            error,
        };

        {
            let this = Self {
                ctx: ctx.clone(),
                dialog: dialog.clone(),
                pin: pin.clone(),
                error: this.error.clone(),
            };
            let pair_weak = pair.downgrade();
            let ctx = ctx.clone();
            let screen_id = screen_id.clone();
            let pairing_id = pairing_id.clone();
            let name = name.clone();
            pair.connect_clicked(move |_| {
                let pin_value = this.pin.text().to_string();
                if pin_value.len() != 4 || !pin_value.chars().all(|ch| ch.is_ascii_digit()) {
                    this.error
                        .set_text("Enter the 4-digit PIN shown on the client");
                    this.error.set_visible(true);
                    return;
                }
                let Some(client) = ctx.client() else {
                    this.error.set_text("The Longcar daemon is unavailable");
                    this.error.set_visible(true);
                    return;
                };
                this.error.set_visible(false);
                if let Some(pair) = pair_weak.upgrade() {
                    pair.set_sensitive(false);
                }
                let dialog = this.dialog.clone();
                let ctx = ctx.clone();
                let screen_id = screen_id.clone();
                let pairing_id = pairing_id.clone();
                let name = name.clone();
                let pair_weak = pair_weak.clone();
                glib::spawn_future_local(async move {
                    match client
                        .pair(&screen_id, &pairing_id, &pin_value, &name)
                        .await
                    {
                        Ok(true) => {
                            dialog.close();
                            ctx.toast(&format!("{name} paired"));
                        }
                        Ok(false) => {
                            if let Some(pair) = pair_weak.upgrade() {
                                pair.set_sensitive(true);
                            }
                            ctx.toast("The PIN was rejected by Sunshine");
                        }
                        Err(err) => {
                            if let Some(pair) = pair_weak.upgrade() {
                                pair.set_sensitive(true);
                            }
                            ctx.toast_err("Pairing failed", &err);
                        }
                    }
                });
            });
        }
        {
            let dialog = dialog.clone();
            cancel.connect_clicked(move |_| {
                dialog.close();
            });
        }

        pin.grab_focus();
        let _ = &this.ctx;
        this
    }

    fn present(&self, parent: &adw::ApplicationWindow) {
        self.error.set_visible(false);
        self.dialog.present(Some(parent));
    }
}

// ---------------------------------------------------------------------------
// Connection info dialog
// ---------------------------------------------------------------------------

struct ConnectionDialog {
    dialog: adw::Dialog,
}

impl ConnectionDialog {
    fn new(ctx: Ctx, screen: ScreenEntry) -> Self {
        let builder =
            gtk::Builder::from_resource("/io/github/ta4anx/Longcar/ui/connection_dialog.ui");
        let dialog: adw::Dialog = builder
            .object("connection_dialog")
            .expect("connection dialog");
        let title: gtk::Label = builder
            .object("connection_title")
            .expect("connection title");
        let address: gtk::Label = builder.object("address_value").expect("address value");
        let endpoint: gtk::Label = builder.object("endpoint_value").expect("endpoint value");
        let copy_address: gtk::Button = builder
            .object("copy_address_button")
            .expect("copy address button");
        let copy_endpoint: gtk::Button = builder
            .object("copy_endpoint_button")
            .expect("copy endpoint button");
        let recommendation: gtk::Label = builder
            .object("recommendation_value")
            .expect("recommendation value");
        let close: gtk::Button = builder.object("close_button").expect("close button");

        title.set_text(&format!("Connect {}", screen.name));
        let host = ctx
            .state()
            .and_then(|state| {
                state
                    .host
                    .addresses
                    .iter()
                    .find(|address| address.preferred)
                    .or_else(|| state.host.addresses.first())
                    .map(|entry| entry.address.clone())
            })
            .unwrap_or_else(|| "unavailable".to_string());
        address.set_text(&host);
        let endpoint_text = format!("{host}:{}", screen.base_port);
        endpoint.set_text(&endpoint_text);

        let portrait = screen.height > screen.width;
        recommendation.set_text(if portrait {
            "Use Artemis in portrait mode. Enable HEVC, request a portrait resolution and cap the bitrate at the screen's limit."
        } else {
            "Use stock Moonlight. Request the screen's resolution, enable HEVC and start with around 30 Mbps; Moonlight adapts downward automatically."
        });
        recommendation.set_wrap(true);

        {
            let ctx = ctx.clone();
            let text = host.clone();
            copy_address.connect_clicked(move |_| ctx.copy(&text, "Address"));
        }
        {
            let ctx = ctx.clone();
            let text = endpoint_text.clone();
            copy_endpoint.connect_clicked(move |_| ctx.copy(&text, "Host:port"));
        }
        {
            let dialog = dialog.clone();
            close.connect_clicked(move |_| {
                dialog.close();
            });
        }

        Self { dialog }
    }

    fn present(&self, parent: &adw::ApplicationWindow) {
        self.dialog.present(Some(parent));
    }
}
