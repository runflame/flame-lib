mod error;
mod state;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
};
use serde::Serialize;

use crate::types::*;

use error::ApiError;
pub use state::ApiState;

type ApiResult<T> = Result<Json<T>, ApiError>;

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/state", get(snapshot))
        .route("/api/bitcoin/blocks", post(create_bitcoin_blocks))
        .route("/api/flame/blocks", post(create_flame_block))
        .route("/api/minters", post(create_minter))
        .route("/api/acquisitions", post(send_acquisition))
        .route("/api/votes", post(send_vote))
        .route("/api/reset", post(reset))
        .fallback(|| async {
            ApiError::new(StatusCode::NOT_FOUND, "not_found", "unknown API route")
        })
        .method_not_allowed_fallback(|| async {
            ApiError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "unsupported HTTP method",
            )
        })
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn(no_cache))
        .with_state(state)
}

async fn no_cache(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    btc_tip: BitcoinBlockTip,
    btc_cursor: BitcoinBlockTip,
}

async fn health(State(state): State<ApiState>) -> ApiResult<Health> {
    Ok(Json(
        state
            .execute(|session| {
                Box::pin(async move {
                    let btc_tip = session.bitcoin_tip().await?;
                    let btc_cursor = session.consensus_cursor().await?;
                    Ok(Health {
                        status: if btc_tip == btc_cursor {
                            "ready"
                        } else {
                            "syncing"
                        },
                        btc_tip,
                        btc_cursor,
                    })
                })
            })
            .await?,
    ))
}

async fn snapshot(State(state): State<ApiState>) -> ApiResult<DemoSnapshot> {
    Ok(Json(
        state
            .execute(|session| Box::pin(session.snapshot()))
            .await?,
    ))
}

async fn create_bitcoin_blocks(
    State(state): State<ApiState>,
    body: Result<Json<CreateBitcoinBlocksRequest>, JsonRejection>,
) -> ApiResult<Vec<BitcoinBlockTip>> {
    let Json(request) = body?;
    Ok(Json(
        state
            .execute(move |session| Box::pin(session.create_bitcoin_blocks(request)))
            .await?,
    ))
}

async fn create_flame_block(
    State(state): State<ApiState>,
    body: Result<Json<CreateFlameBlockRequest>, JsonRejection>,
) -> ApiResult<FlameBlockSnapshot> {
    let Json(request) = body?;
    Ok(Json(
        state
            .execute(move |session| Box::pin(session.create_flame_block(request)))
            .await?,
    ))
}

async fn create_minter(
    State(state): State<ApiState>,
    body: Result<Json<CreateMinterRequest>, JsonRejection>,
) -> ApiResult<MinterSnapshot> {
    let Json(request) = body?;
    Ok(Json(
        state
            .execute(move |session| Box::pin(session.create_minter(request)))
            .await?,
    ))
}

async fn send_acquisition(
    State(state): State<ApiState>,
    body: Result<Json<SendAcquisitionRequest>, JsonRejection>,
) -> ApiResult<SubmittedTransaction> {
    let Json(request) = body?;
    Ok(Json(
        state
            .execute(move |session| Box::pin(session.send_acquisition(request)))
            .await?,
    ))
}

async fn send_vote(
    State(state): State<ApiState>,
    body: Result<Json<SendVoteRequest>, JsonRejection>,
) -> ApiResult<SubmittedTransaction> {
    let Json(request) = body?;
    Ok(Json(
        state
            .execute(move |session| Box::pin(session.send_vote(request)))
            .await?,
    ))
}

async fn reset(
    State(state): State<ApiState>,
    body: Result<Json<ResetRequest>, JsonRejection>,
) -> ApiResult<DemoSnapshot> {
    let Json(request) = body?;
    Ok(Json(state.reset(request).await?))
}
