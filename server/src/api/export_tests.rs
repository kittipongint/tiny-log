//! Export: same filters as the list, every row once, newest first, safe CSV.

use super::setup_tests::{app, test_config};
use crate::config::AuthMode;
use crate::models::log::InsertLog;
use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn get_raw(router: &Router, path: &str) -> (StatusCode, HeaderMap, String) {
    let res = router
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

fn log(ts: i64, app: &str, level: &str, msg: &str, meta: Option<&str>) -> InsertLog {
    InsertLog {
        timestamp_ms: ts,
        app: app.into(),
        level: level.into(),
        source: Some("server".into()),
        message: msg.into(),
        meta_json: meta.map(str::to_string),
    }
}

#[tokio::test]
async fn ndjson_streams_every_matching_row_once_newest_first() {
    let (router, state) = app(test_config(AuthMode::Anonymous, None)).await;
    let now = chrono::Utc::now().timestamp_millis();
    // 2,500 rows over 3 pages; pairs share a timestamp so the keyset tie-break on id matters.
    let rows: Vec<InsertLog> = (0..2500)
        .map(|i| {
            let app = if i % 5 == 0 { "hora" } else { "wordyguru" };
            log(now - (i / 2) * 1000, app, "info", &format!("m{i}"), Some(r#"{"n":1}"#))
        })
        .collect();
    crate::db::logs::insert_logs_batch(&state.logs_db, &rows).await.unwrap();

    let (status, headers, body) = get_raw(&router, "/api/v1/logs/export").await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("application/x-ndjson"));
    let cd = headers[header::CONTENT_DISPOSITION].to_str().unwrap();
    assert!(cd.starts_with("attachment; filename=\"tiny-log-all-") && cd.ends_with(".ndjson\""), "{cd}");

    let lines: Vec<serde_json::Value> =
        body.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2500);
    let mut ids: Vec<i64> = lines.iter().map(|v| v["id"].as_i64().unwrap()).collect();
    let ts: Vec<&str> = lines.iter().map(|v| v["timestamp"].as_str().unwrap()).collect();
    assert!(ts.windows(2).all(|w| w[0] >= w[1]), "newest first");
    assert_eq!(lines[0]["meta"]["n"], 1, "meta stays an object");
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 2500, "no row twice");

    let (_, headers, body) = get_raw(&router, "/api/v1/logs/export?app=hora").await;
    assert_eq!(body.lines().count(), 500);
    assert!(headers[header::CONTENT_DISPOSITION].to_str().unwrap().contains("tiny-log-hora-"));

    let (_, _, body) = get_raw(&router, "/api/v1/logs/export?limit=1234").await;
    assert_eq!(body.lines().count(), 1234);
}

#[tokio::test]
async fn csv_has_bom_header_quoting_and_formula_guard() {
    let (router, state) = app(test_config(AuthMode::Anonymous, None)).await;
    let now = chrono::Utc::now().timestamp_millis();
    crate::db::logs::insert_logs_batch(
        &state.logs_db,
        &[
            log(now, "wordyguru", "error", "a, \"b\"\nc", Some(r#"{"path":"/q/คำ"}"#)),
            log(now - 1, "wordyguru", "warn", "=HYPERLINK(\"x\")", None),
            log(now - 2, "wordyguru", "info", "ภาษาไทย", None),
        ],
    )
    .await
    .unwrap();

    let (status, headers, body) = get_raw(&router, "/api/v1/logs/export?format=csv").await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/csv"));
    assert!(body.starts_with("\u{feff}id,timestamp,app,level,source,message,meta\r\n"));
    assert!(body.contains(",\"a, \"\"b\"\"\nc\",\"{\"\"path\"\":\"\"/q/คำ\"\"}\"\r\n"), "{body}");
    assert!(body.contains(",\"'=HYPERLINK(\"\"x\"\")\","), "{body}");
    assert!(body.contains(",ภาษาไทย,\r\n"), "{body}");
    assert_eq!(body.matches("\r\n").count(), 4);
}

#[tokio::test]
async fn empty_export_is_a_header_or_nothing() {
    let (router, _) = app(test_config(AuthMode::Anonymous, None)).await;
    let (_, _, body) = get_raw(&router, "/api/v1/logs/export").await;
    assert_eq!(body, "");
    let (_, _, body) = get_raw(&router, "/api/v1/logs/export?format=csv").await;
    assert_eq!(body.lines().count(), 1);
}

#[tokio::test]
async fn bad_input_is_a_json_400_not_a_broken_file() {
    let (router, _) = app(test_config(AuthMode::Anonymous, None)).await;
    let (status, _, body) = get_raw(&router, "/api/v1/logs/export?format=xlsx").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("\"error\""));
    let (status, _, _) = get_raw(&router, "/api/v1/logs/export?from=yesterday").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn login_mode_needs_a_session() {
    let (router, _) = app(test_config(AuthMode::Login, Some("tok"))).await;
    let (status, _, _) = get_raw(&router, "/api/v1/logs/export").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
