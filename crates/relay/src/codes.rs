//! Room code service: `K7F-2QX` → host ticket (the protocol is in the `phase-codes` crate).
//!
//! A small HTTP service next to the relay (its own port, 3341 by default). Keeps codes in
//! memory: after a restart hosts recreate their codes themselves when renewing.
//!
//! **Protection**
//! - The service issues the code (random, 30⁶ ≈ 729 million combinations); nobody can pick or
//!   overwrite someone else's code: renewal and deletion need the secret issued at registration.
//! - Per-IP rate limiting (token bucket): code lookup is [`Limits::lookups_per_min`],
//!   registration, renewal and deletion are [`Limits::writes_per_min`].
//! - Brute force is impractical: with 10,000 open rooms one IP limited to 12 requests a
//!   minute finds a random room on average once every ~4 days (729M / 10,000 / 12 / 1440).
//!   And even then a code is not access: only the host lets the guest in ("Let in?" by device ID).
//! - Codes per IP and in total are capped at [`Limits::rooms_per_ip`] and [`Limits::max_rooms`].
//!
//! Behind a reverse proxy (Caddy, nginx for HTTPS) enable [`CodesOptions::trust_forwarded`]:
//! then the IP is taken from the last `X-Forwarded-For` address.

use std::{
    collections::HashMap,
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Bytes, Incoming},
    header,
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use phase_codes::{
    ErrorBody, Lookup, RegisterRequest, Registered, ReleaseRequest, RenewRequest, Renewed,
};
use serde::{Serialize, de::DeserializeOwned};
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, sync::Semaphore, task::JoinHandle};
use tracing::{debug, info, warn};

pub use phase_codes::DEFAULT_PORT;

/// The largest request body.
const MAX_BODY: usize = 4096;
/// How many connections to serve at once.
const MAX_CONNECTIONS: usize = 1024;
/// How long to wait for request headers (slow clients do not hold connections).
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// How often to remove expired codes and empty buckets.
const SWEEP_EVERY: Duration = Duration::from_secs(30);
/// Host secret length in bytes (a hex string in the response).
const SECRET_BYTES: usize = 16;

/// Service limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Code lookups per minute from one IP.
    pub lookups_per_min: u32,
    /// How many lookups can be made in a row.
    pub lookup_burst: u32,
    /// Registrations, renewals and deletions per minute from one IP.
    pub writes_per_min: u32,
    pub write_burst: u32,
    /// Live codes from one IP.
    pub rooms_per_ip: usize,
    /// Live codes in total.
    pub max_rooms: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            lookups_per_min: 12,
            lookup_burst: 6,
            // The host renews its code once a minute; the margin is for several hosts behind one NAT
            // (a school, a dorm).
            writes_per_min: 60,
            write_burst: 20,
            rooms_per_ip: 16,
            max_rooms: 100_000,
        }
    }
}

/// Why a request was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeError {
    RateLimited {
        retry_after: Duration,
    },
    NotFound,
    /// The code belongs to another host.
    Conflict,
    /// Too many codes (from this IP or in total).
    Full,
    BadRequest(&'static str),
}

impl CodeError {
    fn status(&self) -> StatusCode {
        match self {
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict => StatusCode::CONFLICT,
            Self::Full => StatusCode::SERVICE_UNAVAILABLE,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
        }
    }

    fn message(&self) -> &'static str {
        match self {
            Self::RateLimited { .. } => "слишком много запросов",
            Self::NotFound => "нет такой комнаты",
            Self::Conflict => "код занят другой комнатой",
            Self::Full => "слишком много комнат",
            Self::BadRequest(why) => why,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Lookup,
    Write,
}

struct Room {
    ticket: String,
    secret: String,
    ip: IpAddr,
    expires: Instant,
}

/// Token bucket: `tokens` refills at `per_min` per minute up to `burst`.
struct Bucket {
    tokens: f64,
    at: Instant,
}

/// Code store (no networking: time is passed in from outside, so tests do not wait).
pub struct CodeStore {
    limits: Limits,
    ttl: Duration,
    rooms: HashMap<String, Room>,
    per_ip: HashMap<IpAddr, usize>,
    buckets: HashMap<(IpAddr, Kind), Bucket>,
}

impl CodeStore {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            ttl: phase_codes::TTL,
            rooms: HashMap::new(),
            per_ip: HashMap::new(),
            buckets: HashMap::new(),
        }
    }

    /// Host: a new code for a ticket.
    pub fn register(
        &mut self,
        ip: IpAddr,
        ticket: &str,
        now: Instant,
    ) -> Result<Registered, CodeError> {
        self.take(ip, Kind::Write, now)?;
        check_ticket(ticket)?;
        self.check_capacity(ip, now)?;
        let mut code = None;
        for _ in 0..16 {
            let candidate = random_code()?;
            if !self.is_live(&candidate, now) {
                code = Some(candidate);
                break;
            }
        }
        let code = code.ok_or(CodeError::Full)?;
        let secret = random_secret()?;
        self.insert(code.clone(), ticket, secret.clone(), ip, now);
        Ok(Registered {
            code,
            secret,
            ttl_secs: self.ttl.as_secs(),
            renew_secs: phase_codes::RENEW_EVERY.as_secs(),
        })
    }

    /// Host: renew a code. An expired code is created again if nobody took it.
    pub fn renew(
        &mut self,
        ip: IpAddr,
        code: &str,
        secret: &str,
        ticket: &str,
        now: Instant,
    ) -> Result<Renewed, CodeError> {
        self.take(ip, Kind::Write, now)?;
        let code = exact_code(code)?;
        check_ticket(ticket)?;
        check_secret(secret)?;
        let ttl = self.ttl;
        if self.is_live(code, now) {
            let room = self
                .rooms
                .get_mut(code)
                .expect("a live code is in the table");
            if !same_secret(&room.secret, secret) {
                return Err(CodeError::Conflict);
            }
            room.ticket = ticket.to_string();
            room.expires = now + ttl;
        } else {
            self.remove(code);
            self.check_capacity(ip, now)?;
            self.insert(code.to_string(), ticket, secret.to_string(), ip, now);
        }
        Ok(Renewed {
            ttl_secs: ttl.as_secs(),
        })
    }

    /// Host: the room is closed. Deleting twice is not an error.
    pub fn release(
        &mut self,
        ip: IpAddr,
        code: &str,
        secret: &str,
        now: Instant,
    ) -> Result<(), CodeError> {
        self.take(ip, Kind::Write, now)?;
        let code = exact_code(code)?;
        if self.is_live(code, now) && !same_secret(&self.rooms[code].secret, secret) {
            return Err(CodeError::Conflict);
        }
        self.remove(code);
        Ok(())
    }

    /// Guest: the ticket for a code (the code in any case, with or without the dash).
    pub fn lookup(&mut self, ip: IpAddr, code: &str, now: Instant) -> Result<Lookup, CodeError> {
        self.take(ip, Kind::Lookup, now)?;
        let code = phase_codes::normalize(code).ok_or(CodeError::NotFound)?;
        match self.rooms.get(&code) {
            Some(room) if room.expires > now => Ok(Lookup {
                ticket: room.ticket.clone(),
            }),
            _ => Err(CodeError::NotFound),
        }
    }

    /// Removes expired codes and full (long unneeded) buckets.
    pub fn sweep(&mut self, now: Instant) {
        let expired: Vec<String> = self
            .rooms
            .iter()
            .filter(|(_, room)| room.expires <= now)
            .map(|(code, _)| code.clone())
            .collect();
        for code in expired {
            self.remove(&code);
        }
        let limits = self.limits;
        self.buckets.retain(|(_, kind), bucket| {
            let (per_min, burst) = rate(&limits, *kind);
            refilled(bucket, now, per_min, burst) < f64::from(burst)
        });
    }

    /// Live codes right now.
    pub fn len(&self, now: Instant) -> usize {
        self.rooms.values().filter(|r| r.expires > now).count()
    }

    pub fn is_empty(&self, now: Instant) -> bool {
        self.len(now) == 0
    }

    fn is_live(&self, code: &str, now: Instant) -> bool {
        self.rooms.get(code).is_some_and(|room| room.expires > now)
    }

    fn check_capacity(&mut self, ip: IpAddr, now: Instant) -> Result<(), CodeError> {
        if self.per_ip.get(&ip).copied().unwrap_or(0) >= self.limits.rooms_per_ip
            || self.rooms.len() >= self.limits.max_rooms
        {
            // Expired codes may still be in the table: remove them and check again.
            self.sweep(now);
            if self.per_ip.get(&ip).copied().unwrap_or(0) >= self.limits.rooms_per_ip
                || self.rooms.len() >= self.limits.max_rooms
            {
                return Err(CodeError::Full);
            }
        }
        Ok(())
    }

    fn insert(&mut self, code: String, ticket: &str, secret: String, ip: IpAddr, now: Instant) {
        *self.per_ip.entry(ip).or_default() += 1;
        self.rooms.insert(
            code,
            Room {
                ticket: ticket.to_string(),
                secret,
                ip,
                expires: now + self.ttl,
            },
        );
    }

    fn remove(&mut self, code: &str) {
        if let Some(room) = self.rooms.remove(code)
            && let Some(count) = self.per_ip.get_mut(&room.ip)
        {
            *count -= 1;
            if *count == 0 {
                self.per_ip.remove(&room.ip);
            }
        }
    }

    fn take(&mut self, ip: IpAddr, kind: Kind, now: Instant) -> Result<(), CodeError> {
        let (per_min, burst) = rate(&self.limits, kind);
        let bucket = self.buckets.entry((ip, kind)).or_insert(Bucket {
            tokens: f64::from(burst),
            at: now,
        });
        bucket.tokens = refilled(bucket, now, per_min, burst);
        bucket.at = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let wait = (1.0 - bucket.tokens) * 60.0 / f64::from(per_min.max(1));
            Err(CodeError::RateLimited {
                retry_after: Duration::from_secs_f64(wait.ceil().max(1.0)),
            })
        }
    }
}

fn rate(limits: &Limits, kind: Kind) -> (u32, u32) {
    match kind {
        Kind::Lookup => (limits.lookups_per_min, limits.lookup_burst),
        Kind::Write => (limits.writes_per_min, limits.write_burst),
    }
}

fn refilled(bucket: &Bucket, now: Instant, per_min: u32, burst: u32) -> f64 {
    let elapsed = now.saturating_duration_since(bucket.at).as_secs_f64();
    (bucket.tokens + elapsed * f64::from(per_min) / 60.0).min(f64::from(burst))
}

fn exact_code(code: &str) -> Result<&str, CodeError> {
    if phase_codes::is_code(code) {
        Ok(code)
    } else {
        Err(CodeError::BadRequest("это не код комнаты"))
    }
}

fn check_ticket(ticket: &str) -> Result<(), CodeError> {
    if phase_codes::looks_like_ticket(ticket) {
        Ok(())
    } else {
        Err(CodeError::BadRequest("это не приглашение Phase"))
    }
}

fn check_secret(secret: &str) -> Result<(), CodeError> {
    if secret.len() == SECRET_BYTES * 2 && secret.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(CodeError::BadRequest("неверный секрет"))
    }
}

fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

fn random_code() -> Result<String, CodeError> {
    // Extra bytes: biased ones are discarded (see code_from_random).
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| CodeError::Full)?;
    phase_codes::code_from_random(&bytes).ok_or(CodeError::Full)
}

fn random_secret() -> Result<String, CodeError> {
    let mut bytes = [0u8; SECRET_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| CodeError::Full)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

// ---------------------------------------------------------------- HTTP

/// Code service settings.
#[derive(Debug, Clone)]
pub struct CodesOptions {
    pub addr: SocketAddr,
    /// Take the client IP from `X-Forwarded-For` (only behind your own reverse proxy!).
    pub trust_forwarded: bool,
    pub limits: Limits,
}

impl CodesOptions {
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            trust_forwarded: false,
            limits: Limits::default(),
        }
    }
}

/// A running code service. Stops when dropped.
pub struct CodesServer {
    addr: SocketAddr,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for CodesServer {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl std::fmt::Debug for CodesServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodesServer")
            .field("addr", &self.addr)
            .finish()
    }
}

impl CodesServer {
    pub async fn spawn(opts: CodesOptions) -> Result<Self> {
        let listener = TcpListener::bind(opts.addr)
            .await
            .with_context(|| format!("сервис кодов: не удалось открыть {}", opts.addr))?;
        let addr = listener.local_addr()?;
        let store = Arc::new(Mutex::new(CodeStore::new(opts.limits)));

        let sweeper = {
            let store = store.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(SWEEP_EVERY);
                loop {
                    tick.tick().await;
                    lock(&store).sweep(Instant::now());
                }
            })
        };

        let trust = opts.trust_forwarded;
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let acceptor = tokio::spawn(async move {
            loop {
                let (stream, peer) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(err) => {
                        debug!("сервис кодов: ошибка приёма соединения: {err}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    debug!("сервис кодов: слишком много соединений, {peer} отклонён");
                    continue;
                };
                let store = store.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| {
                        let store = store.clone();
                        async move { Ok::<_, Infallible>(handle(&store, peer.ip(), trust, req).await) }
                    });
                    let conn = http1::Builder::new()
                        .timer(TokioTimer::new())
                        .header_read_timeout(HEADER_TIMEOUT)
                        .serve_connection(TokioIo::new(stream), service);
                    if let Err(err) = conn.await {
                        debug!("сервис кодов: соединение с {peer}: {err}");
                    }
                    drop(permit);
                });
            }
        });
        info!("сервис кодов комнат слушает {addr}");
        Ok(Self {
            addr,
            tasks: vec![acceptor, sweeper],
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Address for clients on the same computer: `http://127.0.0.1:<port>`.
    pub fn local_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }
}

fn lock(store: &Mutex<CodeStore>) -> std::sync::MutexGuard<'_, CodeStore> {
    store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

type Reply = Response<Full<Bytes>>;

async fn handle(
    store: &Mutex<CodeStore>,
    peer: IpAddr,
    trust_forwarded: bool,
    req: Request<Incoming>,
) -> Reply {
    let ip = if trust_forwarded {
        forwarded_ip(&req).unwrap_or(peer)
    } else {
        peer
    };
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let code = path.strip_prefix("/rooms/").map(str::to_string);
    let now = Instant::now;

    match (method, path.as_str(), code) {
        (Method::GET, "/healthz", _) => text(StatusCode::OK, "ok"),
        (Method::POST, "/rooms", _) => match body::<RegisterRequest>(req).await {
            Ok(body) => answer(
                lock(store).register(ip, &body.ticket, now()),
                StatusCode::CREATED,
            ),
            Err(err) => error(&err),
        },
        (Method::PUT, _, Some(code)) => match body::<RenewRequest>(req).await {
            Ok(body) => answer(
                lock(store).renew(ip, &code, &body.secret, &body.ticket, now()),
                StatusCode::OK,
            ),
            Err(err) => error(&err),
        },
        (Method::DELETE, _, Some(code)) => match body::<ReleaseRequest>(req).await {
            Ok(body) => match lock(store).release(ip, &code, &body.secret, now()) {
                Ok(()) => empty(StatusCode::NO_CONTENT),
                Err(err) => error(&err),
            },
            Err(err) => error(&err),
        },
        (Method::GET, _, Some(code)) => {
            let result = lock(store).lookup(ip, &code, now());
            if let Err(CodeError::RateLimited { .. }) = &result {
                warn!("сервис кодов: {ip} превысил лимит поиска");
            }
            answer(result, StatusCode::OK)
        }
        _ => json(
            StatusCode::NOT_FOUND,
            &ErrorBody {
                error: "нет такого адреса".into(),
            },
        ),
    }
}

/// Client IP from our own reverse proxy: the last address in `X-Forwarded-For`.
fn forwarded_ip(req: &Request<Incoming>) -> Option<IpAddr> {
    req.headers()
        .get_all("x-forwarded-for")
        .iter()
        .next_back()?
        .to_str()
        .ok()?
        .rsplit(',')
        .next()?
        .trim()
        .parse()
        .ok()
}

async fn body<T: DeserializeOwned>(req: Request<Incoming>) -> Result<T, CodeError> {
    let bytes = Limited::new(req.into_body(), MAX_BODY)
        .collect()
        .await
        .map_err(|_| CodeError::BadRequest("слишком большой запрос"))?
        .to_bytes();
    serde_json::from_slice(&bytes).map_err(|_| CodeError::BadRequest("неверный JSON"))
}

fn answer<T: Serialize>(result: Result<T, CodeError>, ok: StatusCode) -> Reply {
    match result {
        Ok(value) => json(ok, &value),
        Err(err) => error(&err),
    }
}

fn error(err: &CodeError) -> Reply {
    let mut reply = json(
        err.status(),
        &ErrorBody {
            error: err.message().into(),
        },
    );
    if let CodeError::RateLimited { retry_after } = err
        && let Ok(value) = retry_after.as_secs().to_string().parse()
    {
        reply.headers_mut().insert(header::RETRY_AFTER, value);
    }
    reply
}

fn json<T: Serialize>(status: StatusCode, value: &T) -> Reply {
    let body = serde_json::to_vec(value).unwrap_or_default();
    let mut reply = Response::new(Full::new(Bytes::from(body)));
    *reply.status_mut() = status;
    reply.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json; charset=utf-8"),
    );
    reply
}

fn text(status: StatusCode, text: &'static str) -> Reply {
    let mut reply = Response::new(Full::new(Bytes::from_static(text.as_bytes())));
    *reply.status_mut() = status;
    reply
}

fn empty(status: StatusCode) -> Reply {
    let mut reply = Response::new(Full::new(Bytes::new()));
    *reply.status_mut() = status;
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICKET: &str = "endpointaaaabbbbccccdddd";
    const OTHER_TICKET: &str = "endpointeeeeffff";

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([192, 0, 2, last])
    }

    fn roomy() -> Limits {
        Limits {
            lookups_per_min: 1000,
            lookup_burst: 1000,
            writes_per_min: 1000,
            write_burst: 1000,
            ..Limits::default()
        }
    }

    #[test]
    fn register_and_lookup() {
        let mut store = CodeStore::new(roomy());
        let now = Instant::now();
        let reg = store.register(ip(1), TICKET, now).unwrap();
        assert!(phase_codes::is_code(&reg.code), "{}", reg.code);
        assert_eq!(reg.secret.len(), SECRET_BYTES * 2);
        assert_eq!(reg.ttl_secs, 120);
        assert_eq!(reg.renew_secs, 60);
        // The guest types the code however is convenient: lowercase and without the dash.
        let typed = reg.code.to_lowercase().replace('-', "");
        assert_eq!(store.lookup(ip(2), &typed, now).unwrap().ticket, TICKET);
        assert_eq!(
            store.lookup(ip(2), "ZZZ-ZZZ", now),
            Err(CodeError::NotFound)
        );
    }

    #[test]
    fn renew_extends_and_expiry_removes() {
        let mut store = CodeStore::new(roomy());
        let start = Instant::now();
        let reg = store.register(ip(1), TICKET, start).unwrap();
        // Renewing once a minute keeps the code alive past the TTL.
        let mut now = start;
        for _ in 0..5 {
            now += phase_codes::RENEW_EVERY;
            store
                .renew(ip(1), &reg.code, &reg.secret, TICKET, now)
                .unwrap();
        }
        assert!(now - start > phase_codes::TTL);
        assert!(store.lookup(ip(2), &reg.code, now).is_ok());
        // Without renewal the code expires after the TTL.
        let later = now + phase_codes::TTL;
        assert_eq!(
            store.lookup(ip(2), &reg.code, later),
            Err(CodeError::NotFound)
        );
        store.sweep(later);
        assert!(store.is_empty(later));
        assert!(store.per_ip.is_empty());
    }

    #[test]
    fn foreign_secret_cannot_overwrite_or_delete() {
        let mut store = CodeStore::new(roomy());
        let now = Instant::now();
        let reg = store.register(ip(1), TICKET, now).unwrap();
        let thief = "0".repeat(SECRET_BYTES * 2);
        assert_eq!(
            store.renew(ip(9), &reg.code, &thief, OTHER_TICKET, now),
            Err(CodeError::Conflict)
        );
        assert_eq!(
            store.release(ip(9), &reg.code, &thief, now),
            Err(CodeError::Conflict)
        );
        assert_eq!(store.lookup(ip(2), &reg.code, now).unwrap().ticket, TICKET);
        // The host releases the code itself, and it is gone.
        store.release(ip(1), &reg.code, &reg.secret, now).unwrap();
        assert_eq!(
            store.lookup(ip(2), &reg.code, now),
            Err(CodeError::NotFound)
        );
    }

    #[test]
    fn expired_code_is_recreated_by_its_host() {
        let mut store = CodeStore::new(roomy());
        let now = Instant::now();
        let reg = store.register(ip(1), TICKET, now).unwrap();
        let later = now + phase_codes::TTL * 3;
        store.sweep(later);
        assert!(store.is_empty(later));
        // The service "forgot" the code (restart, outage): the host brings it back by renewing.
        store
            .renew(ip(1), &reg.code, &reg.secret, TICKET, later)
            .unwrap();
        assert_eq!(
            store.lookup(ip(2), &reg.code, later).unwrap().ticket,
            TICKET
        );
    }

    #[test]
    fn rate_limit_per_ip() {
        let limits = Limits {
            lookups_per_min: 12,
            lookup_burst: 6,
            ..Limits::default()
        };
        let mut store = CodeStore::new(limits);
        let now = Instant::now();
        for _ in 0..6 {
            assert_eq!(
                store.lookup(ip(7), "ABC-DEF", now),
                Err(CodeError::NotFound)
            );
        }
        let Err(CodeError::RateLimited { retry_after }) = store.lookup(ip(7), "ABC-DEF", now)
        else {
            panic!("the seventh lookup in a row should hit the limit");
        };
        assert_eq!(retry_after, Duration::from_secs(5));
        // Another IP is not affected.
        assert_eq!(
            store.lookup(ip(8), "ABC-DEF", now),
            Err(CodeError::NotFound)
        );
        // After 5 seconds (12 a minute) one more attempt.
        let later = now + Duration::from_secs(5);
        assert_eq!(
            store.lookup(ip(7), "ABC-DEF", later),
            Err(CodeError::NotFound)
        );
        assert!(matches!(
            store.lookup(ip(7), "ABC-DEF", later),
            Err(CodeError::RateLimited { .. })
        ));
    }

    #[test]
    fn rooms_per_ip_are_capped() {
        let limits = Limits {
            rooms_per_ip: 2,
            ..roomy()
        };
        let mut store = CodeStore::new(limits);
        let now = Instant::now();
        store.register(ip(1), TICKET, now).unwrap();
        store.register(ip(1), TICKET, now).unwrap();
        assert_eq!(store.register(ip(1), TICKET, now), Err(CodeError::Full));
        assert!(store.register(ip(2), TICKET, now).is_ok());
        // Expired codes free up room.
        assert!(
            store
                .register(ip(1), TICKET, now + phase_codes::TTL)
                .is_ok()
        );
    }

    #[test]
    fn junk_is_rejected() {
        let mut store = CodeStore::new(roomy());
        let now = Instant::now();
        assert!(matches!(
            store.register(ip(1), "hello", now),
            Err(CodeError::BadRequest(_))
        ));
        let reg = store.register(ip(1), TICKET, now).unwrap();
        assert!(matches!(
            store.renew(ip(1), &reg.code, "short", TICKET, now),
            Err(CodeError::BadRequest(_))
        ));
        assert!(matches!(
            store.renew(ip(1), "abc-def", &reg.secret, TICKET, now),
            Err(CodeError::BadRequest(_))
        ));
    }
}
