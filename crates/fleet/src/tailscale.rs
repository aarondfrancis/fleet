//! Tailnet discovery via `tailscale status --json` (no API key needed; the
//! LocalAPI is an internal detail, the CLI JSON is the supported surface).

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::process::Command;

use crate::cli::{self, CLI_TIMEOUT, CliError};
use crate::model::{TailnetPeer, TailnetSnapshot};

pub fn cli_path() -> PathBuf {
    [
        // Standalone and App Store builds both ship the CLI inside the app bundle.
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/usr/local/bin/tailscale",
        "/opt/homebrew/bin/tailscale",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
    .unwrap_or_else(|| PathBuf::from("tailscale"))
}

pub async fn status() -> Result<TailnetSnapshot, String> {
    let mut command = Command::new(cli_path());
    command.args(["status", "--json"]);
    let out = cli::output(command, CLI_TIMEOUT).await.map_err(|e| match e {
        CliError::Spawn(e) => format!("Couldn't run the Tailscale CLI: {e}"),
        CliError::TimedOut(limit) => format!(
            "The Tailscale CLI didn't respond within {} seconds. Check that Tailscale is running.",
            limit.as_secs()
        ),
    })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            "Tailscale CLI failed".into()
        } else {
            err
        });
    }
    parse_status(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_status(json: &str) -> Result<TailnetSnapshot, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Status {
        backend_state: Option<String>,
        #[serde(rename = "Self")]
        self_: Option<Node>,
        #[serde(default)]
        peer: BTreeMap<String, Node>,
        current_tailnet: Option<Tailnet>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Tailnet {
        name: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Node {
        #[serde(rename = "ID")]
        id: String,
        #[serde(default)]
        host_name: String,
        #[serde(rename = "DNSName", default)]
        dns_name: String,
        #[serde(rename = "OS", default)]
        os: String,
        #[serde(default)]
        tailscale_i_ps: Vec<String>,
        #[serde(default)]
        online: bool,
        last_seen: Option<DateTime<Utc>>,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(rename = "sshHostKeys", default)]
        ssh_host_keys: Vec<String>,
    }

    let s: Status = serde_json::from_str(json).map_err(|e| format!("Unexpected tailscale status output: {e}"))?;
    if let Some(state) = s.backend_state.as_deref()
        && state != "Running"
    {
        return Err(format!("Tailscale is not connected (state: {state})"));
    }
    let peers = s
        .peer
        .into_values()
        .map(|n| TailnetPeer {
            node_id: n.id,
            hostname: n.host_name,
            dns_name: Some(n.dns_name.trim_end_matches('.').to_string()).filter(|d| !d.is_empty()),
            os: n.os,
            ips: n.tailscale_i_ps,
            online: n.online,
            // Tailscale reports the zero time for peers it has never seen offline.
            last_seen: n.last_seen.filter(|t| t.timestamp() > 0),
            tags: n.tags,
            ssh_host_keys: n.ssh_host_keys,
        })
        .collect();
    Ok(TailnetSnapshot {
        local_online: s.self_.as_ref().map(|n| n.online),
        self_node_id: s.self_.map(|n| n.id),
        tailnet_name: s.current_tailnet.and_then(|t| t.name),
        peers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_fixture() {
        let json = r#"{
          "Version": "1.102.3", "BackendState": "Running",
          "Self": {"ID": "nSelf", "HostName": "dev-laptop", "DNSName": "dev-laptop.tail1.ts.net.", "OS": "macOS",
                   "TailscaleIPs": ["100.64.0.1"], "Online": true},
          "CurrentTailnet": {"Name": "example.github", "MagicDNSSuffix": "tail1.ts.net", "MagicDNSEnabled": true},
          "Peer": {
            "nodekey:a": {"ID": "nMini", "HostName": "mac-mini", "DNSName": "mac-mini.tail1.ts.net.", "OS": "macOS",
                          "TailscaleIPs": ["100.64.0.2", "fd7a:115c:a1e0::2"], "Online": true,
                          "LastSeen": "0001-01-01T00:00:00Z"},
            "nodekey:b": {"ID": "nBox", "HostName": "linux-box", "DNSName": "linux-box.tail1.ts.net.", "OS": "linux",
                          "TailscaleIPs": ["100.64.0.3"], "Online": false, "LastSeen": "2026-10-02T08:00:00Z",
                          "Tags": ["tag:runner"], "sshHostKeys": ["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeKeyForTests"]},
            "nodekey:c": {"ID": "nPhone", "HostName": "iphone", "DNSName": "iphone.tail1.ts.net.", "OS": "iOS",
                          "TailscaleIPs": ["100.64.0.4"], "Online": true}
          }
        }"#;
        let snap = parse_status(json).unwrap();
        assert_eq!(snap.self_node_id.as_deref(), Some("nSelf"));
        assert_eq!(snap.tailnet_name.as_deref(), Some("example.github"));
        assert_eq!(snap.peers.len(), 3);
        let mini = snap.peers.iter().find(|p| p.node_id == "nMini").unwrap();
        assert_eq!(mini.dns_name.as_deref(), Some("mac-mini.tail1.ts.net"));
        assert_eq!(mini.last_seen, None);
        assert!(mini.can_host_runners());
        let boxx = snap.peers.iter().find(|p| p.node_id == "nBox").unwrap();
        assert_eq!(boxx.tags, vec!["tag:runner"]);
        assert_eq!(boxx.ssh_host_keys.len(), 1);
        assert!(mini.ssh_host_keys.is_empty());
        assert!(boxx.last_seen.is_some());
        let phone = snap.peers.iter().find(|p| p.node_id == "nPhone").unwrap();
        assert!(!phone.can_host_runners());
    }

    #[test]
    fn stopped_backend_is_an_error() {
        let err = parse_status(r#"{"BackendState": "Stopped", "Peer": {}}"#).unwrap_err();
        assert!(err.contains("Stopped"));
    }
}
