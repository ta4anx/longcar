// SPDX-License-Identifier: GPL-3.0-or-later

//! Port families: one fixed block of TCP/UDP ports per screen slot.
//!
//! Every screen gets `slot` in `0..=MAX_SLOT`, which maps to a 60-port block
//! starting at [`PORT_BASE`]. Sunshine uses the individual ports listed in
//! [`PortFamily`]; the remaining ports in the block stay untouched so that
//! neighbouring blocks never overlap.

use std::net::{TcpListener, UdpSocket};

use crate::config::{Config, ConfigError, MAX_SLOT};

/// First port of the block belonging to slot 0.
pub const PORT_BASE: u16 = 48049;
/// Width of one slot's port block.
pub const PORT_STRIDE: u16 = 60;

/// All ports owned by one screen slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortFamily {
    /// Moonlight/control HTTP port (`base`).
    pub base: u16,
    /// Moonlight/control HTTP port (`base`).
    pub http: u16,
    /// Sunshine web UI / REST HTTPS port (`base + 1`).
    pub web: u16,
    /// Sunshine control HTTPS port (`base - 5`).
    pub https: u16,
    /// RTSP setup TCP port (`base + 21`).
    pub rtsp: u16,
    /// Video UDP port (`base + 9`).
    pub video_udp: u16,
    /// Control UDP port (`base + 10`).
    pub control_udp: u16,
    /// Audio UDP port (`base + 11`).
    pub audio_udp: u16,
}

impl PortFamily {
    /// Derive the port family for `slot`, rejecting slots outside the block range.
    pub fn for_slot(slot: u8) -> Result<Self, ConfigError> {
        if slot > MAX_SLOT {
            return Err(ConfigError::OutOfRange {
                screen: "port".to_string(),
                field: "slot",
                value: slot.to_string(),
            });
        }
        let base = u32::from(PORT_BASE) + u32::from(PORT_STRIDE) * u32::from(slot);
        // The highest port in the family is `base + 21`.
        if base + 21 > 65514 {
            return Err(ConfigError::OutOfRange {
                screen: "port".to_string(),
                field: "slot",
                value: slot.to_string(),
            });
        }
        let base = base as u16;
        Ok(Self {
            base,
            http: base,
            web: base + 1,
            https: base - 5,
            rtsp: base + 21,
            video_udp: base + 9,
            control_udp: base + 10,
            audio_udp: base + 11,
        })
    }

    /// TCP ports that must be free before the screen starts (HTTPS, HTTP, web).
    pub fn tcp_ports(&self) -> [u16; 3] {
        [self.https, self.base, self.web]
    }

    /// UDP ports that must be free before the screen starts.
    pub fn udp_ports(&self) -> [u16; 3] {
        [self.video_udp, self.control_udp, self.audio_udp]
    }
}

/// Smallest slot in `0..=MAX_SLOT` not claimed by a configured screen.
///
/// `except` names one screen ID to ignore, which lets an existing screen reuse
/// its own slot while it is being updated.
pub fn alloc_slot(config: &Config, except: Option<&str>) -> Option<u8> {
    (0..=MAX_SLOT).find(|slot| {
        !config
            .screens
            .iter()
            .any(|screen| screen.slot == *slot && Some(screen.id.as_str()) != except)
    })
}

/// Bind-test one port on `127.0.0.1`.
///
/// Returns `true` only for an address-in-use failure; every other error is
/// treated as "free" so that a missing network stack does not block starts.
pub fn port_in_use(port: u16, udp: bool) -> bool {
    if udp {
        match UdpSocket::bind(("127.0.0.1", port)) {
            Ok(socket) => {
                drop(socket);
                false
            }
            Err(err) => err.kind() == std::io::ErrorKind::AddrInUse,
        }
    } else {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => {
                drop(listener);
                false
            }
            Err(err) => err.kind() == std::io::ErrorKind::AddrInUse,
        }
    }
}

/// Verify that every port of `family` can be bound, TCP first, then UDP.
pub fn check_ports(family: &PortFamily) -> Result<(), ConfigError> {
    for port in family.tcp_ports() {
        if port_in_use(port, false) {
            return Err(ConfigError::PortInUse {
                port,
                proto: "tcp",
                hint: format!("ss -ltnup 'sport = :{port}'"),
            });
        }
    }
    for port in family.udp_ports() {
        if port_in_use(port, true) {
            return Err(ConfigError::PortInUse {
                port,
                proto: "udp",
                hint: format!("ss -ltnup 'sport = :{port}'"),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScreenConfig;
    use crate::model::{Alignment, Av1Mode, CursorMode, Encoder, HevcMode, Position};

    fn screen(id: &str, slot: u8) -> ScreenConfig {
        ScreenConfig {
            id: id.to_string(),
            name: id.to_string(),
            width: 1280,
            height: 720,
            refresh_hz: 60.0,
            scale: 1.0,
            position: Position::Right,
            relative_to: "primary".to_string(),
            alignment: Alignment::Center,
            slot,
            encoder: Encoder::Auto,
            hevc: HevcMode::Auto,
            av1: Av1Mode::Off,
            max_bitrate_kbps: 0,
            max_resolution: String::new(),
            cursor: CursorMode::Embedded,
            audio: true,
            touch: true,
        }
    }

    #[test]
    fn ports_for_slot_zero() {
        let family = PortFamily::for_slot(0).expect("slot 0");
        assert_eq!(family.base, 48049);
        assert_eq!(family.http, 48049);
        assert_eq!(family.web, 48050);
        assert_eq!(family.https, 48044);
        assert_eq!(family.rtsp, 48070);
        assert_eq!(family.video_udp, 48058);
        assert_eq!(family.control_udp, 48059);
        assert_eq!(family.audio_udp, 48060);
        assert_eq!(family.tcp_ports(), [48044, 48049, 48050]);
        assert_eq!(family.udp_ports(), [48058, 48059, 48060]);
    }

    #[test]
    fn ports_for_slot_one() {
        let family = PortFamily::for_slot(1).expect("slot 1");
        assert_eq!(family.base, 48109);
        assert_eq!(family.web, 48110);
        assert_eq!(family.https, 48104);
        assert_eq!(family.rtsp, 48130);
        assert_eq!(family.udp_ports(), [48118, 48119, 48120]);
    }

    #[test]
    fn ports_for_slot_seven() {
        let family = PortFamily::for_slot(7).expect("slot 7");
        assert_eq!(family.base, 48469);
        assert_eq!(family.tcp_ports(), [48464, 48469, 48470]);
        assert_eq!(family.udp_ports(), [48478, 48479, 48480]);
    }

    #[test]
    fn out_of_range_slot_is_rejected() {
        let err = PortFamily::for_slot(8).expect_err("slot 8");
        match err {
            ConfigError::OutOfRange {
                screen,
                field,
                value,
            } => {
                assert_eq!(screen, "port");
                assert_eq!(field, "slot");
                assert_eq!(value, "8");
            }
            other => panic!("unexpected error {other:?}"),
        }
    }

    #[test]
    fn alloc_slot_skips_used_slots() {
        let mut config = Config {
            screens: vec![screen("tablet", 0), screen("phone", 3)],
            presets: Vec::new(),
            ..Config::default()
        };
        assert_eq!(alloc_slot(&config, None), Some(1));
        assert_eq!(alloc_slot(&config, Some("phone")), Some(1));

        config.screens = vec![screen("tablet", 0), screen("phone", 2)];
        assert_eq!(alloc_slot(&config, Some("tablet")), Some(0));
        assert_eq!(alloc_slot(&config, Some("phone")), Some(1));
    }

    #[test]
    fn alloc_slot_returns_none_when_full() {
        let config = Config {
            screens: (0..=MAX_SLOT)
                .map(|slot| screen(&format!("screen{slot}"), slot))
                .collect(),
            presets: Vec::new(),
            ..Config::default()
        };
        assert_eq!(alloc_slot(&config, None), None);
        assert_eq!(alloc_slot(&config, Some("screen4")), Some(4));
    }

    #[test]
    fn busy_tcp_port_is_detected() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral");
        let port = listener.local_addr().expect("addr").port();
        assert!(port_in_use(port, false));
        drop(listener);
    }

    #[test]
    fn free_high_port_is_not_busy() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        assert!(!port_in_use(port, false));
        assert!(!port_in_use(port, true));
    }

    #[test]
    fn check_ports_reports_the_busy_port() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral");
        let port = listener.local_addr().expect("addr").port();
        let family = PortFamily {
            base: port,
            http: port,
            web: port,
            https: port,
            rtsp: port,
            video_udp: port,
            control_udp: port,
            audio_udp: port,
        };
        match check_ports(&family) {
            Err(ConfigError::PortInUse {
                port: p,
                proto,
                hint,
            }) => {
                assert_eq!(p, port);
                assert_eq!(proto, "tcp");
                assert_eq!(hint, format!("ss -ltnup 'sport = :{port}'"));
            }
            other => panic!("expected port-in-use, got {other:?}"),
        }
    }
}
