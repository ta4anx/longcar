// SPDX-License-Identifier: GPL-3.0-or-later

//! Screens page: layout preview, per-screen expander rows and the screen dialog.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gst::prelude::*;
use longcar_core::config::{ScreenConfig, parse_resolution, slugify};
use longcar_core::model::{
    Alignment, Av1Mode, CursorMode, Encoder, HevcMode, Position, ScreenState,
};
use longcar_core::state::{ScreenEntry, StateDoc};
use serde_json::json;

use crate::client::Ctx;
use crate::util;
use crate::widgets::layout_preview::LayoutPreview;

const RESOLUTION_PRESETS: &[(&str, u32, u32)] = &[
    ("2560 × 1600", 2560, 1600),
    ("2560 × 1440", 2560, 1440),
    ("1920 × 1200", 1920, 1200),
    ("1920 × 1080", 1920, 1080),
    ("1280 × 800", 1280, 800),
    ("1080 × 2400", 1080, 2400),
];

const ENCODERS: &[&str] = &["auto", "vaapi", "nvenc", "vulkan", "software"];
const HEVC_MODES: &[&str] = &["auto", "off", "main", "main10"];
const AV1_MODES: &[&str] = &["auto", "off", "main", "main10"];
const POSITIONS: &[&str] = &["left", "right", "above", "below"];
const POSITION_LABELS: &[&str] = &["Left", "Right", "Above", "Below"];
const BITRATES: &[(u32, &str)] = &[
    (0, "No cap"),
    (4000, "4 Mbps"),
    (6000, "6 Mbps"),
    (8000, "8 Mbps"),
    (10000, "10 Mbps"),
    (15000, "15 Mbps"),
    (20000, "20 Mbps"),
    (30000, "30 Mbps"),
];
const AUDIO_TOOLTIP: &str = "Stream this screen's audio to the client";
const TOUCH_TOOLTIP: &str =
    "Send pen/touch events as a virtual touchscreen (off: touches act as pointer input)";
const BITRATE_TOOLTIP: &str = "Maximum streaming bitrate for this screen";
const POSITION_TOOLTIP: &str = "Placement relative to the reference screen";
const ALIGNMENTS: &[&str] = &["start", "center", "end"];
const CURSORS: &[&str] = &["hidden", "embedded", "metadata"];
const SCALES: &[(&str, f64)] = &[
    ("0.5", 0.5),
    ("0.75", 0.75),
    ("1.0", 1.0),
    ("1.25", 1.25),
    ("1.5", 1.5),
    ("1.75", 1.75),
    ("2.0", 2.0),
    ("2.5", 2.5),
    ("3.0", 3.0),
];

#[derive(Clone)]
pub struct ScreensPage {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    root: gtk::Box,
    preview: LayoutPreview,
    list: gtk::ListBox,
    rows: RefCell<Vec<(String, ScreenRow)>>,
    signature: RefCell<String>,
    preview_id: RefCell<Option<String>>,
    pipeline: RefCell<Option<PreviewPipeline>>,
    active: Cell<bool>,
}

#[derive(Clone)]
struct ScreenRow {
    expander: adw::ExpanderRow,
    badge: gtk::Label,
    start: gtk::Button,
    stop: gtk::Button,
    detail: gtk::Label,
    stack: gtk::Stack,
    preview_status: gtk::Label,
    picture: gtk::Picture,
    audio: gtk::Switch,
    touch: gtk::Switch,
    input: gtk::MenuButton,
    keyboard: gtk::CheckButton,
    mouse: gtk::CheckButton,
    controller: gtk::CheckButton,
    bitrate: gtk::DropDown,
    bitrate_model: gtk::StringList,
    bitrate_values: Rc<RefCell<Vec<u32>>>,
    position: gtk::DropDown,
    syncing: Rc<Cell<bool>>,
}

struct PreviewPipeline {
    pipeline: gst::Pipeline,
    _watch: Option<gst::bus::BusWatchGuard>,
}

impl Drop for PreviewPipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl ScreensPage {
    pub fn new(ctx: Ctx) -> Rc<Self> {
        let builder = gtk::Builder::from_resource("/io/github/ta4anx/Longcar/ui/screens_page.ui");
        let root: gtk::Box = builder.object("screens_page").expect("screens_page widget");
        let list: gtk::ListBox = builder.object("screens_list").expect("screens_list");
        list.add_css_class("boxed-list");
        let add_button: gtk::Button = builder.object("add_screen_button").expect("add button");
        let preview_slot: gtk::Box = builder.object("preview_slot").expect("preview slot");

        let preview = LayoutPreview::new();
        preview_slot.append(preview.widget());

        let page = Rc::new(Self {
            inner: Rc::new(Inner {
                ctx,
                root,
                preview,
                list,
                rows: RefCell::new(Vec::new()),
                signature: RefCell::new(String::new()),
                preview_id: RefCell::new(None),
                pipeline: RefCell::new(None),
                active: Cell::new(true),
            }),
        });

        let inner = page.inner.clone();
        add_button.connect_clicked(move |_| {
            let dialog = ScreenDialog::new(inner.ctx.clone(), None);
            dialog.present(&inner.ctx.window);
        });

        page
    }

    pub fn root(&self) -> &gtk::Widget {
        self.inner.root.upcast_ref()
    }

    pub fn update(&self, state: &StateDoc) {
        self.inner
            .preview
            .update(&state.physical_monitors, &state.screens);

        let signature = state
            .screens
            .iter()
            .map(|screen| {
                format!(
                    "{}|{}|{}|{}",
                    screen.id,
                    screen.name,
                    screen.connector.as_deref().unwrap_or("-"),
                    screen
                        .error
                        .as_ref()
                        .map(|err| format!("{err}"))
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\u{1f}");

        let rebuild = {
            let current = self.inner.signature.borrow();
            *current != signature
        };
        if rebuild {
            *self.inner.signature.borrow_mut() = signature;
            self.rebuild(state);
        } else {
            let rows = self.inner.rows.borrow();
            for screen in &state.screens {
                if let Some((_, row)) = rows.iter().find(|(id, _)| id == &screen.id) {
                    update_row(row, screen);
                }
            }
        }
    }

    /// Called when the Screens page becomes visible or hidden.
    pub fn set_active(&self, active: bool) {
        self.inner.active.set(active);
        if !active {
            self.inner.stop_preview();
        } else if let Some(id) = self.expanded_id() {
            self.inner.start_preview(&id);
        }
    }

    fn expanded_id(&self) -> Option<String> {
        self.inner
            .rows
            .borrow()
            .iter()
            .find(|(_, row)| row.expander.is_expanded())
            .map(|(id, _)| id.clone())
    }

    fn rebuild(&self, state: &StateDoc) {
        self.inner.stop_preview();
        while let Some(child) = self.inner.list.first_child() {
            self.inner.list.remove(&child);
        }
        self.inner.rows.borrow_mut().clear();

        if state.screens.is_empty() {
            let row = adw::ActionRow::builder()
                .title("No virtual screens yet")
                .subtitle("Use Add Screen to create one")
                .build();
            self.inner.list.append(&row);
            return;
        }

        for screen in &state.screens {
            let row = self.build_row(screen);
            self.inner.list.append(&row.expander);
            self.inner.rows.borrow_mut().push((screen.id.clone(), row));
        }
    }

    fn build_row(&self, screen: &ScreenEntry) -> ScreenRow {
        let inner = self.inner.clone();
        let expander = adw::ExpanderRow::builder()
            .title(&screen.name)
            .subtitle(summary(screen))
            .build();
        let badge = gtk::Label::new(Some(util::state_label(screen.state)));
        badge.add_css_class(util::state_css_class(screen.state));
        badge.set_valign(gtk::Align::Center);

        let start = util::icon_button(
            "media-playback-start-symbolic",
            &format!("Start {}", screen.name),
        );
        let stop = util::icon_button(
            "media-playback-stop-symbolic",
            &format!("Stop {}", screen.name),
        );
        let edit = util::icon_button("document-edit-symbolic", &format!("Edit {}", screen.name));
        let remove = util::icon_button("user-trash-symbolic", &format!("Remove {}", screen.name));

        {
            let inner = inner.clone();
            let id = screen.id.clone();
            start.connect_clicked(move |_| {
                let inner = inner.clone();
                let id = id.clone();
                let Some(client) = inner.ctx.client() else {
                    inner.ctx.toast("The Longcar daemon is unavailable");
                    return;
                };
                glib::spawn_future_local(async move {
                    if let Err(err) = client.start_screen(&id).await {
                        inner.ctx.toast_err("Could not start screen", &err);
                    }
                });
            });
        }
        {
            let inner = inner.clone();
            let id = screen.id.clone();
            stop.connect_clicked(move |_| {
                let inner = inner.clone();
                let id = id.clone();
                let Some(client) = inner.ctx.client() else {
                    inner.ctx.toast("The Longcar daemon is unavailable");
                    return;
                };
                glib::spawn_future_local(async move {
                    if let Err(err) = client.stop_screen(&id).await {
                        inner.ctx.toast_err("Could not stop screen", &err);
                    }
                });
            });
        }
        {
            let inner = inner.clone();
            let id = screen.id.clone();
            edit.connect_clicked(move |_| {
                let existing = inner
                    .ctx
                    .state()
                    .and_then(|state| state.screens.into_iter().find(|entry| entry.id == id));
                let Some(existing) = existing else {
                    inner.ctx.toast("That screen no longer exists");
                    return;
                };
                let dialog = ScreenDialog::new(inner.ctx.clone(), Some(existing));
                dialog.present(&inner.ctx.window);
            });
        }
        {
            let inner = inner.clone();
            let id = screen.id.clone();
            let name = screen.name.clone();
            remove.connect_clicked(move |_| {
                let alert = adw::AlertDialog::new(
                    Some(&format!("Remove {name}?")),
                    Some(
                        "The virtual screen is removed from the layout. Pairing data is kept unless you erase it.",
                    ),
                );
                alert.add_response("cancel", "Cancel");
                alert.add_response("remove", "Remove");
                alert.add_response("erase", "Remove and erase data");
                alert.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
                alert.set_response_appearance("erase", adw::ResponseAppearance::Destructive);
                let closure_inner = inner.clone();
                let id = id.clone();
                alert.connect_response(None, move |_, response| {
                    if response != "remove" && response != "erase" {
                        return;
                    }
                    let purge = response == "erase";
                    let inner = closure_inner.clone();
                    let id = id.clone();
                    let Some(client) = inner.ctx.client() else {
                        inner.ctx.toast("The Longcar daemon is unavailable");
                        return;
                    };
                    glib::spawn_future_local(async move {
                        match client.remove_screen(&id, purge).await {
                            Ok(()) => inner.ctx.toast("Screen removed"),
                            Err(err) => inner.ctx.toast_err("Could not remove screen", &err),
                        }
                    });
                });
                alert.present(Some(&inner.ctx.window));
            });
        }

        let audio = gtk::Switch::builder()
            .valign(gtk::Align::Center)
            .tooltip_text(AUDIO_TOOLTIP)
            .build();
        audio.update_property(&[gtk::accessible::Property::Label(AUDIO_TOOLTIP)]);
        let touch = gtk::Switch::builder()
            .valign(gtk::Align::Center)
            .tooltip_text(TOUCH_TOOLTIP)
            .build();
        touch.update_property(&[gtk::accessible::Property::Label(TOUCH_TOOLTIP)]);
        let (input, keyboard, mouse, controller) = build_input_menu(screen);
        let bitrate_model =
            gtk::StringList::new(&BITRATES.iter().map(|(_, label)| *label).collect::<Vec<_>>());
        let bitrate = gtk::DropDown::builder()
            .model(&bitrate_model)
            .valign(gtk::Align::Center)
            .tooltip_text(BITRATE_TOOLTIP)
            .width_request(108)
            .build();
        let bitrate_values = Rc::new(RefCell::new(
            BITRATES.iter().map(|(kbps, _)| *kbps).collect::<Vec<_>>(),
        ));
        let position = gtk::DropDown::from_strings(POSITION_LABELS);
        position.set_valign(gtk::Align::Center);
        position.set_tooltip_text(Some(POSITION_TOOLTIP));
        position.set_width_request(92);

        let quick = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        quick.set_valign(gtk::Align::Center);
        quick.append(&labeled_switch("Audio", &audio));
        quick.append(&labeled_switch("Touch", &touch));
        quick.append(&input);
        quick.append(&bitrate);
        quick.append(&position);

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        actions.set_valign(gtk::Align::Center);
        actions.set_halign(gtk::Align::End);
        actions.append(&badge);
        actions.append(&start);
        actions.append(&stop);
        actions.append(&edit);
        actions.append(&remove);

        let suffix = gtk::Box::new(gtk::Orientation::Vertical, 4);
        suffix.set_valign(gtk::Align::Center);
        suffix.append(&quick);
        suffix.append(&actions);
        expander.add_suffix(&suffix);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.set_margin_top(8);
        content.set_margin_bottom(8);
        content.set_margin_start(12);
        content.set_margin_end(12);

        let stack = gtk::Stack::new();
        stack.set_size_request(-1, 190);
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_can_shrink(true);
        let preview_status = gtk::Label::new(Some("Expand to load the live preview"));
        preview_status.set_wrap(true);
        preview_status.add_css_class("dim-label");
        stack.add_named(&picture, Some("picture"));
        stack.add_named(&preview_status, Some("placeholder"));
        stack.set_visible_child_name("placeholder");
        content.append(&stack);

        let detail = gtk::Label::new(Some(&detail_text(screen)));
        detail.add_css_class("longcar-mono");
        detail.add_css_class("caption");
        detail.set_xalign(0.0);
        detail.set_selectable(true);
        content.append(&detail);

        expander.add_row(&content);

        {
            let inner = inner.clone();
            let id = screen.id.clone();
            expander.connect_expanded_notify(move |expander| {
                if expander.is_expanded() {
                    let others: Vec<adw::ExpanderRow> = inner
                        .rows
                        .borrow()
                        .iter()
                        .filter(|(other_id, _)| other_id != &id)
                        .map(|(_, row)| row.expander.clone())
                        .collect();
                    for other in others {
                        other.set_expanded(false);
                    }
                    inner.start_preview(&id);
                } else if inner.preview_id.borrow().as_deref() == Some(id.as_str()) {
                    inner.stop_preview();
                }
            });
        }

        let row = ScreenRow {
            expander,
            badge,
            start,
            stop,
            detail,
            stack,
            preview_status,
            picture,
            audio,
            touch,
            input,
            keyboard,
            mouse,
            controller,
            bitrate,
            bitrate_model,
            bitrate_values,
            position,
            syncing: Rc::new(Cell::new(false)),
        };
        update_row(&row, screen);
        connect_quick_controls(&inner, &row, &screen.id);
        row
    }
}

fn build_input_menu(
    screen: &ScreenEntry,
) -> (
    gtk::MenuButton,
    gtk::CheckButton,
    gtk::CheckButton,
    gtk::CheckButton,
) {
    let button = gtk::MenuButton::builder()
        .icon_name("input-keyboard-symbolic")
        .tooltip_text("Keyboard, mouse and controller input")
        .valign(gtk::Align::Center)
        .build();
    button.update_property(&[gtk::accessible::Property::Label(
        "Keyboard, mouse and controller input",
    )]);
    let popover = gtk::Popover::new();
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 2);
    menu.set_margin_top(6);
    menu.set_margin_bottom(6);
    menu.set_margin_start(8);
    menu.set_margin_end(8);
    let keyboard = input_check("Keyboard", screen.keyboard);
    let mouse = input_check("Mouse", screen.mouse);
    let controller = input_check("Controller", screen.controller);
    menu.append(&keyboard);
    menu.append(&mouse);
    menu.append(&controller);
    popover.set_child(Some(&menu));
    button.set_popover(Some(&popover));
    (button, keyboard, mouse, controller)
}

fn input_check(label: &str, active: bool) -> gtk::CheckButton {
    let check = gtk::CheckButton::with_label(label);
    check.set_active(active);
    check
}

fn labeled_switch(label: &str, switch: &gtk::Switch) -> gtk::Box {
    let holder = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    holder.set_valign(gtk::Align::Center);
    let text = gtk::Label::new(Some(label));
    text.add_css_class("caption");
    text.add_css_class("dim-label");
    holder.append(&text);
    holder.append(switch);
    holder
}

fn connect_quick_controls(inner: &Rc<Inner>, row: &ScreenRow, id: &str) {
    {
        let inner = inner.clone();
        let row = row.clone();
        let id = id.to_string();
        row.audio.clone().connect_active_notify(move |switch| {
            if row.syncing.get() {
                return;
            }
            let enabled = switch.is_active();
            let Some(client) = inner.ctx.client() else {
                inner.ctx.toast("The Longcar daemon is unavailable");
                resync_row(&inner, &row, &id);
                return;
            };
            let inner = inner.clone();
            let row = row.clone();
            let id = id.clone();
            glib::spawn_future_local(async move {
                if let Err(err) = client.set_screen_audio(&id, enabled).await {
                    inner.ctx.toast_err("Could not change audio", &err);
                    resync_row(&inner, &row, &id);
                }
            });
        });
    }
    {
        let inner = inner.clone();
        let row = row.clone();
        let id = id.to_string();
        row.touch.clone().connect_active_notify(move |switch| {
            if row.syncing.get() {
                return;
            }
            let enabled = switch.is_active();
            let Some(client) = inner.ctx.client() else {
                inner.ctx.toast("The Longcar daemon is unavailable");
                resync_row(&inner, &row, &id);
                return;
            };
            let inner = inner.clone();
            let row = row.clone();
            let id = id.clone();
            glib::spawn_future_local(async move {
                if let Err(err) = client.set_screen_touch(&id, enabled).await {
                    inner.ctx.toast_err("Could not change touch input", &err);
                    resync_row(&inner, &row, &id);
                }
            });
        });
    }
    {
        let inner = inner.clone();
        let row = row.clone();
        let id = id.to_string();
        row.bitrate
            .clone()
            .connect_selected_notify(move |dropdown| {
                if row.syncing.get() {
                    return;
                }
                let Some(kbps) = row
                    .bitrate_values
                    .borrow()
                    .get(dropdown.selected() as usize)
                    .copied()
                else {
                    return;
                };
                let current = inner
                    .ctx
                    .state()
                    .and_then(|state| state.screens.into_iter().find(|screen| screen.id == id))
                    .map(|screen| screen.max_bitrate_kbps);
                if current == Some(kbps) {
                    return;
                }
                let Some(client) = inner.ctx.client() else {
                    inner.ctx.toast("The Longcar daemon is unavailable");
                    resync_row(&inner, &row, &id);
                    return;
                };
                let inner = inner.clone();
                let row = row.clone();
                let id = id.clone();
                glib::spawn_future_local(async move {
                    if let Err(err) = client.set_screen_bitrate(&id, kbps).await {
                        inner.ctx.toast_err("Could not change bitrate", &err);
                        resync_row(&inner, &row, &id);
                    }
                });
            });
    }
    {
        let inner = inner.clone();
        let row = row.clone();
        let id = id.to_string();
        row.position
            .clone()
            .connect_selected_notify(move |dropdown| {
                if row.syncing.get() {
                    return;
                }
                let Some(position) = POSITIONS.get(dropdown.selected() as usize) else {
                    return;
                };
                let Some(screen) = inner
                    .ctx
                    .state()
                    .and_then(|state| state.screens.into_iter().find(|screen| screen.id == id))
                else {
                    return;
                };
                if position_of(screen.position) == *position {
                    return;
                }
                let relative_to = screen.relative_to.clone();
                let alignment = format!("{:?}", screen.alignment).to_lowercase();
                let Some(client) = inner.ctx.client() else {
                    inner.ctx.toast("The Longcar daemon is unavailable");
                    resync_row(&inner, &row, &id);
                    return;
                };
                let inner = inner.clone();
                let row = row.clone();
                let id = id.clone();
                let position = (*position).to_string();
                glib::spawn_future_local(async move {
                    match client
                        .set_screen_layout(&id, &position, &relative_to, &alignment)
                        .await
                    {
                        Ok(()) => inner.ctx.toast("Screen position updated"),
                        Err(err) => {
                            inner.ctx.toast_err("Could not move screen", &err);
                            resync_row(&inner, &row, &id);
                        }
                    }
                });
            });
    }
    for (kind, check) in [
        ("keyboard", &row.keyboard),
        ("mouse", &row.mouse),
        ("controller", &row.controller),
    ] {
        let inner = inner.clone();
        let row = row.clone();
        let id = id.to_string();
        let kind = kind.to_string();
        check.connect_toggled(move |check| {
            if row.syncing.get() {
                return;
            }
            let enabled = check.is_active();
            let Some(client) = inner.ctx.client() else {
                inner.ctx.toast("The Longcar daemon is unavailable");
                resync_row(&inner, &row, &id);
                return;
            };
            let inner = inner.clone();
            let row = row.clone();
            let id = id.clone();
            let kind = kind.clone();
            glib::spawn_future_local(async move {
                if let Err(err) = client.set_screen_input(&id, &kind, enabled).await {
                    inner.ctx.toast_err("Could not change input", &err);
                    resync_row(&inner, &row, &id);
                }
            });
        });
    }
}

fn resync_row(inner: &Rc<Inner>, row: &ScreenRow, id: &str) {
    if let Some(screen) = inner
        .ctx
        .state()
        .and_then(|state| state.screens.into_iter().find(|screen| screen.id == id))
    {
        update_row(row, &screen);
    }
}

fn update_row(row: &ScreenRow, screen: &ScreenEntry) {
    row.syncing.set(true);
    row.badge.set_label(util::state_label(screen.state));
    row.badge
        .set_css_classes(&[util::state_css_class(screen.state)]);
    row.expander.set_subtitle(&summary(screen));
    row.start.set_sensitive(
        !screen.state.is_active()
            && screen.state != ScreenState::Stopping
            && screen.state != ScreenState::Preparing,
    );
    row.stop.set_sensitive(screen.state.is_active());
    row.detail.set_label(&detail_text(screen));
    row.audio.set_active(screen.audio);
    row.touch.set_active(screen.touch);
    row.keyboard.set_active(screen.keyboard);
    row.mouse.set_active(screen.mouse);
    row.controller.set_active(screen.controller);
    select_bitrate(row, screen.max_bitrate_kbps);
    row.position.set_selected(position_index(screen.position));
    row.syncing.set(false);

    let streaming = screen.state == ScreenState::Streaming;
    row.audio.set_sensitive(!streaming);
    row.touch.set_sensitive(!streaming);
    row.keyboard.set_sensitive(!streaming);
    row.mouse.set_sensitive(!streaming);
    row.controller.set_sensitive(!streaming);
    row.input.set_sensitive(!streaming);
    row.bitrate.set_sensitive(!streaming);
    let suffix = if streaming {
        " Unavailable while the screen is streaming."
    } else {
        ""
    };
    row.audio
        .set_tooltip_text(Some(&format!("{AUDIO_TOOLTIP}.{suffix}")));
    row.touch
        .set_tooltip_text(Some(&format!("{TOUCH_TOOLTIP}.{suffix}")));
    row.bitrate
        .set_tooltip_text(Some(&format!("{BITRATE_TOOLTIP}.{suffix}")));
    row.position.set_tooltip_text(Some(POSITION_TOOLTIP));
}

fn select_bitrate(row: &ScreenRow, kbps: u32) {
    let mut values = row.bitrate_values.borrow_mut();
    if let Some(index) = values.iter().position(|value| *value == kbps) {
        row.bitrate.set_selected(index as u32);
        return;
    }
    let label = if kbps == 0 {
        "No cap".to_string()
    } else {
        format!("{kbps} kbps")
    };
    row.bitrate_model.append(&label);
    values.push(kbps);
    row.bitrate.set_selected((values.len() - 1) as u32);
}

fn position_of(position: Position) -> &'static str {
    match position {
        Position::Left => "left",
        Position::Right => "right",
        Position::Above => "above",
        Position::Below => "below",
    }
}

fn position_index(position: Position) -> u32 {
    match position {
        Position::Left => 0,
        Position::Right => 1,
        Position::Above => 2,
        Position::Below => 3,
    }
}

fn summary(screen: &ScreenEntry) -> String {
    let relative = if screen.relative_to.is_empty() || screen.relative_to == "primary" {
        "primary".to_string()
    } else {
        screen.relative_to.clone()
    };
    let bitrate = if screen.max_bitrate_kbps == 0 {
        "no cap".to_string()
    } else if screen.max_bitrate_kbps.is_multiple_of(1000) {
        format!("{} Mbps", screen.max_bitrate_kbps / 1000)
    } else {
        format!("{} kbps", screen.max_bitrate_kbps)
    };
    let mut parts = vec![
        format!("{}×{}", screen.width, screen.height),
        format!("{:.0} Hz", screen.refresh_hz),
        format!("{:.2}×", screen.scale),
        format!("{} of {}", position_of(screen.position), relative),
        bitrate,
    ];
    if screen.audio {
        parts.push("audio".to_string());
    }
    if screen.touch {
        parts.push("touch".to_string());
    }
    let mut inputs = Vec::new();
    if screen.keyboard {
        inputs.push("kbd");
    }
    if screen.mouse {
        inputs.push("mouse");
    }
    if screen.controller {
        inputs.push("pad");
    }
    if !inputs.is_empty() {
        parts.push(inputs.join("/"));
    }
    parts.join(" · ")
}

fn detail_text(screen: &ScreenEntry) -> String {
    let mut parts = vec![
        format!("Control 127.0.0.1:{}", screen.base_port),
        format!("Web UI https://127.0.0.1:{}", screen.web_port),
    ];
    if let Some(connector) = &screen.connector {
        parts.push(format!("connector {connector}"));
    }
    if let Some(node) = screen.node_id {
        parts.push(format!("PipeWire node {node}"));
    }
    if screen.bitrate_kbps > 0 {
        parts.push(format!("{} kbps · {} fps", screen.bitrate_kbps, screen.fps));
    }
    if let Some(error) = &screen.error {
        parts.push(format!("failure {error}"));
    }
    parts.join(" · ")
}

impl Inner {
    fn row_widgets(&self, id: &str) -> Option<(gtk::Stack, gtk::Label, gtk::Picture)> {
        self.rows
            .borrow()
            .iter()
            .find(|(row_id, _)| row_id == id)
            .map(|(_, row)| {
                (
                    row.stack.clone(),
                    row.preview_status.clone(),
                    row.picture.clone(),
                )
            })
    }

    fn preview_message(&self, id: &str, message: &str) {
        if self.preview_id.borrow().as_deref() != Some(id) {
            return;
        }
        if let Some((stack, label, _)) = self.row_widgets(id) {
            label.set_text(message);
            stack.set_visible_child_name("placeholder");
        }
    }

    fn stop_preview(&self) {
        self.preview_id.borrow_mut().take();
        self.pipeline.borrow_mut().take();
    }

    fn start_preview(self: &Rc<Self>, id: &str) {
        self.stop_preview();
        if !self.active.get() {
            return;
        }
        self.preview_id.borrow_mut().replace(id.to_string());
        if let Some((stack, label, _)) = self.row_widgets(id) {
            label.set_text("Connecting preview…");
            stack.set_visible_child_name("placeholder");
        }

        let Some(client) = self.ctx.client() else {
            self.preview_message(id, "Preview unavailable: the daemon is not running");
            return;
        };
        let ctx = self.ctx.clone();
        let weak = Rc::downgrade(self);
        let id = id.to_string();
        glib::spawn_future_local(async move {
            let Some(this) = weak.upgrade() else {
                return;
            };
            let node = match client.get_preview_node(&id).await {
                Ok(node) => node,
                Err(err) => {
                    this.preview_message(&id, &format!("Preview unavailable: {err}"));
                    return;
                }
            };
            let screen = ctx
                .state()
                .and_then(|state| state.screens.into_iter().find(|screen| screen.id == id));
            let Some(screen) = screen else {
                this.preview_message(&id, "Preview unavailable: the screen no longer exists");
                return;
            };
            this.build_pipeline(&id, node, &screen);
        });
    }

    fn build_pipeline(self: &Rc<Self>, id: &str, node: u32, screen: &ScreenEntry) {
        if self.preview_id.borrow().as_deref() != Some(id) {
            return;
        }
        let width = screen.width;
        let height = screen.height;
        let num = (screen.refresh_hz * 1000.0).round().max(1.0) as u32;
        let description = format!(
            "pipewiresrc path={node} do-timestamp=true ! video/x-raw,width={width},height={height},max-framerate={num}/1000 ! videoconvert ! gtk4paintablesink name=sink"
        );
        let element = match gst::parse::launch(&description) {
            Ok(element) => element,
            Err(err) => {
                eprintln!("longcar: preview pipeline failed for {id}: {err}");
                self.preview_message(id, &format!("Preview unavailable: {err}"));
                return;
            }
        };
        let pipeline = match element.downcast::<gst::Pipeline>() {
            Ok(pipeline) => pipeline,
            Err(_) => {
                self.preview_message(id, "Preview unavailable: pipeline is not a GstPipeline");
                return;
            }
        };
        let Some(sink) = pipeline.by_name("sink") else {
            self.preview_message(id, "Preview unavailable: gtk4paintablesink is missing");
            return;
        };
        let paintable = sink.property::<gtk::gdk::Paintable>("paintable");

        let watch = pipeline.bus().and_then(|bus| {
            let weak = Rc::downgrade(self);
            let id = id.to_string();
            bus.add_watch_local(move |_, message| {
                if let gst::MessageView::Error(err) = message.view() {
                    let _ = &id;
                    if let Some(this) = weak.upgrade() {
                        let current = this.preview_id.borrow().clone();
                        if let Some(current) = current {
                            this.preview_message(
                                &current,
                                &format!("Preview failed: {}", err.error()),
                            );
                        }
                    }
                }
                glib::ControlFlow::Continue
            })
            .ok()
        });

        if let Err(err) = pipeline.set_state(gst::State::Playing) {
            self.preview_message(id, &format!("Preview failed: {err}"));
            return;
        }

        if let Some((stack, _, picture)) = self.row_widgets(id) {
            picture.set_paintable(Some(&paintable));
            stack.set_visible_child_name("picture");
        }
        *self.pipeline.borrow_mut() = Some(PreviewPipeline {
            pipeline,
            _watch: watch,
        });
    }
}

// ---------------------------------------------------------------------------
// Screen dialog
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ScreenDialog {
    inner: Rc<DialogInner>,
}

struct DialogInner {
    ctx: Ctx,
    dialog: adw::Dialog,
    id: Option<String>,
    name: adw::EntryRow,
    resolution: gtk::DropDown,
    width: gtk::SpinButton,
    height: gtk::SpinButton,
    refresh: gtk::SpinButton,
    scale: gtk::DropDown,
    position: gtk::DropDown,
    relative: gtk::DropDown,
    relative_ids: RefCell<Vec<String>>,
    alignment: gtk::DropDown,
    slot: gtk::SpinButton,
    encoder: gtk::DropDown,
    hevc: gtk::DropDown,
    av1: gtk::DropDown,
    audio: adw::SwitchRow,
    touch: adw::SwitchRow,
    keyboard: adw::SwitchRow,
    mouse: adw::SwitchRow,
    controller: adw::SwitchRow,
    audio_sink: adw::EntryRow,
    bitrate: gtk::SpinButton,
    max_resolution: adw::EntryRow,
    cursor: gtk::DropDown,
    error: gtk::Label,
}

impl ScreenDialog {
    fn new(ctx: Ctx, existing: Option<ScreenEntry>) -> Self {
        let builder = gtk::Builder::from_resource("/io/github/ta4anx/Longcar/ui/screen_dialog.ui");
        let dialog: adw::Dialog = builder.object("screen_dialog").expect("screen dialog");
        let name: adw::EntryRow = builder.object("name_entry").expect("name_entry");
        let resolution: gtk::DropDown = builder.object("resolution_preset").expect("resolution");
        let width: gtk::SpinButton = builder.object("width_spin").expect("width");
        let height: gtk::SpinButton = builder.object("height_spin").expect("height");
        let refresh: gtk::SpinButton = builder.object("refresh_spin").expect("refresh");
        let scale: gtk::DropDown = builder.object("scale_dropdown").expect("scale");
        let position: gtk::DropDown = builder.object("position_dropdown").expect("position");
        let relative: gtk::DropDown = builder.object("relative_dropdown").expect("relative");
        let alignment: gtk::DropDown = builder.object("alignment_dropdown").expect("alignment");
        let slot: gtk::SpinButton = builder.object("slot_spin").expect("slot");
        let encoder: gtk::DropDown = builder.object("encoder_dropdown").expect("encoder");
        let hevc: gtk::DropDown = builder.object("hevc_dropdown").expect("hevc");
        let av1: gtk::DropDown = builder.object("av1_dropdown").expect("av1");
        let audio: adw::SwitchRow = builder.object("audio_switch").expect("audio switch");
        let touch: adw::SwitchRow = builder.object("touch_switch").expect("touch switch");
        let keyboard: adw::SwitchRow = builder.object("keyboard_switch").expect("keyboard switch");
        let mouse: adw::SwitchRow = builder.object("mouse_switch").expect("mouse switch");
        let controller: adw::SwitchRow = builder
            .object("controller_switch")
            .expect("controller switch");
        let audio_sink: adw::EntryRow = builder
            .object("audio_sink_entry")
            .expect("audio sink entry");
        let bitrate: gtk::SpinButton = builder.object("bitrate_spin").expect("bitrate");
        let max_resolution: adw::EntryRow = builder
            .object("max_resolution_entry")
            .expect("max_resolution");
        let cursor: gtk::DropDown = builder.object("cursor_dropdown").expect("cursor");
        let error: gtk::Label = builder.object("error_label").expect("error label");
        let save: gtk::Button = builder.object("save_button").expect("save button");
        let cancel: gtk::Button = builder.object("cancel_button").expect("cancel button");
        let title: gtk::Label = builder.object("dialog_title").expect("dialog title");

        let state_screens = ctx.state().map(|state| state.screens).unwrap_or_default();

        let mut relative_ids = vec!["primary".to_string()];
        let mut relative_labels = vec!["Primary monitor".to_string()];
        for screen in &state_screens {
            if Some(screen.id.as_str()) != existing.as_ref().map(|entry| entry.id.as_str()) {
                relative_ids.push(screen.id.clone());
                relative_labels.push(screen.name.clone());
            }
        }
        let label_refs: Vec<&str> = relative_labels.iter().map(String::as_str).collect();
        relative.set_model(Some(&gtk::StringList::new(&label_refs)));

        if let Some(existing) = &existing {
            title.set_text(&format!("Edit {}", existing.name));
        }

        let inner = Rc::new(DialogInner {
            ctx,
            dialog,
            id: existing.as_ref().map(|screen| screen.id.clone()),
            name,
            resolution,
            width,
            height,
            refresh,
            scale,
            position,
            relative,
            relative_ids: RefCell::new(relative_ids),
            alignment,
            slot,
            encoder,
            hevc,
            av1,
            audio,
            touch,
            keyboard,
            mouse,
            controller,
            audio_sink,
            bitrate,
            max_resolution,
            cursor,
            error,
        });

        inner.fill(existing.as_ref());

        {
            let dropdown_inner = inner.clone();
            inner.resolution.connect_selected_notify(move |dropdown| {
                let selected = dropdown.selected() as usize;
                if let Some((_, w, h)) = RESOLUTION_PRESETS.get(selected) {
                    dropdown_inner.width.set_value(f64::from(*w));
                    dropdown_inner.height.set_value(f64::from(*h));
                }
            });
        }
        {
            let inner = inner.clone();
            let save_weak = save.downgrade();
            save.connect_clicked(move |_| {
                let screen = match inner.collect() {
                    Ok(screen) => screen,
                    Err(message) => {
                        inner.show_error(&message);
                        return;
                    }
                };
                inner.error.set_text("");
                let active_geometry_change = inner
                    .id
                    .as_ref()
                    .and_then(|id| {
                        inner
                            .ctx
                            .state()
                            .and_then(|state| state.screens.into_iter().find(|entry| &entry.id == id))
                    })
                    .map(|old| old.state.is_active() && geometry_changed(&old, &screen))
                    .unwrap_or(false);
                if active_geometry_change {
                    let alert = adw::AlertDialog::new(
                        Some("Change this screen's geometry?"),
                        Some(
                            "Changing the resolution, refresh rate, scale or cursor stops the screen. The next start needs a new approval in GNOME's screen picker.",
                        ),
                    );
                    alert.add_response("cancel", "Cancel");
                    alert.add_response("continue", "Change and restart");
                    alert.set_response_appearance("continue", adw::ResponseAppearance::Destructive);
                    let alert_inner = inner.clone();
                    let save_weak = save_weak.clone();
                    let screen = screen.clone();
                    alert.connect_response(None, move |_, response| {
                        if response == "continue" {
                            submit_screen(&alert_inner, &screen, &save_weak);
                        }
                    });
                    alert.present(Some(&inner.ctx.window));
                } else {
                    submit_screen(&inner, &screen, &save_weak);
                }
            });
        }
        {
            let inner = inner.clone();
            cancel.connect_clicked(move |_| {
                inner.dialog.close();
            });
        }

        Self { inner }
    }

    fn present(&self, parent: &adw::ApplicationWindow) {
        self.inner.error.set_visible(false);
        self.inner.error.set_text("");
        self.inner.dialog.present(Some(parent));
    }
}

impl DialogInner {
    fn show_error(&self, message: &str) {
        self.error.set_text(message);
        self.error.set_visible(true);
    }

    fn fill(&self, existing: Option<&ScreenEntry>) {
        match existing {
            Some(screen) => {
                self.name.set_text(&screen.name);
                self.width.set_value(f64::from(screen.width));
                self.height.set_value(f64::from(screen.height));
                self.refresh.set_value(screen.refresh_hz);
                self.slot.set_value(f64::from(screen.slot));
                self.bitrate.set_value(f64::from(screen.max_bitrate_kbps));
                self.audio.set_active(screen.audio);
                self.touch.set_active(screen.touch);
                self.keyboard.set_active(screen.keyboard);
                self.mouse.set_active(screen.mouse);
                self.controller.set_active(screen.controller);
                self.audio_sink.set_text(&screen.audio_sink);
                self.max_resolution.set_text(&screen.max_resolution);
                util::select_dropdown(
                    &self.position,
                    POSITIONS,
                    &format!("{:?}", screen.position).to_lowercase(),
                );
                select_scale(&self.scale, screen.scale);
                util::select_dropdown(
                    &self.alignment,
                    ALIGNMENTS,
                    &format!("{:?}", screen.alignment).to_lowercase(),
                );
                util::select_dropdown(
                    &self.encoder,
                    ENCODERS,
                    &format!("{:?}", screen.encoder).to_lowercase(),
                );
                util::select_dropdown(
                    &self.hevc,
                    HEVC_MODES,
                    &format!("{:?}", screen.hevc).to_lowercase(),
                );
                util::select_dropdown(
                    &self.av1,
                    AV1_MODES,
                    &format!("{:?}", screen.av1).to_lowercase(),
                );
                util::select_dropdown(
                    &self.cursor,
                    CURSORS,
                    &format!("{:?}", screen.cursor).to_lowercase(),
                );
                let ids = self.relative_ids.borrow();
                if let Some(index) = ids.iter().position(|id| *id == screen.relative_to) {
                    self.relative.set_selected(index as u32);
                }
                drop(ids);
                self.sync_resolution_preset();
            }
            None => {
                self.name.set_text("");
                self.width.set_value(2560.0);
                self.height.set_value(1600.0);
                self.refresh.set_value(120.0);
                self.scale.set_selected(4);
                self.position.set_selected(1);
                self.alignment.set_selected(1);
                self.encoder.set_selected(0);
                self.hevc.set_selected(0);
                self.av1.set_selected(1);
                self.cursor.set_selected(1);
                self.bitrate.set_value(30000.0);
                self.audio.set_active(true);
                self.touch.set_active(true);
                self.keyboard.set_active(true);
                self.mouse.set_active(true);
                self.controller.set_active(true);
                self.audio_sink.set_text("");
                self.max_resolution.set_text("");
                let used: Vec<u8> = self
                    .ctx
                    .state()
                    .map(|state| state.screens.iter().map(|screen| screen.slot).collect())
                    .unwrap_or_default();
                let slot = (0..=7u8).find(|slot| !used.contains(slot)).unwrap_or(0);
                self.slot.set_value(f64::from(slot));
                self.sync_resolution_preset();
            }
        }
    }

    fn sync_resolution_preset(&self) {
        let width = self.width.value() as u32;
        let height = self.height.value() as u32;
        match RESOLUTION_PRESETS
            .iter()
            .position(|(_, w, h)| *w == width && *h == height)
        {
            Some(index) => self.resolution.set_selected(index as u32),
            None => self
                .resolution
                .set_selected(RESOLUTION_PRESETS.len() as u32),
        }
    }

    fn collect(&self) -> Result<ScreenConfig, String> {
        let state = self.ctx.state().unwrap_or_default();
        let name = self.name.text().trim().to_string();
        let width = self.width.value() as u32;
        let height = self.height.value() as u32;
        let slot = self.slot.value() as u8;
        let max_resolution = self.max_resolution.text().trim().to_string();
        validate_fields(
            &state,
            self.id.as_deref(),
            &name,
            width,
            height,
            slot,
            &max_resolution,
        )?;
        let scale = SCALES
            .get(self.scale.selected() as usize)
            .map(|(_, value)| *value)
            .unwrap_or(1.0);
        let id = match &self.id {
            Some(id) => id.clone(),
            None => unique_id(&state, &name),
        };
        let relative = self
            .relative_ids
            .borrow()
            .get(self.relative.selected() as usize)
            .cloned()
            .unwrap_or_else(|| "primary".to_string());
        Ok(ScreenConfig {
            id,
            name,
            width,
            height,
            refresh_hz: self.refresh.value(),
            scale,
            position: parse_position(&self.position),
            relative_to: relative,
            alignment: parse_alignment(&self.alignment),
            slot,
            encoder: parse_encoder(&self.encoder),
            hevc: parse_hevc(&self.hevc),
            av1: parse_av1(&self.av1),
            max_bitrate_kbps: self.bitrate.value() as u32,
            max_resolution,
            cursor: parse_cursor(&self.cursor),
            audio: self.audio.is_active(),
            touch: self.touch.is_active(),
            keyboard: self.keyboard.is_active(),
            mouse: self.mouse.is_active(),
            controller: self.controller.is_active(),
            audio_sink: self.audio_sink.text().trim().to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn submit_screen(
    inner: &Rc<DialogInner>,
    screen: &ScreenConfig,
    save_weak: &glib::WeakRef<gtk::Button>,
) {
    if let Some(save) = save_weak.upgrade() {
        save.set_sensitive(false);
    }
    let Some(client) = inner.ctx.client() else {
        if let Some(save) = save_weak.upgrade() {
            save.set_sensitive(true);
        }
        inner.show_error("The Longcar daemon is unavailable");
        return;
    };
    let json = screen_json(screen).to_string();
    let existing = inner.id.is_some();
    let inner = inner.clone();
    let save_weak = save_weak.clone();
    glib::spawn_future_local(async move {
        let result = if existing {
            client.update_screen(&json).await
        } else {
            client.add_screen(&json).await.map(|_| ())
        };
        match result {
            Ok(()) => {
                inner.dialog.close();
                inner.ctx.toast(if existing {
                    "Screen updated"
                } else {
                    "Screen added"
                });
            }
            Err(err) => {
                if let Some(save) = save_weak.upgrade() {
                    save.set_sensitive(true);
                }
                inner.show_error(&err);
                inner.ctx.toast_err("Could not save screen", &err);
            }
        }
    });
}

fn geometry_changed(old: &ScreenEntry, new: &ScreenConfig) -> bool {
    old.width != new.width
        || old.height != new.height
        || (old.refresh_hz - new.refresh_hz).abs() > 0.01
        || (old.scale - new.scale).abs() > 0.001
        || old.cursor != new.cursor
}

fn select_scale(dropdown: &gtk::DropDown, scale: f64) {
    if let Some(index) = SCALES
        .iter()
        .position(|(_, value)| (value - scale).abs() < 0.001)
    {
        dropdown.set_selected(index as u32);
    }
}

fn unique_id(state: &StateDoc, name: &str) -> String {
    let slug = slugify(name);
    let mut base = if slug.len() < 2 {
        format!("screen-{}", if slug.is_empty() { "screen" } else { &slug })
    } else {
        slug
    };
    if !base.starts_with(|ch: char| ch.is_ascii_lowercase()) {
        base = format!("screen-{base}");
    }
    if base.len() > 32 {
        base.truncate(32);
        while base.ends_with('-') {
            base.pop();
        }
    }
    let mut candidate = base.clone();
    let mut counter = 2;
    while state.screens.iter().any(|screen| screen.id == candidate) {
        let suffix = format!("-{counter}");
        let mut stem = base.clone();
        if stem.len() + suffix.len() > 32 {
            stem.truncate(32 - suffix.len());
            while stem.ends_with('-') {
                stem.pop();
            }
        }
        candidate = format!("{stem}{suffix}");
        counter += 1;
    }
    candidate
}

fn parse_position(dropdown: &gtk::DropDown) -> Position {
    match dropdown.selected() {
        0 => Position::Left,
        2 => Position::Above,
        3 => Position::Below,
        _ => Position::Right,
    }
}

fn parse_alignment(dropdown: &gtk::DropDown) -> Alignment {
    match dropdown.selected() {
        0 => Alignment::Start,
        2 => Alignment::End,
        _ => Alignment::Center,
    }
}

fn parse_encoder(dropdown: &gtk::DropDown) -> Encoder {
    match dropdown.selected() {
        1 => Encoder::Vaapi,
        2 => Encoder::Nvenc,
        3 => Encoder::Vulkan,
        4 => Encoder::Software,
        _ => Encoder::Auto,
    }
}

fn parse_hevc(dropdown: &gtk::DropDown) -> HevcMode {
    match dropdown.selected() {
        1 => HevcMode::Off,
        2 => HevcMode::Main,
        3 => HevcMode::Main10,
        _ => HevcMode::Auto,
    }
}

fn parse_av1(dropdown: &gtk::DropDown) -> Av1Mode {
    match dropdown.selected() {
        1 => Av1Mode::Off,
        2 => Av1Mode::Main,
        3 => Av1Mode::Main10,
        _ => Av1Mode::Auto,
    }
}

fn parse_cursor(dropdown: &gtk::DropDown) -> CursorMode {
    match dropdown.selected() {
        0 => CursorMode::Hidden,
        2 => CursorMode::Metadata,
        _ => CursorMode::Embedded,
    }
}

fn validate_fields(
    state: &StateDoc,
    id: Option<&str>,
    name: &str,
    width: u32,
    height: u32,
    slot: u8,
    max_resolution: &str,
) -> Result<(), String> {
    if name.is_empty() {
        return Err("Enter a display name".to_string());
    }
    if name.len() > 64 || name.contains('\n') || name.contains('\r') || name.contains('#') {
        return Err("Names must be 1–64 characters without newlines or '#'".to_string());
    }
    if !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || !(640..=7680).contains(&width)
        || !(640..=7680).contains(&height)
    {
        return Err("Width and height must be even between 640 and 7680".to_string());
    }
    if let Some(other) = state.screens.iter().find(|screen| {
        Some(screen.id.as_str()) != id && screen.width == width && screen.height == height
    }) {
        return Err(format!(
            "{} × {} is already used by {}",
            width, height, other.name
        ));
    }
    if let Some(other) = state
        .screens
        .iter()
        .find(|screen| Some(screen.id.as_str()) != id && screen.slot == slot)
    {
        return Err(format!("Slot {} is already used by {}", slot, other.name));
    }
    if !max_resolution.is_empty() && parse_resolution(max_resolution).is_none() {
        return Err("Max resolution must be WxH with even 640–7680 values, or empty".to_string());
    }
    Ok(())
}

fn screen_json(screen: &ScreenConfig) -> serde_json::Value {
    json!({
        "id": screen.id,
        "name": screen.name,
        "width": screen.width,
        "height": screen.height,
        "refreshHz": screen.refresh_hz,
        "scale": screen.scale,
        "position": format!("{:?}", screen.position).to_lowercase(),
        "relativeTo": screen.relative_to,
        "alignment": format!("{:?}", screen.alignment).to_lowercase(),
        "slot": screen.slot,
        "encoder": format!("{:?}", screen.encoder).to_lowercase(),
        "hevc": format!("{:?}", screen.hevc).to_lowercase(),
        "av1": format!("{:?}", screen.av1).to_lowercase(),
        "maxBitrateKbps": screen.max_bitrate_kbps,
        "maxResolution": screen.max_resolution,
        "cursor": format!("{:?}", screen.cursor).to_lowercase(),
        "audio": screen.audio,
        "touch": screen.touch,
        "keyboard": screen.keyboard,
        "mouse": screen.mouse,
        "controller": screen.controller,
        "audio_sink": screen.audio_sink,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{
        "schemaVersion": 1,
        "daemon": {"state": "idle", "since": "2026-11-21T12:00:00Z"},
        "screens": [{
            "id": "tablet", "name": "Tablet",
            "state": "off",
            "width": 2560, "height": 1600, "refreshHz": 120.0, "scale": 1.5,
            "slot": 0, "position": "right", "relativeTo": "primary", "alignment": "center",
            "encoder": "auto", "hevc": "auto", "av1": "off",
            "maxBitrateKbps": 30000, "maxResolution": "2560x1600", "cursor": "embedded",
            "logical": null, "connector": null, "nodeId": null,
            "basePort": 48049, "webPort": 48050,
            "connected": false, "bitrateKbps": 0, "fps": 0, "error": null
        }],
        "physicalMonitors": [],
        "presets": [{"id": "tablet", "name": "Tablet", "screens": ["tablet"]}],
        "settings": {
            "autostartScreens": false, "notifications": true,
            "stopWhenIdleMinutes": 0, "defaultPreset": "tablet",
            "audioScreen": "tablet", "restoreLayoutOnStop": true,
            "onboardingComplete": false
        },
        "pendingPairings": [], "devices": [],
        "host": {"addresses": []},
        "warnings": []
    }"#;

    fn state() -> StateDoc {
        StateDoc::from_json(DOC).expect("fixture parses")
    }

    #[test]
    fn duplicate_resolution_is_rejected_inline() {
        let error = validate_fields(&state(), None, "Second tablet", 2560, 1600, 1, "")
            .expect_err("duplicate resolution must be rejected");
        assert!(error.contains("already used by Tablet"), "{error}");
    }

    #[test]
    fn odd_or_out_of_range_dimensions_are_rejected() {
        assert!(validate_fields(&state(), None, "Odd", 2561, 1600, 1, "").is_err());
        assert!(validate_fields(&state(), None, "Small", 320, 200, 1, "").is_err());
    }

    #[test]
    fn duplicate_slot_is_rejected_inline() {
        let error = validate_fields(&state(), None, "Phone", 1920, 1080, 0, "")
            .expect_err("duplicate slot must be rejected");
        assert!(error.contains("slot") || error.contains("Slot"), "{error}");
    }

    #[test]
    fn invalid_max_resolution_is_rejected_inline() {
        assert!(validate_fields(&state(), None, "Phone", 1920, 1080, 1, "1920").is_err());
        assert!(validate_fields(&state(), None, "Phone", 1920, 1080, 1, "").is_ok());
    }

    #[test]
    fn editing_a_screen_ignores_its_own_geometry() {
        assert!(validate_fields(&state(), Some("tablet"), "Tablet", 2560, 1600, 0, "").is_ok());
    }

    #[test]
    fn screen_json_uses_the_daemon_contract() {
        let screen = ScreenConfig {
            id: "tablet".to_string(),
            name: "Tablet".to_string(),
            width: 2560,
            height: 1600,
            refresh_hz: 120.0,
            scale: 1.5,
            position: Position::Right,
            relative_to: "primary".to_string(),
            alignment: Alignment::Center,
            slot: 0,
            encoder: Encoder::Auto,
            hevc: HevcMode::Auto,
            av1: Av1Mode::Off,
            max_bitrate_kbps: 41000,
            max_resolution: "2560x1600".to_string(),
            cursor: CursorMode::Embedded,
            audio: true,
            touch: false,
            keyboard: true,
            mouse: true,
            controller: false,
            audio_sink: String::new(),
        };
        let json = screen_json(&screen);
        assert_eq!(json["maxBitrateKbps"], 41000);
        assert_eq!(json["refreshHz"], 120.0);
        assert_eq!(json["relativeTo"], "primary");
        assert_eq!(json["audio"], true);
        assert_eq!(json["touch"], false);
        assert_eq!(json["keyboard"], true);
        assert_eq!(json["controller"], false);
        assert_eq!(json["audio_sink"], "");
        assert!(json.get("max_bitrate_kbps").is_none());
        assert!(json.get("refresh_hz").is_none());
    }

    #[test]
    fn duplicate_screen_names_get_a_unique_id() {
        assert_eq!(unique_id(&state(), "Tablet"), "tablet-2");
        assert_eq!(unique_id(&state(), "Desk"), "desk");
    }
}
