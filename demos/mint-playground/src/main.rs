#![forbid(unsafe_code)]

mod db;
mod payout;

use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use std::{
    path::PathBuf,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

type Reply<T> = oneshot::Sender<Result<T, db::Error>>;

enum Command {
    Snapshot(bool, Reply<db::Snapshot>),
    Submit(db::ContributionRequest, Reply<db::Submitted>),
    Minter(db::MinterRequest, Reply<db::Minter>),
    Rewards(Reply<Vec<db::RewardBalance>>),
    PayoutState(i64, i64, Reply<db::PayoutState>),
    RecordPayout(db::PendingPayout, Vec<db::PayoutItem>, Reply<()>),
    SettlePayout(String, Option<i64>, Reply<()>),
    Tick(Reply<()>),
    Stop(oneshot::Sender<()>),
}

#[derive(Clone)]
struct Store(mpsc::Sender<Command>);

impl Store {
    fn open(path: PathBuf) -> Result<(Self, std::thread::JoinHandle<()>), String> {
        let (sender, mut receiver) = mpsc::channel(32);
        let (ready, started) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("mint-sqlite".into())
            .spawn(move || {
                let mut database = match db::Database::open(&path) {
                    Ok(database) => database,
                    Err(error) => {
                        let _ = ready.send(Err(error.to_string()));
                        return;
                    }
                };
                let _ = ready.send(Ok(()));
                while let Some(command) = receiver.blocking_recv() {
                    match command {
                        Command::Snapshot(full_history, reply) => {
                            let _ = reply.send(database.snapshot(full_history));
                        }
                        Command::Submit(request, reply) => {
                            let _ = reply.send(database.submit(request));
                        }
                        Command::Minter(request, reply) => {
                            let _ = reply.send(database.minter(request));
                        }
                        Command::Rewards(reply) => {
                            let _ = reply.send(database.rewards());
                        }
                        Command::PayoutState(min_sparks, limit, reply) => {
                            let _ = reply.send(database.payout_state(min_sparks, limit));
                        }
                        Command::RecordPayout(payout, items, reply) => {
                            let _ = reply.send(database.record_payout(payout, items));
                        }
                        Command::SettlePayout(txid, height, reply) => {
                            let _ = reply.send(database.settle_payout(&txid, height));
                        }
                        Command::Tick(reply) => {
                            let _ = reply.send(database.tick());
                        }
                        Command::Stop(reply) => {
                            let _ = reply.send(());
                            break;
                        }
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        started.recv().map_err(|error| error.to_string())??;
        Ok((Self(sender), thread))
    }

    async fn request<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, ApiError> {
        let (reply, result) = oneshot::channel();
        self.0
            .try_send(command(reply))
            .map_err(|_| ApiError::unavailable())?;
        result
            .await
            .map_err(|_| ApiError::unavailable())?
            .map_err(Into::into)
    }

    async fn stop(&self) {
        let (reply, result) = oneshot::channel();
        if self.0.send(Command::Stop(reply)).await.is_ok() {
            let _ = result.await;
        }
    }
}

struct ApiError(StatusCode, String);

impl ApiError {
    fn unavailable() -> Self {
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            "Database busy or unavailable; retry shortly.".into(),
        )
    }
}

impl From<db::Error> for ApiError {
    fn from(error: db::Error) -> Self {
        match error {
            db::Error::Invalid(message) => Self(StatusCode::BAD_REQUEST, message),
            db::Error::Conflict(message) => Self(StatusCode::CONFLICT, message),
            error => {
                eprintln!("Database error: {error}");
                Self(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Database operation failed; no partial accounting was committed.".into(),
                )
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error": self.1}))).into_response()
    }
}

#[derive(Default, serde::Deserialize)]
struct SnapshotQuery {
    #[serde(default)]
    full_history: bool,
}

async fn snapshot(
    State(store): State<Store>,
    Query(query): Query<SnapshotQuery>,
) -> Result<Json<db::Snapshot>, ApiError> {
    store
        .request(|reply| Command::Snapshot(query.full_history, reply))
        .await
        .map(Json)
}

async fn rewards(State(store): State<Store>) -> Result<Json<Vec<db::RewardBalance>>, ApiError> {
    store.request(Command::Rewards).await.map(Json)
}

async fn submit(
    State(store): State<Store>,
    headers: HeaderMap,
    request: Result<Json<db::ContributionRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<db::Submitted>, ApiError> {
    check_origin(&headers)?;
    let Json(request) = request.map_err(|error| {
        ApiError(
            error.status(),
            "Expected contribution JSON with address, budget_sats, duration, request_id and optional minting settings.".into(),
        )
    })?;
    store
        .request(|reply| Command::Submit(request, reply))
        .await
        .map(Json)
}

async fn minter(
    State(store): State<Store>,
    headers: HeaderMap,
    request: Result<Json<db::MinterRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<db::Minter>, ApiError> {
    check_origin(&headers)?;
    let Json(request) = request.map_err(|error| {
        ApiError(
            error.status(),
            "Expected JSON containing address, optional name and optional service_commission_bps."
                .into(),
        )
    })?;
    store
        .request(|reply| Command::Minter(request, reply))
        .await
        .map(Json)
}

fn check_origin(headers: &HeaderMap) -> Result<(), ApiError> {
    // JSON-only writes, no CORS, and an origin check keep other sites out of this local demo.
    if let Some(origin) = headers.get(header::ORIGIN) {
        let host = headers
            .get(header::HOST)
            .and_then(|value| value.to_str().ok());
        let origin = origin.to_str().ok().and_then(|origin| {
            origin
                .strip_prefix("http://")
                .or_else(|| origin.strip_prefix("https://"))
        });
        if host.is_none() || origin != host {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Cross-origin submissions are not allowed.".into(),
            ));
        }
    }
    Ok(())
}

/// Where the built frontend is: `MINT_WEB_DIR` for a deployed binary (the container
/// image); otherwise where `npm run build` in web/ puts it, next to this source tree.
fn web_dir() -> PathBuf {
    std::env::var_os("MINT_WEB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/dist"))
}

const NOT_BUILT: &str = "The frontend is not built. Start the app with ./run.sh; see README.md.";

/// One of the frontend's pages, or 503 while it is not built.
async fn page(web: &std::path::Path, file: &str) -> Response {
    match tokio::fs::read(web.join(file)).await {
        Ok(bytes) => Html(bytes).into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, NOT_BUILT).into_response(),
    }
}

/// A file the frontend build wrote to `assets/`, named by its content and so cached
/// for good. One plain path segment: nothing outside that directory is reachable.
async fn asset(web: &std::path::Path, name: &str) -> Response {
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || b"._-".contains(&byte);
    if name.is_empty() || name.starts_with('.') || !name.bytes().all(plain) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let content_type = match name.rsplit_once('.').map(|(_, extension)| extension) {
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        Some("json" | "map") => "application/json",
        _ => "application/octet-stream",
    };
    match tokio::fs::read(web.join("assets").join(name)).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn router(store: Store, web: PathBuf) -> Router {
    let web = Arc::new(web);
    let page_route = |file: &'static str| {
        let web = web.clone();
        get(move || {
            let web = web.clone();
            async move { page(&web, file).await }
        })
    };
    let assets = {
        let web = web.clone();
        get(move |Path(name): Path<String>| {
            let web = web.clone();
            async move { asset(&web, &name).await }
        })
    };
    Router::new()
        .route("/", page_route("index.html"))
        .route("/mint", get(|| async { Redirect::to("/#demo") }))
        .route("/classic", page_route("classic.html"))
        .route("/classic/mint", page_route("classic.html"))
        .route("/assets/{name}", assets)
        .route("/api/state", get(snapshot))
        .route("/api/rewards", get(rewards))
        .route("/api/contributions", post(submit))
        .route("/api/minter", post(minter))
        .layer(DefaultBodyLimit::max(2048))
        .layer(middleware::from_fn(security_headers))
        .layer(middleware::from_fn(revision_header))
        .with_state(store)
}

/// The commit this build is from, set by the container image; unset in a
/// local ./run.sh build. Every response carries it as `X-Mint-Revision`, which
/// is how to tell from outside whether a deploy has landed.
static REVISION: LazyLock<Option<HeaderValue>> = LazyLock::new(|| {
    std::env::var("MINT_REVISION")
        .ok()
        .and_then(|revision| HeaderValue::from_str(&revision).ok())
        .filter(|revision| !revision.is_empty())
});

async fn revision_header(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    if let Some(revision) = &*REVISION {
        response
            .headers_mut()
            .insert("x-mint-revision", revision.clone());
    }
    response
}

async fn security_headers(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    if !headers.contains_key(header::CONTENT_SECURITY_POLICY) {
        headers.insert(header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; connect-src 'self' https://api.coinbase.com; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'".parse().unwrap());
    }
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    headers.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    if !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    }
    response
}

async fn clock(store: Store) -> Result<(), ApiError> {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (reply, result) = oneshot::channel();
        // HTTP callers shed load; the internal clock waits instead of stopping on a full queue.
        store
            .0
            .send(Command::Tick(reply))
            .await
            .map_err(|_| ApiError::unavailable())?;
        result
            .await
            .map_err(|_| ApiError::unavailable())?
            .map_err(ApiError::from)?;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let first = arguments.next();
    if first
        .as_deref()
        .is_some_and(|value| value == "--new-payout-key")
        && arguments.len() == 0
    {
        return payout::new_key(&mut std::io::stdout().lock());
    }
    let resetting = first.as_deref().is_some_and(|value| value == "--reset-db");
    let path = if resetting { arguments.next() } else { first }
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/mint.sqlite3"));
    if arguments.next().is_some() {
        return Err(std::io::Error::other(
            "Usage: mint-playground [--reset-db] [database-path] | --new-payout-key",
        )
        .into());
    }
    if resetting {
        return db::reset(&path, &mut std::io::stdout().lock());
    }
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let payout = payout::Payout::from_env().map_err(std::io::Error::other)?;
    let (store, thread) = Store::open(path).map_err(std::io::Error::other)?;
    let web = web_dir();
    if !web.join("index.html").is_file() {
        eprintln!("{NOT_BUILT} (expected it in {})", web.display());
    }
    let address = std::env::var("MINT_LISTEN").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("Mint playground: http://{address} (simulated funds, testnet addresses only)");
    if let Some(revision) = &*REVISION {
        println!("Build: {}", revision.to_str().unwrap_or_default());
    }
    let result = tokio::select! {
        result = axum::serve(listener, router(store.clone(), web)) => result,
        result = clock(store.clone()) => Err(std::io::Error::other(result.err().map(|error| error.1).unwrap_or_default())),
        result = async {
            match payout {
                Some(payout) => payout.run(store.clone()).await,
                None => std::future::pending().await,
            }
        } => Err(std::io::Error::other(result.err().unwrap_or_default())),
        result = tokio::signal::ctrl_c() => result,
    };
    store.stop().await;
    thread
        .join()
        .map_err(|_| std::io::Error::other("SQLite thread panicked"))?;
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[test]
    fn a_new_payout_key_is_a_fresh_phrase_a_wallet_opens() {
        let key = || {
            let mut out = Vec::new();
            crate::payout::new_key(&mut out).unwrap();
            String::from_utf8(out).unwrap()
        };
        let (first, second) = (key(), key());
        assert_ne!(first, second);
        assert!(first.ends_with('\n'));
        assert_eq!(first.split_whitespace().count(), 24);
        flamewallet_ffi::Wallet::from_mnemonic(
            first.trim().into(),
            String::new(),
            flamewallet_ffi::Network::Testnet,
            0,
        )
        .unwrap();
    }

    async fn json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1_000_000)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn contribution() -> serde_json::Value {
        let address = flamekd::SpendKey::from_seed(&[42; 64])
            .unwrap()
            .to_recv()
            .to_address()
            .to_bech32(flamekd::Network::Testnet);
        serde_json::json!({"address": address, "budget_sats": 400000, "duration": 200, "request_id": "test-submission-00001"})
    }

    fn post_request(value: &serde_json::Value) -> Request<Body> {
        Request::post("/api/contributions")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .unwrap()
    }

    fn minter_request(value: &serde_json::Value) -> Request<Body> {
        Request::post("/api/minter")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn contribution_api_accepts_service_and_delegation_and_rejects_bad_settings() {
        let (store, thread) = Store::open(PathBuf::from(":memory:")).unwrap();
        let app = router(store.clone(), PathBuf::from("unbuilt"));
        let mut service = contribution();
        service["mode"] = "service".into();
        service["duration"] = 144.into();
        service["budget_sats"] = (144 * 1020).into();
        let response = app.clone().oneshot(post_request(&service)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let service_id = json(response).await["user_id"].as_i64().unwrap();
        let address = flamekd::SpendKey::from_seed(&[43; 64])
            .unwrap()
            .to_recv()
            .to_address()
            .to_bech32(flamekd::Network::Testnet);
        let mut delegated = serde_json::json!({"address":address,"budget_sats":2880,"duration":144,
            "request_id":"delegated-submission-0001","mode":"delegating","service_user_id":service_id});
        let response = app.clone().oneshot(post_request(&delegated)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let accepted = json(response).await;
        store
            .request(Command::Tick)
            .await
            .unwrap_or_else(|e| panic!("{}", e.1));
        assert_eq!(
            json(app.clone().oneshot(post_request(&delegated)).await.unwrap()).await,
            accepted
        );
        let response = app
            .clone()
            .oneshot(Request::get("/api/state").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let state = json(response).await;
        assert_eq!(state["services"][0]["active"], true);
        assert_eq!(state["services"][0]["commission_bps"], 200);
        assert_eq!(state["contributions"][0]["mode"], "delegating");
        assert_eq!(state["contributions"][0]["minter_id"], service_id);
        assert_eq!(state["contributions"][0]["reward_sparks"], "2450000000");
        assert_eq!(state["contributions"][0]["commission_sparks"], "50000000");
        assert_eq!(state["minters"][0]["reward_sparks"], "2550000000");
        assert_eq!(state["totals"]["fees_sats"], "1000");
        delegated["request_id"] = "new-invalid-submission-0001".into();
        for (field, value, status) in [
            (
                "mode",
                serde_json::json!("unknown"),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                "commission_bps",
                serde_json::json!(1.5),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                "commission_bps",
                serde_json::json!(-1),
                StatusCode::BAD_REQUEST,
            ),
            (
                "commission_bps",
                serde_json::json!(10001),
                StatusCode::BAD_REQUEST,
            ),
            (
                "commission_bps",
                serde_json::json!(300),
                StatusCode::BAD_REQUEST,
            ),
            (
                "service_user_id",
                serde_json::json!(999),
                StatusCode::BAD_REQUEST,
            ),
            (
                "service_user_id",
                serde_json::Value::Null,
                StatusCode::BAD_REQUEST,
            ),
            (
                "service_user_id",
                serde_json::json!(true),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                "private_key",
                serde_json::json!("never accepted"),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            let mut invalid = delegated.clone();
            invalid[field] = value;
            assert_eq!(
                app.clone()
                    .oneshot(post_request(&invalid))
                    .await
                    .unwrap()
                    .status(),
                status
            );
        }
        let mut no_fee = service.clone();
        no_fee["request_id"] = "underfunded-service-0001".into();
        no_fee["budget_sats"] = 2880.into();
        assert_eq!(
            app.oneshot(post_request(&no_fee)).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        store.stop().await;
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn identity_commission_api_registers_offers_reprices_history_and_ignores_old_retry_quotes(
    ) {
        let (store, thread) = Store::open(PathBuf::from(":memory:")).unwrap();
        let app = router(store.clone(), PathBuf::from("unbuilt"));
        let mut service = contribution();
        service["mode"] = "service".into();
        service["duration"] = 144.into();
        service["budget_sats"] = (144 * 1020).into();
        let address = service["address"].clone();
        let profile = serde_json::json!({"address":address,"service_commission_bps":333});
        let response = app.clone().oneshot(minter_request(&profile)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json(response).await["service_commission_bps"], 333);
        let response = app
            .clone()
            .oneshot(Request::get("/api/state").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let inactive = json(response).await;
        assert_eq!(inactive["services"][0]["active"], false);
        assert_eq!(inactive["totals"]["contribution_count"], 0);
        let response = app.clone().oneshot(post_request(&service)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let accepted = json(response).await;
        let owner = flamekd::SpendKey::from_seed(&[43; 64])
            .unwrap()
            .to_recv()
            .to_address()
            .to_bech32(flamekd::Network::Testnet);
        let mut delegated = serde_json::json!({"address":owner,"budget_sats":2880,"duration":144,
            "mode":"delegating","service_user_id":1,"request_id":"new-delegated-commission-0001"});
        assert_eq!(
            app.clone()
                .oneshot(post_request(&delegated))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        store
            .request(Command::Tick)
            .await
            .unwrap_or_else(|e| panic!("{}", e.1));
        let changed = serde_json::json!({"address":address,"service_commission_bps":1000});
        assert_eq!(
            app.clone()
                .oneshot(minter_request(&changed))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let loaded = json(
            app.clone()
                .oneshot(minter_request(
                    &serde_json::json!({"address":address,"name":"Service"}),
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(loaded["service_commission_bps"], 1000);
        let response = app
            .clone()
            .oneshot(Request::get("/api/state").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let repriced = json(response).await;
        assert_eq!(repriced["contributions"][0]["commission_bps"], 1000);
        assert_eq!(repriced["contributions"][0]["reward_sparks"], "2250000000");
        assert_eq!(
            repriced["contributions"][0]["commission_sparks"],
            "250000000"
        );
        assert_eq!(repriced["minters"][0]["reward_sparks"], "2750000000");
        assert_eq!(repriced["totals"]["reward_sparks"], "5000000000");
        service["commission_bps"] = 333.into();
        assert_eq!(
            json(app.clone().oneshot(post_request(&service)).await.unwrap()).await,
            accepted
        );
        delegated["commission_bps"] = 333.into();
        assert_eq!(
            app.clone()
                .oneshot(post_request(&delegated))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        service["commission_bps"] = serde_json::Value::Null;
        assert_eq!(
            json(app.clone().oneshot(post_request(&service)).await.unwrap()).await,
            accepted
        );
        delegated["commission_bps"] = serde_json::Value::Null;
        assert_eq!(
            app.clone()
                .oneshot(post_request(&delegated))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let after_null_retry = app
            .clone()
            .oneshot(Request::get("/api/state").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(json(after_null_retry).await, repriced);
        delegated["request_id"] = "obsolete-new-contribution-0001".into();
        let rejected = app.clone().oneshot(post_request(&delegated)).await.unwrap();
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
        assert!(json(rejected).await["error"]
            .as_str()
            .unwrap()
            .contains("identity"));
        for rate in [
            serde_json::json!(-1),
            serde_json::json!(10001),
            serde_json::json!(i64::MAX),
        ] {
            let invalid = serde_json::json!({"address":address,"service_commission_bps":rate});
            assert_eq!(
                app.clone()
                    .oneshot(minter_request(&invalid))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        let invalid = serde_json::json!({"address":address,"service_commission_bps":1.5});
        assert_eq!(
            app.oneshot(minter_request(&invalid))
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        store.stop().await;
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn minter_route_registers_loads_and_renames_one_account_before_minting() {
        let (store, thread) = Store::open(PathBuf::from(":memory:")).unwrap();
        let app = router(store.clone(), PathBuf::from("unbuilt"));
        let address = contribution()["address"].as_str().unwrap().to_owned();
        let profile = serde_json::json!({"address": address});
        let response = app.clone().oneshot(minter_request(&profile)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json(response).await,
            serde_json::json!({"user_id":1,"name":"Minter 1","service_commission_bps":null})
        );
        let uppercase = serde_json::json!({"address": address.to_uppercase()});
        let response = app
            .clone()
            .oneshot(minter_request(&uppercase))
            .await
            .unwrap();
        assert_eq!(
            json(response).await,
            serde_json::json!({"user_id":1,"name":"Minter 1","service_commission_bps":null})
        );
        let renamed = serde_json::json!({"address": address, "name":"  Alice 🔥  "});
        let mut request = minter_request(&renamed);
        request
            .headers_mut()
            .insert(header::HOST, "127.0.0.1:3000".parse().unwrap());
        request
            .headers_mut()
            .insert(header::ORIGIN, "http://127.0.0.1:3000".parse().unwrap());
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json(response).await,
            serde_json::json!({"user_id":1,"name":"Alice 🔥","service_commission_bps":null})
        );
        let response = app.clone().oneshot(minter_request(&profile)).await.unwrap();
        assert_eq!(
            json(response).await,
            serde_json::json!({"user_id":1,"name":"Alice 🔥","service_commission_bps":null})
        );
        let state = store
            .request(|reply| Command::Snapshot(true, reply))
            .await
            .unwrap_or_else(|e| panic!("{}", e.1));
        assert_eq!(state.totals.contribution_count, 0);
        assert!(state.contributions.is_empty());
        let submitted = json(
            app.clone()
                .oneshot(post_request(&contribution()))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            (submitted["id"].as_i64(), submitted["user_id"].as_i64()),
            (Some(1), Some(1))
        );
        store
            .request(Command::Tick)
            .await
            .unwrap_or_else(|e| panic!("{}", e.1));
        let literal = serde_json::json!({"address": address, "name":"<b>Alice</b>"});
        let response = app.oneshot(minter_request(&literal)).await.unwrap();
        assert_eq!(json(response).await["name"], "<b>Alice</b>");
        let rewards = store
            .request(Command::Rewards)
            .await
            .unwrap_or_else(|e| panic!("{}", e.1));
        assert_eq!(
            (
                rewards[0].address.as_str(),
                rewards[0].reward_sparks.as_str()
            ),
            (address.as_str(), "5000000000")
        );
        store.stop().await;
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn minter_route_rejects_bad_names_networks_private_fields_origin_and_large_bodies() {
        let (store, thread) = Store::open(PathBuf::from(":memory:")).unwrap();
        let app = router(store.clone(), PathBuf::from("unbuilt"));
        let address = contribution()["address"].as_str().unwrap().to_owned();
        let mainnet = flamekd::ReceivingAddress::from_bech32(&address, flamekd::Network::Testnet)
            .unwrap()
            .to_bech32(flamekd::Network::Mainnet);
        let mut corrupt = address.clone();
        let last = corrupt.pop().unwrap();
        corrupt.push(if last == 'q' { 'p' } else { 'q' });
        for value in [
            serde_json::json!({"address": "invalid"}),
            serde_json::json!({"address": mainnet}),
            serde_json::json!({"address": corrupt}),
            serde_json::json!({"address": address, "name":"  "}),
            serde_json::json!({"address": address, "name":"Alice\n"}),
            serde_json::json!({"address": address, "name":"x".repeat(65)}),
        ] {
            let response = app.clone().oneshot(minter_request(&value)).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert!(json(response).await["error"].is_string());
        }
        for field in [
            "user_id",
            "reward_address",
            "mnemonic",
            "seed",
            "private_key",
        ] {
            let mut value = serde_json::json!({"address": address});
            value[field] = "must never be accepted".into();
            let response = app.clone().oneshot(minter_request(&value)).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            assert!(!json(response)
                .await
                .to_string()
                .contains("must never be accepted"));
        }
        let mut request = minter_request(&serde_json::json!({"address":address}));
        request
            .headers_mut()
            .insert(header::HOST, "127.0.0.1:3000".parse().unwrap());
        request
            .headers_mut()
            .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let oversized = serde_json::json!({"address":address,"name":"x".repeat(3_000)});
        assert_eq!(
            app.clone()
                .oneshot(minter_request(&oversized))
                .await
                .unwrap()
                .status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        let wrong_type = serde_json::json!({"address":address,"name":123});
        assert_eq!(
            app.clone()
                .oneshot(minter_request(&wrong_type))
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let non_json = Request::post("/api/minter")
            .body(Body::from("address=bad"))
            .unwrap();
        assert_eq!(
            app.oneshot(non_json).await.unwrap().status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        assert!(store
            .request(Command::Rewards)
            .await
            .unwrap_or_else(|e| panic!("{}", e.1))
            .is_empty());
        store.stop().await;
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn clock_waits_for_capacity_instead_of_stopping_on_a_full_queue() {
        let (sender, mut receiver) = mpsc::channel(1);
        let (reply, _result) = oneshot::channel();
        sender
            .try_send(Command::Snapshot(false, reply))
            .ok()
            .unwrap();
        let task = tokio::spawn(clock(Store(sender)));
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!task.is_finished());
        receiver.recv().await.unwrap();
        let command = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        match command {
            Command::Tick(reply) => {
                reply.send(Ok(())).unwrap();
            }
            _ => panic!("Expected the delayed block tick"),
        }
        assert!(!task.is_finished());
        task.abort();
    }

    /// A frontend build as Vite lays it out, for the routes that serve it.
    fn built_web() -> tempfile::TempDir {
        let web = tempfile::tempdir().unwrap();
        std::fs::create_dir(web.path().join("assets")).unwrap();
        for (file, content) in [
            (
                "index.html",
                "<!doctype html><script type=\"module\" src=\"/assets/app-1234abcd.js\"></script>",
            ),
            ("classic.html", "<!doctype html>classic"),
            ("assets/app-1234abcd.js", "console.log('app')"),
            ("assets/app-1234abcd.css", "body{}"),
            ("assets/flamewallet_wasm_bg-1234abcd.wasm", "\0asm"),
        ] {
            std::fs::write(web.path().join(file), content).unwrap();
        }
        web
    }

    #[tokio::test]
    async fn routes_submission_retry_and_public_reward_export() {
        let (store, thread) = Store::open(PathBuf::from(":memory:")).unwrap();
        let web = built_web();
        let app = router(store.clone(), web.path().to_path_buf());
        let fetch = |path: &str| {
            app.clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        };
        for path in [
            "/",
            "/classic",
            "/classic/mint",
            "/assets/app-1234abcd.js",
            "/assets/app-1234abcd.css",
            "/assets/flamewallet_wasm_bg-1234abcd.wasm",
            "/api/state",
        ] {
            let response = fetch(path).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let csp = response.headers()[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap();
            assert!(csp.contains("connect-src 'self' https://api.coinbase.com;"));
            // Nothing is evaluated or styled inline; the wallet is the only WebAssembly.
            assert!(!csp.contains("'unsafe-eval'") && !csp.contains("'unsafe-inline'"));
            assert!(csp.contains("script-src 'self' 'wasm-unsafe-eval';"));
        }
        // Built files are named by their content and cached for good; the pages are not.
        for (path, content_type) in [
            ("/assets/app-1234abcd.js", "text/javascript; charset=utf-8"),
            ("/assets/app-1234abcd.css", "text/css; charset=utf-8"),
            (
                "/assets/flamewallet_wasm_bg-1234abcd.wasm",
                "application/wasm",
            ),
        ] {
            let response = fetch(path).await.unwrap();
            assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "public, max-age=31536000, immutable"
            );
        }
        let response = fetch("/").await.unwrap();
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        // Only the build's assets directory, by plain file names.
        for path in [
            "/assets/..%2fclassic.html",
            "/assets/.hidden",
            "/assets/missing.js",
            "/assets/app-1234abcd.js/x",
            "/assets/",
        ] {
            let response = fetch(path).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
        // Without a build, the pages say so; the API works regardless.
        let unbuilt = router(store.clone(), web.path().join("missing"));
        for (path, status) in [
            ("/", StatusCode::SERVICE_UNAVAILABLE),
            ("/classic", StatusCode::SERVICE_UNAVAILABLE),
            ("/assets/app-1234abcd.js", StatusCode::NOT_FOUND),
            ("/api/state", StatusCode::OK),
        ] {
            let response = unbuilt
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{path}");
        }
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/mint").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::LOCATION], "/#demo");
        let contribution = contribution();
        let first = json(
            app.clone()
                .oneshot(post_request(&contribution))
                .await
                .unwrap(),
        )
        .await;
        let retry = json(
            app.clone()
                .oneshot(post_request(&contribution))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(first, retry);
        assert_eq!(first["user_id"], 1);
        assert_eq!(first["start_block"], 1);
        let mut pending_states = Vec::new();
        for path in ["/api/state", "/api/state?full_history=true"] {
            pending_states.push(
                json(
                    app.clone()
                        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                        .await
                        .unwrap(),
                )
                .await,
            );
        }
        assert_eq!(
            pending_states[0]["contributions"].as_array().unwrap().len(),
            1
        );
        assert_eq!(pending_states[0]["contributions"][0]["delay_blocks"], 0);
        assert_eq!(
            pending_states[1]["contributions"].as_array().unwrap().len(),
            1
        );
        assert_eq!(pending_states[0]["totals"], pending_states[1]["totals"]);
        assert_eq!(pending_states[0]["totals"]["contribution_count"], 1);
        assert_eq!(pending_states[0]["totals"]["burned_sats"], "0");
        assert_eq!(pending_states[0]["totals"]["reward_sparks"], "0");
        store
            .request(Command::Tick)
            .await
            .unwrap_or_else(|error| panic!("{}", error.1));
        let state = json(
            app.clone()
                .oneshot(
                    Request::builder()
                        .uri("/api/state")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(state["totals"]["reward_sparks"], "5000000000");
        assert_eq!(state["contributions"].as_array().unwrap().len(), 1);
        assert_eq!(state["contributions"][0]["user_id"], 1);
        assert_eq!(
            state["contributions"][0]["address"],
            contribution["address"]
        );
        let rewards = json(
            app.oneshot(
                Request::builder()
                    .uri("/api/rewards")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(rewards[0]["address"], contribution["address"]);
        assert_eq!(rewards[0]["reward_sparks"], "5000000000");
        store.stop().await;
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn state_route_limits_dashboard_to_latest_200_but_keeps_chart_history_and_totals() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mint.sqlite3");
        {
            let mut database = db::Database::open(&path).unwrap();
            database
                .submit(serde_json::from_value(contribution()).unwrap())
                .unwrap();
            database.tick().unwrap();
        }
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(
                "INSERT INTO blocks(height, burned_sats, fees_sats, reward_sparks)
                 VALUES (10201, 0, 0, 0);
                 WITH RECURSIVE ids(id) AS (
                     SELECT 2 UNION ALL SELECT id + 1 FROM ids WHERE id < 205
                 )
                 INSERT INTO contributions(id, request_id, user_id, budget_sats, duration, start_block)
                 SELECT id, printf('bulk-contribution-%016d', id), 1, 400000, 200,
                        CASE WHEN id = 205 THEN 1 ELSE 10202 END FROM ids;",
            )
            .unwrap();
        let (store, thread) = Store::open(path).unwrap();
        let app = router(store.clone(), PathBuf::from("unbuilt"));
        let mut states = Vec::new();
        for path in ["/api/state", "/api/state?full_history=true"] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            states.push(json(response).await);
        }
        assert_eq!(states[0]["height"], 10_201);
        assert_eq!(states[0]["contributions"].as_array().unwrap().len(), 200);
        assert_eq!(states[0]["contributions"][0]["id"], 205);
        assert_eq!(states[0]["contributions"][0]["start_block"], 1);
        assert_eq!(states[0]["contributions"][199]["id"], 6);
        assert_eq!(
            states[0]["chart_contributions"].as_array().unwrap().len(),
            205
        );
        assert_eq!(states[0]["chart_contributions"][204]["id"], 1);
        assert!(states[0]["chart_contributions"][0].get("address").is_none());
        assert_eq!(states[1]["contributions"].as_array().unwrap().len(), 205);
        assert_eq!(
            states[1]["contributions"][204]["reward_sparks"],
            "5000000000"
        );
        assert!(states[1].get("chart_contributions").is_none());
        assert_eq!(states[0]["totals"], states[1]["totals"]);
        assert_eq!(states[0]["totals"]["contribution_count"], 205);
        assert_eq!(states[0]["totals"]["burned_sats"], "1000");
        assert_eq!(states[0]["totals"]["fees_sats"], "1000");
        assert_eq!(states[0]["totals"]["reward_sparks"], "5000000000");
        store.stop().await;
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn schedules_nonstep_duration_and_validates_delay_through_json_api() {
        let (store, thread) = Store::open(PathBuf::from(":memory:")).unwrap();
        let app = router(store.clone(), PathBuf::from("unbuilt"));
        let mut value = contribution();
        value["duration"] = 145.into();
        value["delay_blocks"] = 4_464.into();
        let response = app.clone().oneshot(post_request(&value)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let first = json(response).await;
        assert_eq!(first["start_block"], 4_465);
        store
            .request(Command::Tick)
            .await
            .unwrap_or_else(|e| panic!("{}", e.1));
        let retry = json(app.clone().oneshot(post_request(&value)).await.unwrap()).await;
        assert_eq!(first, retry);
        let state = json(
            app.clone()
                .oneshot(
                    Request::builder()
                        .uri("/api/state")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(state["contributions"][0]["delay_blocks"], 4_464);
        assert_eq!(state["contributions"][0]["duration"], 145);
        assert_eq!(state["totals"]["burned_sats"], "0");
        value["delay_blocks"] = 0.into();
        assert_eq!(
            app.clone()
                .oneshot(post_request(&value))
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        value["request_id"] = "another-submission-00001".into();
        for delay in [
            serde_json::json!(-1),
            serde_json::json!(db::MAX_SAFE_INTEGER + 1),
        ] {
            value["delay_blocks"] = delay;
            assert_eq!(
                app.clone()
                    .oneshot(post_request(&value))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        value["delay_blocks"] = serde_json::json!(1.5);
        assert_eq!(
            app.oneshot(post_request(&value)).await.unwrap().status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        store.stop().await;
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn accepts_manual_values_above_slider_limits_through_json_api() {
        let (store, thread) = Store::open(PathBuf::from(":memory:")).unwrap();
        let app = router(store.clone(), PathBuf::from("unbuilt"));
        let mut value = contribution();
        value["budget_sats"] = 200_000_000.into();
        value["duration"] = 5_001.into();
        value["delay_blocks"] = 6_001.into();
        let response = app.clone().oneshot(post_request(&value)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json(response).await["start_block"], 6_002);
        value["request_id"] = "unsafe-manual-number-0001".into();
        for field in ["budget_sats", "duration", "delay_blocks"] {
            let previous = value[field].clone();
            value[field] = (db::MAX_SAFE_INTEGER + 1).into();
            let response = app.clone().oneshot(post_request(&value)).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            value[field] = previous;
        }
        store.stop().await;
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn rejects_cross_origin_private_fields_and_invalid_input() {
        let (store, thread) = Store::open(PathBuf::from(":memory:")).unwrap();
        let app = router(store.clone(), PathBuf::from("unbuilt"));
        let mut request = post_request(&contribution());
        request
            .headers_mut()
            .insert(header::HOST, "127.0.0.1:3000".parse().unwrap());
        request
            .headers_mut()
            .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let mut value = contribution();
        value["mnemonic"] = "must never be accepted".into();
        assert_eq!(
            app.clone()
                .oneshot(post_request(&value))
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let request = Request::post("/api/contributions")
            .body(Body::from("address=bad"))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        value = contribution();
        value["budget_sats"] = 2000.into();
        assert_eq!(
            app.oneshot(post_request(&value)).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        store.stop().await;
        thread.join().unwrap();
    }
}
