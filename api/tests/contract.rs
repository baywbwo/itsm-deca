//! Keeps `openapi/openapi.json` (contract source) and the router in step:
//! every documented operation is routed, and every routed operation is documented.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use itsm_api::config::Config;
use itsm_api::routes::API_ROUTES;
use itsm_api::{AppState, app};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;

const HEALTH_ROUTES: &[(&str, &str)] = &[("get", "/health/live"), ("get", "/health/ready")];
const METHODS: &[&str] = &[
    "get", "put", "post", "delete", "patch", "head", "options", "trace",
];

fn spec() -> Value {
    let raw = include_str!("../../openapi/openapi.json");
    match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => panic!("openapi.json is not valid JSON: {e}"),
    }
}

fn documented_operations(spec: &Value) -> BTreeSet<(String, String)> {
    let mut ops = BTreeSet::new();
    let Some(paths) = spec["paths"].as_object() else {
        panic!("openapi.json has no paths object");
    };
    for (path, item) in paths {
        let Some(item) = item.as_object() else {
            continue;
        };
        for method in item.keys().filter(|k| METHODS.contains(&k.as_str())) {
            ops.insert((method.clone(), path.clone()));
        }
    }
    ops
}

fn routed_operations() -> BTreeSet<(String, String)> {
    API_ROUTES
        .iter()
        .chain(HEALTH_ROUTES)
        .map(|(m, p)| ((*m).to_owned(), (*p).to_owned()))
        .collect()
}

#[test]
fn openapi_matches_route_table() {
    let doc = spec();
    assert_eq!(doc["openapi"], "3.1.0");
    assert_eq!(documented_operations(&doc), routed_operations());
}

#[test]
fn every_operation_has_unique_operation_id() {
    let doc = spec();
    let mut seen = BTreeSet::new();
    for (method, path) in documented_operations(&doc) {
        let id = doc["paths"][&path][&method]["operationId"].as_str();
        let Some(id) = id else {
            panic!("{method} {path} has no operationId");
        };
        assert!(seen.insert(id.to_owned()), "duplicate operationId {id}");
    }
}

#[test]
fn every_local_ref_resolves() {
    fn walk(node: &Value, root: &Value) {
        match node {
            Value::Object(map) => {
                if let Some(Value::String(r)) = map.get("$ref") {
                    let Some(pointer) = r.strip_prefix('#') else {
                        panic!("non-local $ref {r}");
                    };
                    assert!(root.pointer(pointer).is_some(), "unresolved $ref {r}");
                }
                map.values().for_each(|v| walk(v, root));
            }
            Value::Array(items) => items.iter().for_each(|v| walk(v, root)),
            _ => {}
        }
    }
    let doc = spec();
    walk(&doc, &doc);
}

fn uat_state(pool: &PgPool) -> AppState {
    let vars: HashMap<String, String> = [
        ("ITSM_ENV", "uat"),
        ("ITSM_ACCESS_MODE", "uat_superadmin"),
        ("DATABASE_URL", "postgres://unused-in-tests"),
        ("ITSM_COOKIE_SECURE", "false"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    let config = match Config::from_map(&vars) {
        Ok(c) => c,
        Err(e) => panic!("test config invalid: {e}"),
    };
    AppState {
        config: Arc::new(config),
        pool: pool.clone(),
    }
}

/// Each documented operation reaches a handler: no 405, and no bare router 404
/// (handler 404s carry the JSON error envelope).
#[sqlx::test(migrator = "itsm_api::db::MIGRATOR")]
async fn every_documented_operation_is_routed(pool: PgPool) {
    let state = uat_state(&pool);
    for (method, path) in documented_operations(&spec()) {
        let uri = path.replace("{workspace_id}", "00000000-0000-4000-8000-000000000000");
        let method = match Method::from_bytes(method.to_uppercase().as_bytes()) {
            Ok(m) => m,
            Err(e) => panic!("bad method {method}: {e}"),
        };
        let req = match Request::builder()
            .method(method.clone())
            .uri(&uri)
            .body(Body::empty())
        {
            Ok(r) => r,
            Err(e) => panic!("request build failed: {e}"),
        };
        let res = match app(state.clone()).oneshot(req).await {
            Ok(r) => r,
            Err(e) => panic!("router error: {e}"),
        };
        let status = res.status();
        assert_ne!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {uri} not routed"
        );
        if status == StatusCode::NOT_FOUND {
            let json = res
                .headers()
                .get(header::CONTENT_TYPE)
                .is_some_and(|v| v.as_bytes().starts_with(b"application/json"));
            assert!(
                json,
                "{method} {uri} hit the router fallback, not a handler"
            );
        }
    }
}
