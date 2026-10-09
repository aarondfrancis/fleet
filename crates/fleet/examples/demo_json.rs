//! Prints the demo `FleetView` as JSON at a fixed time. Used as the frontend
//! test fixture and the browser-preview dataset:
//!
//!     cargo run -q -p fleet --example demo_json > src/fixtures/demo-fleet.json

#![expect(clippy::unwrap_used, reason = "fixture generator: any failure should stop it loudly")]

use chrono::{TimeZone, Utc};
use fleet::derive::derive;
use fleet::model::DataSource;

fn main() {
    let now = Utc.with_ymd_and_hms(2026, 10, 3, 12, 0, 0).unwrap();
    let (config, obs) = fleet::demo::dataset(now);
    let view = derive(
        &config,
        &obs,
        &fleet::demo_desired::plan_inputs(now),
        now,
        DataSource::Demo,
    );
    println!("{}", serde_json::to_string_pretty(&view).unwrap());
}
