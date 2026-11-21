// SPDX-License-Identifier: GPL-3.0-or-later

//! Main window: sidebar navigation, header start menu, banner and daemon wiring.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use longcar_core::model::warnings;
use longcar_core::state::StateDoc;
use serde_json::json;

use crate::client::{Client, Ctx, DaemonWatcher};
use crate::pages::activity::ActivityPage;
use crate::pages::devices::DevicesPage;
use crate::pages::network::NetworkPage;
use crate::pages::presets::PresetsPage;
use crate::pages::screens::ScreensPage;
use crate::pages::settings::SettingsPage;
use crate::util;

const PAGES: [&str; 6] = [
    "screens", "presets", "devices", "network", "activity", "settings",
];

thread_local! {
    static WINDOW: RefCell<Option<Rc<AppWindow>>> = const { RefCell::new(None) };
}

pub struct AppWindow {
    window: adw::ApplicationWindow,
    ctx: Ctx,
    banner: adw::Banner,
    stack: gtk::Stack,
    sidebar: gtk::ListBox,
    start_button: gtk::MenuButton,
    start_popover_box: gtk::Box,
    unavailable_error: gtk::Label,
    screens: Rc<ScreensPage>,
    presets: Rc<PresetsPage>,
    devices: Rc<DevicesPage>,
    network: Rc<NetworkPage>,
    activity: Rc<ActivityPage>,
    settings: Rc<SettingsPage>,
    state: Rc<RefCell<Option<StateDoc>>>,
    client_slot: Rc<RefCell<Option<Client>>>,
    watcher: RefCell<Option<DaemonWatcher>>,
    onboarding_shown: Cell<bool>,
    capture_only: bool,
    last_page: Rc<RefCell<String>>,
    start_menu_signature: RefCell<String>,
}

/// Create (or focus) the application window.
pub fn present(app: &adw::Application) {
    WINDOW.with(|slot| {
        if let Some(window) = slot.borrow().as_ref() {
            window.window.present();
            return;
        }
        let window = Rc::new(AppWindow::build(app));
        window.window.present();
        register_app_actions(app);
        AppWindow::start_watcher(&window);
        *slot.borrow_mut() = Some(window);
    });
}

impl AppWindow {
    fn build(app: &adw::Application) -> Self {
        let builder = gtk::Builder::from_resource("/io/github/ta4anx/Longcar/ui/window.ui");
        let window: adw::ApplicationWindow = builder.object("window").expect("window.ui");
        window.set_application(Some(app));
        let toast_overlay: adw::ToastOverlay =
            builder.object("toast_overlay").expect("toast overlay");
        let banner: adw::Banner = builder.object("warning_banner").expect("banner");
        let stack: gtk::Stack = builder.object("page_stack").expect("page stack");
        let sidebar: gtk::ListBox = builder.object("sidebar").expect("sidebar");
        sidebar.add_css_class("navigation-sidebar");
        let start_button: gtk::MenuButton = builder.object("start_button").expect("start button");
        let start_popover_box: gtk::Box = builder
            .object("start_popover_box")
            .expect("start popover box");
        let app_menu_button: gtk::MenuButton =
            builder.object("app_menu_button").expect("app menu button");

        {
            let popover = gtk::Popover::new();
            let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
            content.set_margin_top(6);
            content.set_margin_bottom(6);
            content.set_margin_start(6);
            content.set_margin_end(6);
            let about = gtk::Button::with_label("About Longcar");
            about.add_css_class("flat");
            about.set_tooltip_text(Some("Version, license and issue tracker"));
            about.set_action_name(Some("app.about"));
            let quit = gtk::Button::with_label("Quit");
            quit.add_css_class("flat");
            quit.set_tooltip_text(Some("Quit Longcar (the daemon keeps running)"));
            quit.set_action_name(Some("app.quit"));
            content.append(&about);
            content.append(&quit);
            popover.set_child(Some(&content));
            app_menu_button.set_popover(Some(&popover));
        }

        let client_slot: Rc<RefCell<Option<Client>>> = Rc::new(RefCell::new(None));
        let state: Rc<RefCell<Option<StateDoc>>> = Rc::new(RefCell::new(None));
        let last_page: Rc<RefCell<String>> = Rc::new(RefCell::new("screens".to_string()));

        let unavailable_error = gtk::Label::new(None);
        unavailable_error.set_selectable(true);
        unavailable_error.set_wrap(true);
        unavailable_error.set_xalign(0.0);
        unavailable_error.add_css_class("dim-label");

        let ctx = Ctx {
            window: window.clone(),
            toast_overlay: toast_overlay.clone(),
            client: client_slot.clone(),
            state: state.clone(),
            open_logs: {
                let window = window.clone();
                let client_slot = client_slot.clone();
                Rc::new(move || show_logs(&window, &client_slot))
            },
        };

        let screens = ScreensPage::new(ctx.clone());
        let presets = PresetsPage::new(ctx.clone());
        let devices = DevicesPage::new(ctx.clone());
        let network = NetworkPage::new(ctx.clone());
        let activity = ActivityPage::new(ctx.clone());
        let settings = SettingsPage::new(ctx.clone());

        stack.add_named(screens.root(), Some("screens"));
        stack.add_named(presets.root(), Some("presets"));
        stack.add_named(devices.root(), Some("devices"));
        stack.add_named(network.root(), Some("network"));
        stack.add_named(activity.root(), Some("activity"));
        stack.add_named(settings.root(), Some("settings"));

        let unavailable = build_unavailable_page(&unavailable_error);
        stack.add_named(&unavailable, Some("unavailable"));

        {
            let stack = stack.clone();
            let last_page = last_page.clone();
            sidebar.connect_row_selected(move |_, row| {
                let Some(row) = row else {
                    return;
                };
                if let Some(name) = PAGES.get(row.index() as usize) {
                    stack.set_visible_child_name(name);
                    if *name != "unavailable" {
                        *last_page.borrow_mut() = (*name).to_string();
                    }
                }
            });
        }
        {
            let screens = screens.clone();
            let devices = devices.clone();
            let activity = activity.clone();
            let last_page = last_page.clone();
            stack.connect_visible_child_name_notify(move |stack| {
                let name = stack
                    .visible_child_name()
                    .map(|name| name.to_string())
                    .unwrap_or_default();
                screens.set_active(name == "screens");
                devices.set_active(name == "devices");
                activity.set_active(name == "activity");
                if !name.is_empty() && name != "unavailable" {
                    *last_page.borrow_mut() = name;
                }
            });
        }

        let app_window = Self {
            window,
            ctx,
            banner,
            stack,
            sidebar,
            start_button,
            start_popover_box,
            unavailable_error,
            screens,
            presets,
            devices,
            network,
            activity,
            settings,
            state,
            client_slot,
            watcher: RefCell::new(None),
            onboarding_shown: Cell::new(false),
            capture_only: std::env::var_os("LONGCAR_SHOT_DIR").is_some(),
            last_page,
            start_menu_signature: RefCell::new(String::new()),
        };

        app_window.add_window_actions();
        app_window.banner.set_revealed(false);
        app_window.banner.connect_button_clicked({
            let ctx = app_window.ctx.clone();
            move |_| {
                let Some(client) = ctx.client() else {
                    ctx.toast("The Longcar daemon is unavailable");
                    return;
                };
                let ctx = ctx.clone();
                glib::spawn_future_local(async move {
                    match client.restore_layout().await {
                        Ok(()) => ctx.toast("Layout restored"),
                        Err(err) => ctx.toast_err("Could not restore layout", &err),
                    }
                });
            }
        });
        app_window.stack.set_visible_child_name("screens");
        if let Some(row) = app_window.sidebar.row_at_index(0) {
            app_window.sidebar.select_row(Some(&row));
        }
        app_window
    }

    fn add_window_actions(&self) {
        self.add_action("stop-all", {
            let ctx = self.ctx.clone();
            move || {
                let Some(client) = ctx.client() else {
                    ctx.toast("The Longcar daemon is unavailable");
                    return;
                };
                let ctx = ctx.clone();
                glib::spawn_future_local(async move {
                    match client.stop_all().await {
                        Ok(()) => ctx.toast("Stopping all screens"),
                        Err(err) => ctx.toast_err("Could not stop screens", &err),
                    }
                });
            }
        });
        self.add_action("start-default", {
            let ctx = self.ctx.clone();
            move || {
                let Some(client) = ctx.client() else {
                    ctx.toast("The Longcar daemon is unavailable");
                    return;
                };
                let ctx = ctx.clone();
                glib::spawn_future_local(async move {
                    match client.start_default().await {
                        Ok(()) => ctx.toast("Starting the default preset"),
                        Err(err) => ctx.toast_err("Could not start the default preset", &err),
                    }
                });
            }
        });
        self.add_action("start-all", {
            let ctx = self.ctx.clone();
            move || {
                let Some(client) = ctx.client() else {
                    ctx.toast("The Longcar daemon is unavailable");
                    return;
                };
                let ctx = ctx.clone();
                glib::spawn_future_local(async move {
                    match client.start_all().await {
                        Ok(()) => ctx.toast("Starting all screens"),
                        Err(err) => ctx.toast_err("Could not start screens", &err),
                    }
                });
            }
        });
        self.add_action("restore-layout", {
            let ctx = self.ctx.clone();
            move || {
                let Some(client) = ctx.client() else {
                    ctx.toast("The Longcar daemon is unavailable");
                    return;
                };
                let ctx = ctx.clone();
                glib::spawn_future_local(async move {
                    match client.restore_layout().await {
                        Ok(()) => ctx.toast("Layout restored"),
                        Err(err) => ctx.toast_err("Could not restore layout", &err),
                    }
                });
            }
        });
        self.add_action("open-logs", {
            let ctx = self.ctx.clone();
            move || (ctx.open_logs)()
        });
    }

    fn add_action(&self, name: &str, callback: impl Fn() + 'static) {
        let action = gio::SimpleAction::new(name, None);
        action.connect_activate(move |_, _| callback());
        self.window.add_action(&action);
    }

    fn start_watcher(window: &Rc<Self>) {
        let weak = Rc::downgrade(window);
        let on_state = {
            let weak = weak.clone();
            move |state: StateDoc| {
                if let Some(window) = weak.upgrade() {
                    AppWindow::apply_state(&window, state);
                }
            }
        };
        let on_ready = {
            let weak = weak.clone();
            move || {
                if let Some(window) = weak.upgrade() {
                    window
                        .stack
                        .set_visible_child_name(&window.last_page.borrow());
                    window.start_button.set_sensitive(true);
                }
            }
        };
        let on_unavailable = {
            let weak = weak.clone();
            move |message: String| {
                if let Some(window) = weak.upgrade() {
                    window.unavailable_error.set_text(&message);
                    window.stack.set_visible_child_name("unavailable");
                    window.start_button.set_sensitive(false);
                }
            }
        };
        let watcher = DaemonWatcher::new(
            window.client_slot.clone(),
            on_state,
            on_ready,
            on_unavailable,
        );
        // Retry uses the same watcher; expose it through a window action.
        let action = gio::SimpleAction::new("retry-daemon", None);
        {
            let weak = Rc::downgrade(window);
            action.connect_activate(move |_, _| {
                if let Some(window) = weak.upgrade()
                    && let Some(watcher) = window.watcher.borrow().as_ref()
                {
                    watcher.retry();
                }
            });
        }
        window.window.add_action(&action);
        *window.watcher.borrow_mut() = Some(watcher);
    }

    fn apply_state(window: &Rc<Self>, doc: StateDoc) {
        *window.state.borrow_mut() = Some(doc.clone());
        window.update_banner(&doc);
        window.refresh_start_menu(&doc);
        window.screens.update(&doc);
        window.presets.update(&doc);
        window.devices.update(&doc);
        window.network.update(&doc);
        window.activity.update(&doc);
        window.settings.update(&doc);

        if !doc.settings.onboarding_complete
            && !window.onboarding_shown.get()
            && !window.capture_only
            && !doc.screens.is_empty()
        {
            window.onboarding_shown.set(true);
            show_onboarding(window, &doc);
        }
    }

    fn update_banner(&self, doc: &StateDoc) {
        if doc.warnings.is_empty() {
            self.banner.set_revealed(false);
            return;
        }
        let mut labels: Vec<&str> = Vec::new();
        for warning in &doc.warnings {
            labels.push(match warning.as_str() {
                warnings::LAYOUT_RESTORE_AVAILABLE => {
                    "The previous desktop layout can be restored after you stop all screens"
                }
                warnings::PORTAL_TIMEOUT => {
                    "GNOME's screen picker was not answered in time; restart the screen to retry"
                }
                warnings::SUNSHINE_MISSING => {
                    "Sunshine is not installed; managed screens cannot start"
                }
                warnings::DAEMON_DEGRADED => {
                    "One or more screens failed; open Activity or Settings → Diagnostics for details"
                }
                warnings::RELATIVE_TO_INACTIVE => {
                    "A screen is placed relative to a screen that is not active"
                }
                warnings::SESSION_CLOSED => {
                    "GNOME closed the ScreenCast session; the layout snapshot is kept"
                }
                other => other,
            });
        }
        self.banner.set_title(&labels.join(" · "));
        let restore = doc
            .warnings
            .iter()
            .any(|warning| warning == warnings::LAYOUT_RESTORE_AVAILABLE);
        self.banner
            .set_button_label(if restore { Some("Restore") } else { None });
        self.banner.set_revealed(true);
    }

    fn refresh_start_menu(&self, doc: &StateDoc) {
        let signature = doc
            .presets
            .iter()
            .map(|preset| format!("{}:{}", preset.id, preset.name))
            .chain(std::iter::once(doc.settings.default_preset.clone()))
            .collect::<Vec<_>>()
            .join("|");
        if *self.start_menu_signature.borrow() == signature {
            return;
        }
        *self.start_menu_signature.borrow_mut() = signature;

        while let Some(child) = self.start_popover_box.first_child() {
            self.start_popover_box.remove(&child);
        }
        let popover = self.start_button.popover();
        let default_name = doc
            .presets
            .iter()
            .find(|preset| preset.id == doc.settings.default_preset)
            .map(|preset| preset.name.clone());

        let label = match &default_name {
            Some(name) => format!("Start default preset ({name})"),
            None => "Start default preset".to_string(),
        };
        let start_default = gtk::Button::with_label(&label);
        start_default.add_css_class("flat");
        start_default.set_tooltip_text(Some("Start the screens of the default preset"));
        {
            let ctx = self.ctx.clone();
            let popover = popover.clone();
            start_default.connect_clicked(move |_| {
                if let Some(popover) = &popover {
                    popover.popdown();
                }
                let Some(client) = ctx.client() else {
                    ctx.toast("The Longcar daemon is unavailable");
                    return;
                };
                let ctx = ctx.clone();
                glib::spawn_future_local(async move {
                    if let Err(err) = client.start_default().await {
                        ctx.toast_err("Could not start the default preset", &err);
                    }
                });
            });
        }
        self.start_popover_box.append(&start_default);

        let start_all = gtk::Button::with_label("Start all screens");
        start_all.add_css_class("flat");
        start_all.set_tooltip_text(Some("Start every configured virtual screen"));
        {
            let ctx = self.ctx.clone();
            let popover = popover.clone();
            start_all.connect_clicked(move |_| {
                if let Some(popover) = &popover {
                    popover.popdown();
                }
                let Some(client) = ctx.client() else {
                    ctx.toast("The Longcar daemon is unavailable");
                    return;
                };
                let ctx = ctx.clone();
                glib::spawn_future_local(async move {
                    if let Err(err) = client.start_all().await {
                        ctx.toast_err("Could not start screens", &err);
                    }
                });
            });
        }
        self.start_popover_box.append(&start_all);

        if !doc.presets.is_empty() {
            self.start_popover_box
                .append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            for preset in &doc.presets {
                let button = gtk::Button::with_label(&format!("Preset · {}", preset.name));
                button.add_css_class("flat");
                button.set_tooltip_text(Some("Start this preset"));
                {
                    let ctx = self.ctx.clone();
                    let popover = popover.clone();
                    let id = preset.id.clone();
                    let name = preset.name.clone();
                    button.connect_clicked(move |_| {
                        if let Some(popover) = &popover {
                            popover.popdown();
                        }
                        let Some(client) = ctx.client() else {
                            ctx.toast("The Longcar daemon is unavailable");
                            return;
                        };
                        let ctx = ctx.clone();
                        let id = id.clone();
                        let name = name.clone();
                        glib::spawn_future_local(async move {
                            match client.apply_preset(&id).await {
                                Ok(()) => ctx.toast(&format!("Starting {name}")),
                                Err(err) => ctx.toast_err("Could not start preset", &err),
                            }
                        });
                    });
                }
                self.start_popover_box.append(&button);
            }
        }
        self.start_button
            .set_tooltip_text(Some("Start a preset or all screens"));
    }
}

fn build_unavailable_page(error: &gtk::Label) -> adw::StatusPage {
    let page = adw::StatusPage::new();
    page.set_icon_name(Some("dialog-warning-symbolic"));
    page.set_title("Longcar daemon unavailable");
    page.set_description(Some(
        "The app could not reach the Longcar daemon on the session bus.",
    ));
    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.set_halign(gtk::Align::Center);
    content.set_margin_top(12);
    content.append(error);
    let command = gtk::Label::new(Some("systemctl --user start longcar-daemon.service"));
    command.add_css_class("longcar-mono");
    command.set_selectable(true);
    content.append(&command);
    let retry = gtk::Button::with_label("Retry");
    retry.add_css_class("suggested-action");
    retry.set_action_name(Some("win.retry-daemon"));
    retry.set_tooltip_text(Some("Try to reach the daemon again"));
    content.append(&retry);
    page.set_child(Some(&content));
    page
}

fn register_app_actions(app: &adw::Application) {
    if app.lookup_action("quit").is_none() {
        let quit = gio::SimpleAction::new("quit", None);
        let weak = app.downgrade();
        quit.connect_activate(move |_, _| {
            if let Some(app) = weak.upgrade() {
                app.quit();
            }
        });
        app.add_action(&quit);
    }
    if app.lookup_action("about").is_none() {
        let about = gio::SimpleAction::new("about", None);
        let weak = app.downgrade();
        about.connect_activate(move |_, _| {
            if weak.upgrade().is_some() {
                show_about();
            }
        });
        app.add_action(&about);
    }
}

fn show_about() {
    WINDOW.with(|slot| {
        let Some(window) = slot.borrow().as_ref().cloned() else {
            return;
        };
        let dialog = adw::AboutDialog::builder()
            .application_name("Longcar")
            .application_icon(longcar_core::model::APP_ID)
            .developer_name("Kaan Çakmak")
            .version(env!("CARGO_PKG_VERSION"))
            .license_type(gtk::License::Gpl30)
            .website("https://github.com/ta4anx/longcar")
            .issue_url("https://github.com/ta4anx/longcar/issues")
            .developers(vec!["Kaan Çakmak https://github.com/ta4anx".to_string()])
            .copyright("© 2026 Kaan Çakmak")
            .build();
        dialog.present(Some(&window.window));
    });
}

/// Debug hook: render every sidebar page to PNG under `LONGCAR_SHOT_DIR`.
///
/// Used for verification screenshots on sessions where the Shell screenshot API
/// is denied. It only captures the live window with live daemon state.
pub fn capture_pages(dir: String) {
    let Some(window) = WINDOW.with(|slot| slot.borrow().as_ref().cloned()) else {
        eprintln!("shot: the window is not available");
        return;
    };
    glib::spawn_future_local(async move {
        glib::timeout_future_seconds(2).await;
        if let Err(err) = std::fs::create_dir_all(&dir) {
            eprintln!("shot: cannot create {dir}: {err}");
            return;
        }
        for (index, name) in PAGES.iter().enumerate() {
            if let Some(row) = window.sidebar.row_at_index(index as i32) {
                window.sidebar.select_row(Some(&row));
            }
            glib::timeout_future(std::time::Duration::from_millis(450)).await;
            window.window.queue_draw();
            glib::timeout_future(std::time::Duration::from_millis(250)).await;
            save_shot(&window.window, &dir, name);
        }

        // The Settings page is taller than the window; capture its diagnostics
        // group too so verification can see the check results and self-test.
        if let Some(index) = PAGES.iter().position(|name| *name == "settings") {
            if let Some(row) = window.sidebar.row_at_index(index as i32) {
                window.sidebar.select_row(Some(&row));
            }
            glib::timeout_future(std::time::Duration::from_millis(300)).await;
            if let Some(scrolled) = find_scrolled(window.settings.root())
                && let Some(child) = scrolled.child()
                && let Some(bounds) = window.settings.diagnostics_widget().compute_bounds(&child)
            {
                let adjustment = scrolled.vadjustment();
                let max = (adjustment.upper() - adjustment.page_size()).max(0.0);
                adjustment.set_value((f64::from(bounds.y()) - 8.0).clamp(0.0, max));
            }
            glib::timeout_future(std::time::Duration::from_millis(450)).await;
            window.window.queue_draw();
            glib::timeout_future(std::time::Duration::from_millis(250)).await;
            save_shot(&window.window, &dir, "settings-diagnostics");
        }

        if let Some(app) = window.window.application() {
            app.quit();
        }
    });
}

fn save_shot(window: &adw::ApplicationWindow, dir: &str, name: &str) {
    let width = f64::from(window.width());
    let height = f64::from(window.height());
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, width, height);
    let Some(node) = snapshot.to_node() else {
        eprintln!("shot: no render node for {name}");
        return;
    };
    let Some(renderer) = window.renderer() else {
        eprintln!("shot: no renderer for {name}");
        return;
    };
    let texture = renderer.render_texture(&node, None);
    let path = std::path::Path::new(dir).join(format!("longcar-{name}.png"));
    match texture.save_to_png(&path) {
        Ok(()) => println!("shot: {}", path.display()),
        Err(err) => eprintln!("shot: cannot save {}: {err}", path.display()),
    }
}

fn find_scrolled(widget: &gtk::Widget) -> Option<gtk::ScrolledWindow> {
    if let Some(scrolled) = widget.downcast_ref::<gtk::ScrolledWindow>() {
        return Some(scrolled.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_scrolled(&current) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

fn show_logs(window: &adw::ApplicationWindow, client_slot: &Rc<RefCell<Option<Client>>>) {
    let builder = gtk::Builder::from_resource("/io/github/ta4anx/Longcar/ui/logs_dialog.ui");
    let dialog: adw::Dialog = builder.object("logs_dialog").expect("logs dialog");
    let text: gtk::TextView = builder.object("logs_text").expect("logs text");
    let close: gtk::Button = builder.object("close_button").expect("close button");
    let title: gtk::Label = builder.object("logs_title").expect("logs title");
    text.buffer().set_text("Loading logs…");

    {
        let dialog = dialog.clone();
        close.connect_clicked(move |_| {
            dialog.close();
        });
    }
    dialog.present(Some(window));

    let Some(client) = client_slot.borrow().clone() else {
        text.buffer().set_text("The Longcar daemon is unavailable.");
        return;
    };
    let text = text.clone();
    let title = title.clone();
    glib::spawn_future_local(async move {
        match client.get_logs(400).await {
            Ok(json) => match serde_json::from_str::<serde_json::Value>(&json) {
                Ok(value) => {
                    let mut out = String::new();
                    if let Some(lines) = value.get("daemon").and_then(|value| value.as_array()) {
                        out.push_str("== daemon ==\n");
                        for line in lines {
                            if let Some(line) = line.as_str() {
                                out.push_str(&util::redact(line));
                                out.push('\n');
                            }
                        }
                    }
                    if let Some(screens) = value.get("screens").and_then(|value| value.as_array()) {
                        for screen in screens {
                            let id = screen
                                .get("id")
                                .and_then(|value| value.as_str())
                                .unwrap_or("screen");
                            out.push_str(&format!("\n== {id} ==\n"));
                            if let Some(lines) =
                                screen.get("lines").and_then(|value| value.as_array())
                            {
                                for line in lines {
                                    if let Some(line) = line.as_str() {
                                        out.push_str(&util::redact(line));
                                        out.push('\n');
                                    }
                                }
                            }
                        }
                    }
                    text.buffer().set_text(&out);
                    if out.is_empty() {
                        text.buffer().set_text("No log lines were returned.");
                    }
                    title.set_text("Logs");
                }
                Err(err) => text
                    .buffer()
                    .set_text(&format!("Malformed logs payload: {err}")),
            },
            Err(err) => text
                .buffer()
                .set_text(&format!("Could not read logs: {err}")),
        }
    });
}

// ---------------------------------------------------------------------------
// Onboarding
// ---------------------------------------------------------------------------

fn show_onboarding(window: &Rc<AppWindow>, doc: &StateDoc) {
    let builder = gtk::Builder::from_resource("/io/github/ta4anx/Longcar/ui/onboarding_dialog.ui");
    let dialog: adw::Dialog = builder
        .object("onboarding_dialog")
        .expect("onboarding dialog");
    let resolution: gtk::DropDown = builder
        .object("tablet_resolution")
        .expect("tablet resolution");
    let width: gtk::SpinButton = builder.object("tablet_width").expect("tablet width");
    let height: gtk::SpinButton = builder.object("tablet_height").expect("tablet height");
    let custom_box: gtk::Box = builder.object("custom_size_box").expect("custom size box");
    let add_phone: adw::SwitchRow = builder.object("add_phone").expect("add phone");
    let doctor_summary: gtk::Label = builder.object("doctor_summary").expect("doctor summary");
    let check: gtk::Button = builder.object("check_doctor_button").expect("check doctor");
    let start_all: adw::SwitchRow = builder.object("start_all").expect("start all");
    let finish: gtk::Button = builder.object("finish_button").expect("finish");
    let later: gtk::Button = builder.object("later_button").expect("later");

    const TABLET_PRESETS: &[(u32, u32)] = &[(2560, 1600), (1920, 1200), (1280, 800)];
    let existing_tablet = doc
        .screens
        .iter()
        .find(|screen| screen.id == "tablet")
        .cloned();
    if let Some(tablet) = &existing_tablet {
        width.set_value(f64::from(tablet.width));
        height.set_value(f64::from(tablet.height));
    }
    custom_box.set_visible(false);

    {
        let custom_box = custom_box.clone();
        let width = width.clone();
        let height = height.clone();
        resolution.connect_selected_notify(move |dropdown| {
            let index = dropdown.selected() as usize;
            if let Some((w, h)) = TABLET_PRESETS.get(index) {
                width.set_value(f64::from(*w));
                height.set_value(f64::from(*h));
                custom_box.set_visible(false);
            } else {
                custom_box.set_visible(true);
            }
        });
    }
    {
        let ctx = window.ctx.clone();
        let doctor_summary = doctor_summary.clone();
        check.connect_clicked(move |button| {
            let Some(client) = ctx.client() else {
                doctor_summary.set_text("The Longcar daemon is unavailable");
                return;
            };
            button.set_sensitive(false);
            let doctor_summary = doctor_summary.clone();
            let button = button.clone();
            glib::spawn_future_local(async move {
                match client.get_doctor().await {
                    Ok(json) => {
                        let summary = match serde_json::from_str::<serde_json::Value>(&json) {
                            Ok(value) => {
                                let checks = value
                                    .get("checks")
                                    .and_then(|value| value.as_array())
                                    .cloned()
                                    .unwrap_or_default();
                                let mut errors = 0;
                                let mut warnings_count = 0;
                                for check in &checks {
                                    match check.get("status").and_then(|value| value.as_str()) {
                                        Some("error") => errors += 1,
                                        Some("ok") => {}
                                        _ => warnings_count += 1,
                                    }
                                }
                                format!(
                                    "{errors} errors · {warnings_count} warnings. Open Doctor for details and fixes."
                                )
                            }
                            Err(err) => format!("Malformed doctor output: {err}"),
                        };
                        doctor_summary.set_text(&summary);
                    }
                    Err(err) => doctor_summary.set_text(&format!("Could not run checks: {err}")),
                }
                button.set_sensitive(true);
            });
        });
    }
    {
        let dialog = dialog.clone();
        later.connect_clicked(move |_| {
            dialog.close();
        });
    }
    {
        let ctx = window.ctx.clone();
        let dialog = dialog.clone();
        let width = width.clone();
        let height = height.clone();
        let start_all = start_all.clone();
        let add_phone = add_phone.clone();
        let existing_tablet = existing_tablet.clone();
        finish.connect_clicked(move |button| {
            let width_value = width.value() as u32;
            let height_value = height.value() as u32;
            if !width_value.is_multiple_of(2)
                || !height_value.is_multiple_of(2)
                || !(640..=7680).contains(&width_value)
                || !(640..=7680).contains(&height_value)
            {
                ctx.toast("Width and height must be even between 640 and 7680");
                return;
            }
            let Some(client) = ctx.client() else {
                ctx.toast("The Longcar daemon is unavailable");
                return;
            };
            button.set_sensitive(false);
            let ctx = ctx.clone();
            let dialog = dialog.clone();
            let add_phone = add_phone.is_active();
            let start_all = start_all.is_active();
            let tablet = existing_tablet.clone();
            glib::spawn_future_local(async move {
                let tablet_json = json!({
                    "id": "tablet",
                    "name": "Tablet",
                    "width": width_value,
                    "height": height_value,
                    "refreshHz": 120.0,
                    "scale": 1.5,
                    "position": "right",
                    "relativeTo": "primary",
                    "alignment": "center",
                    "slot": tablet.as_ref().map(|screen| screen.slot).unwrap_or(0),
                    "encoder": "auto",
                    "hevc": "auto",
                    "av1": "off",
                    "maxBitrateKbps": 30000,
                    "maxResolution": format!("{width_value}x{height_value}"),
                    "cursor": "embedded",
                });
                let result = match tablet {
                    Some(screen)
                        if screen.width == width_value && screen.height == height_value =>
                    {
                        Ok(())
                    }
                    Some(_) => client.update_screen(&tablet_json.to_string()).await,
                    None => client
                        .add_screen(&tablet_json.to_string())
                        .await
                        .map(|_| ()),
                };
                if let Err(err) = result {
                    ctx.toast_err("Could not save the tablet screen", &err);
                    dialog.close();
                    return;
                }
                if add_phone && let Err(err) = client.add_quick_screen("phone_portrait").await {
                    ctx.toast_err("Could not add the phone screen", &err);
                }
                let settings = json!({ "onboardingComplete": true }).to_string();
                if let Err(err) = client.update_settings(&settings).await {
                    ctx.toast_err("Could not finish onboarding", &err);
                }
                if start_all && let Err(err) = client.start_all().await {
                    ctx.toast_err("Could not start screens", &err);
                }
                dialog.close();
                ctx.toast("Setup complete");
            });
        });
    }
    dialog.present(Some(&window.window));
}
