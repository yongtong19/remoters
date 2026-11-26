use std::net::Ipv4Addr;

use gpui::{Context, SharedString, Window};
use remoters_core::network::{
    self,
    discovery::{DiscoveredHost, InterfaceAddress},
};

use super::state::Desktop;

impl Desktop {
    pub(crate) fn sync_discovered(&mut self) -> bool {
        let Some(scanner) = &self.scanner else {
            return false;
        };
        let latest = scanner.hosts();
        if hosts_equal(&self.discovered, &latest) {
            false
        } else {
            self.discovered = latest;
            true
        }
    }

    pub(crate) fn refresh_discovered(&mut self, cx: &mut Context<Self>) {
        if let Some(scanner) = &self.scanner {
            scanner.probe();
            self.discovered = scanner.hosts();
        }
        cx.notify();
    }

    pub(crate) fn select_discovered(
        &mut self,
        host: &DiscoveredHost,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let address = host.address.to_string();
        self.address
            .update(cx, |input, cx| input.set_value(address, window, cx));
        if self.status.as_ref().is_some_and(|(_, error)| *error) {
            self.status = None;
        }
        self.code.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }
}

pub(crate) fn local_interface_addresses() -> Vec<InterfaceAddress> {
    let mut interfaces: Vec<InterfaceAddress> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|interface| match interface.addr {
            if_addrs::IfAddr::V4(v4) if network::is_allowed_ip(v4.ip) && !v4.ip.is_loopback() => {
                let broadcast = v4.broadcast.or_else(|| subnet_broadcast(v4.ip, v4.netmask));
                Some(InterfaceAddress {
                    ip: v4.ip,
                    broadcast,
                })
            }
            _ => None,
        })
        .collect();
    interfaces.sort_by_key(|iface| (iface.ip, iface.broadcast.is_none()));
    interfaces.dedup_by_key(|iface| iface.ip);
    interfaces.push(InterfaceAddress {
        ip: Ipv4Addr::LOCALHOST,
        broadcast: None,
    });
    interfaces
}

fn subnet_broadcast(ip: Ipv4Addr, netmask: Ipv4Addr) -> Option<Ipv4Addr> {
    let mask = u32::from(netmask);
    if mask == u32::MAX {
        return None;
    }
    Some(Ipv4Addr::from(u32::from(ip) | !mask))
}

pub(crate) fn local_addresses() -> Vec<SharedString> {
    local_interface_addresses()
        .into_iter()
        .map(|iface| iface.ip.to_string().into())
        .collect()
}

pub(crate) fn host_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Desktop"
    }
}

fn hosts_equal(a: &[DiscoveredHost], b: &[DiscoveredHost]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|(left, right)| {
            left.instance_id == right.instance_id
                && left.address == right.address
                && left.host_name == right.host_name
                && left.platform == right.platform
                && left.display_label == right.display_label
                && left.view_only == right.view_only
        })
}
