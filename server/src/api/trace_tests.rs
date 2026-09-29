//! Correlation id: lines of one request listed in order, and grouped into traces.

use super::setup_tests::{app, test_config};
use crate::config::AuthMode;
use crate::models::log::NewLog;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn get_json(router: &Router, path: &str) -> (StatusCode, Value) {
    let res = router
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn line(ts: i64, app: &str, level: &str, msg: &str, meta: Value) -> NewLog {
    NewLog {
        app: app.into(),
        level: level.into(),
        message: msg.into(),
        timestamp: Some(ts.to_string()),
        source: Some("server".into()),
        meta: Some(meta),
    }
}

#[tokio::test]
async fn trace_filter_and_grouping() {
    let (router, state) = app(test_config(AuthMode::Anonymous, None)).await;
    let now = chrono::Utc::now().timestamp_millis();
    let rows = vec![
        line(now - 5000, "wordyguru", "info", "GET /q/x start", json!({"request_id": "r1", "path": "/q/x"})),
        line(now - 4000, "wordyguru", "error", "search failed", json!({"request_id": "r1"})),
        line(now - 3000, "wordyguru", "error", "resource_load_error", json!({"request_id": "r1", "log_origin": "browser"})),
        line(now - 2000, "hora", "warn", "slow", json!({"correlation_id": "c2"})),
        line(now - 1000, "hora", "info", "no id", json!({"path": "/"})),
        line(now - 500, "hora", "info", "numeric id", json!({"trace_id": 42})),
        line(now - 400, "hora", "info", "empty id", json!({"request_id": ""})),
    ];
    let inserts: Vec<_> = rows.into_iter().map(|r| r.validate_and_normalize().unwrap()).collect();
    // one through the single-row path, the rest batched: both must fill trace_id
    crate::db::logs::insert_log(&state.logs_db, &inserts[0]).await.unwrap();
    crate::db::logs::insert_logs_batch(&state.logs_db, &inserts[1..]).await.unwrap();

    let (st, body) = get_json(&router, "/api/v1/logs?trace=r1&order=asc").await;
    assert_eq!(st, StatusCode::OK);
    let msgs: Vec<&str> = body["logs"].as_array().unwrap().iter().map(|l| l["message"].as_str().unwrap()).collect();
    assert_eq!(msgs, ["GET /q/x start", "search failed", "resource_load_error"], "oldest first");

    let (_, body) = get_json(&router, "/api/v1/logs?trace=c2").await;
    assert_eq!(body["logs"].as_array().unwrap().len(), 1, "correlation_id counts too");
    let (_, body) = get_json(&router, "/api/v1/logs?trace=42").await;
    assert_eq!(body["logs"].as_array().unwrap().len(), 1, "numeric trace_id stored as text");

    let (st, body) = get_json(&router, "/api/v1/traces").await;
    assert_eq!(st, StatusCode::OK);
    let traces = body["traces"].as_array().unwrap();
    let ids: Vec<&str> = traces.iter().map(|t| t["trace_id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["42", "c2", "r1"], "newest activity first, no empty or missing ids");
    let r1 = &traces[2];
    assert_eq!(r1["lines"], 3);
    assert_eq!(r1["level"], "error");
    assert_eq!(r1["first_message"], "GET /q/x start");
    assert_eq!(r1["last_ms"].as_i64().unwrap() - r1["first_ms"].as_i64().unwrap(), 2000);

    // A search that matches one line still returns the whole trace's numbers.
    let (_, body) = get_json(&router, "/api/v1/traces?search=search%20failed").await;
    let traces = body["traces"].as_array().unwrap();
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0]["lines"], 3);

    // Export honours the trace filter.
    let res = router
        .clone()
        .oneshot(Request::get("/api/v1/logs/export?trace=r1").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(String::from_utf8(bytes.to_vec()).unwrap().lines().count(), 3);
}

#[tokio::test]
async fn before_pages_back_through_older_rows() {
    let (router, state) = app(test_config(AuthMode::Anonymous, None)).await;
    let now = chrono::Utc::now().timestamp_millis();
    // two rows share a timestamp so the page edge has to break the tie on id
    let stamps = [now - 5000, now - 4000, now - 3000, now - 3000, now - 1000];
    let inserts: Vec<_> = stamps
        .iter()
        .enumerate()
        .map(|(i, ts)| line(*ts, "wordyguru", "info", &format!("m{i}"), json!({})).validate_and_normalize().unwrap())
        .collect();
    crate::db::logs::insert_logs_batch(&state.logs_db, &inserts).await.unwrap();

    let msgs = |body: &Value| -> Vec<String> {
        body["logs"].as_array().unwrap().iter().map(|l| l["message"].as_str().unwrap().to_string()).collect()
    };
    let (_, p1) = get_json(&router, "/api/v1/logs?limit=2").await;
    assert_eq!(msgs(&p1), ["m4", "m3"]);
    let last = p1["logs"][1]["id"].as_i64().unwrap();

    // a new line arriving between pages must not shift the next page
    let late = line(now, "wordyguru", "info", "late", json!({})).validate_and_normalize().unwrap();
    crate::db::logs::insert_log(&state.logs_db, &late).await.unwrap();

    let (st, p2) = get_json(&router, &format!("/api/v1/logs?limit=2&before={last}")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(msgs(&p2), ["m2", "m1"]);
    let last = p2["logs"][1]["id"].as_i64().unwrap();
    let (_, p3) = get_json(&router, &format!("/api/v1/logs?limit=2&before={last}")).await;
    assert_eq!(msgs(&p3), ["m0"]);

    // filters still apply on older pages
    let (_, none) = get_json(&router, &format!("/api/v1/logs?app=hora&before={last}")).await;
    assert!(none["logs"].as_array().unwrap().is_empty());
}
