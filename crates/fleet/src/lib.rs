//! Runner Dashboard core, independent of the UI shell.
//!
//! - `github`, `gh`, `tailscale`, `machine`: read-only data sources
//! - `engine`: config, secrets, polling and user actions
//! - `derive`: pure `(config, observations, plan inputs, now, source) -> FleetView`
//! - `issues`: the attention findings attached to each view
//! - `monitoring`: local machine history, alerts and notification requests
//! - `desired`, `fleet_plan`: the inventory, its checks and reviewed changes
//! - `runner_up`: runner-up central pools, read over SSH
//! - `rollout`: repositories moving to the central pools, and their old runners
//! - `local_api`: the read-only loopback HTTP API
//! - `add_machine`: inventory entries for tailnet devices outside the inventory
//! - `demo`, `demo_desired`: a fictional dataset run through the same `derive`

pub mod add_machine;
mod atomic_write;
mod cli;
pub mod demo;
pub mod demo_desired;
pub mod derive;
pub mod desired;
pub mod engine;
pub mod fleet_plan;
pub mod gh;
pub mod github;
pub mod issues;
pub mod labels;
pub mod local_api;
pub mod machine;
pub mod machine_diagnostics;
pub mod model;
pub mod monitoring;
pub mod rollout;
pub mod runner_up;
pub mod sync;
pub mod tailscale;

#[cfg(test)]
mod derive_tests;
#[cfg(test)]
mod test_server;
