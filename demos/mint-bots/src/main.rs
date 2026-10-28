#![forbid(unsafe_code)]

mod bots;
mod config;
mod journal;
mod keys;
mod playground;
mod strategy;

use std::{
    ops::ControlFlow,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    extract::{DefaultBodyLimit, Query, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot, RwLock};

use crate::{bots::Status, config::Config, journal::Journal};

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// Breaks to stop the journal's thread.
type Job = Box<dyn FnOnce(&mut Journal) -> ControlFlow<()> + Send>;

/// The journal's connection, owned by one thread; everything else sends it
/// jobs.
#[derive(Clone)]
pub struct Store(mpsc::Sender<Job>);

impl Store {
    fn open(path: PathBuf) -> Result<(Self, std::thread::JoinHandle<()>), String> {
        let (sender, mut receiver) = mpsc::channel::<Job>(64);
        let (ready, started) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("bots-sqlite".into())
            .spawn(move || {
                let mut journal = match Journal::open(&path) {
                    Ok(journal) => journal,
                    Err(error) => {
                        let _ = ready.send(Err(format!("{}: {error}", path.display())));
                        return;
                    }
                };
                let _ = ready.send(Ok(()));
                while let Some(job) = receiver.blocking_recv() {
                    if job(&mut journal).is_break() {
                        break;
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        started.recv().map_err(|error| error.to_string())??;
        Ok((Self(sender), thread))
    }

    async fn enqueue<T: Send + 'static>(
        &self,
        wait: bool,
        job: impl FnOnce(&mut Journal) -> Result<T, journal::Error> + Send + 'static,
    ) -> Result<T, String> {
        let (reply, result) = oneshot::channel();
        let job: Job = Box::new(move |journal| {
            let _ = reply.send(job(journal));
            ControlFlow::Continue(())
        });
        if wait {
            self.0.send(job).await.map_err(|_| "journal stopped")?;
        } else {
            self.0
                .try_send(job)
                .map_err(|_| "journal busy; retry shortly")?;
        }
        result
            .await
            .map_err(|_| "journal stopped".to_string())?
            .map_err(|error| format!("journal: {error}"))
    }

    /// Waits for room in the queue: what the bots use.
    pub async fn call<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Journal) -> Result<T, journal::Error> + Send + 'static,
    ) -> Result<T, String> {
        self.enqueue(true, job).await
    }

    /// Open page connections still hold senders after the server stops, so
    /// the thread is told to stop rather than waiting for them to go.
    async fn stop(&self) {
        let (reply, result) = oneshot::channel();
        let job: Job = Box::new(move |_| {
            let _ = reply.send(());
            ControlFlow::Break(())
        });
        if self.0.send(job).await.is_ok() {
            let _ = result.await;
        }
    }

    /// Sheds load instead: what page requests use.
    async fn try_call<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Journal) -> Result<T, journal::Error> + Send + 'static,
    ) -> Result<T, ApiError> {
        self.enqueue(false, job)
            .await
            .map_err(|message| ApiError(StatusCode::SERVICE_UNAVAILABLE, message))
    }
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error": self.1}))).into_response()
    }
}

#[derive(Clone)]
struct App {
    store: Store,
    config: Arc<RwLock<Config>>,
    status: Arc<Mutex<Status>>,
}

async fn status(State(app): State<App>) -> Json<serde_json::Value> {
    let config = app.config.read().await.clone();
    let status = app
        .status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    Json(serde_json::json!({ "config": config, "status": status }))
}

#[derive(Deserialize)]
struct EventsQuery {
    before: Option<i64>,
    filter: Option<String>,
}

async fn events(
    State(app): State<App>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<Vec<journal::Event>>, ApiError> {
    app.store
        .try_call(move |journal| journal.events(query.before, query.filter.as_deref(), 100))
        .await
        .map(Json)
}

#[derive(Deserialize)]
struct SamplesQuery {
    bot: String,
    /// Unix milliseconds.
    since: i64,
}

async fn samples(
    State(app): State<App>,
    Query(query): Query<SamplesQuery>,
) -> Result<Json<Vec<journal::Sample>>, ApiError> {
    if !bots::BOTS.contains(&query.bot.as_str()) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "Unknown bot.".into()));
    }
    app.store
        .try_call(move |journal| journal.samples(&query.bot, query.since, 500))
        .await
        .map(Json)
}

async fn update_config(
    State(app): State<App>,
    headers: HeaderMap,
    config: Result<Json<Config>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Config>, ApiError> {
    check_origin(&headers)?;
    let Json(config) = config.map_err(|error| ApiError(error.status(), error.body_text()))?;
    config
        .validate()
        .map_err(|message| ApiError(StatusCode::BAD_REQUEST, message))?;
    // Held until the new settings are journaled, so concurrent saves apply in order.
    let mut current = app.config.write().await;
    if *current == config {
        return Ok(Json(config));
    }
    let changes = changes(&current, &config);
    let (from, to) = (current.clone(), config.clone());
    app.store
        .try_call(move |journal| {
            journal.set_setting("config", &to)?;
            journal.log(
                now_ms(),
                None,
                "config",
                None,
                &format!("Settings changed: {}.", changes.join("; ")),
                Some(&serde_json::json!({ "from": from, "to": to })),
            )
        })
        .await?;
    *current = config.clone();
    Ok(Json(config))
}

/// `clock.enabled: false → true` for each field that differs.
fn changes(from: &Config, to: &Config) -> Vec<String> {
    let (from, to) = (serde_json::json!(from), serde_json::json!(to));
    let mut changes = Vec::new();
    for bot in bots::BOTS {
        let (Some(before), Some(after)) = (from[bot].as_object(), to[bot].as_object()) else {
            continue;
        };
        for (field, value) in after {
            if before.get(field) != Some(value) {
                changes.push(format!("{bot}.{field}: {} → {value}", before[field]));
            }
        }
    }
    changes
}

fn check_origin(headers: &HeaderMap) -> Result<(), ApiError> {
    // JSON-only writes, no CORS, and an origin check keep other sites from
    // riding on a signed-in browser.
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
                "Cross-origin requests are not allowed.".into(),
            ));
        }
    }
    Ok(())
}

async fn security_headers(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'".parse().unwrap());
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    headers.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

fn router(app: App) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/style.css"),
                )
            }),
        )
        .route("/api/status", get(status))
        .route("/api/events", get(events))
        .route("/api/samples", get(samples))
        .route("/api/config", axum::routing::put(update_config))
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn(security_headers))
        .with_state(app)
}

async fn terminated() -> std::io::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let first = arguments.next();
    if first.as_deref().is_some_and(|value| value == "--new-key") && arguments.len() == 0 {
        return keys::new_key(&mut std::io::stdout().lock());
    }
    let path = first
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/bots.sqlite3"));
    if arguments.next().is_some() {
        return Err(std::io::Error::other("Usage: mint-bots [database-path] | --new-key").into());
    }
    let file = std::env::var_os("MINT_BOTS_MNEMONIC_FILE")
        .ok_or("Set MINT_BOTS_MNEMONIC_FILE to a file holding the bots' BIP-39 phrase (mint-bots --new-key makes one).")?;
    let phrase = zeroize::Zeroizing::new(
        std::fs::read_to_string(&file)
            .map_err(|error| format!("{}: {error}", file.to_string_lossy()))?,
    );
    let addresses = keys::addresses(&phrase)?;
    drop(phrase);
    let playground = playground::Playground::new(
        &std::env::var("MINT_BOTS_PLAYGROUND").unwrap_or_else(|_| "http://127.0.0.1:3000".into()),
    )?;
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let (store, thread) = Store::open(path)?;

    let started_at = now_ms();
    let config = match store
        .call(|journal| journal.setting::<Config>("config"))
        .await?
    {
        Some(config) => config,
        None => {
            let config = Config::new(started_at / 1_000);
            let saved = config.clone();
            store
                .call(move |journal| journal.set_setting("config", &saved))
                .await?;
            config
        }
    };
    let revision = std::env::var("MINT_REVISION")
        .ok()
        .filter(|revision| !revision.is_empty());
    bots::log(
        &store,
        None,
        "start",
        None,
        format!(
            "Started{}: clock bot {}, minimax bot {}, playground {}.",
            revision
                .as_deref()
                .map(|revision| format!(" at {revision}"))
                .unwrap_or_default(),
            addresses[0],
            addresses[1],
            playground.url()
        ),
        Some(serde_json::json!({ "revision": revision, "addresses": addresses })),
    )
    .await;
    let status = Arc::new(Mutex::new(Status {
        started_at,
        revision,
        playground: playground.url(),
        ..Default::default()
    }));
    let config = Arc::new(RwLock::new(config));
    let runner = bots::Runner::new(
        store.clone(),
        playground,
        addresses,
        config.clone(),
        status.clone(),
    )
    .await?;

    let address = std::env::var("MINT_BOTS_LISTEN").unwrap_or_else(|_| "127.0.0.1:3100".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("Mint bots: http://{address}");
    let app = App {
        store: store.clone(),
        config,
        status,
    };
    let result = tokio::select! {
        result = axum::serve(listener, router(app)) => result,
        () = runner.run() => Ok(()),
        result = terminated() => result,
    };
    bots::log(&store, None, "stop", None, "Stopped.".into(), None).await;
    store.stop().await;
    thread
        .join()
        .map_err(|_| std::io::Error::other("SQLite thread panicked"))?;
    result?;
    Ok(())
}
