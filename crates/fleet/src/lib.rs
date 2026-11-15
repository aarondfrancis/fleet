//! Runner Dashboard core, independent of the UI shell.
//!
//! - `github`, `gh`, `tailscale`, `machine`: read-only data sources
//! - `runner_version`: runner versions compared with recent `actions/runner` releases
//! - `engine`: config, secrets, polling and user actions
//! - `derive`: pure `(config, observations, plan inputs, now, source) -> FleetView`
//! - `issues`: the attention findings attached to each view
//! - `monitoring`: local machine history, alerts and notification requests
//! - `desired`, `fleet_plan`: the inventory, its checks and reviewed changes
//! - `local_api`: the read-only loopback HTTP API
//! - `demo`, `demo_desired`: a fictional dataset run through the same `derive`

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
pub mod runner_version;
pub mod sync;
pub mod tailscale;

#[cfg(test)]
mod derive_tests;
#[cfg(test)]
mod test_server;
