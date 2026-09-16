use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::state::AppState;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use futures::stream;
use std::convert::Infallible;
use std::time::Duration;

pub async fn stream_logs(
    State(state): State<AppState>,
    _user: AuthUser,
) -> AppResult<impl IntoResponse> {
    let rx = state.broadcaster.subscribe();

    let stream = stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(entry) => {
                    let data = match serde_json::to_string(&entry) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let event = Event::default().event("log").data(data);
                    return Some((Ok::<_, Infallible>(event), rx));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    ))
}
