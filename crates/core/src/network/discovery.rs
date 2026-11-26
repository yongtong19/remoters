use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddrV4, UdpSocket},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

pub const DISCOVERY_PORT: u16 = 47822;
pub const MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 77, 82);

const MAGIC: &[u8; 5] = b"RMTD1";
const KIND_ANNOUNCE: u8 = 1;
const KIND_BYE: u8 = 2;
const KIND_PROBE: u8 = 3;
const MAX_STRING_BYTES: usize = 64;
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(2);
const STALE_AFTER: Duration = Duration::from_secs(6);
const RECV_TIMEOUT: Duration = Duration::from_millis(120);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceAddress {
    pub ip: Ipv4Addr,
    pub broadcast: Option<Ipv4Addr>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostAdvertisement {
    pub instance_id: u64,
    pub port: u16,
    pub host_name: String,
    pub platform: String,
    pub display_label: String,
    pub view_only: bool,
    pub interfaces: Vec<InterfaceAddress>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredHost {
    pub instance_id: u64,
    pub address: SocketAddrV4,
    pub host_name: String,
    pub platform: String,
    pub display_label: String,
    pub view_only: bool,
    pub last_seen: Instant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DiscoveryPacket {
    Announce {
        instance_id: u64,
        port: u16,
        address: Ipv4Addr,
        view_only: bool,
        host_name: String,
        platform: String,
        display_label: String,
    },
    Bye {
        instance_id: u64,
    },
    Probe,
}

pub fn random_instance_id() -> Result<u64> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("random failure: {error}"))?;
    Ok(u64::from_be_bytes(bytes))
}

pub fn local_hostname() -> String {
    #[cfg(unix)]
    if let Some(name) = unix_hostname() {
        return clean_hostname(&name);
    }
    for key in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(value) = std::env::var(key) {
            let cleaned = clean_hostname(&value);
            if !cleaned.is_empty() {
                return cleaned;
            }
        }
    }
    "Remoters Host".into()
}

#[cfg(unix)]
fn unix_hostname() -> Option<String> {
    unsafe extern "C" {
        fn gethostname(name: *mut u8, len: usize) -> i32;
    }
    let mut buffer = [0u8; 256];
    let rc = unsafe { gethostname(buffer.as_mut_ptr(), buffer.len()) };
    if rc != 0 {
        return None;
    }
    let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    std::str::from_utf8(&buffer[..end]).ok().map(str::to_owned)
}

fn clean_hostname(raw: &str) -> String {
    let trimmed = raw.trim();
    let stripped = trimmed.strip_suffix(".local").unwrap_or(trimmed);
    if stripped.is_empty() {
        "Remoters Host".into()
    } else {
        stripped.to_owned()
    }
}

pub struct DiscoveryBeacon {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl DiscoveryBeacon {
    pub fn start(advertisement: HostAdvertisement) -> Result<Self> {
        Self::start_on_port(advertisement, DISCOVERY_PORT, MULTICAST_GROUP)
    }

    pub(crate) fn start_on_port(
        advertisement: HostAdvertisement,
        target_port: u16,
        multicast_group: Ipv4Addr,
    ) -> Result<Self> {
        let sockets = build_sender_sockets(&advertisement.interfaces)?;
        let listener = bind_probe_listener(target_port, &advertisement.interfaces, multicast_group);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();

        send_announce_all(&sockets, &advertisement, target_port, multicast_group);

        let worker = thread::Builder::new()
            .name("lan-discovery-beacon".into())
            .spawn(move || {
                let mut last_sent = Instant::now();
                let mut buf = [0u8; 256];
                while !stop_flag.load(Ordering::Acquire) {
                    let mut probed = false;
                    if let Some(ref sock) = listener {
                        if let Ok((len, _)) = sock.recv_from(&mut buf) {
                            if matches!(decode_packet(&buf[..len]), Ok(DiscoveryPacket::Probe)) {
                                probed = true;
                            }
                        }
                    } else {
                        thread::sleep(RECV_TIMEOUT);
                    }

                    if probed || last_sent.elapsed() >= ANNOUNCE_INTERVAL {
                        send_announce_all(&sockets, &advertisement, target_port, multicast_group);
                        last_sent = Instant::now();
                    }
                }
                send_bye_all(&sockets, &advertisement, target_port, multicast_group);
            })
            .context("Failed to start discovery beacon thread.")?;

        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for DiscoveryBeacon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub struct DiscoveryScanner {
    stop: Arc<AtomicBool>,
    hosts: Arc<Mutex<HashMap<(u64, SocketAddrV4), DiscoveredHost>>>,
    probers: Vec<(InterfaceAddress, UdpSocket)>,
    target_port: u16,
    multicast_group: Ipv4Addr,
    worker: Option<JoinHandle<()>>,
}

impl DiscoveryScanner {
    pub fn start(interfaces: Vec<InterfaceAddress>) -> Result<Self> {
        Self::start_on_port(interfaces, DISCOVERY_PORT, MULTICAST_GROUP)
    }

    pub(crate) fn start_on_port(
        interfaces: Vec<InterfaceAddress>,
        listen_port: u16,
        multicast_group: Ipv4Addr,
    ) -> Result<Self> {
        let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, listen_port))
            .with_context(|| {
                format!("Failed to bind discovery listener on UDP port {listen_port}.")
            })?;
        socket.set_read_timeout(Some(RECV_TIMEOUT))?;
        let _ = socket.set_broadcast(true);
        let actual_port = socket.local_addr()?.port();

        if multicast_group.is_multicast() {
            for iface in &interfaces {
                if !iface.ip.is_loopback() && !iface.ip.is_unspecified() {
                    let _ = socket.join_multicast_v4(&multicast_group, &iface.ip);
                }
            }
            let _ = socket.join_multicast_v4(&multicast_group, &Ipv4Addr::UNSPECIFIED);
        }

        let probers = build_prober_sockets(&interfaces);
        let hosts = Arc::new(Mutex::new(HashMap::new()));
        let hosts_ref = hosts.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();

        let worker = thread::Builder::new()
            .name("lan-discovery-scanner".into())
            .spawn(move || {
                let mut buf = [0u8; 512];
                while !stop_flag.load(Ordering::Acquire) {
                    match socket.recv_from(&mut buf) {
                        Ok((len, peer)) => {
                            if let Ok(packet) = decode_packet(&buf[..len]) {
                                let now = Instant::now();
                                if let Ok(mut map) = hosts_ref.lock() {
                                    apply_packet(&mut map, packet, peer.ip(), now);
                                    prune_stale(&mut map, now);
                                }
                            }
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock
                                    | std::io::ErrorKind::TimedOut
                                    | std::io::ErrorKind::Interrupted
                                    | std::io::ErrorKind::ConnectionReset
                            ) =>
                        {
                            if let Ok(mut map) = hosts_ref.lock() {
                                prune_stale(&mut map, Instant::now());
                            }
                        }
                        Err(_) => break,
                    }
                }
            })
            .context("Failed to start discovery scanner thread.")?;

        let scanner = Self {
            stop,
            hosts,
            probers,
            target_port: actual_port,
            multicast_group,
            worker: Some(worker),
        };
        scanner.probe();
        Ok(scanner)
    }

    #[cfg(test)]
    pub(crate) fn port(&self) -> u16 {
        self.target_port
    }

    pub fn probe(&self) {
        let bytes = encode_probe();
        for (iface, sock) in &self.probers {
            send_to_targets(sock, *iface, self.target_port, self.multicast_group, &bytes);
        }
    }

    pub fn hosts(&self) -> Vec<DiscoveredHost> {
        let now = Instant::now();
        let Ok(mut map) = self.hosts.lock() else {
            return Vec::new();
        };
        prune_stale(&mut map, now);
        let mut list: Vec<_> = map.values().cloned().collect();
        list.sort_by(|a, b| {
            a.host_name
                .cmp(&b.host_name)
                .then_with(|| a.address.to_string().cmp(&b.address.to_string()))
        });
        list
    }
}

impl Drop for DiscoveryScanner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn apply_packet(
    map: &mut HashMap<(u64, SocketAddrV4), DiscoveredHost>,
    packet: DiscoveryPacket,
    peer_ip: std::net::IpAddr,
    now: Instant,
) {
    match packet {
        DiscoveryPacket::Announce {
            instance_id,
            port,
            address,
            view_only,
            host_name,
            platform,
            display_label,
        } => {
            let resolved_ip = if !address.is_unspecified() {
                address
            } else if let std::net::IpAddr::V4(v4) = peer_ip {
                v4
            } else {
                return;
            };
            if port == 0 || !crate::network::is_allowed_ip(resolved_ip) {
                return;
            }
            let socket_addr = SocketAddrV4::new(resolved_ip, port);
            map.insert(
                (instance_id, socket_addr),
                DiscoveredHost {
                    instance_id,
                    address: socket_addr,
                    host_name,
                    platform,
                    display_label,
                    view_only,
                    last_seen: now,
                },
            );
        }
        DiscoveryPacket::Bye { instance_id } => {
            map.retain(|(id, _), _| *id != instance_id);
        }
        DiscoveryPacket::Probe => {}
    }
}

fn prune_stale(map: &mut HashMap<(u64, SocketAddrV4), DiscoveredHost>, now: Instant) {
    map.retain(|_, host| now.saturating_duration_since(host.last_seen) <= STALE_AFTER);
}

fn configure_sender_socket(sock: &UdpSocket) {
    let _ = sock.set_broadcast(true);
    let _ = sock.set_multicast_ttl_v4(1);
    let _ = sock.set_multicast_loop_v4(true);
}

fn collect_interface_sockets(
    interfaces: &[InterfaceAddress],
) -> Vec<(InterfaceAddress, UdpSocket)> {
    let mut seen = Vec::<Ipv4Addr>::new();
    let mut sockets = Vec::new();
    for &iface in interfaces {
        if iface.ip.is_unspecified() || seen.contains(&iface.ip) {
            continue;
        }
        seen.push(iface.ip);
        if let Ok(sock) = UdpSocket::bind(SocketAddrV4::new(iface.ip, 0)) {
            configure_sender_socket(&sock);
            sockets.push((iface, sock));
        }
    }
    sockets
}

fn build_sender_sockets(
    interfaces: &[InterfaceAddress],
) -> Result<Vec<(InterfaceAddress, UdpSocket)>> {
    let mut sockets = collect_interface_sockets(interfaces);
    if sockets.is_empty() {
        let sock = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
            .context("Failed to bind discovery sender socket.")?;
        configure_sender_socket(&sock);
        sockets.push((
            InterfaceAddress {
                ip: Ipv4Addr::UNSPECIFIED,
                broadcast: None,
            },
            sock,
        ));
    }
    Ok(sockets)
}

fn build_prober_sockets(interfaces: &[InterfaceAddress]) -> Vec<(InterfaceAddress, UdpSocket)> {
    let mut sockets = collect_interface_sockets(interfaces);
    if sockets.is_empty()
        && let Ok(sock) = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
    {
        configure_sender_socket(&sock);
        sockets.push((
            InterfaceAddress {
                ip: Ipv4Addr::UNSPECIFIED,
                broadcast: None,
            },
            sock,
        ));
    }
    sockets
}

fn bind_probe_listener(
    port: u16,
    interfaces: &[InterfaceAddress],
    multicast_group: Ipv4Addr,
) -> Option<UdpSocket> {
    let sock = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).ok()?;
    sock.set_read_timeout(Some(RECV_TIMEOUT)).ok()?;
    if multicast_group.is_multicast() {
        for iface in interfaces {
            if !iface.ip.is_loopback() && !iface.ip.is_unspecified() {
                let _ = sock.join_multicast_v4(&multicast_group, &iface.ip);
            }
        }
        let _ = sock.join_multicast_v4(&multicast_group, &Ipv4Addr::UNSPECIFIED);
    }
    Some(sock)
}

fn send_announce_all(
    sockets: &[(InterfaceAddress, UdpSocket)],
    adv: &HostAdvertisement,
    target_port: u16,
    multicast_group: Ipv4Addr,
) {
    for (iface, sock) in sockets {
        let bytes = encode_announce(
            adv.instance_id,
            adv.port,
            iface.ip,
            adv.view_only,
            &adv.host_name,
            &adv.platform,
            &adv.display_label,
        );
        send_to_targets(sock, *iface, target_port, multicast_group, &bytes);
    }
}

fn send_bye_all(
    sockets: &[(InterfaceAddress, UdpSocket)],
    adv: &HostAdvertisement,
    target_port: u16,
    multicast_group: Ipv4Addr,
) {
    let bytes = encode_bye(adv.instance_id);
    for (iface, sock) in sockets {
        send_to_targets(sock, *iface, target_port, multicast_group, &bytes);
    }
}

fn send_to_targets(
    sock: &UdpSocket,
    iface: InterfaceAddress,
    target_port: u16,
    multicast_group: Ipv4Addr,
    bytes: &[u8],
) {
    if iface.ip.is_loopback() {
        let _ = sock.send_to(bytes, SocketAddrV4::new(Ipv4Addr::LOCALHOST, target_port));
        return;
    }
    if multicast_group.is_multicast() {
        let _ = sock.send_to(bytes, SocketAddrV4::new(multicast_group, target_port));
    }
    if let Some(broadcast) = iface.broadcast {
        let _ = sock.send_to(bytes, SocketAddrV4::new(broadcast, target_port));
    }
}

fn truncate_utf8(input: &str, max_bytes: usize) -> &str {
    if input.len() <= max_bytes {
        return input;
    }
    let mut end = max_bytes;
    while !input.is_char_boundary(end) {
        end -= 1;
    }
    &input[..end]
}

fn push_short_str(out: &mut Vec<u8>, value: &str) {
    let clipped = truncate_utf8(value, MAX_STRING_BYTES);
    out.push(clipped.len() as u8);
    out.extend_from_slice(clipped.as_bytes());
}

fn read_short_str<'a>(data: &'a [u8], cursor: &mut usize) -> Result<String> {
    if *cursor >= data.len() {
        bail!("Truncated discovery string length.");
    }
    let len = usize::from(data[*cursor]);
    *cursor += 1;
    if len > MAX_STRING_BYTES || *cursor + len > data.len() {
        bail!("Invalid discovery string bounds.");
    }
    let s = std::str::from_utf8(&data[*cursor..*cursor + len])
        .context("Invalid UTF-8 in discovery packet.")?
        .to_owned();
    *cursor += len;
    Ok(s)
}

fn encode_announce(
    instance_id: u64,
    port: u16,
    address: Ipv4Addr,
    view_only: bool,
    host_name: &str,
    platform: &str,
    display_label: &str,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(96);
    out.extend_from_slice(MAGIC);
    out.push(KIND_ANNOUNCE);
    out.extend_from_slice(&instance_id.to_be_bytes());
    out.extend_from_slice(&port.to_be_bytes());
    out.extend_from_slice(&address.octets());
    out.push(u8::from(view_only));
    push_short_str(&mut out, host_name);
    push_short_str(&mut out, platform);
    push_short_str(&mut out, display_label);
    out
}

fn encode_bye(instance_id: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(14);
    out.extend_from_slice(MAGIC);
    out.push(KIND_BYE);
    out.extend_from_slice(&instance_id.to_be_bytes());
    out
}

fn encode_probe() -> [u8; 6] {
    [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], MAGIC[4], KIND_PROBE]
}

fn decode_packet(data: &[u8]) -> Result<DiscoveryPacket> {
    if data.len() < 6 || &data[..5] != MAGIC {
        bail!("Invalid discovery magic.");
    }
    match data[5] {
        KIND_ANNOUNCE => {
            if data.len() < 21 {
                bail!("Truncated announce packet.");
            }
            let instance_id = u64::from_be_bytes(data[6..14].try_into()?);
            let port = u16::from_be_bytes(data[14..16].try_into()?);
            let address = Ipv4Addr::new(data[16], data[17], data[18], data[19]);
            let view_only = (data[20] & 1) != 0;
            let mut cursor = 21;
            let host_name = read_short_str(data, &mut cursor)?;
            let platform = read_short_str(data, &mut cursor)?;
            let display_label = read_short_str(data, &mut cursor)?;
            Ok(DiscoveryPacket::Announce {
                instance_id,
                port,
                address,
                view_only,
                host_name,
                platform,
                display_label,
            })
        }
        KIND_BYE => {
            if data.len() < 14 {
                bail!("Truncated bye packet.");
            }
            let instance_id = u64::from_be_bytes(data[6..14].try_into()?);
            Ok(DiscoveryPacket::Bye { instance_id })
        }
        KIND_PROBE => Ok(DiscoveryPacket::Probe),
        other => bail!("Unknown discovery packet type {other}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_round_trip_and_utf8_truncation() {
        let long_name = "中".repeat(30);
        let encoded = encode_announce(
            42,
            47821,
            Ipv4Addr::new(192, 168, 1, 20),
            true,
            &long_name,
            "Windows",
            "Primary display · 2560 × 1440",
        );
        let decoded = decode_packet(&encoded).unwrap();
        match decoded {
            DiscoveryPacket::Announce {
                instance_id,
                port,
                address,
                view_only,
                host_name,
                platform,
                display_label,
            } => {
                assert_eq!(instance_id, 42);
                assert_eq!(port, 47821);
                assert_eq!(address, Ipv4Addr::new(192, 168, 1, 20));
                assert!(view_only);
                assert!(host_name.len() <= MAX_STRING_BYTES);
                assert_eq!(platform, "Windows");
                assert_eq!(display_label, "Primary display · 2560 × 1440");
            }
            other => panic!("unexpected packet: {other:?}"),
        }

        assert_eq!(
            decode_packet(&encode_bye(99)).unwrap(),
            DiscoveryPacket::Bye { instance_id: 99 }
        );
        assert_eq!(
            decode_packet(&encode_probe()).unwrap(),
            DiscoveryPacket::Probe
        );
    }

    #[test]
    fn loopback_beacon_and_scanner_discover_and_bye() {
        let loopback = vec![InterfaceAddress {
            ip: Ipv4Addr::LOCALHOST,
            broadcast: None,
        }];
        let scanner =
            DiscoveryScanner::start_on_port(loopback.clone(), 0, Ipv4Addr::UNSPECIFIED).unwrap();
        let adv = HostAdvertisement {
            instance_id: 0x1234_5678,
            port: 47821,
            host_name: "Test-PC".into(),
            platform: "Windows".into(),
            display_label: "Virtual display".into(),
            view_only: false,
            interfaces: loopback,
        };

        let beacon =
            DiscoveryBeacon::start_on_port(adv, scanner.port(), Ipv4Addr::UNSPECIFIED).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut found = Vec::new();
        while Instant::now() < deadline {
            found = scanner.hosts();
            if !found.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].host_name, "Test-PC");
        assert_eq!(
            found[0].address,
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, 47821)
        );

        drop(beacon);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if scanner.hosts().is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(30));
        }
        assert!(scanner.hosts().is_empty());
    }

    #[test]
    fn multi_interface_bye_and_stale_pruning() {
        let mut map = HashMap::new();
        let t0 = Instant::now();
        for addr in [Ipv4Addr::new(192, 168, 1, 10), Ipv4Addr::new(10, 0, 0, 10)] {
            apply_packet(
                &mut map,
                DiscoveryPacket::Announce {
                    instance_id: 7,
                    port: 47821,
                    address: addr,
                    view_only: false,
                    host_name: "Studio".into(),
                    platform: "macOS".into(),
                    display_label: "Virtual display".into(),
                },
                addr.into(),
                t0,
            );
        }
        apply_packet(
            &mut map,
            DiscoveryPacket::Announce {
                instance_id: 8,
                port: 47821,
                address: Ipv4Addr::new(192, 168, 1, 20),
                view_only: true,
                host_name: "Workstation".into(),
                platform: "Windows".into(),
                display_label: "Main display".into(),
            },
            Ipv4Addr::new(192, 168, 1, 20).into(),
            t0,
        );
        // Non-local and port-0 announces must be ignored.
        apply_packet(
            &mut map,
            DiscoveryPacket::Announce {
                instance_id: 9,
                port: 47821,
                address: Ipv4Addr::new(8, 8, 8, 8),
                view_only: false,
                host_name: "Public".into(),
                platform: "Linux".into(),
                display_label: "None".into(),
            },
            Ipv4Addr::new(8, 8, 8, 8).into(),
            t0,
        );
        apply_packet(
            &mut map,
            DiscoveryPacket::Announce {
                instance_id: 10,
                port: 0,
                address: Ipv4Addr::new(192, 168, 1, 30),
                view_only: false,
                host_name: "ZeroPort".into(),
                platform: "Linux".into(),
                display_label: "None".into(),
            },
            Ipv4Addr::new(192, 168, 1, 30).into(),
            t0,
        );
        assert_eq!(map.len(), 3);

        // Bye clears all interfaces for instance 7 while leaving instance 8 intact.
        apply_packet(
            &mut map,
            DiscoveryPacket::Bye { instance_id: 7 },
            Ipv4Addr::new(192, 168, 1, 10).into(),
            t0,
        );
        assert_eq!(map.len(), 1);
        assert!(map.values().all(|host| host.instance_id == 8));

        // Stale entry is pruned after STALE_AFTER.
        prune_stale(&mut map, t0 + STALE_AFTER + Duration::from_millis(1));
        assert!(map.is_empty());
    }
}
