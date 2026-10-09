//! OpenAPI 3.1 contract with JSON Schemas generated from the actual Rust views.

use axum::Json;
use schemars::JsonSchema;
use serde_json::{Map, Value, json};
use std::sync::OnceLock;

use super::MAX_PAGE_SIZE;
use super::checks::{CheckJob, CheckRequest, DEFAULT_LOG_LINES, LogQuery, LogRead, MAX_LOG_LINES};
use super::common::ErrorBody;
use super::queries::{self, NoQuery, Parameters};
use crate::model::{FleetView, MachineView};
use crate::monitoring::{EVENT_PAGE_SIZE, EVENT_RETENTION, MAX_EVENTS};

pub(super) fn endpoint_descriptions() -> Value {
    json!({
        "GET /": "Capabilities, limits and semantics",
        "GET /v1/summary": "Fleet counts, GitHub and tailnet status; attention equals issues.length",
        "GET /v1/fleet": "Complete snapshot, including shared issues and all machine categories",
        "GET /v1/machines": "Machine list; scope=all (default), fleet, or discovered",
        "GET /v1/machines/{id}": "Machine by exact ID, inventory alias, or unambiguous name",
        "GET /v1/runners": "Runner list, filtered by machine, org, activity, label, or current-job repo",
        "GET /v1/runners/{key}": "Runner by exact key or unambiguous name; optional org",
        "GET /v1/queue": "Self-hosted queue filtered by org, repo and label; scope metadata stays fleet-wide",
        "GET /v1/discovered": "Other machines and ignored tailnet peers",
        "GET /v1/desired": "Global plan plus per-host plans, runner status, blocked reasons and doctor checks; optional machine",
        "GET /v1/monitoring": format!("Current alerts and latest {EVENT_PAGE_SIZE} events; optional machine"),
        "GET /v1/events": "Retained history, newest first; machine or machineKey, before, limit",
        "GET /v1/issues": "Shared attention findings; machine, category, stale, muted, after, limit",
        "GET /v1/changes": "Compare the current revision with since",
        "GET /v1/logs": "Bounded log tail for an inventory runner; runner, org, lines",
        "POST /v1/checks": "Start a read-only refresh, inventory check, or doctor job; optional machine",
        "GET /v1/checks/{id}": "Read check status and completion result",
        "GET /v1/openapi.json": "OpenAPI 3.1; provenance appears in response headers"
    })
}

pub(super) async fn openapi(Parameters(_): Parameters<NoQuery>) -> Json<Value> {
    static DOCUMENT: OnceLock<Value> = OnceLock::new();
    Json(DOCUMENT.get_or_init(document).clone())
}

fn reference(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}
fn array(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}
fn string() -> Value {
    json!({ "type": "string" })
}
fn object(properties: Value) -> Value {
    let required: Vec<_> = properties
        .as_object()
        .expect("schema properties are an object")
        .keys()
        .cloned()
        .collect();
    json!({ "type": "object", "properties": properties, "required": required })
}
fn envelope(schema: Value) -> Value {
    json!({ "allOf": [reference("Metadata"), schema] })
}
fn collection(field: &str, item: Value) -> Value {
    envelope(object(
        json!({ field: array(item), "total": { "type": "integer", "minimum": 0 },
        "nextCursor": { "type": ["string", "null"] } }),
    ))
}

fn register<T: JsonSchema>(components: &mut Map<String, Value>) -> Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(T)).expect("JSON Schema serializes to JSON");
    rewrite_refs(&mut schema);
    let root = schema.as_object_mut().expect("a generated root schema is an object");
    if let Some(Value::Object(defs)) = root.remove("$defs") {
        components.extend(defs);
    }
    root.remove("$schema");
    let name = T::schema_name().into_owned();
    components.insert(name.clone(), schema);
    reference(&name)
}

fn rewrite_refs(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if let Some(Value::String(reference)) = object.get_mut("$ref") {
                *reference = reference.replace("#/$defs/", "#/components/schemas/");
            }
            for value in object.values_mut() {
                rewrite_refs(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                rewrite_refs(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn parameters<T: JsonSchema>(components: &mut Map<String, Value>) -> Vec<Value> {
    register::<T>(components);
    let schema = components
        .get(T::schema_name().as_ref())
        .expect("just registered query schema");
    let required = schema["required"].as_array().cloned().unwrap_or_default();
    schema["properties"]
        .as_object()
        .into_iter()
        .flat_map(|p| p.iter())
        .map(|(name, schema)| {
            let mut schema = schema.clone();
            if name == "limit" {
                schema = json!({ "type": "integer", "minimum": 1, "maximum": MAX_PAGE_SIZE });
            }
            if name == "lines" {
                schema =
                    json!({ "type": "integer", "minimum": 1, "maximum": MAX_LOG_LINES, "default": DEFAULT_LOG_LINES });
            }
            if name == "before" {
                schema = json!({ "type": "string", "pattern": "^[0-9]{20}$" });
            }
            json!({ "in": "query", "name": name, "required": required.contains(&json!(name)), "schema": schema })
        })
        .collect()
}

fn document() -> Value {
    let mut components = Map::new();
    let fleet = register::<FleetView>(&mut components);
    let machine = register::<MachineView>(&mut components);
    let runner = register::<queries::FlatRunner>(&mut components);
    let check = register::<CheckJob>(&mut components);
    let check_request = register::<CheckRequest>(&mut components);
    let logs = register::<LogRead>(&mut components);
    register::<ErrorBody>(&mut components);
    components.insert(
        "Metadata".into(),
        object(json!({
            "apiVersion": { "type": "integer", "const": 1 }, "source": { "enum": ["live", "demo"] },
            "generatedAt": { "type": "string", "format": "date-time" }, "revision": string(),
        })),
    );
    let host = object(
        json!({ "machineId": string(), "machineKey": string(), "machineName": string(),
        "desired": reference("MachineDesired"), "runners": array(object(json!({ "key": string(), "name": string(), "desired": reference("RunnerDesired") }))) }),
    );
    let endpoints = endpoint_descriptions();
    let routes = vec![
        (
            "/",
            envelope(object(
                json!({ "name": string(), "version": string(), "readOnly": { "const": true },
            "capabilities": { "type": "object" }, "limits": { "type": "object", "additionalProperties": { "type": "integer" } },
            "endpoints": { "type": "object", "additionalProperties": string() }, "semantics": { "type": "object", "additionalProperties": string() } }),
            )),
            parameters::<NoQuery>(&mut components),
        ),
        (
            "/v1/summary",
            envelope(object(
                json!({ "summary": reference("FleetSummary"), "orgs": array(reference("OrgStatusView")), "tailnet": reference("TailnetStatusView") }),
            )),
            parameters::<NoQuery>(&mut components),
        ),
        ("/v1/fleet", envelope(fleet), parameters::<NoQuery>(&mut components)),
        (
            "/v1/machines",
            json!({ "allOf": [collection("machines", machine.clone()), object(json!({ "scope": { "enum": ["all", "fleet", "discovered"] } }))] }),
            parameters::<queries::MachineQuery>(&mut components),
        ),
        (
            "/v1/machines/{id}",
            envelope(machine),
            parameters::<NoQuery>(&mut components),
        ),
        (
            "/v1/runners",
            collection("runners", runner.clone()),
            parameters::<queries::RunnerQuery>(&mut components),
        ),
        (
            "/v1/runners/{key}",
            envelope(runner),
            parameters::<queries::OrgQuery>(&mut components),
        ),
        (
            "/v1/queue",
            envelope(object(
                json!({ "queue": reference("QueueView"), "total": { "type": "integer" }, "nextCursor": { "type": ["string", "null"] } }),
            )),
            parameters::<queries::QueueQuery>(&mut components),
        ),
        (
            "/v1/discovered",
            envelope(object(
                json!({ "otherMachines": array(reference("MachineView")), "ignored": array(reference("DiscoveredPeerView")) }),
            )),
            parameters::<NoQuery>(&mut components),
        ),
        (
            "/v1/desired",
            envelope(object(
                json!({ "plan": reference("FleetPlanStatus"), "hosts": array(host) }),
            )),
            parameters::<queries::MachineFilter>(&mut components),
        ),
        (
            "/v1/monitoring",
            envelope(object(
                json!({ "monitoring": { "oneOf": [reference("FleetMonitoring"), reference("FleetMachineMonitoring")] } }),
            )),
            parameters::<queries::MachineFilter>(&mut components),
        ),
        (
            "/v1/events",
            envelope(object(
                json!({ "events": array(reference("FleetMachineEvent")), "hasMore": { "type": "boolean" }, "nextCursor": { "type": ["string", "null"] }, "retentionDays": { "const": EVENT_RETENTION.num_days() }, "maxEvents": { "const": MAX_EVENTS } }),
            )),
            parameters::<queries::EventQuery>(&mut components),
        ),
        (
            "/v1/issues",
            collection("issues", reference("FleetIssue")),
            parameters::<queries::IssueQuery>(&mut components),
        ),
        (
            "/v1/changes",
            envelope(object(json!({ "changed": { "type": "boolean" } }))),
            parameters::<queries::ChangesQuery>(&mut components),
        ),
        ("/v1/logs", envelope(logs), parameters::<LogQuery>(&mut components)),
        (
            "/v1/checks",
            envelope(object(json!({ "check": check.clone() }))),
            parameters::<NoQuery>(&mut components),
        ),
        (
            "/v1/checks/{id}",
            envelope(object(json!({ "check": check }))),
            parameters::<NoQuery>(&mut components),
        ),
        (
            "/v1/openapi.json",
            json!({ "type": "object", "required": ["openapi", "info", "paths", "components"] }),
            parameters::<NoQuery>(&mut components),
        ),
    ];
    let mut paths = Map::new();
    for (path, schema, mut parameters) in routes {
        let post = path == "/v1/checks";
        let method = if post { "post" } else { "get" };
        let description = endpoints[format!("{} {path}", method.to_ascii_uppercase())]
            .as_str()
            .unwrap_or("Read snapshot");
        if let Some(start) = path.find('{') {
            parameters.push(
                json!({ "in": "path", "name": &path[start + 1..path.len() - 1], "required": true, "schema": string(),
                "description": "URL-encode the complete identifier, including slashes in local runner keys." }),
            );
        }
        if !post {
            parameters.push(json!({ "in": "header", "name": "If-None-Match", "schema": string(), "required": false }));
        }
        let mut responses = Map::new();
        responses.insert(
            if post { "202" } else { "200" }.into(),
            json!({ "description": description,
            "headers": { "ETag": { "schema": string(), "description": "Weak validator for conditional GET" },
                "X-Data-Source": { "schema": { "enum": ["demo", "live"] } }, "X-Fleet-Revision": { "schema": string() },
                "X-Api-Version": { "schema": { "type": "string", "const": "1" } } },
            "content": { "application/json": { "schema": schema } } }),
        );
        if post {
            responses["202"]["headers"]["Location"] =
                json!({ "schema": string(), "description": "Poll this URL for completion" });
        } else {
            responses.insert(
                "304".into(),
                json!({ "description": "Representation unchanged; no response body" }),
            );
        }
        for code in [
            "400", "403", "404", "405", "409", "413", "415", "422", "429", "500", "502",
        ] {
            responses.insert(
                code.into(),
                json!({ "description": "Structured API error; only busy responses are automatically retryable",
                "headers": { "Retry-After": { "schema": string(), "description": "Seconds before retrying a 429" } },
                "content": { "application/json": { "schema": reference("ErrorBody") } } }),
            );
        }
        let mut operation = json!({ "summary": description, "operationId": format!("{}_{}", method, path.trim_start_matches('/').replace(['/', '{', '}', '.'], "_")),
            "parameters": parameters, "responses": responses });
        if post {
            operation["requestBody"] =
                json!({ "required": true, "content": { "application/json": { "schema": check_request } } });
        }
        paths.insert(path.into(), json!({ method: operation }));
    }
    json!({ "openapi": "3.1.0", "info": { "title": "Runner Dashboard local API", "version": "1",
        "description": "Loopback only. Rejects Origin headers and non-loopback Host values. All successful JSON reads include apiVersion, source, generatedAt, revision; OpenAPI uses metadata headers. HEAD is supported on GET routes and has no body. Unknown query fields are rejected. No runner mutation or token creation endpoints. See GET / for pagination, freshness and retention semantics." },
        "servers": [{ "url": "/", "description": "Current loopback listener; use local-api.json for discovery" }],
        "paths": paths, "components": { "schemas": components } })
}
