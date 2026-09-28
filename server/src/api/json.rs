//! `Json<T>` whose rejections are JSON too. axum's own 400/413/415/422 answer in plain
//! text; batching clients need the 413 to say how far to split.

use crate::error::AppError;
use crate::state::AppState;
use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use axum::Json;
use serde::de::DeserializeOwned;

pub struct JsonBody<T>(pub T);

impl<T> FromRequest<AppState> for JsonBody<T>
where
    T: DeserializeOwned,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => {
                let status = rejection.status();
                if status == StatusCode::PAYLOAD_TOO_LARGE {
                    Err(state.config.payload_too_large())
                } else {
                    Err(AppError::Status(status, rejection.body_text()))
                }
            }
        }
    }
}
