//! The "phase/mc/1" protocol over a QUIC connection.
//!
//! The guest opens every bidirectional stream. Its first byte is the stream type
//! ([`StreamKind`]). The host sees a new stream only after its first bytes, so
//! the header is sent right away.
//!
//! - `Control`: the guest sends [`Hello`], the host answers with [`HostInfo`]. Both messages
//!   carry an optional game nickname (`nickname`): the host sees who is knocking, the guest sees whom it
//!   connected to. The player states the nickname, it is **not** an identity check: admission is decided
//!   by device ID only.
//! - `Tcp`: after that, raw bytes of one game TCP connection in both directions.
//! - `Echo`: diagnostics. The host returns every frame of [`ECHO_FRAME_LEN`] bytes.
//! - `Mods`: room mods (see [`crate::mods`]). The guest asks for the mod list or one file
//!   by its hash. Opened only if the host announced a share in [`HostInfo::mods`]:
//!   an older host does not know this stream.
//! - `Look`: the guest's in-game styling (see [`LookRequest`]). Opened only if
//!   the host announced it in [`RoomInfo::look`].
//!
//! Control messages: a `u32` length (big-endian) + JSON. Easy to read and debug,
//! and the messages are small and rare.

use anyhow::{Context, Result, bail};
use phase_lang::tr;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Protocol identifier in TLS (ALPN). Other iroh applications will not connect to us.
/// A client still called LanBridge (`lanbridge/mc/1`) gets a TLS
/// "unsupported protocol" rejection right away instead of a hung connection: the old name has no
/// public users, so the old ALPN is not supported.
pub const ALPN: &[u8] = b"phase/mc/1";

/// Protocol version. Host and guest must match.
pub const PROTOCOL_VERSION: u32 = 1;

/// Program version (from Cargo.toml), for diagnostics.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Maximum control message size.
const MAX_CONTROL_MSG: usize = 64 * 1024;

/// Echo probe frame size: sequence number (u64) + send time in microseconds (u64).
pub const ECHO_FRAME_LEN: usize = 16;

/// QUIC connection close and stream reset codes.
pub mod code {
    /// Normal shutdown.
    pub const NORMAL: u32 = 0;
    /// Protocol error (unknown stream type, version mismatch).
    pub const BAD_PROTOCOL: u32 = 400;
    /// The device is not on the allow list.
    pub const NOT_ALLOWED: u32 = 403;
    /// The host could not connect to the game.
    pub const GAME_UNREACHABLE: u32 = 502;
    /// The local TCP connection broke with an error.
    pub const TCP_ERROR: u32 = 1;
}

/// Reason for an access denial. Sent together with the [`code::NOT_ALLOWED`] code so
/// the guest sees what exactly happened and what to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// The device is not on the allow list, and the host has "let in?" questions turned off.
    NotAllowed,
    /// The host said "no".
    Denied,
    /// Too many devices already wait in line for the host's answer.
    Busy,
    /// The host did not answer the question in time.
    Timeout,
    /// No free slots in the room. `max` is how many players the room holds including
    /// the host (0 means the host did not say).
    RoomFull { max: u32 },
}

/// Prefix of the "room full" reason, followed by the number of slots (`room-full:5`).
const ROOM_FULL: &str = "room-full";

impl RejectReason {
    pub fn to_bytes(self) -> Vec<u8> {
        match self {
            Self::NotAllowed => b"not-allowed".to_vec(),
            Self::Denied => b"denied".to_vec(),
            Self::Busy => b"busy".to_vec(),
            Self::Timeout => b"timeout".to_vec(),
            Self::RoomFull { max } => format!("{ROOM_FULL}:{max}").into_bytes(),
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        match text {
            "not-allowed" => Some(Self::NotAllowed),
            "denied" => Some(Self::Denied),
            "busy" => Some(Self::Busy),
            "timeout" => Some(Self::Timeout),
            _ => {
                let rest = text.strip_prefix(ROOM_FULL)?;
                // The number of slots is optional: `room-full` without it is a clear reason too.
                let max = match rest.strip_prefix(':') {
                    Some(number) => number.parse().ok()?,
                    None if rest.is_empty() => 0,
                    None => return None,
                };
                Some(Self::RoomFull { max })
            }
        }
    }

    /// Explanation for the guest.
    pub fn explain(self) -> String {
        match self {
            Self::NotAllowed => tr!(
                "этого устройства нет в списке разрешённых у хоста, а новые устройства он не принимает. Попросите хоста разрешить ваш ID (в консольной версии: host --allow <ID>)",
                "this device is not on the host's allowed list, and the host is not accepting new devices. Ask the host to allow your ID (in the console version: host --allow <ID>)"
            )
            .into(),
            Self::Denied => tr!("хост ответил «нет»", "the host said no").into(),
            Self::Busy => tr!(
                "у хоста уже длинная очередь из желающих войти. Попробуйте ещё раз через пару минут",
                "the host already has a long line of players waiting to get in. Try again in a couple of minutes"
            )
            .into(),
            Self::Timeout => tr!(
                "хост не ответил на вопрос «пустить?» вовремя",
                "the host did not answer the \"let them in?\" prompt in time"
            )
            .into(),
            Self::RoomFull { max: 0 } => {
                tr!("комната заполнена: свободных мест нет", "the room is full: no free slots").into()
            }
            Self::RoomFull { max } => tr!(
                format!("комната заполнена: {max} из {max}"),
                format!("the room is full: {max} of {max}")
            ),
        }
    }
}

/// Stream type (the first byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StreamKind {
    Control = 1,
    Tcp = 2,
    Echo = 3,
    Mods = 4,
    Look = 5,
}

impl StreamKind {
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Control),
            2 => Some(Self::Tcp),
            3 => Some(Self::Echo),
            4 => Some(Self::Mods),
            5 => Some(Self::Look),
            _ => None,
        }
    }
}

/// The guest's first message on the control stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    pub app_version: String,
    /// The guest's game nickname. Optional: old program versions do not send it.
    /// Read it through [`Hello::nickname`], which sanitizes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    /// Guest rights (Phase Pro): a token signed by the Phase server. The tunnel does not
    /// parse it; the host checks it ([`crate::host::RoomService::hello`]). The field
    /// is optional: without it the guest counts as Phase Free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entitlement: Option<String>,
}

/// Maximum length of the rights token in the hello.
pub const ENTITLEMENT_MAX_LEN: usize = 2048;

impl Hello {
    /// A message of the current program version with the guest's nickname (if set).
    pub fn current(nickname: Option<&str>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            app_version: APP_VERSION.to_string(),
            nickname: nickname.and_then(sanitize_nickname),
            entitlement: None,
        }
    }

    /// The same with a rights token (a token that is too long is not sent).
    pub fn with_entitlement(mut self, token: Option<&str>) -> Self {
        self.entitlement = token
            .filter(|t| !t.is_empty() && t.len() <= ENTITLEMENT_MAX_LEN)
            .map(str::to_string);
        self
    }

    /// The rights token, if sent and not longer than [`ENTITLEMENT_MAX_LEN`].
    pub fn entitlement(&self) -> Option<&str> {
        self.entitlement
            .as_deref()
            .filter(|t| !t.is_empty() && t.len() <= ENTITLEMENT_MAX_LEN)
    }

    /// The guest's nickname, safe to show (no control characters, at most
    /// [`NICKNAME_MAX_CHARS`]). `None` means no nickname was sent or it is empty.
    pub fn nickname(&self) -> Option<String> {
        self.nickname.as_deref().and_then(sanitize_nickname)
    }
}

/// The host's answer: what it shares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    pub protocol_version: u32,
    pub app_version: String,
    /// Game port on the host's PC. `None` means game forwarding is off (diagnostics only).
    pub game_port: Option<u16>,
    /// World name to show the guest.
    pub motd: String,
    /// Voice mod UDP ports on the host's PC. The guest opens the same ones locally.
    #[serde(default)]
    pub udp_ports: Vec<u16>,
    /// The host's game version from the status request (for example `1.20.1`), if known.
    /// Optional: old program versions do not send it.
    #[serde(default)]
    pub game_version: Option<String>,
    /// The host's game nickname. Optional: old program versions do not send it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    /// Mod share. `None` means the host runs an old program version: it has no mods
    /// stream ([`StreamKind::Mods`]) and it must not be opened. A newer host
    /// always sends the field, even when the share is off (`files == 0`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mods: Option<ModsOffer>,
    /// Room: slots and the host's plan. `None` means the host's program version has no plans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<RoomInfo>,
}

/// What the guest knows about the room.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomInfo {
    /// How many players the room holds including the host (0 means no limit).
    pub max_players: u32,
    /// The host has Phase Pro: the room is on the priority relay, the host sets the styling.
    #[serde(default)]
    pub pro: bool,
    /// The host understands the styling stream ([`StreamKind::Look`]). An older host lacks this
    /// field, and such a stream must not be opened on it.
    #[serde(default)]
    pub look: bool,
}

/// The guest asks about its in-game styling and can toggle the silhouette glow
/// **on itself**: the guest never affects other players.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookRequest {
    /// `Some` turns its own glow on or off; `None` only asks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glow: Option<bool>,
}

/// How the guest looks in the host's world. The host decides everything: the guest only shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookState {
    /// In-game styling is on at the host.
    #[serde(default)]
    pub enabled: bool,
    /// The nickname the guest's game joined the world under (`None` means the guest has not joined yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_name: Option<String>,
    /// Nickname color: a Phase palette color ID (`ice_sapphire`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Prefix without brackets (`VIP`, `PRO`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// The guest's silhouette glows through walls.
    #[serde(default)]
    pub glow: bool,
    /// The guest may toggle its own glow.
    #[serde(default)]
    pub can_glow: bool,
}

/// Maximum length of [`LookState`] text fields after sanitizing.
const LOOK_TEXT_MAX_CHARS: usize = 32;

impl LookState {
    /// Text from the host is shown in the guest's window, so sanitize it.
    pub fn sanitized(mut self) -> Self {
        let clean = |text: Option<String>| {
            text.map(|t| sanitize_text(&t, LOOK_TEXT_MAX_CHARS))
                .filter(|t| !t.is_empty())
        };
        self.game_name = clean(self.game_name);
        self.color = clean(self.color);
        self.prefix = clean(self.prefix);
        self
    }
}

/// A short summary of the room's mods. The guest requests the list itself ([`crate::mods::ModManifest`])
/// on a separate stream: for a big modpack it does not fit in a control message, and an older
/// guest must not get an answer it cannot read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModsOffer {
    /// How many mods the host shares (0 means no mods are required).
    pub files: u32,
    /// Their total size, in bytes.
    pub total_size: u64,
}

pub async fn write_kind<W: AsyncWrite + Unpin>(w: &mut W, kind: StreamKind) -> Result<()> {
    w.write_all(&[kind as u8]).await.context(tr!(
        "не удалось отправить тип потока",
        "could not send the stream type"
    ))
}

pub async fn read_kind<R: AsyncRead + Unpin>(r: &mut R) -> Result<StreamKind> {
    let byte = r.read_u8().await.context(tr!(
        "не удалось прочитать тип потока",
        "could not read the stream type"
    ))?;
    StreamKind::from_byte(byte).with_context(|| {
        tr!(
            format!("неизвестный тип потока: {byte}"),
            format!("unknown stream type: {byte}")
        )
    })
}

/// Maximum length of the world name shown to the guest.
pub const MOTD_MAX_CHARS: usize = 64;

/// Makes text from the other side safe to print: removes control characters
/// (including ESC, which can spoof console output) and trims the length.
pub fn sanitize_text(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect()
}

/// Maximum game nickname length.
pub const NICKNAME_MAX_CHARS: usize = 24;

/// Invisible characters that can fake a nickname's look: writing direction overrides
/// (the nickname would read backwards), zero-width spaces, the byte order mark.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
    )
}

/// Brings a game nickname to a safe form: no control or invisible characters, any
/// whitespace becomes a plain space (runs collapse to one), no leading or trailing spaces, at most
/// [`NICKNAME_MAX_CHARS`] characters. `None` means nothing is left after sanitizing.
///
/// Both our own nickname before sending and **any** nickname received over the network or
/// read from a file are sanitized this way.
pub fn sanitize_nickname(text: &str) -> Option<String> {
    let full = normalize_nickname(text);
    let cut: String = full.chars().take(NICKNAME_MAX_CHARS).collect();
    // Truncation may have landed on a space between words.
    let cut = cut.trim_end();
    (!cut.is_empty()).then(|| cut.to_string())
}

/// Same as [`sanitize_nickname`], but without length truncation: to tell
/// the user "the nickname is too long" instead of silently shortening what they typed.
pub fn normalize_nickname(text: &str) -> String {
    let visible: String = text
        .chars()
        .filter(|&c| !is_invisible(c) && (!c.is_control() || c.is_whitespace()))
        .collect();
    visible.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Short form of a device ID for the interface and the log: `#` and the first 8 characters.
pub fn short_id(id: &impl std::fmt::Display) -> String {
    let full = id.to_string();
    format!("#{}", full.chars().take(8).collect::<String>())
}

/// How to name a device in the log: `Nick (#1603817a)` or just `#1603817a`.
pub fn peer_label(id: &impl std::fmt::Display, nickname: Option<&str>) -> String {
    match nickname {
        Some(nick) => format!("{nick} ({})", short_id(id)),
        None => short_id(id),
    }
}

pub async fn write_msg<W, T>(w: &mut W, msg: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    write_msg_limited(w, msg, MAX_CONTROL_MSG).await
}

pub async fn read_msg<R, T>(r: &mut R) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    read_msg_limited(r, MAX_CONTROL_MSG).await
}

/// Like [`write_msg`], but with its own size limit (the mod list is bigger than ordinary
/// control messages).
pub async fn write_msg_limited<W, T>(w: &mut W, msg: &T, max_len: usize) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let data = serde_json::to_vec(msg)?;
    if data.len() > max_len {
        let len = data.len();
        bail!(tr!(
            format!("служебное сообщение слишком большое: {len} байт"),
            format!("control message is too large: {len} bytes")
        ));
    }
    let len = u32::try_from(data.len())?;
    w.write_u32(len).await?;
    w.write_all(&data).await?;
    Ok(())
}

/// Like [`read_msg`], but with its own size limit.
pub async fn read_msg_limited<R, T>(r: &mut R, max_len: usize) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let len = r.read_u32().await.context(tr!(
        "не удалось прочитать длину сообщения",
        "could not read the message length"
    ))? as usize;
    if len > max_len {
        bail!(tr!(
            format!("служебное сообщение слишком большое: {len} байт"),
            format!("control message is too large: {len} bytes")
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await.context(tr!(
        "не удалось прочитать сообщение",
        "could not read the message"
    ))?;
    serde_json::from_slice(&buf).context(tr!(
        "повреждённое служебное сообщение",
        "corrupted control message"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_info_from_older_host_without_new_fields() {
        // This is how a host answers in versions before voice ports and the game version.
        let old = r#"{"protocol_version":1,"app_version":"0.1.0","game_port":51234,"motd":"Мир"}"#;
        let info: HostInfo = serde_json::from_str(old).unwrap();
        assert_eq!(info.game_port, Some(51234));
        assert!(info.udp_ports.is_empty());
        assert_eq!(info.game_version, None);
        assert_eq!(info.nickname, None);
        assert_eq!(info.mods, None, "a host without a mod share");
    }

    #[test]
    fn mods_offer_is_optional_both_ways() {
        let mut info = HostInfo {
            protocol_version: PROTOCOL_VERSION,
            app_version: APP_VERSION.into(),
            game_port: Some(25565),
            motd: "Мир".into(),
            udp_ports: Vec::new(),
            game_version: None,
            nickname: None,
            mods: None,
            room: None,
        };
        // Without a share the answer is the same as in the version without mods.
        assert!(!serde_json::to_string(&info).unwrap().contains("mods"));

        info.mods = Some(ModsOffer {
            files: 42,
            total_size: 123_456_789,
        });
        let json = serde_json::to_string(&info).unwrap();
        assert!(
            json.contains(r#""mods":{"files":42,"total_size":123456789}"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<HostInfo>(&json).unwrap(), info);

        // An older guest reads the answer with a struct lacking the mods field, and does not break.
        #[derive(Deserialize)]
        struct OldHostInfo {
            game_port: Option<u16>,
            #[serde(default)]
            nickname: Option<String>,
        }
        let old: OldHostInfo = serde_json::from_str(&json).unwrap();
        assert_eq!((old.game_port, old.nickname), (Some(25565), None));
        assert_eq!(StreamKind::from_byte(4), Some(StreamKind::Mods));
    }

    #[test]
    fn hello_without_nickname_matches_older_versions() {
        // An older guest: there is no nickname field at all.
        let old = r#"{"protocol_version":1,"app_version":"0.1.0"}"#;
        let hello: Hello = serde_json::from_str(old).unwrap();
        assert_eq!(hello.nickname, None);
        assert_eq!(hello.nickname(), None);

        // Without a nickname the message is byte for byte the same as in the old version.
        let json = serde_json::to_string(&Hello::current(None)).unwrap();
        assert_eq!(
            json,
            format!(r#"{{"protocol_version":{PROTOCOL_VERSION},"app_version":"{APP_VERSION}"}}"#)
        );
    }

    #[test]
    fn hello_with_nickname_roundtrips_and_older_versions_ignore_the_field() {
        let hello = Hello::current(Some("  GrayIsko1337 "));
        assert_eq!(hello.nickname.as_deref(), Some("GrayIsko1337"));
        let json = serde_json::to_string(&hello).unwrap();
        assert!(json.contains(r#""nickname":"GrayIsko1337""#), "{json}");
        assert_eq!(serde_json::from_str::<Hello>(&json).unwrap(), hello);

        // An old program version reads the same message with a struct lacking the nickname field.
        #[derive(Deserialize)]
        struct OldHello {
            protocol_version: u32,
            app_version: String,
        }
        let old: OldHello = serde_json::from_str(&json).unwrap();
        assert_eq!(old.protocol_version, PROTOCOL_VERSION);
        assert_eq!(old.app_version, APP_VERSION);
    }

    #[test]
    fn nickname_from_the_network_is_cleaned_on_read() {
        // A foreign program can send anything: only a clean nickname reaches the interface.
        let raw = serde_json::json!({
            "protocol_version": 1,
            "app_version": "x",
            "nickname": "\u{1b}[2J  Злой\u{202e}\nГость   с очень длинным ником 1234567890",
        })
        .to_string();
        let hello: Hello = serde_json::from_str(&raw).unwrap();
        let nick = hello.nickname().unwrap();
        assert_eq!(nick, "[2J Злой Гость с очень д");
        assert_eq!(nick.chars().count(), NICKNAME_MAX_CHARS);

        let blank = serde_json::json!({
            "protocol_version": 1,
            "app_version": "x",
            "nickname": " \t\u{200b} ",
        })
        .to_string();
        assert_eq!(
            serde_json::from_str::<Hello>(&blank).unwrap().nickname(),
            None
        );
    }

    #[test]
    fn host_info_nickname_is_optional_both_ways() {
        let mut info = HostInfo {
            protocol_version: PROTOCOL_VERSION,
            app_version: APP_VERSION.into(),
            game_port: Some(25565),
            motd: "Мир".into(),
            udp_ports: Vec::new(),
            game_version: None,
            nickname: None,
            mods: None,
            room: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains("nickname"), "{json}");

        info.nickname = Some("HostNick".into());
        let json = serde_json::to_string(&info).unwrap();
        assert_eq!(serde_json::from_str::<HostInfo>(&json).unwrap(), info);

        // An older host reads the answer with a struct lacking the nickname field.
        #[derive(Deserialize)]
        struct OldHostInfo {
            motd: String,
        }
        assert_eq!(
            serde_json::from_str::<OldHostInfo>(&json).unwrap().motd,
            "Мир"
        );
    }

    #[test]
    fn sanitize_nickname_rules() {
        assert_eq!(sanitize_nickname("Steve").as_deref(), Some("Steve"));
        assert_eq!(sanitize_nickname("  Alex_01  ").as_deref(), Some("Alex_01"));
        assert_eq!(
            sanitize_nickname("Серый   Волк").as_deref(),
            Some("Серый Волк")
        );
        assert_eq!(sanitize_nickname("a\tb\r\nc").as_deref(), Some("a b c"));
        assert_eq!(sanitize_nickname("a\u{7}b\u{1b}c").as_deref(), Some("abc"));
        assert_eq!(sanitize_nickname(""), None);
        assert_eq!(sanitize_nickname(" \n\u{feff}"), None);
        // Exactly 24 characters pass, the 25th is dropped; no trailing space remains.
        assert_eq!(sanitize_nickname(&"x".repeat(24)).unwrap().len(), 24);
        assert_eq!(sanitize_nickname(&"x".repeat(40)).unwrap().len(), 24);
        assert_eq!(
            sanitize_nickname(&format!("{} y", "x".repeat(23))),
            Some("x".repeat(23))
        );
    }

    #[test]
    fn short_id_and_label() {
        let id = "1603817af6c0ffee";
        assert_eq!(short_id(&id), "#1603817a");
        assert_eq!(
            peer_label(&id, Some("GrayIsko1337")),
            "GrayIsko1337 (#1603817a)"
        );
        assert_eq!(peer_label(&id, None), "#1603817a");
    }

    #[tokio::test]
    async fn message_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let info = HostInfo {
            protocol_version: PROTOCOL_VERSION,
            app_version: APP_VERSION.into(),
            game_port: Some(51234),
            motd: "Мой мир".into(),
            udp_ports: vec![51234, 24454],
            game_version: Some("1.20.1".into()),
            nickname: Some("Хозяин".into()),
            mods: Some(ModsOffer {
                files: 3,
                total_size: 4096,
            }),
            room: Some(RoomInfo {
                max_players: 5,
                pro: false,
                look: true,
            }),
        };
        write_kind(&mut a, StreamKind::Control).await.unwrap();
        write_msg(&mut a, &info).await.unwrap();
        assert_eq!(read_kind(&mut b).await.unwrap(), StreamKind::Control);
        let got: HostInfo = read_msg(&mut b).await.unwrap();
        assert_eq!(got, info);
    }

    #[test]
    fn reject_reasons_roundtrip_and_room_full_carries_the_limit() {
        for reason in [
            RejectReason::NotAllowed,
            RejectReason::Denied,
            RejectReason::Busy,
            RejectReason::Timeout,
            RejectReason::RoomFull { max: 5 },
            RejectReason::RoomFull { max: 20 },
        ] {
            assert_eq!(RejectReason::from_bytes(&reason.to_bytes()), Some(reason));
        }
        assert_eq!(RejectReason::RoomFull { max: 5 }.to_bytes(), b"room-full:5");
        assert_eq!(
            RejectReason::from_bytes(b"room-full"),
            Some(RejectReason::RoomFull { max: 0 })
        );
        assert_eq!(RejectReason::from_bytes(b"room-full:x"), None);
        assert_eq!(RejectReason::from_bytes(b"room-fuller"), None);
        assert_eq!(RejectReason::from_bytes(b"\xff"), None);
        assert_eq!(
            RejectReason::RoomFull { max: 5 }.explain(),
            "комната заполнена: 5 из 5"
        );
    }

    #[test]
    fn entitlement_and_room_are_optional_both_ways() {
        // Without a token the hello is byte for byte the same as before (see hello_without_nickname_…).
        let hello = Hello::current(Some("Steve")).with_entitlement(Some("phase1.aa.bb"));
        let json = serde_json::to_string(&hello).unwrap();
        assert!(json.contains(r#""entitlement":"phase1.aa.bb""#), "{json}");
        assert_eq!(serde_json::from_str::<Hello>(&json).unwrap(), hello);
        // A token that is too long is neither sent nor read.
        let long = "x".repeat(ENTITLEMENT_MAX_LEN + 1);
        assert_eq!(
            Hello::current(None)
                .with_entitlement(Some(&long))
                .entitlement,
            None
        );
        let raw =
            serde_json::json!({ "protocol_version": 1, "app_version": "x", "entitlement": long });
        let hello: Hello = serde_json::from_value(raw).unwrap();
        assert_eq!(hello.entitlement(), None);

        // An older guest: the room field does not bother it; an older host does not send it.
        let old = r#"{"protocol_version":1,"app_version":"0.1.0","game_port":1,"motd":"Мир"}"#;
        assert_eq!(serde_json::from_str::<HostInfo>(old).unwrap().room, None);
    }

    #[test]
    fn sanitize_removes_control_chars_and_limits_length() {
        assert_eq!(sanitize_text("Мой\x1b[2J мир\n\u{9b}", 64), "Мой[2J мир");
        assert_eq!(sanitize_text(&"а".repeat(100), 5), "ааааа");
    }

    #[tokio::test]
    async fn rejects_unknown_kind_and_oversized_message() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(&[42]).await.unwrap();
        assert!(read_kind(&mut b).await.is_err());

        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_u32((MAX_CONTROL_MSG + 1) as u32).await.unwrap();
        assert!(read_msg::<_, Hello>(&mut b).await.is_err());
    }
}
