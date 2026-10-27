use super::*;
use crate::engine::{MemorySecrets, SecretStore};
use crate::model::{AccountKind, AccountSetup, OrgAuth, RepoScope};
use axum::Json;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

fn engine(demo: bool) -> (tempfile::TempDir, Arc<Engine>) {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::load(dir.path().join("config.json"), Arc::new(MemorySecrets::default())).unwrap();
    e.set_inventory_path(Some(dir.path().join("missing.toml").display().to_string()))
        .unwrap();
    e.set_demo_mode(demo).unwrap();
    (dir, Arc::new(e))
}

async fn call(e: &Arc<Engine>, req: Request<Body>) -> (StatusCode, Value) {
    let mut req = req;
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 50000))));
    let resp = router(e.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 4 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn get(path: &str) -> Request<Body> {
    Request::get(path)
        .header("host", "127.0.0.1:47622")
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn index_and_reads() {
    let (_d, e) = engine(true);
    let (s, body) = call(&e, get("/")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["readOnly"], true);
    assert_eq!(body["source"], "demo");

    let (_, body) = call(&e, get("/v1/summary")).await;
    assert_eq!(body["summary"]["machines"], 5);

    let (_, body) = call(&e, get("/v1/machines/studio")).await;
    assert_eq!(body["displayName"], "studio");

    let (_, body) = call(&e, get("/v1/runners")).await;
    let runners = body["runners"].as_array().unwrap();
    // Inventory runners, a retained legacy runner, one hosted pool runner and a repository runner.
    assert_eq!(runners.len(), 10);
    assert!(
        runners
            .iter()
            .any(|r| r["machineName"] == "studio" && r["name"] == "studio-1")
    );

    // Keys contain ':' and work as a path tail.
    let (s, body) = call(&e, get("/v1/runners/gh:acme-demo:101")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["currentJob"]["name"], "build (macOS)");

    let (_, body) = call(&e, get("/v1/queue")).await;
    assert_eq!(body["queue"]["jobs"].as_array().unwrap().len(), 2);

    let (s, _) = call(&e, get("/v1/machines/nope")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&e, get("/v2/whatever")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn refuses_writes_browsers_rebinding_and_remote_peers() {
    let (_d, e) = engine(true);
    let post = Request::post("/v1/fleet")
        .header("host", "127.0.0.1:47622")
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(&e, post).await.0, StatusCode::METHOD_NOT_ALLOWED);

    let rebind = Request::get("/v1/fleet")
        .header("host", "evil.example:47622")
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(&e, rebind).await.0, StatusCode::FORBIDDEN);

    let browser = Request::get("/v1/fleet")
        .header("host", "localhost:47622")
        .header("origin", "https://evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(&e, browser).await.0, StatusCode::FORBIDDEN);

    let mut remote = get("/v1/fleet");
    remote
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([100, 64, 0, 9], 50000))));
    let resp = router(e.clone()).oneshot(remote).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn binds_loopback_only_and_serves() {
    let (_d, e) = engine(true);
    let listener = bind(0).await.unwrap();
    let addr = listener.local_addr().unwrap();
    assert!(addr.ip().is_loopback());
    tokio::spawn(serve(listener, e));
    let body: Value = reqwest::get(format!("http://{addr}/v1/summary"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["source"], "demo");
}

#[tokio::test]
async fn views_carry_no_secrets() {
    let credentials = ["sentinel-first-credential-7f32", "sentinel-second-credential-98ab"];
    let secrets = Arc::new(MemorySecrets::default());
    let dir = tempfile::tempdir().unwrap();
    let e = Arc::new(Engine::load(dir.path().join("config.json"), secrets.clone()).unwrap());
    // Validate and store credentials through the normal engine path against a fake GitHub.
    let fake = Router::new().route(
        "/orgs/{org}/actions/runners",
        axum::routing::get(|| async { Json(json!({"total_count": 0, "runners": []})) }),
    );
    let api_base = crate::test_server::serve(fake).await;
    for (login, credential) in ["secret-first", "secret-second"].into_iter().zip(credentials) {
        e.add_org(
            login,
            OrgAuth::Token,
            Some(credential),
            AccountSetup {
                kind: AccountKind::Organization,
                repo_scope: RepoScope::None,
                runner_repos: Vec::new(),
            },
            Some(api_base.clone()),
        )
        .await
        .unwrap();
        assert_eq!(secrets.get(login).unwrap().as_deref(), Some(credential));
    }
    for demo in [false, true] {
        e.set_demo_mode(demo).unwrap();
        let serialized = serde_json::to_string(&e.view()).unwrap();
        for credential in credentials {
            assert!(!serialized.contains(credential));
        }
        for path in [
            "/",
            "/v1/summary",
            "/v1/fleet",
            "/v1/machines",
            "/v1/runners",
            "/v1/queue",
            "/v1/discovered",
            "/v1/desired",
            "/v1/monitoring",
        ] {
            let (status, body) = call(&e, get(path)).await;
            assert_eq!(status, StatusCode::OK);
            let serialized = serde_json::to_string(&body).unwrap();
            for credential in credentials {
                assert!(!serialized.contains(credential), "credential leaked in {path}");
            }
        }
    }
}

#[tokio::test]
async fn lookup_prefers_exact_identity_and_reports_ambiguity() {
    let dir = tempfile::tempdir().unwrap();
    let mut peers = crate::demo::dataset(chrono::Utc::now()).1.tailscale.unwrap().value;
    peers.peers.truncate(2);
    for (peer, name) in peers.peers.iter_mut().zip(["alpha", "beta"]) {
        peer.node_id = name.into();
        peer.hostname = name.into();
        peer.online = true;
    }
    let engine = Arc::new(
        Engine::load(dir.path().join("config.json"), Arc::new(MemorySecrets::default()))
            .unwrap()
            .with_tailscale_source(move || {
                let peers = peers.clone();
                async move { Ok(peers) }
            }),
    );
    engine
        .set_inventory_path(Some(dir.path().join("absent.toml").display().to_string()))
        .unwrap();
    engine.poll_tailscale().await;
    engine.set_machine_preferences("ts:alpha", "ts:beta", "").unwrap();
    engine.set_machine_preferences("ts:beta", "zzzz", "").unwrap();
    let (status, found) = call(&engine, get("/v1/machines/ts:beta")).await;
    assert_eq!((status, found["id"].as_str()), (StatusCode::OK, Some("ts:beta")));
    for id in ["ts:alpha", "ts:beta"] {
        engine.set_machine_preferences(id, "Shared", "").unwrap();
    }
    let (status, found) = call(&engine, get("/v1/machines/shared")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(found["code"], "ambiguous_identifier");
    assert_eq!(found["candidates"], json!(["ts:alpha", "ts:beta"]));
    let (_, summary) = call(&engine, get("/v1/summary")).await;
    let (_, issues) = call(&engine, get("/v1/issues")).await;
    assert_eq!(summary["summary"]["attention"], issues["total"]);
    assert!(
        issues["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"] == "inventory")
    );
}

#[test]
fn runner_keys_precede_names_and_org_disambiguates_names() {
    let (_dir, engine) = engine(true);
    let mut view = engine.view();
    let mut runners = super::queries::flat_runners(&view).into_iter().map(|r| r.runner);
    let mut first = runners.next().unwrap();
    let mut second = runners.next().unwrap();
    first.key = "gh:first:1".into();
    first.name = "gh:second:2".into();
    first.org = Some("first".into());
    second.key = "gh:second:2".into();
    second.name = "shared".into();
    second.org = Some("second".into());
    view.machines.clear();
    view.other_machines.clear();
    view.unmatched_runners = vec![first, second];
    assert_eq!(
        super::queries::resolve_runner(&view, "gh:second:2", None)
            .unwrap()
            .runner
            .key,
        "gh:second:2"
    );
    view.unmatched_runners[0].name = "shared".into();
    let error = super::queries::resolve_runner(&view, "SHARED", None).unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.body.candidates, ["gh:first:1", "gh:second:2"]);
    assert_eq!(
        super::queries::resolve_runner(&view, "shared", Some("SECOND"))
            .unwrap()
            .runner
            .key,
        "gh:second:2"
    );
}

#[tokio::test]
async fn metadata_scopes_filters_and_host_plans_are_complete() {
    let (_dir, engine) = engine(true);
    let (_, all) = call(&engine, get("/v1/machines")).await;
    assert_eq!(all["machines"].as_array().unwrap().len(), 7);
    let (_, fleet) = call(&engine, get("/v1/machines?scope=fleet")).await;
    let (_, others) = call(&engine, get("/v1/machines?scope=discovered")).await;
    assert_eq!((fleet["total"].as_u64(), others["total"].as_u64()), (Some(5), Some(2)));
    for path in [
        "/v1/machines/inv:studio",
        "/v1/runners/gh:acme-demo:101",
        "/v1/discovered",
        "/v1/desired",
    ] {
        let (status, body) = call(&engine, get(path)).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        assert_eq!(body["source"], "demo", "{path}");
        assert_eq!(body["apiVersion"], 1);
        assert!(body["generatedAt"].is_string() && body["revision"].is_string());
    }
    let (_, desired) = call(&engine, get("/v1/desired?machine=inv:studio")).await;
    assert_eq!(desired["hosts"].as_array().unwrap().len(), 1);
    let expected = fleet["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["displayName"] == "studio")
        .unwrap();
    for field in ["host", "plan", "pendingChanges", "checks", "error"] {
        assert_eq!(desired["hosts"][0]["desired"][field], expected["desired"][field]);
    }

    assert!(!desired["hosts"][0]["runners"].as_array().unwrap().is_empty());
    let (_, runners) = call(
        &engine,
        get("/v1/runners?machine=inv:studio&org=acme-demo&activity=busy&label=self-hosted"),
    )
    .await;
    assert_eq!(runners["runners"].as_array().unwrap().len(), 1);
    assert_eq!(runners["runners"][0]["name"], "studio-1");
    let (_, queue) = call(&engine, get("/v1/queue?org=absent")).await;
    assert_eq!(queue["total"], 0);
    let (_, page) = call(&engine, get("/v1/machines?limit=2")).await;
    assert_eq!(page["machines"].as_array().unwrap().len(), 2);
    let (_, next) = call(
        &engine,
        get(&format!(
            "/v1/machines?limit=2&after={}",
            page["nextCursor"].as_str().unwrap()
        )),
    )
    .await;
    assert!(next["machines"][0]["id"].as_str().unwrap() > page["machines"][1]["id"].as_str().unwrap());
}

#[tokio::test]
async fn history_pages_reach_older_and_removed_machines_without_demo_leaks() {
    let dir = tempfile::tempdir().unwrap();
    let now = chrono::Utc::now();
    let events: Vec<_> = (1..=155)
        .map(|id| crate::monitoring::FleetMachineEvent {
            id: format!("{id:020}"),
            machine_key: if id % 2 == 0 { "inv:removed" } else { "inv:other" }.into(),
            machine_name: "Private live machine".into(),
            at: now - chrono::Duration::minutes(156 - id),
            kind: crate::monitoring::FleetEventKind::Online,
            detail: "Observed online".into(),
        })
        .collect();
    std::fs::write(
        dir.path().join("monitoring.json"),
        json!({ "version": 1, "next_id": 156, "machines": {}, "events": events }).to_string(),
    )
    .unwrap();
    let engine = Arc::new(Engine::load(dir.path().join("config.json"), Arc::new(MemorySecrets::default())).unwrap());
    engine
        .set_inventory_path(Some(dir.path().join("absent.toml").display().to_string()))
        .unwrap();
    let mut ids = Vec::new();
    let mut before = String::new();
    loop {
        let (_, page) = call(&engine, get(&format!("/v1/events?limit=60{before}"))).await;
        ids.extend(
            page["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["id"].as_str().unwrap().to_owned()),
        );
        if page["hasMore"] == false {
            assert!(page["nextCursor"].is_null());
            break;
        }
        before = format!("&before={}", page["nextCursor"].as_str().unwrap());
    }
    assert_eq!(ids, (1..=155).rev().map(|id| format!("{id:020}")).collect::<Vec<_>>());
    let (_, filtered) = call(&engine, get("/v1/events?machineKey=inv:removed&limit=200")).await;
    assert_eq!(filtered["events"].as_array().unwrap().len(), 77);
    engine.set_demo_mode(true).unwrap();
    let (_, demo) = call(&engine, get("/v1/events?limit=200")).await;
    assert_eq!(demo["source"], "demo");
    assert!(!demo["events"].as_array().unwrap().is_empty());
    assert!(!demo.to_string().contains("Private live machine"));
    let (_, machine) = call(&engine, get("/v1/events?machine=inv:studio")).await;
    assert!(
        machine["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["machineKey"] == "inv:studio")
    );
}

#[tokio::test]
async fn conditional_reads_ignore_generated_time_and_head_is_empty() {
    let (_dir, engine) = engine(false);
    let router = router(engine.clone());
    let response = router.clone().oneshot(get("/v1/summary")).await.unwrap();
    let etag = response.headers()["etag"].clone();
    let first: Value =
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 4 << 20).await.unwrap()).unwrap();
    let mut request = get("/v1/summary");
    request.headers_mut().insert("if-none-match", etag);
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert!(
        axum::body::to_bytes(response.into_body(), 4 << 20)
            .await
            .unwrap()
            .is_empty()
    );
    let (_, changed) = call(
        &engine,
        get(&format!("/v1/changes?since={}", first["revision"].as_str().unwrap())),
    )
    .await;
    assert_eq!(changed["changed"], false);
    engine.set_demo_mode(true).unwrap();
    let (_, changed) = call(
        &engine,
        get(&format!("/v1/changes?since={}", first["revision"].as_str().unwrap())),
    )
    .await;
    assert_eq!(changed["changed"], true);
    let request = Request::head("/v1/machines")
        .header("host", "localhost")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.headers()["x-data-source"], "demo");
    assert!(
        axum::body::to_bytes(response.into_body(), 4 << 20)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn rejects_invalid_input_and_demo_troubleshooting_with_structured_errors() {
    let (_dir, engine) = engine(true);
    for path in [
        "/v1/fleet?unknown=1",
        "/v1/machines?scope=nope",
        "/v1/runners?limit=0",
        "/v1/events?limit=201",
        "/v1/events?before=garbage",
        "/v1/events?machine=x&machineKey=y",
        "/v1/monitoring?before=00000000000000000001",
    ] {
        let (status, body) = call(&engine, get(path)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
        assert_eq!(body["code"], "invalid_request");
    }
    let post = Request::post("/v1/checks")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"kind":"doctor"}"#))
        .unwrap();
    let (status, body) = call(&engine, post).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("demo_mode"))
    );
    assert_eq!(
        call(&engine, get("/v1/logs?runner=studio-1")).await.0,
        StatusCode::CONFLICT
    );
    engine.set_demo_mode(false).unwrap();
    for body in [r#"{"kind":"apply"}"#, r#"{"kind":"doctor","secret":"no"}"#, "bad json"] {
        let request = Request::post("/v1/checks")
            .header("host", "localhost")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let (status, error) = call(&engine, request).await;
        assert!(status.is_client_error());
        assert_eq!(error["code"], "invalid_request");
    }
}

#[tokio::test]
async fn schema_has_every_route_and_resolves_all_model_references() {
    let (_dir, engine) = engine(true);
    let (status, schema) = call(&engine, get("/v1/openapi.json")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(schema["openapi"], "3.1.0");
    assert!(!schema.as_object().unwrap().contains_key("source"));
    fn refs(value: &Value, document: &Value) {
        match value {
            Value::Object(object) => {
                if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                    assert!(
                        document.pointer(reference.strip_prefix('#').unwrap()).is_some(),
                        "Unresolved {reference}"
                    );
                }
                for value in object.values() {
                    refs(value, document);
                }
            }
            Value::Array(values) => {
                for value in values {
                    refs(value, document);
                }
            }
            _ => {}
        }
    }
    refs(&schema, &schema);
    for route in super::schema::endpoint_descriptions().as_object().unwrap().keys() {
        let (method, path) = route.split_once(' ').unwrap();
        assert!(
            schema["paths"][path][method.to_ascii_lowercase()].is_object(),
            "{route}"
        );
    }
    assert_eq!(
        schema["components"]["schemas"]["CheckRequest"]["additionalProperties"],
        false
    );
}

#[tokio::test]
async fn plain_text_errors_keep_a_status_appropriate_code() {
    let (_d, e) = engine(true);
    let state = ApiState {
        engine: e,
        checks: Arc::new(checks::CheckStore::default()),
        logs: Arc::new(tokio::sync::Semaphore::new(1)),
    };
    let app = Router::new()
        .route(
            "/v1/too-large",
            axum::routing::get(|| async { (StatusCode::PAYLOAD_TOO_LARGE, "Body too large") }),
        )
        .route(
            "/v1/broken",
            axum::routing::get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "Broken") }),
        )
        .layer(middleware::from_fn_with_state(state.clone(), common::request))
        .with_state(state);
    for (path, status, code) in [
        ("/v1/too-large", StatusCode::PAYLOAD_TOO_LARGE, "invalid_request"),
        ("/v1/broken", StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    ] {
        let resp = app.clone().oneshot(get(path)).await.unwrap();
        assert_eq!(resp.status(), status);
        let body: Value =
            serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap()).unwrap();
        assert_eq!(body["code"], code, "{path}");
    }
}
