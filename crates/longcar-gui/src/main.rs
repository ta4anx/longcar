// SPDX-License-Identifier: GPL-3.0-or-later

mod client;
mod pages;
mod util;
mod widgets;
mod window;

use adw::prelude::*;

const CSS_RESOURCE: &str = "/io/github/ta4anx/Longcar/style.css";

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_resource(CSS_RESOURCE);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn main() -> glib::ExitCode {
    gio::resources_register_include!("longcar.gresource")
        .expect("failed to register the Longcar resource bundle");

    // The preview pipeline uses GStreamer; initialise it before the app starts.
    if let Err(err) = gst::init() {
        eprintln!("longcar: GStreamer initialisation failed: {err}");
    }

    let app = adw::Application::builder()
        .application_id(longcar_core::model::APP_ID)
        .build();

    app.connect_startup(|_| load_css());
    app.connect_activate(window::present);
    if let Ok(dir) = std::env::var("LONGCAR_SHOT_DIR") {
        app.connect_activate(move |_| window::capture_pages(dir.clone()));
    }

    app.set_accels_for_action("app.quit", &["<primary>q"]);
    app.set_accels_for_action("win.start-all", &["<primary>Return"]);
    app.set_accels_for_action("win.stop-all", &["<primary>period"]);
    app.set_accels_for_action("win.start-default", &["<primary>d"]);
    app.set_accels_for_action("win.open-logs", &["<primary>l"]);

    app.run()
}
