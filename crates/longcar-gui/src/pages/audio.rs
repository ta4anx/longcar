// SPDX-License-Identifier: GPL-3.0-or-later

//! Audio page: host output mixer, stale-default recovery, sinks and per-screen audio.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use serde::Deserialize;
use serde_json::json;

use crate::client::Ctx;
use crate::util;

#[derive(Clone)]
pub struct AudioPage {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    root: gtk::Box,
    host_row: adw::ActionRow,
    host_badge: gtk::Label,
    stale_row: adw::ActionRow,
    host_volume: gtk::Scale,
    host_volume_label: gtk::Label,
    host_mute: gtk::ToggleButton,
    restore: gtk::Button,
    auto_restore: adw::SwitchRow,
    default_sink: RefCell<Option<SinkInfo>>,
    sinks_list: gtk::ListBox,
    screens_list: gtk::ListBox,
    sinks_signature: RefCell<String>,
    screens_signature: RefCell<String>,
    syncing: Cell<bool>,
    active: Cell<bool>,
    sequence: Cell<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AudioInfo {
    #[serde(default)]
    default_sink: Option<SinkInfo>,
    #[serde(default)]
    sinks: Vec<SinkInfo>,
    #[serde(default)]
    screens: Vec<AudioScreen>,
    #[serde(default)]
    stale_default: bool,
    #[serde(default)]
    restore_host_audio: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SinkInfo {
    #[serde(default)]
    id: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    is_default: bool,
    #[serde(default)]
    volume: f64,
    #[serde(default)]
    muted: bool,
}

impl SinkInfo {
    /// Volume as a 0–100 percentage.
    fn percent(&self) -> u8 {
        (self.volume.clamp(0.0, 1.0) * 100.0).round() as u8
    }

    /// A sink is inaudible when muted or at zero volume.
    fn silent(&self) -> bool {
        self.muted || self.percent() == 0
    }

    fn title(&self) -> String {
        if self.description.is_empty() {
            self.name.clone()
        } else {
            self.description.clone()
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AudioScreen {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    audio: bool,
    #[serde(default)]
    audio_sink: String,
    #[serde(default)]
    effective_sink: String,
}

impl AudioPage {
    pub fn new(ctx: Ctx) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_vexpand(true);

        let scroll = gtk::ScrolledWindow::new();
        scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroll.set_vexpand(true);
        let clamp = adw::Clamp::builder()
            .maximum_size(780)
            .tightening_threshold(640)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.set_margin_top(18);
        content.set_margin_bottom(24);
        content.set_margin_start(18);
        content.set_margin_end(18);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let title = gtk::Label::new(Some("Audio"));
        title.add_css_class("title-1");
        title.set_xalign(0.0);
        title.set_hexpand(true);
        let refresh = util::icon_button(
            "view-refresh-symbolic",
            "Read the current host output and sinks",
        );
        header.append(&title);
        header.append(&refresh);
        content.append(&header);

        let host_group = adw::PreferencesGroup::builder()
            .title("Host output")
            .description(
                "Where this computer plays sound; while streaming Sunshine captures a virtual sink",
            )
            .build();
        let host_row = adw::ActionRow::builder()
            .title("Reading host output…")
            .subtitle("The daemon has not answered yet")
            .build();
        let host_badge = gtk::Label::new(Some("MUTED"));
        host_badge.add_css_class("caption");
        host_badge.add_css_class("error");
        host_badge.set_valign(gtk::Align::Center);
        host_badge.set_visible(false);
        host_row.add_suffix(&host_badge);

        let stale_row = adw::ActionRow::builder()
            .title("Host speakers are muted")
            .subtitle(
                "Sunshine's virtual sink is still the default. Restore a physical output to hear the host again.",
            )
            .build();
        let stale_icon = gtk::Image::from_icon_name("audio-volume-muted-symbolic");
        stale_icon.set_valign(gtk::Align::Center);
        stale_row.add_prefix(&stale_icon);
        stale_row.set_visible(false);

        let volume_row = adw::ActionRow::builder()
            .title("Host volume")
            .subtitle("Gain and mute of the default output")
            .build();
        let host_volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        host_volume.set_width_request(170);
        host_volume.set_draw_value(false);
        host_volume.set_valign(gtk::Align::Center);
        host_volume.set_tooltip_text(Some("Volume of the default host sink"));
        let host_volume_label = gtk::Label::new(Some("—"));
        host_volume_label.add_css_class("caption");
        host_volume_label.add_css_class("dim-label");
        host_volume_label.set_width_chars(4);
        let host_mute = gtk::ToggleButton::with_label("Mute");
        host_mute.add_css_class("flat");
        host_mute.set_valign(gtk::Align::Center);
        host_mute.set_tooltip_text(Some("Mute or unmute the default host sink"));
        let volume_suffix = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        volume_suffix.set_valign(gtk::Align::Center);
        volume_suffix.append(&host_volume);
        volume_suffix.append(&host_volume_label);
        volume_suffix.append(&host_mute);
        volume_row.add_suffix(&volume_suffix);

        let restore_row = adw::ActionRow::builder()
            .title("Restore host output")
            .subtitle("Move the default sink back to a physical output")
            .build();
        let restore = util::text_button("Restore", "Restore the host's physical audio output");
        restore.add_css_class("suggested-action");
        restore.set_sensitive(false);
        restore_row.add_suffix(&restore);
        restore_row.set_activatable_widget(Some(&restore));

        let auto_restore = adw::SwitchRow::builder()
            .title("Restore automatically when screens stop")
            .subtitle("Restores the host sink, volume and mute state when screens stop")
            .build();

        host_group.add(&host_row);
        host_group.add(&stale_row);
        host_group.add(&volume_row);
        host_group.add(&restore_row);
        host_group.add(&auto_restore);
        content.append(&host_group);

        let sinks_group = adw::PreferencesGroup::builder()
            .title("Host sinks")
            .description("Outputs PipeWire reports on this computer")
            .build();
        let sinks_list = gtk::ListBox::new();
        sinks_list.set_selection_mode(gtk::SelectionMode::None);
        sinks_list.add_css_class("boxed-list");
        sinks_group.add(&sinks_list);
        content.append(&sinks_group);

        let screens_group = adw::PreferencesGroup::builder()
            .title("Screens")
            .description("Stream each screen's audio and choose the host sink Sunshine captures")
            .build();
        let screens_list = gtk::ListBox::new();
        screens_list.set_selection_mode(gtk::SelectionMode::None);
        screens_list.add_css_class("boxed-list");
        screens_group.add(&screens_list);
        content.append(&screens_group);

        clamp.set_child(Some(&content));
        scroll.set_child(Some(&clamp));
        root.append(&scroll);

        let page = Rc::new(Self {
            inner: Rc::new(Inner {
                ctx,
                root,
                host_row,
                host_badge,
                stale_row,
                host_volume,
                host_volume_label,
                host_mute,
                restore,
                auto_restore,
                default_sink: RefCell::new(None),
                sinks_list,
                screens_list,
                sinks_signature: RefCell::new(String::new()),
                screens_signature: RefCell::new(String::new()),
                syncing: Cell::new(false),
                active: Cell::new(false),
                sequence: Cell::new(0),
            }),
        });

        {
            let inner = page.inner.clone();
            refresh.connect_clicked(move |_| inner.refresh());
        }
        {
            let inner = page.inner.clone();
            page.inner.restore.connect_clicked(move |button| {
                let Some(client) = inner.ctx.client() else {
                    inner.ctx.toast("The Longcar daemon is unavailable");
                    return;
                };
                button.set_sensitive(false);
                let inner = inner.clone();
                glib::spawn_future_local(async move {
                    match client.restore_host_audio().await {
                        Ok(()) => inner.ctx.toast("Host output restored"),
                        Err(err) => {
                            inner
                                .ctx
                                .toast_err("Could not restore the host output", &err);
                        }
                    }
                    inner.refresh();
                });
            });
        }
        {
            let inner = page.inner.clone();
            let scheduled = Rc::new(Cell::new(false));
            page.inner.host_volume.connect_value_changed(move |scale| {
                if inner.syncing.get() {
                    return;
                }
                let percent = scale.value().round().clamp(0.0, 100.0) as u8;
                inner.host_volume_label.set_text(&format!("{percent}%"));
                if scheduled.get() {
                    return;
                }
                scheduled.set(true);
                let inner = inner.clone();
                let scheduled = scheduled.clone();
                glib::timeout_add_local_once(Duration::from_millis(250), move || {
                    scheduled.set(false);
                    let Some(sink) = inner.default_sink.borrow().clone() else {
                        return;
                    };
                    let percent = inner.host_volume.value().round().clamp(0.0, 100.0) as u8;
                    let Some(client) = inner.ctx.client() else {
                        inner.ctx.toast("The Longcar daemon is unavailable");
                        inner.refresh();
                        return;
                    };
                    let inner = inner.clone();
                    glib::spawn_future_local(async move {
                        match client.set_sink_volume(sink.id, percent).await {
                            Ok(()) => inner.ctx.toast("Host volume updated"),
                            Err(err) => {
                                inner.ctx.toast_err("Could not set the host volume", &err);
                            }
                        }
                        inner.refresh();
                    });
                });
            });
        }
        {
            let inner = page.inner.clone();
            page.inner.host_mute.connect_toggled(move |button| {
                if inner.syncing.get() {
                    return;
                }
                let Some(sink) = inner.default_sink.borrow().clone() else {
                    return;
                };
                let muted = button.is_active();
                let Some(client) = inner.ctx.client() else {
                    inner.ctx.toast("The Longcar daemon is unavailable");
                    inner.refresh();
                    return;
                };
                let inner = inner.clone();
                glib::spawn_future_local(async move {
                    match client.set_sink_muted(sink.id, muted).await {
                        Ok(()) => inner.ctx.toast(if muted {
                            "Host output muted"
                        } else {
                            "Host output unmuted"
                        }),
                        Err(err) => inner.ctx.toast_err("Could not mute the host output", &err),
                    }
                    inner.refresh();
                });
            });
        }
        {
            let inner = page.inner.clone();
            page.inner.auto_restore.connect_active_notify(move |row| {
                if inner.syncing.get() {
                    return;
                }
                let enabled = row.is_active();
                let Some(client) = inner.ctx.client() else {
                    inner.ctx.toast("The Longcar daemon is unavailable");
                    inner.refresh();
                    return;
                };
                let inner = inner.clone();
                glib::spawn_future_local(async move {
                    let payload = json!({ "restoreHostAudio": enabled }).to_string();
                    match client.update_settings(&payload).await {
                        Ok(()) => inner.ctx.toast("Audio restore preference updated"),
                        Err(err) => {
                            inner.ctx.toast_err("Could not update settings", &err);
                        }
                    }
                    inner.refresh();
                });
            });
        }

        page
    }

    pub fn root(&self) -> &gtk::Widget {
        self.inner.root.upcast_ref()
    }

    /// Called when the page becomes visible or hidden.
    pub fn set_active(&self, active: bool) {
        self.inner.active.set(active);
        if active {
            self.inner.refresh();
        }
    }

    pub fn refresh(&self) {
        self.inner.refresh();
    }

    pub fn is_active(&self) -> bool {
        self.inner.active.get()
    }
}

impl Inner {
    fn refresh(self: &Rc<Self>) {
        let sequence = self.sequence.get().wrapping_add(1);
        self.sequence.set(sequence);
        let Some(client) = self.ctx.client() else {
            self.placeholder("The Longcar daemon is unavailable");
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = client.get_audio_info().await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            if this.sequence.get() != sequence {
                return;
            }
            match result {
                Ok(json) => match serde_json::from_str::<AudioInfo>(&json) {
                    Ok(info) => this.apply(&info),
                    Err(err) => {
                        let message = format!("Malformed audio info: {err}");
                        this.ctx.toast(&message);
                        this.placeholder(&message);
                    }
                },
                Err(err) => {
                    this.ctx.toast_err("Could not read audio information", &err);
                    this.placeholder(&err);
                }
            }
        });
    }

    fn apply(self: &Rc<Self>, info: &AudioInfo) {
        self.syncing.set(true);
        match &info.default_sink {
            Some(sink) => {
                let title = if sink.description.is_empty() {
                    sink.name.clone()
                } else {
                    format!("{} — {}", sink.name, sink.description)
                };
                self.host_row.set_title(&title);
                self.host_row.set_subtitle("Default output");
                self.host_volume.set_sensitive(true);
                self.host_mute.set_sensitive(true);
                self.host_volume.set_value(f64::from(sink.percent()));
                self.host_volume_label
                    .set_text(&format!("{}%", sink.percent()));
                self.host_mute.set_active(sink.muted);
                self.host_badge.set_visible(sink.silent());
            }
            None => {
                self.host_row.set_title("No default output");
                self.host_row
                    .set_subtitle("PipeWire reported no default sink");
                self.host_volume.set_sensitive(false);
                self.host_mute.set_sensitive(false);
                self.host_volume_label.set_text("—");
                self.host_mute.set_active(false);
                self.host_badge.set_visible(false);
            }
        }
        *self.default_sink.borrow_mut() = info.default_sink.clone();
        self.stale_row.set_visible(info.stale_default);
        self.restore.set_sensitive(info.stale_default);
        self.auto_restore.set_active(info.restore_host_audio);
        self.syncing.set(false);

        self.rebuild_sinks(info);
        self.rebuild_screens(info);
    }

    fn placeholder(&self, message: &str) {
        self.syncing.set(true);
        self.host_row.set_title("Audio information unavailable");
        self.host_row.set_subtitle(message);
        self.host_badge.set_visible(false);
        self.stale_row.set_visible(false);
        self.restore.set_sensitive(false);
        self.host_volume.set_sensitive(false);
        self.host_mute.set_sensitive(false);
        self.host_volume_label.set_text("—");
        *self.default_sink.borrow_mut() = None;
        self.syncing.set(false);
        *self.sinks_signature.borrow_mut() = String::new();
        *self.screens_signature.borrow_mut() = String::new();
        clear_list(&self.sinks_list, message);
        clear_list(&self.screens_list, "Per-screen audio is unavailable");
    }

    fn rebuild_sinks(self: &Rc<Self>, info: &AudioInfo) {
        let signature = info
            .sinks
            .iter()
            .map(|sink| {
                format!(
                    "{}|{}|{}|{}|{:.2}|{}",
                    sink.id, sink.name, sink.description, sink.is_default, sink.volume, sink.muted
                )
            })
            .collect::<Vec<_>>()
            .join("\u{1f}");
        if *self.sinks_signature.borrow() == signature {
            return;
        }
        *self.sinks_signature.borrow_mut() = signature;
        while let Some(child) = self.sinks_list.first_child() {
            self.sinks_list.remove(&child);
        }
        if info.sinks.is_empty() {
            self.sinks_list
                .append(&util::dim_label("PipeWire reported no sinks"));
            return;
        }
        for sink in &info.sinks {
            let row = adw::ActionRow::builder().title(sink.title()).build();
            let suffix = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            suffix.set_valign(gtk::Align::Center);
            let name = gtk::Label::new(Some(&sink.name));
            name.add_css_class("longcar-mono");
            name.add_css_class("caption");
            name.add_css_class("dim-label");
            name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            name.set_max_width_chars(42);
            name.set_selectable(true);
            suffix.append(&name);
            if sink.is_default {
                let badge = gtk::Label::new(Some("default"));
                badge.add_css_class("caption");
                badge.add_css_class("accent");
                suffix.append(&badge);
            }
            let volume = gtk::Label::new(Some(&format!("{}%", sink.percent())));
            volume.add_css_class("caption");
            volume.add_css_class("dim-label");
            suffix.append(&volume);
            if sink.silent() {
                let badge = gtk::Label::new(Some("MUTED"));
                badge.add_css_class("caption");
                badge.add_css_class("error");
                suffix.append(&badge);
            }
            let mute = gtk::ToggleButton::with_label("Mute");
            mute.add_css_class("flat");
            mute.set_valign(gtk::Align::Center);
            mute.set_active(sink.muted);
            mute.set_tooltip_text(Some("Mute or unmute this host sink"));
            suffix.append(&mute);
            row.add_suffix(&suffix);
            self.sinks_list.append(&row);

            let inner = self.clone();
            let sink_id = sink.id;
            mute.connect_toggled(move |button| {
                if inner.syncing.get() {
                    return;
                }
                let muted = button.is_active();
                let Some(client) = inner.ctx.client() else {
                    inner.ctx.toast("The Longcar daemon is unavailable");
                    inner.refresh();
                    return;
                };
                let inner = inner.clone();
                glib::spawn_future_local(async move {
                    match client.set_sink_muted(sink_id, muted).await {
                        Ok(()) => {
                            inner
                                .ctx
                                .toast(if muted { "Sink muted" } else { "Sink unmuted" })
                        }
                        Err(err) => inner.ctx.toast_err("Could not mute the sink", &err),
                    }
                    inner.refresh();
                });
            });
        }
    }

    fn rebuild_screens(self: &Rc<Self>, info: &AudioInfo) {
        let mut sink_names: Vec<String> = info.sinks.iter().map(|sink| sink.name.clone()).collect();
        sink_names.dedup();
        let mut signature_parts: Vec<String> = sink_names.clone();
        signature_parts.extend(info.screens.iter().map(|screen| {
            format!(
                "{}|{}|{}|{}|{}|{}",
                screen.id,
                screen.name,
                screen.state,
                screen.audio,
                screen.audio_sink,
                screen.effective_sink
            )
        }));
        let signature = signature_parts.join("\u{1f}");
        if *self.screens_signature.borrow() == signature {
            return;
        }
        *self.screens_signature.borrow_mut() = signature;
        while let Some(child) = self.screens_list.first_child() {
            self.screens_list.remove(&child);
        }
        if info.screens.is_empty() {
            self.screens_list
                .append(&util::dim_label("No screens are configured"));
            return;
        }

        let mut labels = vec!["Host default".to_string()];
        labels.extend(sink_names.iter().cloned());
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();

        for screen in &info.screens {
            let streaming = screen.state == "streaming";
            let row = gtk::ListBoxRow::new();
            row.set_activatable(false);
            row.set_selectable(false);
            let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
            content.set_margin_top(10);
            content.set_margin_bottom(10);
            content.set_margin_start(12);
            content.set_margin_end(12);

            let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
            text.set_hexpand(true);
            let name = gtk::Label::new(Some(&screen.name));
            name.add_css_class("heading");
            name.set_xalign(0.0);
            let detail = gtk::Label::new(Some(&screen_detail(screen)));
            detail.add_css_class("caption");
            detail.add_css_class("dim-label");
            detail.set_xalign(0.0);
            detail.set_wrap(true);
            text.append(&name);
            text.append(&detail);
            let switch = gtk::Switch::builder()
                .active(screen.audio)
                .valign(gtk::Align::Center)
                .sensitive(!streaming)
                .tooltip_text(if streaming {
                    "Unavailable while this screen is streaming"
                } else {
                    "Stream this screen's audio to the client"
                })
                .build();
            top.append(&text);
            top.append(&switch);
            content.append(&top);

            let sink_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let sink_label = gtk::Label::new(Some("Sink"));
            sink_label.add_css_class("caption");
            sink_label.add_css_class("dim-label");
            let dropdown = gtk::DropDown::from_strings(&label_refs);
            dropdown.set_hexpand(true);
            dropdown.set_sensitive(!streaming);
            dropdown.set_tooltip_text(Some(
                "Host sink Sunshine captures for this screen; Host default follows the host output",
            ));
            let selected = if screen.audio_sink.is_empty() {
                0
            } else {
                sink_names
                    .iter()
                    .position(|name| *name == screen.audio_sink)
                    .map(|index| index + 1)
                    .unwrap_or(0)
            };
            dropdown.set_selected(selected as u32);
            sink_line.append(&sink_label);
            sink_line.append(&dropdown);
            content.append(&sink_line);

            row.set_child(Some(&content));
            self.screens_list.append(&row);

            let inner = self.clone();
            let id = screen.id.clone();
            switch.connect_active_notify(move |switch| {
                if inner.syncing.get() {
                    return;
                }
                let enabled = switch.is_active();
                let Some(client) = inner.ctx.client() else {
                    inner.ctx.toast("The Longcar daemon is unavailable");
                    inner.refresh();
                    return;
                };
                let inner = inner.clone();
                let id = id.clone();
                glib::spawn_future_local(async move {
                    match client.set_screen_audio(&id, enabled).await {
                        Ok(()) => inner.ctx.toast("Screen audio updated"),
                        Err(err) => inner.ctx.toast_err("Could not change screen audio", &err),
                    }
                    inner.refresh();
                });
            });

            let inner = self.clone();
            let id = screen.id.clone();
            let sink_names = sink_names.clone();
            dropdown.connect_selected_notify(move |dropdown| {
                if inner.syncing.get() {
                    return;
                }
                let index = dropdown.selected() as usize;
                let sink = match index {
                    0 => String::new(),
                    _ => match sink_names.get(index - 1) {
                        Some(name) => name.clone(),
                        None => return,
                    },
                };
                let Some(client) = inner.ctx.client() else {
                    inner.ctx.toast("The Longcar daemon is unavailable");
                    inner.refresh();
                    return;
                };
                let inner = inner.clone();
                let id = id.clone();
                glib::spawn_future_local(async move {
                    match client.set_screen_audio_sink(&id, &sink).await {
                        Ok(()) => inner.ctx.toast("Audio sink updated"),
                        Err(err) => inner.ctx.toast_err("Could not change the audio sink", &err),
                    }
                    inner.refresh();
                });
            });
        }
    }
}

fn screen_detail(screen: &AudioScreen) -> String {
    let effective = if screen.effective_sink.is_empty() {
        "no sink".to_string()
    } else {
        screen.effective_sink.clone()
    };
    format!(
        "{} · audio {} · effective sink {}",
        util::enum_label(&screen.state),
        if screen.audio { "on" } else { "off" },
        effective
    )
}

fn clear_list(list: &gtk::ListBox, message: &str) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    list.append(&util::dim_label(message));
}
