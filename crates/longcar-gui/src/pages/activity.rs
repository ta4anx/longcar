// SPDX-License-Identifier: GPL-3.0-or-later

//! Activity page: live bitrate sparklines, stream details and session history.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use longcar_core::model::ScreenState;
use longcar_core::state::{ScreenEntry, SessionRecord, StateDoc};

use crate::client::Ctx;
use crate::util;

#[derive(Clone)]
pub struct ActivityPage {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    root: gtk::Box,
    cards: gtk::Box,
    history: gtk::ListBox,
    samples: Rc<RefCell<HashMap<String, VecDeque<u32>>>>,
    card_widgets: RefCell<HashMap<String, CardWidgets>>,
    history_records: RefCell<Vec<SessionRecord>>,
    signature: RefCell<String>,
    timer: RefCell<Option<glib::SourceId>>,
    last_refresh: Cell<Option<Instant>>,
}

#[derive(Clone)]
struct CardWidgets {
    fps: gtk::Label,
    codec: gtk::Label,
    resolution: gtk::Label,
    bitrate: gtk::Label,
    duration: gtk::Label,
    area: gtk::DrawingArea,
}

struct Sparkline;

impl Sparkline {
    fn build(samples: Rc<RefCell<HashMap<String, VecDeque<u32>>>>, id: String) -> gtk::DrawingArea {
        let area = gtk::DrawingArea::builder()
            .content_height(64)
            .hexpand(true)
            .tooltip_text("Bitrate over the last 60 samples")
            .build();
        area.add_css_class("layout-preview");
        area.set_draw_func(move |area, cr, width, height| {
            let data = samples.borrow();
            let Some(values) = data.get(&id) else {
                return;
            };
            let width = f64::from(width);
            let height = f64::from(height);
            let foreground = area.color();
            let accent = adw::StyleManager::default().accent_color_rgba();
            let fg_red = f64::from(foreground.red());
            let fg_green = f64::from(foreground.green());
            let fg_blue = f64::from(foreground.blue());
            cr.set_line_width(1.5);
            cr.set_source_rgba(fg_red, fg_green, fg_blue, 0.25);
            cr.rectangle(0.0, 0.0, width, height);
            let _ = cr.stroke();
            if values.len() < 2 {
                return;
            }
            let max = values.iter().copied().max().unwrap_or(1).max(1) as f64;
            let step = width / (values.len() - 1).max(1) as f64;
            for (index, value) in values.iter().enumerate() {
                let x = index as f64 * step;
                let y = height - (f64::from(*value) / max) * (height - 6.0) - 3.0;
                if index == 0 {
                    cr.move_to(x, y);
                } else {
                    cr.line_to(x, y);
                }
            }
            cr.set_source_rgba(
                f64::from(accent.red()),
                f64::from(accent.green()),
                f64::from(accent.blue()),
                0.95,
            );
            let _ = cr.stroke();
        });
        area
    }
}

impl ActivityPage {
    pub fn new(ctx: Ctx) -> Rc<Self> {
        let builder = gtk::Builder::from_resource("/io/github/ta4anx/Longcar/ui/activity_page.ui");
        let root: gtk::Box = builder.object("activity_page").expect("activity_page");
        let cards: gtk::Box = builder.object("activity_cards").expect("activity cards");
        let history: gtk::ListBox = builder.object("history_list").expect("history list");
        history.add_css_class("boxed-list");
        let save: gtk::Button = builder
            .object("save_diagnostics_button")
            .expect("save button");
        let refresh: gtk::Button = builder
            .object("refresh_history_button")
            .expect("refresh history");

        let page = Rc::new(Self {
            inner: Rc::new(Inner {
                ctx,
                root,
                cards,
                history,
                samples: Rc::new(RefCell::new(HashMap::new())),
                card_widgets: RefCell::new(HashMap::new()),
                history_records: RefCell::new(Vec::new()),
                signature: RefCell::new(String::new()),
                timer: RefCell::new(None),
                last_refresh: Cell::new(None),
            }),
        });

        {
            let inner = page.inner.clone();
            save.connect_clicked(move |_| inner.save_diagnostics());
        }
        {
            let inner = page.inner.clone();
            refresh.connect_clicked(move |_| {
                let inner = inner.clone();
                inner.refresh_history();
            });
        }

        page
    }

    pub fn root(&self) -> &gtk::Widget {
        self.inner.root.upcast_ref()
    }

    pub fn update(self: &Rc<Self>, state: &StateDoc) {
        let signature = state
            .screens
            .iter()
            .map(|screen| {
                format!(
                    "{}|{}|{}|{}",
                    screen.id,
                    screen.state.as_str(),
                    screen.connected,
                    screen.error.is_some()
                )
            })
            .collect::<Vec<_>>()
            .join("\u{1f}");

        {
            let mut samples = self.inner.samples.borrow_mut();
            let active: Vec<&str> = state
                .screens
                .iter()
                .filter(|screen| screen.state.is_active())
                .map(|screen| screen.id.as_str())
                .collect();
            samples.retain(|id, _| active.contains(&id.as_str()));
            for screen in &state.screens {
                if screen.state.is_active() {
                    let queue = samples.entry(screen.id.clone()).or_default();
                    if queue.len() >= 60 {
                        queue.pop_front();
                    }
                    queue.push_back(screen.bitrate_kbps);
                }
            }
        }

        if *self.inner.signature.borrow() != signature {
            *self.inner.signature.borrow_mut() = signature;
            self.inner.rebuild_cards(state);
        } else {
            for screen in &state.screens {
                self.inner.update_card(screen);
            }
        }

        if state
            .screens
            .iter()
            .any(|screen| matches!(screen.state, ScreenState::Streaming | ScreenState::Ready))
        {
            let due = self
                .inner
                .last_refresh
                .get()
                .map(|instant| instant.elapsed() > Duration::from_secs(10))
                .unwrap_or(true);
            if due {
                self.inner.refresh_history();
            }
        }
    }

    pub fn set_active(self: &Rc<Self>, active: bool) {
        if active {
            self.inner.refresh_history();
            if self.inner.timer.borrow().is_none() {
                let weak = Rc::downgrade(&self.inner);
                let id = glib::timeout_add_seconds_local(15, move || {
                    if let Some(inner) = weak.upgrade() {
                        inner.refresh_history();
                    }
                    glib::ControlFlow::Continue
                });
                *self.inner.timer.borrow_mut() = Some(id);
            }
        } else if let Some(timer) = self.inner.timer.borrow_mut().take() {
            timer.remove();
        }
    }
}

impl Inner {
    fn rebuild_cards(&self, state: &StateDoc) {
        while let Some(child) = self.cards.first_child() {
            self.cards.remove(&child);
        }
        self.card_widgets.borrow_mut().clear();
        let active: Vec<&ScreenEntry> = state
            .screens
            .iter()
            .filter(|screen| screen.state.is_active())
            .collect();
        if active.is_empty() {
            let status = adw::StatusPage::new();
            status.set_icon_name(Some("utilities-system-monitor-symbolic"));
            status.set_title("Nothing is streaming");
            status.set_description(Some(
                "Start a screen to watch its bitrate, codec and session.",
            ));
            self.cards.append(&status);
            return;
        }
        for screen in active {
            let card = self.build_card(screen);
            self.cards.append(&card);
        }
    }

    fn build_card(&self, screen: &ScreenEntry) -> gtk::Widget {
        let container = gtk::Box::new(gtk::Orientation::Vertical, 8);
        container.add_css_class("card");
        container.set_margin_top(4);
        container.set_margin_bottom(4);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let title = gtk::Label::new(Some(&screen.name));
        title.add_css_class("title-4");
        title.set_xalign(0.0);
        title.set_hexpand(true);
        let state = gtk::Label::new(Some(util::state_label(screen.state)));
        state.add_css_class(util::state_css_class(screen.state));
        header.append(&title);
        header.append(&state);
        container.append(&header);

        let area = Sparkline::build(self.samples.clone(), screen.id.clone());
        container.append(&area);

        let stats = gtk::Grid::builder()
            .row_spacing(2)
            .column_spacing(16)
            .build();
        let fps = util::dim_label("");
        let codec = util::dim_label("");
        let resolution = util::dim_label("");
        let bitrate = util::dim_label("");
        let duration = util::dim_label("");
        for (index, (caption, label)) in [
            ("Bitrate", &bitrate),
            ("FPS", &fps),
            ("Codec", &codec),
            ("Resolution", &resolution),
            ("Duration", &duration),
        ]
        .iter()
        .enumerate()
        {
            let caption_label = gtk::Label::new(Some(caption));
            caption_label.add_css_class("caption");
            caption_label.add_css_class("dim-label");
            caption_label.set_xalign(0.0);
            stats.attach(&caption_label, 0, index as i32, 1, 1);
            stats.attach(*label, 1, index as i32, 1, 1);
        }
        container.append(&stats);

        let widgets = CardWidgets {
            fps,
            codec,
            resolution,
            bitrate,
            duration,
            area,
        };
        self.card_widgets
            .borrow_mut()
            .insert(screen.id.clone(), widgets);
        self.update_card(screen);
        container.upcast()
    }

    fn update_card(&self, screen: &ScreenEntry) {
        let widgets = self.card_widgets.borrow();
        let Some(card) = widgets.get(&screen.id) else {
            return;
        };
        card.fps.set_text(&format!("{} fps", screen.fps));
        card.codec.set_text(&format!(
            "{}{}",
            util::enum_label(&format!("{:?}", screen.encoder).to_lowercase()),
            if screen.connected {
                " · connected"
            } else {
                ""
            }
        ));
        card.resolution
            .set_text(&format!("{} × {}", screen.width, screen.height));
        card.bitrate
            .set_text(&format!("{} kbps", screen.bitrate_kbps));
        card.duration.set_text(&self.duration_for(&screen.id));
        card.area.queue_draw();
    }

    fn duration_for(&self, screen_id: &str) -> String {
        let records = self.history_records.borrow();
        let Some(record) = records
            .iter()
            .find(|record| record.screen_id == screen_id && record.ended_at.is_none())
        else {
            return "—".to_string();
        };
        let Ok(started) = glib::DateTime::from_iso8601(&record.started_at, None) else {
            return "—".to_string();
        };
        let Ok(now) = glib::DateTime::now_utc() else {
            return "—".to_string();
        };
        let seconds = (now.to_unix() - started.to_unix()).max(0);
        format_duration(seconds)
    }

    fn refresh_history(self: &Rc<Self>) {
        let Some(client) = self.ctx.client() else {
            return;
        };
        let weak = Rc::downgrade(self);
        self.last_refresh.set(Some(Instant::now()));
        glib::spawn_future_local(async move {
            let Ok(json) = client.get_history(50).await else {
                return;
            };
            let Ok(records) = serde_json::from_str::<Vec<SessionRecord>>(&json) else {
                return;
            };
            let Some(this) = weak.upgrade() else {
                return;
            };
            *this.history_records.borrow_mut() = records;
            this.render_history();
            for (id, card) in this.card_widgets.borrow().iter() {
                card.duration.set_text(&this.duration_for(id));
            }
        });
    }

    fn render_history(&self) {
        while let Some(child) = self.history.first_child() {
            self.history.remove(&child);
        }
        let records = self.history_records.borrow();
        if records.is_empty() {
            self.history
                .append(&util::dim_label("No session history yet"));
            return;
        }
        for record in records.iter() {
            let title = format!(
                "{} · {}",
                record.screen_id,
                record.started_at.replace(['T', 'Z'], " ")
            );
            let ended = record
                .ended_at
                .as_deref()
                .map(|value| value.replace(['T', 'Z'], " "))
                .unwrap_or_else(|| "active".to_string());
            let mut parts = vec![format!("ended {ended}")];
            if let Some(fps) = record.fps {
                parts.push(format!("{fps} fps"));
            }
            if let (Some(width), Some(height)) = (record.width, record.height) {
                parts.push(format!("{width} × {height}"));
            }
            if let Some(kbps) = record.avg_bitrate_kbps {
                parts.push(format!("{kbps} kbps avg"));
            }
            if let Some(kbps) = record.max_bitrate_kbps {
                parts.push(format!("{kbps} kbps max"));
            }
            if let Some(reason) = &record.reason {
                parts.push(reason.clone());
            }
            let row = adw::ActionRow::builder()
                .title(&title)
                .subtitle(parts.join(" · "))
                .build();
            self.history.append(&row);
        }
    }

    fn save_diagnostics(&self) {
        let state = self.ctx.state();
        let Some(state) = state else {
            self.ctx.toast("No daemon state to save");
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title("Save diagnostics")
            .initial_name("longcar-diagnostics.txt")
            .build();
        let ctx = self.ctx.clone();
        let history = self.history_records.borrow().clone();
        let future = dialog.save_future(Some(&ctx.window));
        glib::spawn_future_local(async move {
            let file = match future.await {
                Ok(file) => file,
                Err(err) => {
                    if !err.matches(gtk::DialogError::Dismissed) {
                        ctx.toast_err("Could not save diagnostics", &err.to_string());
                    }
                    return;
                }
            };
            let Some(path) = file.path() else {
                ctx.toast("The selected location has no local path");
                return;
            };
            let history_json =
                serde_json::to_string_pretty(&history).unwrap_or_else(|_| "[]".to_string());
            let text = format!(
                "Longcar diagnostics\nversion: {}\nhost: {}\n\n## State\n{}\n\n## Sessions\n{}\n",
                env!("CARGO_PKG_VERSION"),
                glib::host_name(),
                util::redact(&state.to_json()),
                util::redact(&history_json)
            );
            match std::fs::write(&path, text) {
                Ok(()) => ctx.toast(&format!("Diagnostics saved to {}", path.display())),
                Err(err) => ctx.toast_err("Could not write diagnostics", &err.to_string()),
            }
        });
    }
}

fn format_duration(seconds: i64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}
