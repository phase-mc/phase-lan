//! Code service client (HTTP or HTTPS).

use std::time::Duration;

use phase_lang::tr;
use reqwest::{Method, StatusCode};
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    ErrorBody, Lookup, RegisterRequest, Registered, ReleaseRequest, RenewRequest, Renewed,
};

/// How long to wait for the service: it is small and should answer right away.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Code service error. The text is in the app's language (`phase_lang`).
#[derive(Debug)]
pub enum CodesError {
    /// No room with this code (or the code has expired).
    NotFound,
    /// The code is taken by another host (the secret did not match).
    Conflict,
    /// Too many requests from this IP.
    RateLimited,
    /// The service did not answer or answered with something unintelligible.
    Unavailable(String),
    /// Bad service address or request.
    Invalid(String),
}

impl std::fmt::Display for CodesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => {
                f.write_str(tr!("комнаты с таким кодом нет", "no room with this code"))
            }
            Self::Conflict => f.write_str(tr!(
                "код занят другой комнатой",
                "this code is taken by another room"
            )),
            Self::RateLimited => f.write_str(tr!(
                "слишком много запросов, подождите минуту",
                "too many requests, wait a minute"
            )),
            Self::Unavailable(why) => write!(
                f,
                "{}: {why}",
                tr!(
                    "сервис кодов недоступен",
                    "the room code service is unavailable"
                )
            ),
            Self::Invalid(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for CodesError {}

impl CodesError {
    /// Whether to offer the long invite instead of a code.
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable(_) | Self::Invalid(_))
    }
}

pub type Result<T> = std::result::Result<T, CodesError>;

/// Client for one code service.
#[derive(Debug, Clone)]
pub struct CodesClient {
    base: String,
    http: reqwest::Client,
}

impl CodesClient {
    /// `base` is the service address, for example `https://codes.example.com` or
    /// `http://127.0.0.1:3341`.
    pub fn new(base: &str) -> Result<Self> {
        let base = base.trim().trim_end_matches('/');
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(CodesError::Invalid(format!(
                "адрес сервиса кодов «{base}» должен начинаться с http:// или https://"
            )));
        }
        // reqwest is built without its own crypto provider (as in iroh), so use ring.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("phase/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|err| {
                CodesError::Invalid(tr!(
                    format!("клиент сервиса кодов: {err}"),
                    format!("room code service client: {err}")
                ))
            })?;
        Ok(Self {
            base: base.to_string(),
            http,
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// Host: get a new code for a ticket.
    pub async fn register(&self, ticket: &str) -> Result<Registered> {
        let body = RegisterRequest {
            ticket: ticket.into(),
        };
        self.call(Method::POST, "/rooms", Some(&body)).await
    }

    /// Host: renew the code (or create it again after the service restarted).
    pub async fn renew(&self, code: &str, secret: &str, ticket: &str) -> Result<Renewed> {
        let body = RenewRequest {
            ticket: ticket.into(),
            secret: secret.into(),
        };
        self.call(Method::PUT, &room_path(code)?, Some(&body)).await
    }

    /// Host: the room is closed, the code is no longer needed.
    pub async fn release(&self, code: &str, secret: &str) -> Result<()> {
        let body = ReleaseRequest {
            secret: secret.into(),
        };
        self.send(Method::DELETE, &room_path(code)?, Some(&body))
            .await
            .map(drop)
    }

    /// Guest: the room ticket for a code. The code can be passed as the user typed it.
    pub async fn lookup(&self, code: &str) -> Result<String> {
        let lookup: Lookup = self
            .call::<(), _>(Method::GET, &room_path(code)?, None)
            .await?;
        Ok(lookup.ticket)
    }

    async fn call<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T> {
        let bytes = self.send(method, path, body).await?;
        serde_json::from_slice(&bytes).map_err(|err| {
            CodesError::Unavailable(tr!(
                format!("непонятный ответ: {err}"),
                format!("unintelligible response: {err}")
            ))
        })
    }

    async fn send<B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<Vec<u8>> {
        let mut request = self.http.request(method, format!("{}{path}", self.base));
        if let Some(body) = body {
            let json =
                serde_json::to_vec(body).map_err(|err| CodesError::Invalid(err.to_string()))?;
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(json);
        }
        let response = request
            .send()
            .await
            .map_err(|err| CodesError::Unavailable(err.to_string()))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|err| CodesError::Unavailable(err.to_string()))?;
        match status {
            s if s.is_success() => Ok(bytes.to_vec()),
            StatusCode::NOT_FOUND => Err(CodesError::NotFound),
            StatusCode::CONFLICT => Err(CodesError::Conflict),
            StatusCode::TOO_MANY_REQUESTS => Err(CodesError::RateLimited),
            other => {
                let reason = serde_json::from_slice::<ErrorBody>(&bytes)
                    .map(|b| b.error)
                    .unwrap_or_default();
                Err(CodesError::Unavailable(tr!(
                    format!("ответ {other} {reason}"),
                    format!("response {other} {reason}")
                )))
            }
        }
    }
}

fn room_path(code: &str) -> Result<String> {
    crate::normalize(code)
        .map(|code| format!("/rooms/{code}"))
        .ok_or_else(|| {
            CodesError::Invalid(tr!(
                format!("«{code}» — не код комнаты"),
                format!("\"{code}\" is not a room code")
            ))
        })
}
