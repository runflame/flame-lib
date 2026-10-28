//! The mint playground's HTTP API, as a client: the same three calls its
//! pages make. One connection per request; the playground is on loopback
//! and the bots ask a few times a minute.

use std::{fmt, time::Duration};

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    header::{CONTENT_TYPE, HOST},
    Method, Request, StatusCode,
};
use hyper_util::rt::TokioIo;
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};

const TIMEOUT: Duration = Duration::from_secs(15);

/// The snapshot carries every contribution's schedule, so it grows over the
/// week; this is far above what it reaches and stops a runaway response.
const MAX_BODY: usize = 64 << 20;

#[derive(Debug)]
pub enum Error {
    /// No answer: the request may or may not have taken effect.
    Unreachable(String),
    /// Busy or failing: worth retrying unchanged.
    Busy(StatusCode, String),
    /// Refused for good: retrying the same request cannot succeed.
    Rejected(StatusCode, String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(message) => write!(f, "playground unreachable: {message}"),
            Self::Busy(status, message) | Self::Rejected(status, message) => {
                write!(f, "playground answered {}: {message}", status.as_u16())
            }
        }
    }
}

/// The parts of `GET /api/state` the bots use.
#[derive(Debug, Deserialize)]
pub struct State {
    pub height: i64,
    #[serde(default)]
    pub chart_contributions: Vec<Schedule>,
    pub minters: Vec<MinterSummary>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Schedule {
    pub user_id: i64,
    pub budget_sats: i64,
    pub duration: i64,
    pub start_block: i64,
    pub mode: String,
    pub service_user_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct MinterSummary {
    pub address: String,
    #[serde(deserialize_with = "decimal")]
    pub burned_sats: i64,
    #[serde(deserialize_with = "decimal")]
    pub fees_sats: i64,
    #[serde(deserialize_with = "decimal")]
    pub reward_sparks: i64,
}

#[derive(Debug, Serialize)]
pub struct MinterRequest<'a> {
    pub address: &'a str,
    pub name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_commission_bps: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct Minter {
    pub user_id: i64,
}

#[derive(Debug, Serialize)]
pub struct ContributionRequest<'a> {
    pub address: &'a str,
    pub budget_sats: i64,
    pub duration: i64,
    pub delay_blocks: i64,
    pub request_id: &'a str,
    pub mode: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct Submitted {
    pub id: i64,
    pub start_block: i64,
}

/// Cumulative amounts arrive as decimal strings.
fn decimal<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    String::deserialize(deserializer)?
        .parse()
        .map_err(serde::de::Error::custom)
}

pub struct Playground {
    /// `host:port`, also sent as the Host header.
    authority: String,
}

impl Playground {
    /// Takes `http://host:port`; the playground is reached on loopback, so
    /// there is no TLS.
    pub fn new(url: &str) -> Result<Self, String> {
        let authority = url
            .strip_prefix("http://")
            .map(|rest| rest.trim_end_matches('/'))
            .filter(|rest| !rest.is_empty() && !rest.contains('/'))
            .ok_or_else(|| format!("playground URL must be http://host:port, not {url}"))?;
        Ok(Self {
            authority: authority.into(),
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.authority)
    }

    pub async fn state(&self) -> Result<State, Error> {
        self.send(Method::GET, "/api/state", None).await
    }

    pub async fn minter(&self, request: &MinterRequest<'_>) -> Result<Minter, Error> {
        self.send(Method::POST, "/api/minter", Some(json(request)))
            .await
    }

    pub async fn contribute(&self, request: &ContributionRequest<'_>) -> Result<Submitted, Error> {
        self.send(Method::POST, "/api/contributions", Some(json(request)))
            .await
    }

    async fn send<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<T, Error> {
        let unreachable = |error: &dyn fmt::Display| Error::Unreachable(error.to_string());
        let exchange = async {
            let stream = tokio::net::TcpStream::connect(&self.authority)
                .await
                .map_err(|error| unreachable(&error))?;
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(stream))
                    .await
                    .map_err(|error| unreachable(&error))?;
            let connection = tokio::spawn(connection);
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header(HOST, &self.authority);
            if body.is_some() {
                request = request.header(CONTENT_TYPE, "application/json");
            }
            let request = request
                .body(Full::new(Bytes::from(body.unwrap_or_default())))
                .map_err(|error| unreachable(&error))?;
            let response = sender
                .send_request(request)
                .await
                .map_err(|error| unreachable(&error))?;
            let status = response.status();
            let bytes = Limited::new(response.into_body(), MAX_BODY)
                .collect()
                .await
                .map_err(|error| unreachable(&error))?
                .to_bytes();
            drop(sender);
            connection.abort();
            Ok((status, bytes))
        };
        let (status, bytes) = tokio::time::timeout(TIMEOUT, exchange)
            .await
            .map_err(|_| Error::Unreachable(format!("no answer within {TIMEOUT:?}")))??;
        if status.is_success() {
            // A 2xx the bots cannot read is the playground's fault, not the
            // request's: retry it like a failure.
            return serde_json::from_slice(&bytes)
                .map_err(|error| Error::Busy(status, format!("unreadable response: {error}")));
        }
        // The playground's errors are {"error": "..."}; anything else is shown as text.
        let message = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| value.get("error")?.as_str().map(String::from))
            .unwrap_or_else(|| String::from_utf8_lossy(&bytes).chars().take(200).collect());
        if status.is_client_error() && status != StatusCode::TOO_MANY_REQUESTS {
            Err(Error::Rejected(status, message))
        } else {
            Err(Error::Busy(status, message))
        }
    }
}

fn json(value: &impl Serialize) -> Vec<u8> {
    serde_json::to_vec(value).expect("request types always serialize")
}
