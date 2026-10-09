//! Read-only Runner Fleet commands with the original CLI's `--json` output:
//!
//!   cargo run -p fleet --example fleet -- inventory|status|plan|doctor|health [HOST] [--runner NAME]
//!
//! Mutations (apply, restart, retire, decommission) happen in the app, through
//! a reviewed plan; this tool never mints a token or changes anything.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use fleet::desired::{inventory, operations, parity, plan, real_boundaries, state};
use fleet::machine::{MachineSource, MachineTarget};

/// Limit for each GitHub request and SSH command, as in the app.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let take = |args: &mut Vec<String>, flag: &str| -> Option<String> {
        let i = args.iter().position(|a| a == flag)?;
        args.remove(i);
        (i < args.len()).then(|| args.remove(i))
    };
    let path = take(&mut args, "--inventory")
        .map(PathBuf::from)
        .unwrap_or_else(inventory::default_inventory_path);
    let runner = take(&mut args, "--runner");
    args.retain(|a| a != "--json");
    let (command, host) = (args.first().cloned().unwrap_or_default(), args.get(1).cloned());

    let result = (|| -> Result<(serde_json::Value, bool), String> {
        let inventory = inventory::load_inventory_from_env(&path).map_err(|e| e.0)?;
        let (github, remote) = real_boundaries(&inventory.organization.name, COMMAND_TIMEOUT);
        match command.as_str() {
            "inventory" => Ok((parity::inventory(&inventory), true)),
            "health" => {
                let id = host.as_deref().ok_or("health requires an inventory host name")?;
                let target = MachineTarget::Inventory(inventory.find_host(id).map_err(|e| e.0)?.clone());
                let report = fleet::machine::remote_client().inspect(&target).map_err(|e| e.0)?;
                Ok((serde_json::to_value(report).map_err(|e| e.to_string())?, true))
            }
            "status" => {
                let snapshot =
                    state::collect_snapshot(&inventory, &github, &remote, host.as_deref()).map_err(|e| e.0)?;
                let rows = state::build_status_rows(&inventory, &snapshot, host.as_deref()).map_err(|e| e.0)?;
                warn(&snapshot);
                Ok((parity::status(&rows), true))
            }
            "plan" => {
                let snapshot =
                    state::collect_snapshot(&inventory, &github, &remote, host.as_deref()).map_err(|e| e.0)?;
                let items =
                    plan::generate_plan(&inventory, &snapshot, host.as_deref(), runner.as_deref()).map_err(|e| e.0)?;
                warn(&snapshot);
                Ok((parity::plan(&items), true))
            }
            "doctor" => {
                let checks = operations::doctor(&inventory, &github, &remote, host.as_deref()).map_err(|e| e.0)?;
                let ok = !checks
                    .iter()
                    .any(|c| c.status == fleet::desired::model::CheckStatus::Fail);
                Ok((parity::doctor(&checks), ok))
            }
            other => Err(format!(
                "unknown or unsupported command {other:?}; use inventory, status, plan, doctor or health"
            )),
        }
    })();
    match result {
        Ok((value, ok)) => {
            println!("{}", serde_json::to_string_pretty(&value).expect("json"));
            if ok { ExitCode::SUCCESS } else { ExitCode::from(1) }
        }
        Err(e) => {
            eprintln!("fleet: error: {e}");
            ExitCode::from(2)
        }
    }
}

fn warn(snapshot: &fleet::desired::model::FleetSnapshot) {
    if let Some(e) = &snapshot.github_error {
        eprintln!("warning: GitHub state unavailable: {e}");
    }
    for (host, e) in &snapshot.host_errors {
        eprintln!("warning: {host} unavailable: {e}");
    }
    for e in &snapshot.policy_errors {
        eprintln!("warning: {e}");
    }
}
