//! Add machine: the inventory entry for a tailnet device outside the inventory.
//! Built from the tailnet snapshot alone. Nothing here contacts the device.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::desired::model::{Platform, SshTransport};
use crate::model::TailnetPeer;

/// A tailnet device no inventory host claims, with the `[[hosts]]` entry that adds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TailnetDeviceView {
    pub node_id: String,
    pub name: String,
    pub inventory_entry: String,
}

/// `None` for a device no inventory platform fits, such as a phone.
pub(crate) fn device_view(peer: &TailnetPeer, organization: Option<&str>) -> Option<TailnetDeviceView> {
    let platform = match peer.os.to_ascii_lowercase().as_str() {
        "macos" => Platform::DarwinArm64,
        "linux" => Platform::LinuxX64,
        "windows" => Platform::WindowsX64,
        _ => return None,
    };
    let folder = organization.unwrap_or("runners");
    let root = if platform.is_windows() {
        format!("C:\\gh-runners\\{folder}")
    } else {
        format!("~/gh-runners/{folder}")
    };
    let transport = if peer.ssh_host_keys.is_empty() {
        SshTransport::Openssh
    } else {
        SshTransport::Tailscale
    };
    let platform_note = if platform.is_windows() {
        " # Tailscale doesn't report the CPU: use windows-arm64 on an ARM machine."
    } else {
        ""
    };
    let quote = |value: &str| toml::Value::String(value.into()).to_string();
    let inventory_entry = format!(
        "[[hosts]]\nid = {}\nplatform = {}{platform_note}\nssh = {}\nssh_transport = {}\nroot = {}\nservice_mode = {}\n",
        quote(&host_id(&peer.hostname)),
        quote(platform.as_str()),
        quote(&peer.address()),
        quote(transport.as_str()),
        quote(&root),
        quote(platform.service_modes()[0].as_str()),
    );
    Some(TailnetDeviceView {
        node_id: peer.node_id.clone(),
        name: peer.hostname.clone(),
        inventory_entry,
    })
}

/// A valid inventory host ID from a tailnet hostname.
fn host_id(hostname: &str) -> String {
    let id: String = hostname
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .skip_while(|c| !c.is_ascii_alphanumeric())
        .collect();
    if id.is_empty() { "machine".into() } else { id }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desired::inventory::parse_inventory;
    use crate::desired::model::ServiceMode;

    const ORGANIZATION: &str = "[organization]\nname = \"acme\"\nurl = \"https://github.com/acme\"\n\n";

    fn peer(hostname: &str, os: &str, ssh_host_keys: Vec<String>) -> TailnetPeer {
        TailnetPeer {
            node_id: format!("n-{hostname}"),
            hostname: hostname.into(),
            dns_name: Some(format!("{}.example.ts.net", hostname.to_lowercase().replace(' ', "-"))),
            os: os.into(),
            ips: vec!["100.64.0.9".into()],
            online: true,
            last_seen: None,
            tags: Vec::new(),
            ssh_host_keys,
        }
    }

    #[test]
    fn each_entry_is_a_valid_inventory_host() {
        for (os, platform, mode, transport, keys) in [
            (
                "macOS",
                Platform::DarwinArm64,
                ServiceMode::Launchagent,
                SshTransport::Tailscale,
                vec!["key".into()],
            ),
            (
                "linux",
                Platform::LinuxX64,
                ServiceMode::Systemd,
                SshTransport::Openssh,
                Vec::new(),
            ),
            (
                "windows",
                Platform::WindowsX64,
                ServiceMode::WindowsService,
                SshTransport::Openssh,
                Vec::new(),
            ),
        ] {
            let device = device_view(&peer("Dev Box", os, keys), Some("acme")).unwrap();
            assert_eq!(device.name, "Dev Box");
            let inventory = parse_inventory(
                &format!("{ORGANIZATION}{}", device.inventory_entry),
                &Default::default(),
            )
            .unwrap_or_else(|e| panic!("{os}: {e}\n{}", device.inventory_entry));
            let host = &inventory.hosts[0];
            assert_eq!(host.id, "dev-box");
            assert_eq!(host.platform, platform);
            assert_eq!(host.service_mode, mode);
            assert_eq!(host.ssh_transport, transport);
            assert_eq!(host.ssh_target.as_deref(), Some("dev-box.example.ts.net"));
            assert!(host.runners.is_empty());
        }
    }

    #[test]
    fn devices_no_platform_fits_are_not_offered() {
        assert!(device_view(&peer("phone", "iOS", Vec::new()), Some("acme")).is_none());
        assert!(device_view(&peer("tv", "android", Vec::new()), None).is_none());
    }
}
