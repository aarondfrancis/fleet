//! Native delivery adapter. The core decides when an alert is new or recovered.

use fleet::monitoring::{FleetEventKind, FleetMachineEvent};
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

/// Success means the native API accepted the request, not that the OS displayed it.
pub fn send_alerts(app: &AppHandle, events: &[FleetMachineEvent]) -> Result<(), String> {
    request_alerts(events, |title, body| {
        app.notification()
            .builder()
            .title(title)
            .body(body)
            .show()
            .map_err(|e| e.to_string())
    })
}

fn request_alerts(
    events: &[FleetMachineEvent],
    send: impl FnOnce(String, String) -> Result<(), String>,
) -> Result<(), String> {
    if events.is_empty() {
        return Ok(());
    }
    // One notification per monitoring round, including simultaneous fleet outages.
    let title = if events.len() == 1 {
        let event = &events[0];
        let label = match event.kind {
            FleetEventKind::Online | FleetEventKind::DiskRecovered => "Recovered",
            _ => "Alert",
        };
        format!("{label}: {}", event.machine_name)
    } else {
        format!("Fleet: {} monitoring updates", events.len())
    };
    let mut body = events
        .iter()
        .take(3)
        .map(|e| format!("{}: {}", e.machine_name, e.detail))
        .collect::<Vec<_>>()
        .join("\n");
    if events.len() > 3 {
        body.push_str(&format!(
            "\nAnd {} more. Open the fleet overview for details.",
            events.len() - 3
        ));
    }
    send(title, body)
}

pub fn test(app: &AppHandle) -> Result<(), String> {
    app.notification()
        .builder()
        .title("Runner Dashboard notifications")
        .body("Desktop alerts are available. Machine monitoring and event history remain read-only.")
        .show()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: FleetEventKind) -> FleetMachineEvent {
        FleetMachineEvent {
            id: "1".into(),
            machine_key: "inv:studio".into(),
            machine_name: "studio".into(),
            at: chrono::Utc::now(),
            kind,
            detail: "Free space recovered.".into(),
        }
    }

    #[test]
    fn adapter_returns_native_request_failure_and_acceptance() {
        let events = [event(FleetEventKind::DiskRecovered)];
        let failed = request_alerts(&events, |title, body| {
            assert_eq!(title, "Recovered: studio");
            assert_eq!(body, "studio: Free space recovered.");
            Err("fake native API failure".into())
        });
        assert_eq!(failed, Err("fake native API failure".into()));
        assert_eq!(request_alerts(&events, |_, _| Ok(())), Ok(()));
        assert_eq!(
            request_alerts(&[], |_, _| panic!("empty batch must not call the native API")),
            Ok(())
        );
    }

    #[test]
    fn adapter_groups_events_and_bounds_notification_body() {
        let events = vec![event(FleetEventKind::LowDisk); 5];
        assert_eq!(
            request_alerts(&events, |title, body| {
                assert_eq!(title, "Fleet: 5 monitoring updates");
                assert_eq!(body.matches("studio:").count(), 3);
                assert!(body.ends_with("And 2 more. Open the fleet overview for details."));
                Ok(())
            }),
            Ok(())
        );
    }
}
