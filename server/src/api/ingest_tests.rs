//! Ingest contract, retention chunking, per-connection PRAGMAs and the idle broadcast.

use super::setup_tests::{app, call, test_config};
use crate::config::AuthMode;
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};

fn post_json(path: &str, body: String) -> Request<Body> {
    Request::post(path)
        .header(header::AUTHORIZATION, "Bearer test-api-key")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

fn entry(msg: &str) -> Value {
    json!({ "app": "wordyguru", "level": "info", "message": msg })
}

async fn count_logs(state: &crate::state::AppState) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM logs")
        .fetch_one(&state.logs_db)
        .await
        .unwrap()
}

#[tokio::test]
async fn pragmas_reach_every_pooled_connection() {
    let (_router, state) = app(test_config(AuthMode::Anonymous, None)).await;

    // Hold all 5 at once so the pool can't hand out the same one twice.
    let mut conns = Vec::new();
    for _ in 0..5 {
        conns.push(state.logs_db.acquire().await.unwrap());
    }
    for conn in conns.iter_mut() {
        let mut got = Vec::new();
        for pragma in [
            "cache_size",
            "temp_store",
            "journal_size_limit",
            "wal_autocheckpoint",
            "auto_vacuum",
        ] {
            let v: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("PRAGMA {pragma}")))
                .fetch_one(&mut **conn)
                .await
                .unwrap();
            got.push(v);
        }
        assert_eq!(got, [-20000, 2, 67_108_864, 1000, 2]);
    }
}

#[tokio::test]
async fn retention_deletes_in_chunks_and_keeps_recent_rows() {
    let (_router, state) = app(test_config(AuthMode::Anonymous, None)).await;
    let now = chrono::Utc::now().timestamp_millis();
    for i in 0..15i64 {
        let ts = if i < 12 { 1_000 + i } else { now };
        sqlx::query("INSERT INTO logs (timestamp_ms, app, level, message) VALUES (?, 'a', 'info', 'm')")
            .bind(ts)
            .execute(&state.logs_db)
            .await
            .unwrap();
    }

    let deleted = crate::db::logs::delete_in_chunks(&state.logs_db, "logs", now - 1, 5)
        .await
        .unwrap();
    assert_eq!(deleted, 12); // 5 + 5 + 2
    assert_eq!(count_logs(&state).await, 3);

    // Metrics tables go through the same path.
    assert_eq!(
        crate::db::metrics::delete_older_than(&state.metrics_db, now).await.unwrap(),
        0
    );
}

#[tokio::test]
async fn bad_entries_reject_the_batch_and_name_their_indexes() {
    let (router, state) = app(test_config(AuthMode::Anonymous, None)).await;
    let body = json!({ "logs": [
        entry("ok"),
        { "app": "wordyguru", "level": "loud", "message": "x" },
        entry("ok"),
        { "app": "", "level": "info", "message": "x" },
    ]});

    let (status, _, res) = call(&router, post_json("/api/v1/logs/batch", body.to_string())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(res["error"], "invalid level: loud");
    assert_eq!(res["rejected"][0]["index"], 1);
    assert_eq!(res["rejected"][1]["index"], 3);
    assert_eq!(res["rejected"].as_array().unwrap().len(), 2);
    assert_eq!(count_logs(&state).await, 0);
}

#[tokio::test]
async fn too_many_entries_is_413_json_with_limits() {
    let (router, _state) = app(test_config(AuthMode::Anonymous, None)).await;
    let logs: Vec<Value> = (0..501).map(|i| entry(&format!("m{i}"))).collect();

    let (status, _, res) =
        call(&router, post_json("/api/v1/logs/batch", json!({ "logs": logs }).to_string())).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(res["max_batch"], 500);
    assert_eq!(res["max_body_bytes"], 1024 * 1024);
}

#[tokio::test]
async fn oversize_body_is_413_json_not_plain_text() {
    let (router, _state) = app(test_config(AuthMode::Anonymous, None)).await;
    let router = router.layer(DefaultBodyLimit::max(1024 * 1024));
    let big = "x".repeat(60 * 1024);
    let logs: Vec<Value> = (0..20).map(|_| entry(&big)).collect(); // ~1.2 MB

    let (status, _, res) =
        call(&router, post_json("/api/v1/logs/batch", json!({ "logs": logs }).to_string())).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(res["error"], "payload too large");
    assert_eq!(res["max_body_bytes"], 1024 * 1024);
}

#[tokio::test]
async fn extractor_rejections_are_json_and_keep_their_status() {
    let (router, _state) = app(test_config(AuthMode::Anonymous, None)).await;

    // wrong shape → 422
    let (status, _, res) = call(&router, post_json("/api/v1/logs", r#"{"app":"a"}"#.into())).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(res["error"].as_str().unwrap().contains("missing field"));

    // broken JSON → 400
    let (status, _, res) = call(&router, post_json("/api/v1/logs/batch", "{\"logs\": [".into())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(res["error"].is_string());

    // no content-type → 415
    let req = Request::post("/api/v1/logs")
        .header(header::AUTHORIZATION, "Bearer test-api-key")
        .body(Body::from(entry("m").to_string()))
        .unwrap();
    let (status, _, res) = call(&router, req).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert!(res["error"].is_string());
}

#[tokio::test]
async fn unix_seconds_timestamp_is_stored_as_that_time() {
    let (router, state) = app(test_config(AuthMode::Anonymous, None)).await;
    let secs = chrono::Utc::now().timestamp() - 60;
    let mut e = entry("m");
    e["timestamp"] = json!(secs.to_string());

    let (status, _, _) = call(&router, post_json("/api/v1/logs", e.to_string())).await;
    assert_eq!(status, StatusCode::OK);
    let ts: i64 = sqlx::query_scalar("SELECT timestamp_ms FROM logs")
        .fetch_one(&state.logs_db)
        .await
        .unwrap();
    assert_eq!(ts, secs * 1000);
}

#[tokio::test]
async fn broadcast_reaches_viewers_and_is_skipped_without_them() {
    let (router, state) = app(test_config(AuthMode::Anonymous, None)).await;

    // Nobody watching: insert still succeeds.
    assert_eq!(state.broadcaster.receiver_count(), 0);
    let (status, _, _) = call(&router, post_json("/api/v1/logs", entry("quiet").to_string())).await;
    assert_eq!(status, StatusCode::OK);

    let mut rx = state.broadcaster.subscribe();
    let mut e = entry("seen");
    e["meta"] = json!({ "route": "/search" });
    let body = json!({ "logs": [e, entry("seen too")] });
    let (status, _, _) = call(&router, post_json("/api/v1/logs/batch", body.to_string())).await;
    assert_eq!(status, StatusCode::OK);

    let first = rx.try_recv().unwrap();
    assert_eq!(first.message, "seen");
    assert_eq!(first.meta.unwrap()["route"], "/search");
    assert_eq!(rx.try_recv().unwrap().message, "seen too");
    assert!(rx.try_recv().is_err()); // "quiet" was never queued
}
