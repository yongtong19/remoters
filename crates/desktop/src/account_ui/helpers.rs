use anyhow::{Context as _, Result, anyhow};
use remoters_core::{
    account::{AccountUser, DeviceRecord},
    network::{self, Pairing},
};

use super::state::{AccountConnectTarget, AccountSelection, DeviceStatusKind};

pub fn device_status_kind(device: &DeviceRecord) -> DeviceStatusKind {
    if !device.online {
        DeviceStatusKind::Offline
    } else if device.sharing {
        DeviceStatusKind::Sharing
    } else {
        DeviceStatusKind::Online
    }
}

pub fn device_status_label(device: &DeviceRecord) -> &'static str {
    let s = crate::i18n::tr();
    match device_status_kind(device) {
        DeviceStatusKind::Sharing => s.status_sharing,
        DeviceStatusKind::Online => s.status_online,
        DeviceStatusKind::Offline => s.status_offline,
    }
}

pub fn device_can_connect(device: &DeviceRecord) -> bool {
    parse_device_connect_target(device).is_ok()
}

pub fn parse_device_connect_target(device: &DeviceRecord) -> Result<AccountConnectTarget> {
    let s = crate::i18n::tr();
    if !device.online {
        return Err(anyhow!("{}", s.err_device_offline));
    }
    if !device.sharing {
        return Err(anyhow!("{}", s.err_device_not_sharing));
    }
    let raw_address = device
        .host_address
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context(s.err_device_no_address)?;
    let address = network::endpoint(raw_address)
        .with_context(|| crate::i18n::fmt_invalid_device_address(raw_address))?;
    let raw_code = device
        .pairing_code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context(s.err_device_no_pairing_code)?;
    let pairing = Pairing::parse(raw_code).context(s.err_device_invalid_pairing_code)?;
    Ok(AccountConnectTarget { address, pairing })
}

pub fn connect_disabled_reason(device: &DeviceRecord) -> Option<String> {
    parse_device_connect_target(device)
        .err()
        .map(|err| err.to_string())
}

pub fn format_user_title(user: &AccountUser) -> String {
    user.display_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("@{}", user.username))
}

pub fn format_user_subtitle(user: &AccountUser) -> String {
    format!("@{}", user.username)
}

pub fn resolve_selected_device<'a>(
    devices: &'a [DeviceRecord],
    selection: &AccountSelection,
) -> Option<&'a DeviceRecord> {
    match selection {
        AccountSelection::Device(device_id) => {
            devices.iter().find(|device| &device.device_id == device_id)
        }
        AccountSelection::None | AccountSelection::Auth => None,
    }
}

pub fn reconcile_selection_with_devices(
    selection: &AccountSelection,
    devices: &[DeviceRecord],
) -> AccountSelection {
    match selection {
        AccountSelection::Device(device_id)
            if !devices.iter().any(|device| &device.device_id == device_id) =>
        {
            AccountSelection::None
        }
        other => other.clone(),
    }
}

pub fn next_selection_after_removal(
    selection: &AccountSelection,
    removed_device_id: &str,
) -> AccountSelection {
    match selection {
        AccountSelection::Device(device_id) if device_id == removed_device_id => {
            AccountSelection::None
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use remoters_core::account::{AccountUser, DeviceRecord};
    use remoters_core::network::Pairing;

    use super::{
        AccountSelection, DeviceStatusKind, connect_disabled_reason, device_can_connect,
        device_status_kind, device_status_label, format_user_subtitle, format_user_title,
        next_selection_after_removal, parse_device_connect_target,
        reconcile_selection_with_devices, resolve_selected_device,
    };

    fn sample_device(
        id: &str,
        online: bool,
        sharing: bool,
        addr: Option<&str>,
        code: Option<&str>,
    ) -> DeviceRecord {
        DeviceRecord {
            device_id: id.to_owned(),
            name: format!("Device-{id}"),
            platform: "macOS".to_owned(),
            host_address: addr.map(ToOwned::to_owned),
            pairing_code: code.map(ToOwned::to_owned),
            sharing,
            view_only: false,
            display_label: "Virtual display · Match viewer".to_owned(),
            last_seen_unix_ms: 1_700_000_000_000,
            online,
        }
    }

    #[test]
    fn device_status_helpers_distinguish_sharing_online_and_offline() {
        crate::i18n::with_locale_for_test(crate::i18n::Locale::En, || {
            let sharing = sample_device("1", true, true, Some("192.168.1.10:47821"), None);
            assert_eq!(device_status_kind(&sharing), DeviceStatusKind::Sharing);
            assert_eq!(device_status_label(&sharing), "Sharing");

            let online = sample_device("2", true, false, Some("192.168.1.11"), None);
            assert_eq!(device_status_kind(&online), DeviceStatusKind::Online);
            assert_eq!(device_status_label(&online), "Online");

            let offline = sample_device("3", false, true, Some("192.168.1.12:47821"), None);
            assert_eq!(device_status_kind(&offline), DeviceStatusKind::Offline);
            assert_eq!(device_status_label(&offline), "Offline");
        });

        crate::i18n::with_locale_for_test(crate::i18n::Locale::ZhCn, || {
            let sharing = sample_device("1", true, true, Some("192.168.1.10:47821"), None);
            assert_eq!(device_status_label(&sharing), "共享中");
            let online = sample_device("2", true, false, Some("192.168.1.11"), None);
            assert_eq!(device_status_label(&online), "在线");
            let offline = sample_device("3", false, true, Some("192.168.1.12:47821"), None);
            assert_eq!(device_status_label(&offline), "离线");
        });
    }

    #[test]
    fn device_connect_target_requires_online_sharing_address_and_pairing_code() {
        crate::i18n::with_locale_for_test(crate::i18n::Locale::En, || {
            let valid_code = "A".repeat(75);
            assert!(Pairing::parse(&valid_code).is_ok());

            let ready = sample_device(
                "1",
                true,
                true,
                Some("192.168.50.24:47821"),
                Some(&valid_code),
            );
            assert!(device_can_connect(&ready));
            assert!(connect_disabled_reason(&ready).is_none());
            let target = parse_device_connect_target(&ready).expect("valid target");
            assert_eq!(target.address.to_string(), "192.168.50.24:47821");

            let offline = sample_device(
                "2",
                false,
                true,
                Some("192.168.50.24:47821"),
                Some(&valid_code),
            );
            assert!(!device_can_connect(&offline));
            assert!(
                connect_disabled_reason(&offline)
                    .unwrap()
                    .contains("offline")
            );

            let not_sharing = sample_device("3", true, false, Some("192.168.50.24"), None);
            assert!(!device_can_connect(&not_sharing));
            assert!(
                connect_disabled_reason(&not_sharing)
                    .unwrap()
                    .contains("not currently sharing")
            );

            let missing_code = sample_device("4", true, true, Some("192.168.50.24:47821"), None);
            assert!(!device_can_connect(&missing_code));
            assert!(
                connect_disabled_reason(&missing_code)
                    .unwrap()
                    .contains("pairing code")
            );
        });
    }

    #[test]
    fn selection_reconciliation_and_removal_preserve_valid_state() {
        let devices = vec![
            sample_device("a", true, false, None, None),
            sample_device("b", false, false, None, None),
        ];
        let sel_b = AccountSelection::Device("b".to_owned());
        assert_eq!(
            resolve_selected_device(&devices, &sel_b).map(|d| d.device_id.as_str()),
            Some("b")
        );
        assert_eq!(reconcile_selection_with_devices(&sel_b, &devices), sel_b);
        assert_eq!(
            reconcile_selection_with_devices(&AccountSelection::Device("missing".into()), &devices),
            AccountSelection::None
        );
        assert_eq!(
            reconcile_selection_with_devices(&AccountSelection::Auth, &devices),
            AccountSelection::Auth
        );
        assert_eq!(
            next_selection_after_removal(&sel_b, "b"),
            AccountSelection::None
        );
        assert_eq!(next_selection_after_removal(&sel_b, "a"), sel_b);
    }

    #[test]
    fn user_formatting_prefers_display_name_with_username_fallback() {
        let with_name = AccountUser {
            id: "u1".to_owned(),
            username: "alice".to_owned(),
            display_name: Some("Alice Wang".to_owned()),
            created_at_unix_ms: 0,
        };
        assert_eq!(format_user_title(&with_name), "Alice Wang");
        assert_eq!(format_user_subtitle(&with_name), "@alice");

        let without_name = AccountUser {
            id: "u2".to_owned(),
            username: "bob".to_owned(),
            display_name: Some("   ".to_owned()),
            created_at_unix_ms: 0,
        };
        assert_eq!(format_user_title(&without_name), "@bob");
        assert_eq!(format_user_subtitle(&without_name), "@bob");
    }
}
