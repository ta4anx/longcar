// SPDX-License-Identifier: GPL-3.0-or-later

//! D-Bus client for the Longcar daemon and the shared GUI application model.
//!
//! Every call is asynchronous and is driven from `glib::spawn_future_local`;
//! the GTK main loop is never blocked.

use std::cell::RefCell;
use std::rc::Rc;

use futures_util::StreamExt;
use gtk::prelude::*;
use longcar_core::model::BUS_NAME;
use longcar_core::state::StateDoc;

#[zbus::proxy(
    interface = "io.github.ta4anx.Longcar.Daemon.Control",
    default_service = "io.github.ta4anx.Longcar.Daemon",
    default_path = "/io/github/ta4anx/Longcar/Daemon"
)]
pub trait DaemonControl {
    fn get_state(&self) -> zbus::Result<String>;
    fn start_default(&self) -> zbus::Result<()>;
    fn start_all(&self) -> zbus::Result<()>;
    fn stop_all(&self) -> zbus::Result<()>;
    fn start_screen(&self, id: &str) -> zbus::Result<()>;
    fn stop_screen(&self, id: &str) -> zbus::Result<()>;
    fn add_screen(&self, json: &str) -> zbus::Result<String>;
    fn update_screen(&self, json: &str) -> zbus::Result<()>;
    fn remove_screen(&self, id: &str, purge: bool) -> zbus::Result<()>;
    fn apply_preset(&self, preset_id: &str) -> zbus::Result<()>;
    fn add_quick_screen(&self, kind: &str) -> zbus::Result<String>;
    fn update_settings(&self, json: &str) -> zbus::Result<()>;
    fn set_presets(&self, json: &str) -> zbus::Result<()>;
    fn set_audio_screen(&self, id: &str) -> zbus::Result<()>;
    fn list_pairings(&self, screen_id: &str) -> zbus::Result<String>;
    fn pair(&self, screen_id: &str, pairing_id: &str, pin: &str, name: &str) -> zbus::Result<bool>;
    fn cancel_pairing(&self, screen_id: &str, pairing_id: &str) -> zbus::Result<()>;
    fn unpair(&self, screen_id: &str, uuid: &str) -> zbus::Result<()>;
    fn set_client_enabled(&self, screen_id: &str, uuid: &str, enabled: bool) -> zbus::Result<()>;
    fn discover_hosts(&self) -> zbus::Result<String>;
    fn get_history(&self, limit: u32) -> zbus::Result<String>;
    fn get_logs(&self, limit: u32) -> zbus::Result<String>;
    fn get_doctor(&self) -> zbus::Result<String>;
    fn run_doctor_fix(&self, fix_id: &str) -> zbus::Result<String>;
    fn doctor_display_test(&self) -> zbus::Result<(bool, String)>;
    fn get_preview_node(&self, screen_id: &str) -> zbus::Result<u32>;
    fn restore_layout(&self) -> zbus::Result<()>;
    fn discard_layout_snapshot(&self) -> zbus::Result<()>;

    #[zbus(signal)]
    fn state_changed(&self, json: &str) -> zbus::Result<()>;
}

/// Human-readable rendering of a D-Bus failure.
pub fn err_str(err: zbus::Error) -> String {
    match &err {
        zbus::Error::MethodError(name, message, _) => {
            let short = name.rsplit('.').next().unwrap_or(name.as_str());
            match message {
                Some(text) if !text.is_empty() => format!("{short}: {text}"),
                _ => short.to_string(),
            }
        }
        other => other.to_string(),
    }
}

/// Async daemon client. Cheap to clone.
#[derive(Clone)]
pub struct Client {
    proxy: DaemonControlProxy<'static>,
}

impl Client {
    pub async fn connect() -> Result<Self, String> {
        let connection = zbus::Connection::session().await.map_err(err_str)?;
        let proxy = DaemonControlProxy::new(&connection)
            .await
            .map_err(err_str)?;
        Ok(Self { proxy })
    }

    pub async fn get_state(&self) -> Result<StateDoc, String> {
        let json = self.proxy.get_state().await.map_err(err_str)?;
        StateDoc::from_json(&json).map_err(|err| format!("malformed daemon state: {err}"))
    }

    pub async fn start_default(&self) -> Result<(), String> {
        self.proxy.start_default().await.map_err(err_str)
    }
    pub async fn start_all(&self) -> Result<(), String> {
        self.proxy.start_all().await.map_err(err_str)
    }
    pub async fn stop_all(&self) -> Result<(), String> {
        self.proxy.stop_all().await.map_err(err_str)
    }
    pub async fn start_screen(&self, id: &str) -> Result<(), String> {
        self.proxy.start_screen(id).await.map_err(err_str)
    }
    pub async fn stop_screen(&self, id: &str) -> Result<(), String> {
        self.proxy.stop_screen(id).await.map_err(err_str)
    }
    pub async fn add_screen(&self, json: &str) -> Result<String, String> {
        self.proxy.add_screen(json).await.map_err(err_str)
    }
    pub async fn update_screen(&self, json: &str) -> Result<(), String> {
        self.proxy.update_screen(json).await.map_err(err_str)
    }
    pub async fn remove_screen(&self, id: &str, purge: bool) -> Result<(), String> {
        self.proxy.remove_screen(id, purge).await.map_err(err_str)
    }
    pub async fn apply_preset(&self, id: &str) -> Result<(), String> {
        self.proxy.apply_preset(id).await.map_err(err_str)
    }
    pub async fn add_quick_screen(&self, kind: &str) -> Result<String, String> {
        self.proxy.add_quick_screen(kind).await.map_err(err_str)
    }
    pub async fn update_settings(&self, json: &str) -> Result<(), String> {
        self.proxy.update_settings(json).await.map_err(err_str)
    }
    pub async fn set_presets(&self, json: &str) -> Result<(), String> {
        self.proxy.set_presets(json).await.map_err(err_str)
    }
    pub async fn set_audio_screen(&self, id: &str) -> Result<(), String> {
        self.proxy.set_audio_screen(id).await.map_err(err_str)
    }
    pub async fn list_pairings(&self, screen_id: &str) -> Result<String, String> {
        self.proxy.list_pairings(screen_id).await.map_err(err_str)
    }
    pub async fn pair(
        &self,
        screen_id: &str,
        pairing_id: &str,
        pin: &str,
        name: &str,
    ) -> Result<bool, String> {
        self.proxy
            .pair(screen_id, pairing_id, pin, name)
            .await
            .map_err(err_str)
    }
    pub async fn cancel_pairing(&self, screen_id: &str, pairing_id: &str) -> Result<(), String> {
        self.proxy
            .cancel_pairing(screen_id, pairing_id)
            .await
            .map_err(err_str)
    }
    pub async fn unpair(&self, screen_id: &str, uuid: &str) -> Result<(), String> {
        self.proxy.unpair(screen_id, uuid).await.map_err(err_str)
    }
    pub async fn set_client_enabled(
        &self,
        screen_id: &str,
        uuid: &str,
        enabled: bool,
    ) -> Result<(), String> {
        self.proxy
            .set_client_enabled(screen_id, uuid, enabled)
            .await
            .map_err(err_str)
    }
    pub async fn discover_hosts(&self) -> Result<String, String> {
        self.proxy.discover_hosts().await.map_err(err_str)
    }
    pub async fn get_history(&self, limit: u32) -> Result<String, String> {
        self.proxy.get_history(limit).await.map_err(err_str)
    }
    pub async fn get_logs(&self, limit: u32) -> Result<String, String> {
        self.proxy.get_logs(limit).await.map_err(err_str)
    }
    pub async fn get_doctor(&self) -> Result<String, String> {
        self.proxy.get_doctor().await.map_err(err_str)
    }
    pub async fn run_doctor_fix(&self, fix_id: &str) -> Result<String, String> {
        self.proxy.run_doctor_fix(fix_id).await.map_err(err_str)
    }
    pub async fn doctor_display_test(&self) -> Result<(bool, String), String> {
        self.proxy.doctor_display_test().await.map_err(err_str)
    }
    pub async fn get_preview_node(&self, screen_id: &str) -> Result<u32, String> {
        self.proxy
            .get_preview_node(screen_id)
            .await
            .map_err(err_str)
    }
    pub async fn restore_layout(&self) -> Result<(), String> {
        self.proxy.restore_layout().await.map_err(err_str)
    }
}

/// Shared application context handed to every page.
#[derive(Clone)]
pub struct Ctx {
    pub window: adw::ApplicationWindow,
    pub toast_overlay: adw::ToastOverlay,
    pub client: Rc<RefCell<Option<Client>>>,
    pub state: Rc<RefCell<Option<StateDoc>>>,
    pub open_logs: Rc<dyn Fn()>,
}

impl Ctx {
    pub fn toast(&self, message: &str) {
        self.toast_overlay.add_toast(adw::Toast::new(message));
    }

    pub fn toast_err(&self, action: &str, err: &str) {
        self.toast(&format!("{action}: {err}"));
    }

    pub fn client(&self) -> Option<Client> {
        self.client.borrow().clone()
    }

    pub fn state(&self) -> Option<StateDoc> {
        self.state.borrow().clone()
    }

    pub fn copy(&self, text: &str, what: &str) {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(text);
            self.toast(&format!("{what} copied"));
        }
    }
}

/// Watches the daemon bus name, keeping state fresh and the proxy usable.
pub struct DaemonWatcher {
    inner: Rc<WatcherInner>,
}

struct WatcherInner {
    client_slot: Rc<RefCell<Option<Client>>>,
    on_state: Box<dyn Fn(StateDoc)>,
    on_ready: Box<dyn Fn()>,
    on_unavailable: Box<dyn Fn(String)>,
    watch_id: RefCell<Option<gio::WatcherId>>,
    task: RefCell<Option<glib::JoinHandle<()>>>,
}

impl DaemonWatcher {
    pub fn new<State, Ready, Unavailable>(
        client_slot: Rc<RefCell<Option<Client>>>,
        on_state: State,
        on_ready: Ready,
        on_unavailable: Unavailable,
    ) -> Self
    where
        State: Fn(StateDoc) + 'static,
        Ready: Fn() + 'static,
        Unavailable: Fn(String) + 'static,
    {
        let inner = Rc::new(WatcherInner {
            client_slot,
            on_state: Box::new(on_state),
            on_ready: Box::new(on_ready),
            on_unavailable: Box::new(on_unavailable),
            watch_id: RefCell::new(None),
            task: RefCell::new(None),
        });

        let appeared = {
            let inner = inner.clone();
            move |_: gio::DBusConnection, _name: &str, _owner: &str| {
                inner.clone().connect();
            }
        };
        let vanished = {
            let inner = inner.clone();
            move |_: gio::DBusConnection, _name: &str| {
                (inner.on_unavailable)(
                    "The Longcar daemon stopped. Start it with: systemctl --user start longcar-daemon.service"
                        .to_string(),
                );
            }
        };
        let watch_id = gio::bus_watch_name(
            gio::BusType::Session,
            BUS_NAME,
            gio::BusNameWatcherFlags::NONE,
            appeared,
            vanished,
        );
        *inner.watch_id.borrow_mut() = Some(watch_id);
        Self { inner }
    }

    /// Ask for a fresh `GetState` and a new signal subscription.
    pub fn retry(&self) {
        self.inner.clone().connect();
    }
}

impl Drop for DaemonWatcher {
    fn drop(&mut self) {
        self.inner.abort_task();
        if let Some(id) = self.inner.watch_id.borrow_mut().take() {
            gio::bus_unwatch_name(id);
        }
    }
}

impl WatcherInner {
    fn abort_task(&self) {
        if let Some(handle) = self.task.borrow_mut().take() {
            handle.abort();
        }
    }

    fn connect(self: Rc<Self>) {
        self.abort_task();
        let this = self.clone();
        let handle = glib::spawn_future_local(async move {
            let client = match Client::connect().await {
                Ok(client) => client,
                Err(err) => {
                    (this.on_unavailable)(err);
                    return;
                }
            };
            let state = match client.get_state().await {
                Ok(state) => state,
                Err(err) => {
                    (this.on_unavailable)(err);
                    return;
                }
            };
            *this.client_slot.borrow_mut() = Some(client.clone());
            (this.on_ready)();
            (this.on_state)(state);

            let mut stream = match client.proxy.receive_state_changed().await {
                Ok(stream) => stream,
                Err(err) => {
                    (this.on_unavailable)(err_str(err));
                    return;
                }
            };
            while let Some(signal) = stream.next().await {
                let parsed = signal
                    .args()
                    .map_err(|err| format!("unexpected StateChanged payload: {err}"))
                    .and_then(|args| {
                        StateDoc::from_json(args.json())
                            .map_err(|err| format!("malformed state: {err}"))
                    });
                match parsed {
                    Ok(doc) => (this.on_state)(doc),
                    // Never crash on a malformed payload; keep the last good state.
                    Err(err) => eprintln!("longcar: ignoring StateChanged: {err}"),
                }
            }
        });
        *self.task.borrow_mut() = Some(handle);
    }
}
