//! Windows network settings: inventory, probe, plan and apply.

use super::super::model::{AdapterDns, FleetPlanAction, NetworkState};
use super::super::plan::generate_plan;
use super::super::remote::{FleetRemote, parse_network_probe, watchdog_version};
use super::super::state::collect_snapshot;
use super::*;

fn windows_host(extra: &str) -> String {
    inventory_toml(&format!(
        r#"
[[hosts]]
id = "win"
platform = "windows-x64"
ssh = "win-host"
root = "C:\\gh-runners\\acme"
service_mode = "windows-service"
labels = ["fleet", "windows-build"]
{extra}
runners = [{{ name = "win-01", directory = "runner-01" }}]
"#
    ))
}

fn adapter(ipv4: &[&str], ipv6: &[&str]) -> AdapterDns {
    AdapterDns {
        ipv4: ipv4.iter().map(ToString::to_string).collect(),
        ipv6: ipv6.iter().map(ToString::to_string).collect(),
    }
}

fn state(dns: &[(&str, AdapterDns)], watchdog: Option<&str>) -> NetworkState {
    NetworkState {
        dns: dns.iter().map(|(n, a)| (n.to_string(), a.clone())).collect(),
        watchdog: watchdog.map(String::from),
    }
}

/// `win-01` online, configured and running, so only network items remain.
fn healthy(fleet: &FakeFleet) {
    let mut world = fleet.world();
    world.github.insert(
        "win-01".into(),
        gr(
            "win-01",
            "online",
            false,
            &["self-hosted", "fleet", "windows-build"],
            Some(true),
        ),
    );
    world.github.get_mut("win-01").unwrap().custom_labels = ["fleet".into(), "windows-build".into()].into();
    world.local.insert(
        "win-01".into(),
        LocalRunner::new("win-01", "C:\\gh-runners\\acme\\runner-01", true, "running", None),
    );
}

#[test]
fn inventory_reads_dns_and_watchdog_on_windows_only() {
    let inventory = load(&windows_host(
        "dns = [\"1.1.1.1\", \"2606:4700:4700::1111\"]\nnetwork_watchdog = true",
    ))
    .unwrap();
    let host = inventory.find_host("win").unwrap();
    assert_eq!(host.dns, ["1.1.1.1", "2606:4700:4700::1111"]);
    assert!(host.network_watchdog);
    assert!(host.manages_network());
    assert!(
        !load(&windows_host(""))
            .unwrap()
            .find_host("win")
            .unwrap()
            .manages_network()
    );

    let mac = base().replace(
        "labels = [\"fleet\", \"macos-build\"]",
        "labels = [\"fleet\", \"macos-build\"]\nnetwork_watchdog = true",
    );
    assert_eq!(
        load_err(&mac),
        "test: dns and network_watchdog are supported only on Windows"
    );
    assert_eq!(
        load_err(&windows_host("dns = [\"one.one.one.one\"]")),
        "win: dns server is not an IP address: 'one.one.one.one'"
    );
    assert_eq!(
        load_err(&windows_host("network_watchdog = \"yes\"")),
        "network_watchdog must be true or false"
    );
}

#[test]
fn network_probe_rows_parse_by_adapter_and_require_the_end_marker() {
    let output = format!(
        "banner\nRFN\tdns\tWi-Fi\t192.0.2.1\tfe80::1\nRFN\tdns\tEthernet\t\t\nRFN\twatchdog\t{}\nRFN\tend\n",
        watchdog_version()
    );
    let parsed = parse_network_probe(&output).unwrap();
    assert_eq!(
        parsed,
        state(
            &[
                ("Wi-Fi", adapter(&["192.0.2.1"], &["fe80::1"])),
                ("Ethernet", adapter(&[], &[]))
            ],
            Some(watchdog_version()),
        )
    );
    assert!(parse_network_probe("RFN\tdns\tWi-Fi\t1.1.1.1\t\n").is_err());
    assert!(parse_network_probe("RFN\tdns\tWi-Fi\nRFN\tend\n").is_err());
}

#[test]
fn plan_names_each_adapter_and_the_watchdog_until_they_match() {
    let inventory = load(&windows_host(
        "dns = [\"1.1.1.1\", \"1.0.0.1\"]\nnetwork_watchdog = true",
    ))
    .unwrap();
    let mut snap = snapshot(Vec::new(), Vec::new());
    snap.network.insert(
        "win".into(),
        state(
            &[
                ("Wi-Fi", adapter(&["192.0.2.1"], &["fe80::1"])),
                ("Ethernet", adapter(&["203.0.113.5", "203.0.113.6"], &[])),
            ],
            None,
        ),
    );
    let items: Vec<_> = generate_plan(&inventory, &snap, None, None)
        .unwrap()
        .into_iter()
        .filter(|i| i.action == FleetPlanAction::ConfigureNetwork)
        .collect();
    assert_eq!(items.len(), 1);
    assert!(items[0].apply && !items[0].blocked);
    assert_eq!((items[0].host.as_str(), items[0].runner.as_str()), ("win", ""));
    assert_eq!(
        items[0].reason,
        "set DNS on Ethernet to 1.1.1.1, 1.0.0.1 (now 203.0.113.5, 203.0.113.6); \
         set DNS on Wi-Fi to 1.1.1.1, 1.0.0.1 (now 192.0.2.1, fe80::1); install the network watchdog"
    );

    // IPv6 servers only count when the inventory lists some.
    snap.network.insert(
        "win".into(),
        state(
            &[("Wi-Fi", adapter(&["1.1.1.1", "1.0.0.1"], &["fe80::1"]))],
            Some(watchdog_version()),
        ),
    );
    let plan = generate_plan(&inventory, &snap, None, None).unwrap();
    assert!(
        plan.iter().all(|i| i.action != FleetPlanAction::ConfigureNetwork),
        "{plan:?}"
    );

    snap.network.insert(
        "win".into(),
        state(
            &[("Wi-Fi", adapter(&["1.1.1.1", "1.0.0.1"], &[]))],
            Some("# runner-fleet network watchdog v0"),
        ),
    );
    let plan = generate_plan(&inventory, &snap, None, None).unwrap();
    let item = plan
        .iter()
        .find(|i| i.action == FleetPlanAction::ConfigureNetwork)
        .unwrap();
    assert_eq!(
        item.reason,
        "update the network watchdog (# runner-fleet network watchdog v0)"
    );

    // A single-runner plan never includes host settings.
    let plan = generate_plan(&inventory, &snap, None, Some("win-01")).unwrap();
    assert!(plan.iter().all(|i| i.action != FleetPlanAction::ConfigureNetwork));
}

#[test]
fn snapshot_reads_network_only_for_hosts_that_manage_it() {
    let inventory = load(&windows_host("network_watchdog = true")).unwrap();
    let fleet = FakeFleet::default();
    let snap = collect_snapshot(&inventory, &fleet, &fleet, None).unwrap();
    assert_eq!(snap.network["win"], NetworkState::default());
    assert!(fleet.world().reads.contains(&"network win".to_string()));

    let plain = load(&windows_host("")).unwrap();
    let fleet = FakeFleet::default();
    let snap = collect_snapshot(&plain, &fleet, &fleet, None).unwrap();
    assert!(snap.network.is_empty());
    assert!(!fleet.world().reads.contains(&"network win".to_string()));
}

#[test]
fn apply_configures_network_once_and_verifies_it() {
    let inventory = load(&windows_host(
        "dns = [\"1.1.1.1\", \"1.0.0.1\"]\nnetwork_watchdog = true",
    ))
    .unwrap();
    let fleet = FakeFleet::default();
    healthy(&fleet);
    fleet
        .world()
        .network
        .insert("win".into(), state(&[("Wi-Fi", adapter(&["192.0.2.1"], &[]))], None));
    let messages = ops(&inventory, &fleet, Secrets::default())
        .apply_host("win", None)
        .unwrap();
    assert_eq!(fleet.world().calls, ["network win"]);
    assert_eq!(
        messages[0],
        "configured network on win: set DNS on Wi-Fi to 1.1.1.1, 1.0.0.1 (now 192.0.2.1); install the network watchdog"
    );

    // Already matching: nothing runs.
    fleet.world().calls.clear();
    ops(&inventory, &fleet, Secrets::default())
        .apply_host("win", None)
        .unwrap();
    assert!(fleet.world().calls.is_empty());

    // Applying one runner leaves host settings alone.
    fleet.world().network.get_mut("win").unwrap().watchdog = None;
    ops(&inventory, &fleet, Secrets::default())
        .apply_host("win", Some("win-01"))
        .unwrap();
    assert!(fleet.world().calls.is_empty());
}

#[test]
fn network_scripts_carry_settings_and_the_watchdog_source() {
    let inventory = load(&windows_host(
        "dns = [\"1.1.1.1\", \"1.0.0.1\"]\nnetwork_watchdog = true",
    ))
    .unwrap();
    let host = inventory.find_host("win").unwrap();
    let (commands, client) = recording_remote();
    let _ = client.configure_network(host);
    let script = decoded(&commands);
    assert!(
        script.starts_with("$env:RF_DNS = '1.1.1.1,1.0.0.1'\n$env:RF_WATCHDOG = 'true'\n"),
        "{script}"
    );
    assert!(script.contains("$env:RF_WATCHDOG_SOURCE = '# runner-fleet network watchdog v1"));
    assert!(script.contains("Register-ScheduledTask -TaskName $taskName"));

    let plain = load(&windows_host("")).unwrap();
    let error = client.configure_network(plain.find_host("win").unwrap()).unwrap_err();
    assert_eq!(error.0, "win declares no network settings");
}
