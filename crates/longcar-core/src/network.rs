// SPDX-License-Identifier: GPL-3.0-or-later

//! Host networking: local addresses, mDNS discovery, gateway latency and firewall hints.

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::ports::PortFamily;

const MDNS_SERVICE: &str = "_nvstream._tcp.local.";
const MDNS_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("cannot read routing table: {0}")]
    Route(String),
    #[error("no default gateway is configured")]
    NoGateway,
    #[error("cannot run ping: {0}")]
    Ping(String),
    #[error("cannot parse ping output")]
    PingOutput,
}

/// How a local address is reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AddrKind {
    Wifi,
    Ethernet,
    Other,
}

impl AddrKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wifi => "wifi",
            Self::Ethernet => "ethernet",
            Self::Other => "other",
        }
    }
}

/// One local IPv4 address a client can connect to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostAddress {
    pub interface: String,
    pub address: String,
    pub kind: AddrKind,
    pub preferred: bool,
}

/// A Sunshine host discovered over mDNS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredHost {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub addresses: Vec<String>,
    pub is_self: bool,
}

/// Result of `ping_gateway`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PingStats {
    pub min: f64,
    pub avg: f64,
    pub max: f64,
    pub jitter: f64,
    pub loss: f64,
}

/// Parse `/proc/net/route`, returning the default-route interface and gateway.
pub fn parse_proc_net_route(text: &str) -> Option<(String, Ipv4Addr)> {
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        let interface = fields[0];
        let destination = fields[1];
        let gateway_hex = fields[2];
        if destination != "00000000" {
            continue;
        }
        let Ok(raw) = u32::from_str_radix(gateway_hex, 16) else {
            continue;
        };
        // /proc/net/route stores the address in host byte order (little endian).
        let gateway = Ipv4Addr::from(raw.to_le_bytes());
        if gateway.is_unspecified() {
            continue;
        }
        return Some((interface.to_string(), gateway));
    }
    None
}

fn proc_net_route() -> Result<String, NetworkError> {
    std::fs::read_to_string("/proc/net/route")
        .map_err(|e| NetworkError::Route(format!("/proc/net/route: {e}")))
}

/// Interface carrying the default route.
pub fn default_route_iface() -> Option<String> {
    proc_net_route()
        .ok()
        .and_then(|text| parse_proc_net_route(&text).map(|(iface, _)| iface))
}

/// Default gateway address.
pub fn default_gateway() -> Option<Ipv4Addr> {
    proc_net_route()
        .ok()
        .and_then(|text| parse_proc_net_route(&text).map(|(_, gateway)| gateway))
}

fn interface_kind(interface: &str) -> AddrKind {
    if Path::new(&format!("/sys/class/net/{interface}/wireless")).exists() {
        return AddrKind::Wifi;
    }
    match std::fs::read_to_string(format!("/sys/class/net/{interface}/type")) {
        Ok(text) if text.trim() == "1" => AddrKind::Ethernet,
        _ => AddrKind::Other,
    }
}

/// Local non-loopback IPv4 addresses, preferred one first.
pub fn host_addresses() -> Vec<HostAddress> {
    let default_iface = default_route_iface();
    let mut out = Vec::new();
    if let Ok(addrs) = if_addrs::get_if_addrs() {
        for addr in addrs {
            if addr.is_loopback() {
                continue;
            }
            let if_addrs::IfAddr::V4(v4) = &addr.addr else {
                continue;
            };
            if v4.ip.is_link_local() {
                continue;
            }
            out.push(HostAddress {
                interface: addr.name.clone(),
                address: v4.ip.to_string(),
                kind: interface_kind(&addr.name),
                preferred: false,
            });
        }
    }
    match &default_iface {
        Some(iface) => {
            if let Some(entry) = out.iter_mut().find(|entry| &entry.interface == iface) {
                entry.preferred = true;
            }
        }
        None => {
            if let Some(entry) = out.first_mut() {
                entry.preferred = true;
            }
        }
    }
    if !out.iter().any(|entry| entry.preferred)
        && let Some(entry) = out.first_mut()
    {
        entry.preferred = true;
    }
    out.sort_by(|a, b| {
        b.preferred
            .cmp(&a.preferred)
            .then_with(|| a.interface.cmp(&b.interface))
    });
    out
}

/// Network address for `address`/`prefix` as `network/prefix`.
pub fn network_cidr(address: Ipv4Addr, prefix: u8) -> Option<String> {
    if prefix > 32 {
        return None;
    }
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix))
    };
    let network = Ipv4Addr::from(u32::from(address) & mask);
    Some(format!("{network}/{prefix}"))
}

/// Network CIDR (`address & netmask`, e.g. `192.168.2.0/24`) of the interface that
/// owns `address`, suitable for `ufw allow from ...`.
pub fn iface_cidr(address: &str) -> Option<String> {
    let target: Ipv4Addr = address.parse().ok()?;
    let addrs = if_addrs::get_if_addrs().ok()?;
    for addr in addrs {
        if let if_addrs::IfAddr::V4(v4) = &addr.addr
            && v4.ip == target
        {
            return network_cidr(v4.ip, v4.prefixlen);
        }
    }
    None
}

/// `ufw allow from <cidr> to any port <p> proto tcp|udp` lines for one screen.
pub fn firewall_commands(iface_cidr: &str, family: &PortFamily) -> Vec<String> {
    let mut lines = Vec::new();
    for port in [family.base, family.web, family.https, family.rtsp] {
        lines.push(format!(
            "ufw allow from {iface_cidr} to any port {port} proto tcp"
        ));
    }
    for port in family.udp_ports() {
        lines.push(format!(
            "ufw allow from {iface_cidr} to any port {port} proto udp"
        ));
    }
    lines
}

/// Discover Sunshine hosts advertising `_nvstream._tcp.local.`.
///
/// Blocking; call from `spawn_blocking`. Returns an empty list on timeout.
pub fn discover_hosts() -> Vec<DiscoveredHost> {
    let local: HashSet<String> = host_addresses()
        .into_iter()
        .map(|entry| entry.address)
        .collect();
    let Ok(daemon) = mdns_sd::ServiceDaemon::new() else {
        return Vec::new();
    };
    let Ok(receiver) = daemon.browse(MDNS_SERVICE) else {
        let _ = daemon.shutdown();
        return Vec::new();
    };
    let deadline = Instant::now() + MDNS_TIMEOUT;
    let mut hosts: Vec<DiscoveredHost> = Vec::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                let addresses: Vec<String> = info
                    .get_addresses()
                    .iter()
                    .map(|addr| addr.to_string())
                    .collect();
                let is_self = addresses.iter().any(|addr| local.contains(addr));
                let host = DiscoveredHost {
                    name: info.get_fullname().trim_end_matches('.').to_string(),
                    host: info.get_hostname().trim_end_matches('.').to_string(),
                    port: info.get_port(),
                    addresses,
                    is_self,
                };
                if !hosts
                    .iter()
                    .any(|existing| existing.name == host.name && existing.port == host.port)
                {
                    hosts.push(host);
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = daemon.stop_browse(MDNS_SERVICE);
    let _ = daemon.shutdown();
    hosts.sort_by(|a, b| a.name.cmp(&b.name));
    hosts
}

/// Parse `ping` summary lines.
pub fn parse_ping_output(text: &str) -> Result<PingStats, NetworkError> {
    let rtt = regex::Regex::new(r"rtt [^=]*= ([\d.]+)/([\d.]+)/([\d.]+)/([\d.]+) ms")
        .map_err(|_| NetworkError::PingOutput)?;
    let loss = regex::Regex::new(r"([\d.]+)% packet loss").map_err(|_| NetworkError::PingOutput)?;
    let rtt = rtt.captures(text).ok_or(NetworkError::PingOutput)?;
    let parse = |index: usize| -> Result<f64, NetworkError> {
        rtt.get(index)
            .and_then(|m| m.as_str().parse::<f64>().ok())
            .ok_or(NetworkError::PingOutput)
    };
    let loss = loss
        .captures(text)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<f64>().ok())
        .unwrap_or(0.0);
    Ok(PingStats {
        min: parse(1)?,
        avg: parse(2)?,
        max: parse(3)?,
        jitter: parse(4)?,
        loss,
    })
}

/// Ping the default gateway ten times and summarise the round-trip times.
pub fn ping_gateway() -> Result<PingStats, NetworkError> {
    let gateway = default_gateway().ok_or(NetworkError::NoGateway)?;
    let output = std::process::Command::new("ping")
        .args(["-n", "-c", "10", "-i", "0.2", "-W", "1"])
        .arg(gateway.to_string())
        .output()
        .map_err(|e| NetworkError::Ping(e.to_string()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    parse_ping_output(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_route_is_parsed() {
        let table = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
wlp3s0\t00000000\t0102A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0
wlp3s0\t0002A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0
";
        let (iface, gateway) = parse_proc_net_route(table).unwrap();
        assert_eq!(iface, "wlp3s0");
        assert_eq!(gateway, Ipv4Addr::new(192, 168, 2, 1));
    }

    #[test]
    fn missing_default_route_is_none() {
        let table = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
wlp3s0\t0002A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0
";
        assert!(parse_proc_net_route(table).is_none());
        assert!(parse_proc_net_route("").is_none());
    }

    #[test]
    fn ping_summary_is_parsed() {
        let text = "\
PING 192.168.2.1 (192.168.2.1) 56(84) bytes of data.
64 bytes from 192.168.2.1: icmp_seq=1 ttl=64 time=2.11 ms

--- 192.168.2.1 ping statistics ---
10 packets transmitted, 9 received, 10% packet loss, time 1802ms
rtt min/avg/max/mdev = 1.902/3.456/7.123/0.987 ms
";
        let stats = parse_ping_output(text).unwrap();
        assert!((stats.min - 1.902).abs() < 1e-9);
        assert!((stats.avg - 3.456).abs() < 1e-9);
        assert!((stats.max - 7.123).abs() < 1e-9);
        assert!((stats.jitter - 0.987).abs() < 1e-9);
        assert!((stats.loss - 10.0).abs() < 1e-9);
    }

    #[test]
    fn ping_without_rtt_is_an_error() {
        let text = "ping: connect: Network is unreachable\n";
        assert!(matches!(
            parse_ping_output(text),
            Err(NetworkError::PingOutput)
        ));
    }

    #[test]
    fn network_cidr_masks_the_host_bits() {
        assert_eq!(
            network_cidr(Ipv4Addr::new(192, 168, 2, 11), 24).unwrap(),
            "192.168.2.0/24"
        );
        assert_eq!(
            network_cidr(Ipv4Addr::new(10, 1, 2, 3), 16).unwrap(),
            "10.1.0.0/16"
        );
        assert_eq!(
            network_cidr(Ipv4Addr::new(172, 16, 5, 4), 12).unwrap(),
            "172.16.0.0/12"
        );
        assert_eq!(
            network_cidr(Ipv4Addr::new(8, 8, 8, 8), 32).unwrap(),
            "8.8.8.8/32"
        );
        assert!(network_cidr(Ipv4Addr::new(8, 8, 8, 8), 33).is_none());
    }

    #[test]
    fn firewall_lines_cover_every_port() {
        let family = PortFamily::for_slot(0).unwrap();
        let lines = firewall_commands("192.168.2.0/24", &family);
        assert_eq!(lines.len(), 7);
        assert_eq!(
            lines[0],
            "ufw allow from 192.168.2.0/24 to any port 48049 proto tcp"
        );
        assert_eq!(
            lines[1],
            "ufw allow from 192.168.2.0/24 to any port 48050 proto tcp"
        );
        assert_eq!(
            lines[2],
            "ufw allow from 192.168.2.0/24 to any port 48044 proto tcp"
        );
        assert_eq!(
            lines[3],
            "ufw allow from 192.168.2.0/24 to any port 48070 proto tcp"
        );
        assert_eq!(
            lines[4],
            "ufw allow from 192.168.2.0/24 to any port 48058 proto udp"
        );
        assert_eq!(
            lines[5],
            "ufw allow from 192.168.2.0/24 to any port 48059 proto udp"
        );
        assert_eq!(
            lines[6],
            "ufw allow from 192.168.2.0/24 to any port 48060 proto udp"
        );
    }

    #[test]
    fn host_addresses_skip_loopback_and_mark_one_preferred() {
        let addresses = host_addresses();
        assert!(
            addresses
                .iter()
                .all(|entry| !entry.address.starts_with("127."))
        );
        if !addresses.is_empty() {
            assert_eq!(
                addresses.iter().filter(|entry| entry.preferred).count(),
                1,
                "{addresses:?}"
            );
            assert!(addresses[0].preferred);
        }
    }
}
