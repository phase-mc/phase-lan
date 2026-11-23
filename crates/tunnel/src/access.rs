//! Whom the host lets in.
//!
//! The decision is made **only on the host side**: by its local list of
//! allowed devices or by its answer to the "let in?" question. The check runs right
//! after the encrypted connection is established ([`EndpointHooks::after_handshake`]),
//! before the host program sees the connection. It is repeated
//! in the host handler ([`crate::host`]).
//!
//! **Nickname and ID.** Admission is tied to the device ID (key), which cannot be forged. The
//! player states the nickname in the first message ([`wire::Hello`]): it is a label for
//! convenience, not proof. So the "let in?" question shows the nickname together with the ID,
//! and the allow list keeps the nickname only as a note next to the ID.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt, fs,
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use anyhow::{Context, Result};
use iroh::{
    EndpointId,
    endpoint::{
        AfterHandshakeOutcome, Connection, EndpointHooks, RecvStream, SendStream, Side, VarInt,
    },
};
use n0_future::boxed::BoxFuture;
use phase_lang::tr;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::{
    events::{AccessDecision, EventSink, TunnelEvent},
    fsutil,
    wire::{self, Hello, RejectReason, StreamKind},
};

/// How long to wait for the host's answer to the "let in?" question.
pub const APPROVE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long to wait for a new device's hello (it carries the nickname) before asking
/// "let in?" without a nickname. The guest sends the hello right after connecting, so
/// there is usually no wait at all.
pub const INTRO_TIMEOUT: Duration = Duration::from_secs(3);

/// Start of a nickname note line in the list file: `#@ <ID> <nickname>`.
const NICK_NOTE: &str = "#@";

/// How many pre-read streams to keep until the host handler picks them up.
const EARLY_KEEP: usize = 32;

/// Asks the host whether to let an unknown device in (for example, with a console question).
pub trait Approver: Send + Sync + std::fmt::Debug + 'static {
    /// `nickname` is the nickname the device gave (already sanitized). `None` means the device
    /// did not introduce itself (an old program version).
    fn approve(&self, id: EndpointId, nickname: Option<String>) -> BoxFuture<bool>;
}

/// Allowed devices: ID → last known nickname.
pub type AllowedDevices = BTreeMap<EndpointId, Option<String>>;

/// Parses the text of the allow list file.
///
/// - empty lines and lines with `#` are comments;
/// - `<ID>` is an allowed device (the format of all program versions);
/// - `#@ <ID> <nickname>` is the last known nickname of that device. For old program
///   versions this is an ordinary comment, so they can still read the file;
/// - `<ID> <nickname>` is the same on one line (so a nickname can be added by hand).
///
/// A nickname without an ID line allows nothing and is dropped.
pub fn parse_allowed(text: &str) -> Result<AllowedDevices> {
    let mut allowed = AllowedDevices::new();
    let mut notes: BTreeMap<EndpointId, String> = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if let Some(note) = line.strip_prefix(NICK_NOTE) {
            let (id, nick) = split_first_word(note.trim_start());
            // A broken note is just a comment.
            if let (Ok(id), Some(nick)) = (id.parse::<EndpointId>(), wire::sanitize_nickname(nick))
            {
                notes.insert(id, nick);
            }
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (id, nick) = split_first_word(line);
        let id: EndpointId = id.parse().with_context(|| {
            tr!(
                format!("неверный ID: {line}"),
                format!("invalid ID: {line}")
            )
        })?;
        let nick = wire::sanitize_nickname(nick);
        let entry = allowed.entry(id).or_default();
        if nick.is_some() {
            *entry = nick;
        }
    }
    for (id, nick) in notes {
        if let Some(entry) = allowed.get_mut(&id) {
            *entry = Some(nick);
        }
    }
    Ok(allowed)
}

fn split_first_word(text: &str) -> (&str, &str) {
    text.split_once(char::is_whitespace).unwrap_or((text, ""))
}

/// Text of the allow list file (see [`parse_allowed`]).
pub fn format_allowed(allowed: &AllowedDevices) -> String {
    let mut text = String::from(tr!(
        "# Phase: устройства, которым разрешено подключаться (один ID на строку).\n\
         # Строка «#@ <ID> <ник>» — последний известный ник устройства. Это подпись для\n\
         # удобства: доступ даёт только строка с ID.\n",
        "# Phase: devices allowed to connect (one ID per line).\n\
         # A line \"#@ <ID> <nickname>\" is the device's last known nickname. It is only a\n\
         # label for convenience: access is granted by the ID line alone.\n",
    ));
    for (id, nick) in allowed {
        text.push_str(&format!("{id}\n"));
        if let Some(nick) = nick {
            text.push_str(&format!("{NICK_NOTE} {id} {nick}\n"));
        }
    }
    text
}

/// The guest's first stream, read by the admission check before the host handler saw
/// the connection: it provides the nickname for the "let in?" question. The handler takes
/// the stream and answers it as usual.
pub(crate) struct EarlyStream {
    pub kind: StreamKind,
    /// The guest's hello (only for the control stream, if it was read).
    pub hello: Option<Hello>,
    pub send: SendStream,
    pub recv: RecvStream,
}

impl EarlyStream {
    fn nickname(&self) -> Option<String> {
        self.hello.as_ref().and_then(Hello::nickname)
    }
}

/// Streams read in advance: connection number → stream.
#[derive(Clone, Default)]
struct EarlyStreams(Arc<Mutex<HashMap<usize, EarlyStream>>>);

impl fmt::Debug for EarlyStreams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "EarlyStreams({})",
            self.0.lock().expect("early streams lock").len()
        )
    }
}

/// Room slots: how many players the room holds and which devices are connected now.
///
/// **Devices** are counted (one device can have several connections) plus the host
/// itself. The limit can change while the room is open: it does not kick those already connected.
/// Cheap to clone: all copies share one counter.
#[derive(Debug, Clone, Default)]
pub struct RoomGate {
    /// How many players the room holds including the host (0 means no limit).
    limit: Arc<std::sync::atomic::AtomicU32>,
    /// Connected devices and their connection counts.
    live: Arc<Mutex<HashMap<EndpointId, u32>>>,
}

impl RoomGate {
    /// How many players the room holds including the host (0 means no limit).
    pub fn limit(&self) -> u32 {
        self.limit.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_limit(&self, max_players: u32) {
        self.limit
            .store(max_players, std::sync::atomic::Ordering::Relaxed);
    }

    /// How many guest devices are connected now.
    pub fn guests(&self) -> usize {
        self.live.lock().expect("room gate lock").len()
    }

    /// A new device would not fit. An already connected device (a second
    /// connection, a reconnect) does not take a slot.
    pub fn is_full_for(&self, id: &EndpointId) -> bool {
        let live = self.live.lock().expect("room gate lock");
        Self::full(&live, self.limit(), id)
    }

    fn full(live: &HashMap<EndpointId, u32>, limit: u32, id: &EndpointId) -> bool {
        // The host always takes one slot.
        limit != 0 && !live.contains_key(id) && live.len() + 1 >= limit as usize
    }

    /// A device connection takes a slot. `false` means no slots (the check and the write
    /// happen under one lock: two guests cannot take the last slot at once).
    pub(crate) fn enter(&self, id: EndpointId) -> bool {
        let mut live = self.live.lock().expect("room gate lock");
        if Self::full(&live, self.limit(), &id) {
            return false;
        }
        *live.entry(id).or_default() += 1;
        true
    }

    /// A device connection closed; when it is the last one, the slot is freed.
    pub(crate) fn leave(&self, id: &EndpointId) {
        let mut live = self.live.lock().expect("room gate lock");
        if let Some(count) = live.get_mut(id) {
            *count -= 1;
            if *count == 0 {
                live.remove(id);
            }
        }
    }
}

/// The allow list. Can be saved to a file (see [`parse_allowed`]).
/// Cheap to clone: all copies share one list.
#[derive(Debug, Clone)]
pub struct AccessList {
    allowed: Arc<RwLock<AllowedDevices>>,
    file: Option<PathBuf>,
    early: EarlyStreams,
    gate: RoomGate,
}

impl AccessList {
    /// An in-memory list only (for tests).
    pub fn in_memory(ids: impl IntoIterator<Item = EndpointId>) -> Self {
        Self {
            allowed: Arc::new(RwLock::new(ids.into_iter().map(|id| (id, None)).collect())),
            file: None,
            early: EarlyStreams::default(),
            gate: RoomGate::default(),
        }
    }

    /// Loads the list from a file (if there is no file, an empty list).
    pub fn load(file: PathBuf) -> Result<Self> {
        let allowed = match fs::read_to_string(&file) {
            Ok(text) => parse_allowed(&text).with_context(|| {
                tr!(
                    format!("ошибка в {}", file.display()),
                    format!("error in {}", file.display())
                )
            })?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => AllowedDevices::new(),
            Err(err) => {
                return Err(err).with_context(|| {
                    tr!(
                        format!("не удалось прочитать {}", file.display()),
                        format!("could not read {}", file.display())
                    )
                });
            }
        };
        Ok(Self {
            allowed: Arc::new(RwLock::new(allowed)),
            file: Some(file),
            early: EarlyStreams::default(),
            gate: RoomGate::default(),
        })
    }

    pub fn is_allowed(&self, id: &EndpointId) -> bool {
        self.allowed
            .read()
            .expect("access list lock")
            .contains_key(id)
    }

    /// Room slots (see [`RoomGate`]).
    pub fn gate(&self) -> &RoomGate {
        &self.gate
    }

    pub fn len(&self) -> usize {
        self.allowed.read().expect("access list lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Allowed devices and their last known nicknames.
    pub fn devices(&self) -> AllowedDevices {
        self.allowed.read().expect("access list lock").clone()
    }

    /// Last known nickname of an allowed device.
    pub fn nickname(&self, id: &EndpointId) -> Option<String> {
        self.allowed
            .read()
            .expect("access list lock")
            .get(id)
            .cloned()
            .flatten()
    }

    /// Adds a device. Returns `true` if it was not there before.
    pub fn allow(&self, id: EndpointId) -> Result<bool> {
        self.allow_as(id, None)
    }

    /// Adds a device and remembers its nickname (if given). Returns `true`
    /// if the device was not there before.
    pub fn allow_as(&self, id: EndpointId, nickname: Option<&str>) -> Result<bool> {
        let nickname = nickname.and_then(wire::sanitize_nickname);
        let (added, changed) = {
            let mut allowed = self.allowed.write().expect("access list lock");
            let added = !allowed.contains_key(&id);
            let entry = allowed.entry(id).or_default();
            let renamed = nickname.is_some() && *entry != nickname;
            if renamed {
                *entry = nickname;
            }
            (added, added || renamed)
        };
        if changed {
            self.save()?;
        }
        Ok(added)
    }

    /// Removes a device from the list. Returns `true` if it was there. A connection that is
    /// already open stays open; the device goes through the admission check again next time.
    pub fn revoke(&self, id: &EndpointId) -> Result<bool> {
        let removed = self
            .allowed
            .write()
            .expect("access list lock")
            .remove(id)
            .is_some();
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// Remembers a new nickname of an already allowed device. Returns `true` if the nickname
    /// changed. A device not on the list is not added.
    pub fn remember_nickname(&self, id: &EndpointId, nickname: &str) -> Result<bool> {
        let Some(nickname) = wire::sanitize_nickname(nickname) else {
            return Ok(false);
        };
        let changed = {
            let mut allowed = self.allowed.write().expect("access list lock");
            match allowed.get_mut(id) {
                Some(entry) if entry.as_deref() != Some(nickname.as_str()) => {
                    *entry = Some(nickname);
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.save()?;
        }
        Ok(changed)
    }

    fn save(&self) -> Result<()> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        let text = format_allowed(&self.allowed.read().expect("access list lock"));
        fsutil::write_atomic(file, text.as_bytes())
    }

    /// The admission check has already read the connection's first stream: leaves it for the handler.
    pub(crate) fn stash_early(&self, conn_id: usize, stream: EarlyStream) {
        let mut early = self.early.0.lock().expect("early streams lock");
        // The handler takes the stream right after the check. If entries pile up, nobody
        // will take them (the handler is not running): do not keep others' connections open.
        if early.len() >= EARLY_KEEP {
            early.clear();
        }
        early.insert(conn_id, stream);
    }

    /// The stream the admission check read for this connection (if any).
    pub(crate) fn take_early(&self, conn_id: usize) -> Option<EarlyStream> {
        self.early
            .0
            .lock()
            .expect("early streams lock")
            .remove(&conn_id)
    }
}

/// Admission check on every incoming connection.
#[derive(Debug)]
pub struct AccessHook {
    list: AccessList,
    approver: Option<Arc<dyn Approver>>,
    /// At most one new device waits for approval at a time. The rest are rejected
    /// right away: so a stranger with a ticket cannot flood the host with questions,
    /// and the time to answer counts from the moment the question is shown.
    pending: Arc<Semaphore>,
    /// Devices the host rejected in this session: we do not ask again.
    denied: Arc<RwLock<BTreeSet<EndpointId>>>,
    /// Decisions about new devices, for the interface.
    events: EventSink,
}

impl AccessHook {
    pub fn new(list: AccessList, approver: Option<Arc<dyn Approver>>) -> Self {
        Self {
            list,
            approver,
            pending: Arc::new(Semaphore::new(1)),
            denied: Arc::default(),
            events: EventSink::default(),
        }
    }

    /// Report decisions about new devices ([`TunnelEvent::Access`]).
    pub fn with_events(mut self, events: EventSink) -> Self {
        self.events = events;
        self
    }

    fn report(&self, id: &EndpointId, nickname: Option<&str>, decision: AccessDecision) {
        self.events.emit(TunnelEvent::Access {
            peer: id.to_string(),
            nickname: nickname.map(str::to_string),
            decision,
        });
    }

    fn is_denied(&self, id: &EndpointId) -> bool {
        self.denied.read().expect("denied lock").contains(id)
    }

    async fn decide(&self, conn: &Connection) -> AfterHandshakeOutcome {
        // Outgoing connections (we connect somewhere ourselves) are not checked.
        if conn.side() != Side::Server {
            return AfterHandshakeOutcome::Accept;
        }
        if conn.alpn() != wire::ALPN {
            return AfterHandshakeOutcome::Reject {
                error_code: VarInt::from_u32(wire::code::BAD_PROTOCOL),
                reason: b"unsupported protocol".to_vec(),
            };
        }
        let id = conn.remote_id();
        // Slots before everything else: if there are none, there is no point asking "let in?",
        // and a friend from the allow list gets a clear rejection instead of silence.
        if self.list.gate().is_full_for(&id) {
            let max = self.list.gate().limit();
            info!(
                "устройство {} отклонено: комната заполнена ({max} из {max})",
                wire::short_id(&id)
            );
            self.report(
                &id,
                self.list.nickname(&id).as_deref(),
                AccessDecision::RoomFull,
            );
            return reject(RejectReason::RoomFull { max });
        }
        if self.list.is_allowed(&id) {
            return AfterHandshakeOutcome::Accept;
        }
        let Some(approver) = &self.approver else {
            info!(
                "отклонено неизвестное устройство {} (вопросы выключены)",
                wire::short_id(&id)
            );
            self.report(&id, None, AccessDecision::PromptsOff);
            return reject(RejectReason::NotAllowed);
        };
        if self.is_denied(&id) {
            info!(
                "устройство {} отклонено: вы уже ответили ему «нет»",
                wire::short_id(&id)
            );
            self.report(&id, None, AccessDecision::AlreadyDenied);
            return reject(RejectReason::Denied);
        }
        let Ok(_permit) = self.pending.clone().try_acquire_owned() else {
            info!(
                "устройство {} отклонено: вы ещё не ответили на вопрос о другом устройстве",
                wire::short_id(&id)
            );
            self.report(&id, None, AccessDecision::Busy);
            return reject(RejectReason::Busy);
        };
        // While we waited, this device may have been approved or rejected (a second connection).
        if self.list.is_allowed(&id) {
            return AfterHandshakeOutcome::Accept;
        }
        if self.is_denied(&id) {
            return reject(RejectReason::Denied);
        }
        // Who is knocking: the guest has already sent a hello with a nickname, read it before asking.
        let intro = read_intro(conn).await;
        let nickname = intro.as_ref().and_then(EarlyStream::nickname);
        let who = wire::peer_label(&id, nickname.as_deref());
        let answer = tokio::select! {
            answer = tokio::time::timeout(APPROVE_TIMEOUT, approver.approve(id, nickname.clone())) => answer.ok(),
            // The guest left without waiting for an answer: the question is no longer needed, the slot is freed.
            _ = conn.closed() => {
                info!("{who} отключился, не дождавшись ответа — вопрос отменён, отвечать не нужно");
                self.report(&id, nickname.as_deref(), AccessDecision::GuestLeft);
                return reject(RejectReason::Timeout);
            }
        };
        match answer {
            Some(true) => {
                if let Err(err) = self.list.allow_as(id, nickname.as_deref()) {
                    warn!("не удалось сохранить список разрешённых: {err:#}");
                }
                if let Some(intro) = intro {
                    self.list.stash_early(conn.stable_id(), intro);
                }
                info!("{who} разрешён");
                self.report(&id, nickname.as_deref(), AccessDecision::Allowed);
                AfterHandshakeOutcome::Accept
            }
            Some(false) => {
                self.denied.write().expect("denied lock").insert(id);
                info!("{who} отклонён хостом");
                self.report(&id, nickname.as_deref(), AccessDecision::Denied);
                reject(RejectReason::Denied)
            }
            None => {
                info!(
                    "нет ответа за {} с — {who} отклонён",
                    APPROVE_TIMEOUT.as_secs()
                );
                self.report(&id, nickname.as_deref(), AccessDecision::Timeout);
                reject(RejectReason::Timeout)
            }
        }
    }
}

/// Reads a new device's first stream: on the control stream, a hello with the nickname.
/// `None` means the device sent nothing within [`INTRO_TIMEOUT`] or sent junk:
/// the "let in?" question will be asked without a nickname.
async fn read_intro(conn: &Connection) -> Option<EarlyStream> {
    let read = async {
        let (send, mut recv) = conn.accept_bi().await.ok()?;
        let kind = wire::read_kind(&mut recv).await.ok()?;
        let hello = match kind {
            StreamKind::Control => wire::read_msg::<_, Hello>(&mut recv).await.ok(),
            StreamKind::Tcp | StreamKind::Echo | StreamKind::Mods | StreamKind::Look => None,
        };
        Some(EarlyStream {
            kind,
            hello,
            send,
            recv,
        })
    };
    tokio::time::timeout(INTRO_TIMEOUT, read)
        .await
        .ok()
        .flatten()
}

fn reject(reason: RejectReason) -> AfterHandshakeOutcome {
    AfterHandshakeOutcome::Reject {
        error_code: VarInt::from_u32(wire::code::NOT_ALLOWED),
        reason: reason.to_bytes(),
    }
}

impl EndpointHooks for AccessHook {
    fn after_handshake<'a>(
        &'a self,
        conn: &'a Connection,
    ) -> impl Future<Output = AfterHandshakeOutcome> + Send + 'a {
        self.decide(conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn temp_file(name: &str) -> PathBuf {
        let file =
            std::env::temp_dir().join(format!("phase-test-{name}-{}.txt", std::process::id()));
        let _ = fs::remove_file(&file);
        file
    }

    #[test]
    fn room_gate_counts_devices_and_the_host() {
        let ids: Vec<_> = (0..5).map(|_| SecretKey::generate().public()).collect();
        let gate = RoomGate::default();
        // Without a limit, slots never run out.
        assert!(!gate.is_full_for(&ids[0]));

        // A room for 3 players: the host and two guests.
        gate.set_limit(3);
        assert!(gate.enter(ids[0]));
        assert!(gate.enter(ids[1]));
        assert_eq!(gate.guests(), 2);
        assert!(gate.is_full_for(&ids[2]));
        assert!(!gate.enter(ids[2]), "no slot for the third guest");
        // A second connection of the same device does not take a slot.
        assert!(!gate.is_full_for(&ids[0]));
        assert!(gate.enter(ids[0]));
        gate.leave(&ids[0]);
        assert!(gate.is_full_for(&ids[2]), "the device is still connected");
        gate.leave(&ids[0]);
        assert!(gate.enter(ids[2]), "the slot was freed");

        // Limit raised: new guests get in; lowered: connected ones are not kicked.
        gate.set_limit(5);
        assert!(gate.enter(ids[3]));
        gate.set_limit(2);
        assert_eq!(gate.guests(), 3);
        assert!(gate.is_full_for(&ids[4]));
        gate.leave(&ids[4]); // a stranger leaving breaks nothing
        assert_eq!(gate.guests(), 3);
    }

    #[test]
    fn revoke_removes_and_saves() {
        let file = temp_file("revoke");
        let (a, b) = (
            SecretKey::generate().public(),
            SecretKey::generate().public(),
        );
        let list = AccessList::load(file.clone()).unwrap();
        list.allow_as(a, Some("Alex")).unwrap();
        list.allow(b).unwrap();

        assert!(list.revoke(&a).unwrap());
        assert!(!list.is_allowed(&a));
        assert!(!list.revoke(&a).unwrap(), "already gone");

        // The file follows the list, so a later load does not bring the device back.
        let reloaded = AccessList::load(file.clone()).unwrap();
        assert!(!reloaded.is_allowed(&a));
        assert!(reloaded.is_allowed(&b));
        let _ = fs::remove_file(&file);
    }

    #[test]
    fn list_persists_to_file() {
        let file = temp_file("allow");
        let id = SecretKey::generate().public();

        let list = AccessList::load(file.clone()).unwrap();
        assert!(!list.is_allowed(&id));
        assert!(list.allow(id).unwrap());
        assert!(!list.allow(id).unwrap(), "adding again changes nothing");

        let reloaded = AccessList::load(file.clone()).unwrap();
        assert!(reloaded.is_allowed(&id));
        assert_eq!(reloaded.nickname(&id), None);
        fs::remove_file(&file).unwrap();
    }

    #[test]
    fn old_format_without_nicknames_still_loads() {
        // This is how versions before nicknames wrote the file: a header comment and one ID per line.
        let (a, b) = (
            SecretKey::generate().public(),
            SecretKey::generate().public(),
        );
        let text = format!(
            "# LanBridge: устройства, которым разрешено подключаться\n{a}\n\n  {b}  \n# конец\n"
        );
        let allowed = parse_allowed(&text).unwrap();
        assert_eq!(allowed.len(), 2);
        assert_eq!(allowed[&a], None);
        assert_eq!(allowed[&b], None);
    }

    #[test]
    fn nicknames_survive_save_and_load() {
        let file = temp_file("allow-nicks");
        let (friend, silent) = (
            SecretKey::generate().public(),
            SecretKey::generate().public(),
        );

        let list = AccessList::load(file.clone()).unwrap();
        assert!(list.allow_as(friend, Some("GrayIsko1337")).unwrap());
        assert!(list.allow(silent).unwrap());
        // A nickname with a space: in the file it runs to the end of the line.
        assert!(list.remember_nickname(&friend, "Серый Волк").unwrap());
        assert!(
            !list.remember_nickname(&friend, "Серый Волк").unwrap(),
            "same nickname, the file is not rewritten"
        );
        // Approving again without a nickname does not erase the remembered one.
        assert!(!list.allow(friend).unwrap());

        let reloaded = AccessList::load(file.clone()).unwrap();
        assert_eq!(reloaded.len(), 2);
        assert_eq!(reloaded.nickname(&friend).as_deref(), Some("Серый Волк"));
        assert_eq!(reloaded.nickname(&silent), None);
        assert_eq!(reloaded.devices(), list.devices());
        fs::remove_file(&file).unwrap();
    }

    #[test]
    fn file_with_nicknames_is_readable_by_older_versions() {
        // Reading the list in versions before nicknames: comments are skipped, every other
        // line must be an ID in full.
        fn old_load(text: &str) -> Result<BTreeSet<EndpointId>> {
            let mut allowed = BTreeSet::new();
            for line in text.lines().map(str::trim) {
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                allowed.insert(
                    line.parse()
                        .with_context(|| format!("неверный ID: {line}"))?,
                );
            }
            Ok(allowed)
        }

        let (a, b) = (
            SecretKey::generate().public(),
            SecretKey::generate().public(),
        );
        let mut allowed = AllowedDevices::new();
        allowed.insert(a, Some("GrayIsko1337".into()));
        allowed.insert(b, None);
        let text = format_allowed(&allowed);

        assert_eq!(old_load(&text).unwrap(), BTreeSet::from([a, b]));
        assert_eq!(parse_allowed(&text).unwrap(), allowed);
    }

    #[test]
    fn nickname_as_second_column_and_stray_notes() {
        let (a, b, stranger) = (
            SecretKey::generate().public(),
            SecretKey::generate().public(),
            SecretKey::generate().public(),
        );
        let text = format!(
            "{a} Steve \u{1b}[31mRed\n\
             {b}\n\
             #@ {b} Alex\n\
             #@ {stranger} Mallory\n\
             #@ не-ID просто комментарий\n\
             #@\n"
        );
        let allowed = parse_allowed(&text).unwrap();
        // A nickname in the second column is sanitized the same way as one from the network.
        assert_eq!(allowed[&a].as_deref(), Some("Steve [31mRed"));
        assert_eq!(allowed[&b].as_deref(), Some("Alex"));
        // A nickname note without an ID line gives no access.
        assert!(!allowed.contains_key(&stranger));
        assert_eq!(allowed.len(), 2);
    }

    #[test]
    fn broken_id_is_an_error_not_a_silent_skip() {
        let err = parse_allowed("не-ID\n").unwrap_err();
        assert!(format!("{err:#}").contains("неверный ID"), "{err:#}");
    }

    #[test]
    fn nickname_of_unknown_device_is_not_remembered() {
        let list = AccessList::in_memory([]);
        let stranger = SecretKey::generate().public();
        assert!(!list.remember_nickname(&stranger, "Mallory").unwrap());
        assert!(!list.is_allowed(&stranger));
        assert_eq!(list.nickname(&stranger), None);
    }
}
