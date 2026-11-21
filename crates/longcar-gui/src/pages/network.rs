// SPDX-License-Identifier: GPL-3.0-or-later

//! Network page: host addresses, port families, firewall hints, discovery and latency.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use longcar_core::network::{default_gateway, firewall_commands, iface_cidr, parse_ping_output};
use longcar_core::ports::PortFamily;
use longcar_core::state::StateDoc;

use crate::client::Ctx;
use crate::util;

/// Mirrors the daemon's download payload; used before any test has populated
/// `payloadBytes` in the state document.
const DEFAULT_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct NetworkPage {
    inner: Rc<Inner>,
}

struct Inner {
    ctx: Ctx,
    root: gtk::Box,
    addresses: gtk::ListBox,
    wifi: gtk::ListBox,
    speed: gtk::ListBox,
    speed_group: adw::PreferencesGroup,
    ports: gtk::Box,
    firewall: gtk::TextView,
    discovered: gtk::ListBox,
    discover_button: gtk::Button,
    latency_button: gtk::Button,
    latency_result: gtk::Label,
    signature: RefCell<String>,
}

impl NetworkPage {
    pub fn new(ctx: Ctx) -> Rc<Self> {
        let builder = gtk::Builder::from_resource("/io/github/ta4anx/Longcar/ui/network_page.ui");
        let root: gtk::Box = builder.object("network_page").expect("network_page");
        let addresses: gtk::ListBox = builder.object("address_list").expect("address list");
        addresses.add_css_class("boxed-list");
        let wifi: gtk::ListBox = builder.object("wifi_list").expect("wifi list");
        wifi.add_css_class("boxed-list");
        let speed: gtk::ListBox = builder.object("speed_list").expect("speed list");
        speed.add_css_class("boxed-list");
        let speed_group: adw::PreferencesGroup =
            builder.object("speed_group").expect("speed group");
        let ports: gtk::Box = builder.object("port_list").expect("port list");
        let firewall: gtk::TextView = builder.object("firewall_text").expect("firewall text");
        let discovered: gtk::ListBox = builder.object("discovered_list").expect("discovered list");
        discovered.add_css_class("boxed-list");
        firewall.add_css_class("longcar-mono");
        let discover_button: gtk::Button =
            builder.object("discover_button").expect("discover button");
        let latency_button: gtk::Button = builder.object("latency_button").expect("latency button");
        let latency_result: gtk::Label = builder.object("latency_result").expect("latency result");
        let firewall_copy: gtk::Button = builder
            .object("copy_firewall_button")
            .expect("copy firewall button");

        let page = Rc::new(Self {
            inner: Rc::new(Inner {
                ctx,
                root,
                addresses,
                wifi,
                speed,
                speed_group,
                ports,
                firewall,
                discovered,
                discover_button,
                latency_button,
                latency_result,
                signature: RefCell::new(String::new()),
            }),
        });

        {
            let inner = page.inner.clone();
            page.inner.discover_button.connect_clicked(move |button| {
                button.set_sensitive(false);
                let Some(client) = inner.ctx.client() else {
                    inner.ctx.toast("The Longcar daemon is unavailable");
                    button.set_sensitive(true);
                    return;
                };
                let inner = inner.clone();
                let button = button.clone();
                glib::spawn_future_local(async move {
                    match client.discover_hosts().await {
                        Ok(json) => inner.show_discovered(&json),
                        Err(err) => inner.ctx.toast_err("Discovery failed", &err),
                    }
                    button.set_sensitive(true);
                });
            });
        }
        {
            let inner = page.inner.clone();
            page.inner.latency_button.connect_clicked(move |button| {
                let Some(gateway) = default_gateway() else {
                    inner
                        .latency_result
                        .set_text("No default gateway is configured");
                    return;
                };
                button.set_sensitive(false);
                inner
                    .latency_result
                    .set_text(&format!("Pinging {gateway}…"));
                let inner = inner.clone();
                let button = button.clone();
                run_ping(gateway, inner, button);
            });
        }
        {
            let inner = page.inner.clone();
            firewall_copy.connect_clicked(move |_| {
                let buffer = inner.firewall.buffer();
                let text = buffer
                    .text(&buffer.start_iter(), &buffer.end_iter(), false)
                    .to_string();
                if text.trim().is_empty() {
                    inner.ctx.toast("No firewall commands to copy");
                } else {
                    inner.ctx.copy(&text, "Firewall commands");
                }
            });
        }

        page
    }

    pub fn root(&self) -> &gtk::Widget {
        self.inner.root.upcast_ref()
    }

    pub fn update(&self, state: &StateDoc) {
        let addresses = state
            .host
            .addresses
            .iter()
            .map(|entry| format!("{}:{}:{}", entry.interface, entry.address, entry.preferred))
            .collect::<Vec<_>>()
            .join(",");
        let screens = state
            .screens
            .iter()
            .map(|screen| format!("{}:{}", screen.id, screen.slot))
            .collect::<Vec<_>>()
            .join(",");
        let wifi = state
            .host
            .wifi
            .as_ref()
            .map(|link| {
                format!(
                    "{}:{}:{:?}:{:?}:{:?}:{:?}:{}",
                    link.ssid,
                    link.interface,
                    link.signal_dbm,
                    link.rx_bitrate_mbps,
                    link.tx_bitrate_mbps,
                    link.channel,
                    link.connected
                )
            })
            .unwrap_or_default();
        let test = format!(
            "{}|{}|{:?}|{:?}|{:?}|{:?}|{:?}|{}",
            state.speed_test.running,
            state.speed_test.url,
            state.speed_test.expires_at,
            state.speed_test.download_mbps,
            state.speed_test.upload_mbps,
            state.speed_test.measured_at,
            state.speed_test.client,
            state.speed_test.error.as_deref().unwrap_or(""),
        );
        let signature = format!("{addresses}|{screens}|{wifi}|{test}");
        if *self.inner.signature.borrow() == signature {
            return;
        }
        *self.inner.signature.borrow_mut() = signature;
        self.inner.render(state);
    }
}

impl Inner {
    fn render(&self, state: &StateDoc) {
        while let Some(child) = self.addresses.first_child() {
            self.addresses.remove(&child);
        }
        if state.host.addresses.is_empty() {
            self.addresses.append(&util::dim_label(
                "No non-loopback IPv4 address was found on this host",
            ));
        } else {
            for entry in &state.host.addresses {
                let row = adw::ActionRow::builder()
                    .title(&entry.address)
                    .subtitle(format!(
                        "{} · {}{}",
                        entry.interface,
                        util::enum_label(&entry.kind),
                        if entry.preferred { " · preferred" } else { "" }
                    ))
                    .build();
                row.add_css_class("longcar-mono");
                let copy = util::icon_button("edit-copy-symbolic", "Copy this address");
                {
                    let ctx = self.ctx.clone();
                    let text = entry.address.clone();
                    copy.connect_clicked(move |_| ctx.copy(&text, "Address"));
                }
                row.add_suffix(&copy);
                self.addresses.append(&row);
            }
        }

        while let Some(child) = self.ports.first_child() {
            self.ports.remove(&child);
        }
        self.ports.append(&port_table(state));

        self.fill_firewall(state);
        self.render_wifi(state);
        self.render_speed(state);
    }

    fn render_wifi(&self, state: &StateDoc) {
        clear_list(&self.wifi);
        let Some(link) = &state.host.wifi else {
            self.wifi.append(&calm_row(
                "The preferred address is not wireless — no link to report",
            ));
            return;
        };
        let header = adw::ActionRow::builder()
            .title(if link.ssid.is_empty() {
                "Hidden network"
            } else {
                &link.ssid
            })
            .subtitle(&link.interface)
            .build();
        self.wifi.append(&header);

        match link.signal_dbm {
            Some(dbm) => {
                let row = adw::ActionRow::builder()
                    .title("Signal")
                    .subtitle(signal_quality(dbm))
                    .build();
                let value = gtk::Label::new(Some(&format!("{dbm} dBm")));
                value.add_css_class("longcar-mono");
                value.set_valign(gtk::Align::Center);
                row.add_suffix(&value);
                self.wifi.append(&row);
            }
            None => self
                .wifi
                .append(&calm_row("The driver did not report a signal strength")),
        }

        let rates = match (link.rx_bitrate_mbps, link.tx_bitrate_mbps) {
            (Some(rx), Some(tx)) => format!("rx {} / tx {} Mbps", rate(rx), rate(tx)),
            _ => "The driver did not report negotiated rates".to_string(),
        };
        let row = adw::ActionRow::builder()
            .title("Negotiated rates")
            .subtitle(&rates)
            .build();
        self.wifi.append(&row);

        let mut channel = Vec::new();
        if let Some(value) = link.channel {
            channel.push(format!("ch {value}"));
        }
        if let Some(value) = link.width_mhz {
            channel.push(format!("{value} MHz"));
        }
        if let Some(value) = link.frequency_mhz {
            channel.push(format!("{value} MHz"));
        }
        let channel = channel.join(" · ");
        if channel.is_empty() {
            self.wifi
                .append(&calm_row("The driver did not report channel details"));
        } else {
            let row = adw::ActionRow::builder()
                .title("Channel")
                .subtitle(&channel)
                .build();
            self.wifi.append(&row);
        }
    }

    fn render_speed(&self, state: &StateDoc) {
        clear_list(&self.speed);
        let test = &state.speed_test;
        let payload_bytes = if test.payload_bytes > 0 {
            test.payload_bytes
        } else {
            DEFAULT_PAYLOAD_BYTES
        };
        let download = human_bytes(payload_bytes);
        let upload = human_bytes(payload_bytes / 4);
        self.speed_group.set_description(Some(&format!(
            "Measure the real throughput between this PC and your tablet/phone · downloads {download}, uploads {upload}"
        )));

        if let Some(error) = &test.error {
            let label = gtk::Label::new(Some(&format!("Warning: {error}")));
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.set_margin_top(6);
            label.set_margin_bottom(6);
            label.set_margin_start(12);
            label.set_margin_end(12);
            label.add_css_class("warning");
            self.speed.append(&label);
        }

        if test.running {
            let row = adw::ActionRow::builder()
                .title("Test running")
                .subtitle("Open this address in the device's browser, keep the page open")
                .build();
            row.add_suffix(&self.stop_button());
            self.speed.append(&row);

            let url_row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .margin_top(6)
                .margin_bottom(6)
                .margin_start(12)
                .margin_end(12)
                .build();
            let url = gtk::Label::new(Some(&test.url));
            url.set_selectable(true);
            url.set_wrap(true);
            url.set_xalign(0.0);
            url.set_hexpand(true);
            url.add_css_class("longcar-mono");
            url_row.append(&url);
            let copy = util::icon_button("edit-copy-symbolic", "Copy the test URL");
            {
                let ctx = self.ctx.clone();
                let text = test.url.clone();
                copy.connect_clicked(move |_| ctx.copy(&text, "Test URL"));
            }
            url_row.append(&copy);
            self.speed.append(&url_row);

            self.speed.append(&calm_row(&match &test.expires_at {
                Some(expires) => format!("Expires at {expires}"),
                None => "Expires in about 2 minutes".to_string(),
            }));
            return;
        }

        if let (Some(down), Some(up)) = (test.download_mbps, test.upload_mbps) {
            let mut detail = Vec::new();
            if let Some(at) = &test.measured_at {
                detail.push(format!("measured {at}"));
            }
            if let Some(client) = &test.client {
                detail.push(format!("from {client}"));
            }
            let row = adw::ActionRow::builder()
                .title(format!("{down:.1} Mbps down · {up:.1} Mbps up"))
                .subtitle(detail.join(" · "))
                .build();
            row.add_suffix(&self.start_button());
            self.speed.append(&row);

            let stream_kbps: u32 = state
                .screens
                .iter()
                .filter(|screen| {
                    matches!(
                        screen.state,
                        longcar_core::model::ScreenState::Ready
                            | longcar_core::model::ScreenState::Streaming
                    )
                })
                .map(|screen| screen.max_bitrate_kbps)
                .sum();
            if stream_kbps > 0 {
                self.speed.append(&calm_row(&format!(
                    "For comparison, your stream uses {:.1} Mbps",
                    f64::from(stream_kbps) / 1000.0
                )));
            }
            return;
        }

        let row = adw::ActionRow::builder()
            .title("Measure the link")
            .subtitle("Measure the real throughput between this PC and your tablet/phone")
            .build();
        row.add_suffix(&self.start_button());
        self.speed.append(&row);
    }

    fn start_button(&self) -> gtk::Button {
        let button = util::text_button("Start test", "Start the throughput test listener");
        let ctx = self.ctx.clone();
        button.connect_clicked(move |button| {
            let Some(client) = ctx.client() else {
                ctx.toast("The Longcar daemon is unavailable");
                return;
            };
            button.set_sensitive(false);
            let ctx = ctx.clone();
            let button = button.clone();
            glib::spawn_future_local(async move {
                if let Err(err) = client.start_speed_test().await {
                    ctx.toast_err("Could not start the speed test", &err);
                    button.set_sensitive(true);
                }
            });
        });
        button
    }

    fn stop_button(&self) -> gtk::Button {
        let button = util::text_button("Stop", "Stop the throughput test listener");
        let ctx = self.ctx.clone();
        button.connect_clicked(move |_| {
            let Some(client) = ctx.client() else {
                ctx.toast("The Longcar daemon is unavailable");
                return;
            };
            let ctx = ctx.clone();
            glib::spawn_future_local(async move {
                if let Err(err) = client.stop_speed_test().await {
                    ctx.toast_err("Could not stop the speed test", &err);
                }
            });
        });
        button
    }

    fn fill_firewall(&self, state: &StateDoc) {
        let Some(address) = state
            .host
            .addresses
            .iter()
            .find(|entry| entry.preferred)
            .or_else(|| state.host.addresses.first())
        else {
            self.set_text("No firewall rules are needed: no host address was found.");
            return;
        };
        let Some(cidr) = iface_cidr(&address.address) else {
            self.set_text(&format!(
                "Could not determine the network prefix for {}",
                address.address
            ));
            return;
        };
        let mut lines = Vec::new();
        for screen in &state.screens {
            let Ok(family) = PortFamily::for_slot(screen.slot) else {
                continue;
            };
            lines.extend(firewall_commands(&cidr, &family));
        }
        if lines.is_empty() {
            self.set_text("No configured screens, so no firewall rules are needed yet.");
        } else {
            self.set_text(&lines.join("\n"));
        }
    }

    fn set_text(&self, text: &str) {
        self.firewall.buffer().set_text(text);
    }

    fn show_discovered(&self, json: &str) {
        while let Some(child) = self.discovered.first_child() {
            self.discovered.remove(&child);
        }
        let hosts = match serde_json::from_str::<serde_json::Value>(json) {
            Ok(value) => value.as_array().cloned().unwrap_or_default(),
            Err(err) => {
                self.ctx
                    .toast_err("Malformed discovery result", &err.to_string());
                return;
            }
        };
        if hosts.is_empty() {
            self.discovered.append(&util::dim_label(
                "No Sunshine hosts answered mDNS within 3 seconds",
            ));
            return;
        }
        for host in hosts {
            let name = host
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("Sunshine host");
            let address = host
                .get("host")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let port = host
                .get("port")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let is_self = host
                .get("isSelf")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let addresses = host
                .get("addresses")
                .and_then(|value| value.as_array())
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| entry.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let row = adw::ActionRow::builder()
                .title(name)
                .subtitle(format!(
                    "{address}:{port}{}{}",
                    if addresses.is_empty() { "" } else { " · " },
                    addresses
                ))
                .build();
            if is_self {
                let badge = gtk::Label::new(Some("this host"));
                badge.add_css_class("pill");
                badge.add_css_class("caption");
                row.add_suffix(&badge);
            }
            self.discovered.append(&row);
        }
    }
}

fn port_table(state: &StateDoc) -> gtk::Widget {
    let grid = gtk::Grid::builder()
        .row_spacing(6)
        .column_spacing(14)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    let headers = [
        "Screen", "Control", "Web UI", "HTTPS", "RTSP", "Video", "Control", "Audio",
    ];
    for (column, header) in headers.iter().enumerate() {
        let label = gtk::Label::new(Some(header));
        label.add_css_class("caption");
        label.add_css_class("dim-label");
        label.set_xalign(0.0);
        grid.attach(&label, column as i32, 0, 1, 1);
    }
    if state.screens.is_empty() {
        let label = util::dim_label("No screens configured");
        grid.attach(&label, 0, 1, 1, 1);
        return grid.upcast();
    }
    for (index, screen) in state.screens.iter().enumerate() {
        let row = index as i32 + 1;
        let name = gtk::Label::new(Some(&screen.name));
        name.set_xalign(0.0);
        grid.attach(&name, 0, row, 1, 1);
        let Ok(family) = PortFamily::for_slot(screen.slot) else {
            let label = util::dim_label("invalid slot");
            grid.attach(&label, 1, row, 1, 1);
            continue;
        };
        let cells = [
            family.base,
            family.web,
            family.https,
            family.rtsp,
            family.video_udp,
            family.control_udp,
            family.audio_udp,
        ];
        for (column, port) in cells.iter().enumerate() {
            let label = gtk::Label::new(Some(&port.to_string()));
            label.add_css_class("longcar-mono");
            label.set_xalign(0.0);
            grid.attach(&label, column as i32 + 1, row, 1, 1);
        }
    }
    grid.upcast()
}

fn clear_list(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

/// A single calm, dimmed row used when there is nothing actionable to show.
fn calm_row(text: &str) -> gtk::Label {
    let label = util::dim_label(text);
    label.set_wrap(true);
    label.set_margin_top(8);
    label.set_margin_bottom(8);
    label.set_margin_start(12);
    label.set_margin_end(12);
    label
}

fn signal_quality(dbm: i32) -> &'static str {
    if dbm >= -50 {
        "excellent"
    } else if dbm >= -60 {
        "good"
    } else if dbm >= -70 {
        "fair"
    } else {
        "weak"
    }
}

/// Render a negotiated bitrate without a pointless `.0`.
fn rate(mbps: f64) -> String {
    if (mbps.fract()).abs() < f64::EPSILON {
        format!("{mbps:.0}")
    } else {
        format!("{mbps:.1}")
    }
}

fn human_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{bytes} bytes")
    }
}

fn run_ping(gateway: std::net::Ipv4Addr, inner: Rc<Inner>, button: gtk::Button) {
    let argv = vec![
        std::ffi::OsString::from("ping"),
        std::ffi::OsString::from("-n"),
        std::ffi::OsString::from("-c"),
        std::ffi::OsString::from("10"),
        std::ffi::OsString::from("-i"),
        std::ffi::OsString::from("0.2"),
        std::ffi::OsString::from("-W"),
        std::ffi::OsString::from("1"),
        std::ffi::OsString::from(gateway.to_string()),
    ];
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(|arg| arg.as_os_str()).collect();
    let flags = gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE;
    let subprocess = match gio::Subprocess::newv(&args, flags) {
        Ok(subprocess) => subprocess,
        Err(err) => {
            inner
                .latency_result
                .set_text(&format!("Could not run ping: {err}"));
            button.set_sensitive(true);
            return;
        }
    };
    glib::spawn_future_local(async move {
        let result = subprocess.communicate_utf8_future(None).await;
        match result {
            Ok((stdout, stderr)) => {
                let text = stdout.map(|text| text.to_string()).unwrap_or_default();
                match parse_ping_output(&text) {
                    Ok(stats) => inner.latency_result.set_text(&format!(
                        "min {:.1} ms · avg {:.1} ms · max {:.1} ms · jitter {:.1} ms · loss {:.0}%",
                        stats.min, stats.avg, stats.max, stats.jitter, stats.loss
                    )),
                    Err(err) => {
                        let stderr = stderr.map(|text| text.to_string()).unwrap_or_default();
                        inner.latency_result.set_text(&format!(
                            "Could not parse ping output ({err}){}",
                            if stderr.trim().is_empty() {
                                String::new()
                            } else {
                                format!(": {}", stderr.trim())
                            }
                        ));
                    }
                }
            }
            Err(err) => inner
                .latency_result
                .set_text(&format!("Could not run ping: {err}")),
        }
        button.set_sensitive(true);
    });
}
