//! Minecraft server status query (Server List Ping): the same thing the "Multiplayer" screen
//! does when it shows the version, name and player count.
//!
//! The host needs it to make sure that a Minecraft server really runs on the found port
//! (and not another program), and to learn the world's version.
//!
//! Two protocol variants:
//! - **modern** (1.7 and newer): a Handshake packet (`0x00`, next state is
//!   "status") + Status Request (`0x00`); the answer is JSON;
//! - **legacy** (1.4–1.6): bytes `FE 01`; the answer is an `FF` packet with a UTF-16 string.
//!   1.7+ servers understand it too, but we only try it if the modern one did not work.
//!
//! The query changes nothing on the server and does not require signing in to an account.

use phase_lang::tr;
use std::{net::SocketAddr, time::Duration};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

/// How long to wait for an answer to one query variant.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);
/// Maximum response size. It may contain the server icon and a mod list (Forge),
/// but not more than a couple of megabytes.
const MAX_PACKET: usize = 2 * 1024 * 1024;
/// Protocol version in the query: −1 means "find out the server's version" (the convention for a status query).
const PROBE_PROTOCOL: i32 = -1;
const MAX_TEXT_CHARS: usize = 128;

/// Server status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerStatus {
    /// Game version, for example `1.20.1` (as the server reports it; modded servers
    /// may have text like `Paper 1.20.1`).
    pub version: String,
    /// Game protocol number (for example 763 for 1.20.1).
    pub protocol: Option<i64>,
    /// Server description (MOTD) without color codes.
    pub motd: String,
    pub players_online: Option<i64>,
    pub players_max: Option<i64>,
    /// Only the legacy protocol variant answered (versions before 1.7).
    pub legacy: bool,
}

impl ServerStatus {
    /// Short form for output: "1.20.1 · players 1/8".
    pub fn summary(&self) -> String {
        let mut text = if self.version.is_empty() {
            tr!("версия неизвестна", "version unknown").to_string()
        } else {
            self.version.clone()
        };
        if let (Some(online), Some(max)) = (self.players_online, self.players_max) {
            text.push_str(&tr!(
                format!(" · игроков {online}/{max}"),
                format!(" · players {online}/{max}")
            ));
        }
        text
    }
}

/// Requests the status: first the modern way, then the legacy one.
/// An error means the port does not host a Minecraft server (or it did not answer in time).
pub async fn query(addr: SocketAddr, timeout: Duration) -> Result<ServerStatus> {
    match query_modern(addr, timeout).await {
        Ok(status) => Ok(status),
        Err(modern) => query_legacy(addr, timeout).await.map_err(|legacy| {
            anyhow!(tr!(
                format!(
                    "на {addr} не отвечает сервер Minecraft (современный запрос: {modern:#}; \
                     старый запрос: {legacy:#})"
                ),
                format!(
                    "no Minecraft server is responding on {addr} (modern query: {modern:#}; \
                     legacy query: {legacy:#})"
                )
            ))
        }),
    }
}

/// Modern status query (1.7+).
pub async fn query_modern(addr: SocketAddr, timeout: Duration) -> Result<ServerStatus> {
    tokio::time::timeout(timeout, async {
        let mut stream = TcpStream::connect(addr)
            .await
            .context(tr!("нет подключения", "no connection"))?;
        stream.set_nodelay(true)?;
        stream.write_all(&handshake_packet(addr)).await?;
        stream.write_all(&[0x01, 0x00]).await?; // Status Request: length 1, packet id 0
        let json = read_status_response(&mut stream).await?;
        parse_status_json(&json)
    })
    .await
    .context(tr!(
        "сервер не ответил вовремя",
        "the server did not respond in time"
    ))?
}

/// Legacy status query (1.4–1.6).
pub async fn query_legacy(addr: SocketAddr, timeout: Duration) -> Result<ServerStatus> {
    tokio::time::timeout(timeout, async {
        let mut stream = TcpStream::connect(addr)
            .await
            .context(tr!("нет подключения", "no connection"))?;
        stream.write_all(&[0xFE, 0x01]).await?;
        let id = stream
            .read_u8()
            .await
            .context(tr!("нет ответа", "no response"))?;
        ensure!(
            id == 0xFF,
            tr!(
                format!("неожиданный ответ (байт {id:#04x})"),
                format!("unexpected response (byte {id:#04x})")
            )
        );
        let chars = stream.read_u16().await? as usize;
        let mut raw = vec![0u8; chars * 2];
        stream
            .read_exact(&mut raw)
            .await
            .context(tr!("ответ оборван", "the response was cut off"))?;
        let units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_be_bytes(pair))
            .collect();
        parse_legacy(&String::from_utf16_lossy(&units))
    })
    .await
    .context(tr!(
        "сервер не ответил вовремя",
        "the server did not respond in time"
    ))?
}

/// Handshake packet: packet id 0, protocol version, address, port, next state 1.
fn handshake_packet(addr: SocketAddr) -> Vec<u8> {
    let host = addr.ip().to_string();
    let mut body = Vec::with_capacity(32);
    write_varint(&mut body, 0x00);
    write_varint(&mut body, PROBE_PROTOCOL);
    write_varint(&mut body, host.len() as i32);
    body.extend_from_slice(host.as_bytes());
    body.extend_from_slice(&addr.port().to_be_bytes());
    write_varint(&mut body, 1); // 1 is status
    let mut packet = Vec::with_capacity(body.len() + 5);
    write_varint(&mut packet, body.len() as i32);
    packet.extend_from_slice(&body);
    packet
}

/// Reads a Status Response and returns its JSON text.
async fn read_status_response<R: AsyncRead + Unpin>(r: &mut R) -> Result<String> {
    let len = read_varint(r)
        .await
        .context(tr!("нет ответа", "no response"))?;
    let len = usize::try_from(len)
        .map_err(|_| anyhow!(tr!("отрицательная длина пакета", "negative packet length")))?;
    ensure!(
        len > 0 && len <= MAX_PACKET,
        tr!(
            format!("недопустимая длина пакета: {len}"),
            format!("invalid packet length: {len}")
        )
    );
    let mut packet = vec![0u8; len];
    r.read_exact(&mut packet)
        .await
        .context(tr!("ответ оборван", "the response was cut off"))?;
    let mut body = packet.as_slice();
    let id = read_varint(&mut body).await?;
    ensure!(
        id == 0x00,
        tr!(
            format!("неожиданный номер пакета: {id}"),
            format!("unexpected packet ID: {id}")
        )
    );
    let text_len = read_varint(&mut body).await?;
    let text_len = usize::try_from(text_len)
        .map_err(|_| anyhow!(tr!("отрицательная длина строки", "negative string length")))?;
    ensure!(
        text_len <= body.len(),
        tr!(
            "строка длиннее пакета",
            "the string is longer than the packet"
        )
    );
    String::from_utf8(body[..text_len].to_vec())
        .context(tr!("ответ не в UTF-8", "the response is not UTF-8"))
}

fn parse_status_json(json: &str) -> Result<ServerStatus> {
    let value: Value =
        serde_json::from_str(json).context(tr!("ответ — не JSON", "the response is not JSON"))?;
    let version = value
        .get("version")
        .context(tr!("в ответе нет версии", "the response has no version"))?;
    let name = version.get("name").and_then(Value::as_str).context(tr!(
        "в ответе нет названия версии",
        "the response has no version name"
    ))?;
    let players = value.get("players");
    Ok(ServerStatus {
        version: clean(name),
        protocol: version.get("protocol").and_then(Value::as_i64),
        motd: value
            .get("description")
            .map(|d| clean(&chat_to_plain(d)))
            .unwrap_or_default(),
        players_online: players
            .and_then(|p| p.get("online"))
            .and_then(Value::as_i64),
        players_max: players.and_then(|p| p.get("max")).and_then(Value::as_i64),
        legacy: false,
    })
}

/// Legacy protocol response: `§1\0protocol\0version\0MOTD\0online\0max`
/// (1.4–1.6) or `MOTD§online§max` (before 1.4).
fn parse_legacy(text: &str) -> Result<ServerStatus> {
    if let Some(rest) = text.strip_prefix("§1\0") {
        let parts: Vec<&str> = rest.split('\0').collect();
        if parts.len() < 5 {
            bail!(tr!(
                "неполный ответ старого протокола",
                "incomplete response from the legacy protocol"
            ));
        }
        return Ok(ServerStatus {
            version: clean(parts[1]),
            protocol: parts[0].parse().ok(),
            motd: clean(parts[2]),
            players_online: parts[3].parse().ok(),
            players_max: parts[4].parse().ok(),
            legacy: true,
        });
    }
    let parts: Vec<&str> = text.rsplitn(3, '§').collect();
    match parts.as_slice() {
        [max, online, motd] if max.parse::<i64>().is_ok() && online.parse::<i64>().is_ok() => {
            Ok(ServerStatus {
                version: String::new(),
                protocol: None,
                motd: clean(motd),
                players_online: online.parse().ok(),
                players_max: max.parse().ok(),
                legacy: true,
            })
        }
        _ => bail!(tr!(
            "ответ не похож на статус сервера Minecraft",
            "the response does not look like a Minecraft server status"
        )),
    }
}

/// Text from a Minecraft "chat component": a string, a `{text, extra}` object or an array.
pub fn chat_to_plain(value: &Value) -> String {
    let mut out = String::new();
    collect_text(value, &mut out, 0);
    out
}

fn collect_text(value: &Value, out: &mut String, depth: usize) {
    if depth > 16 || out.chars().count() > MAX_TEXT_CHARS {
        return; // guard against overly deep and long responses
    }
    match value {
        Value::String(s) => out.push_str(s),
        Value::Array(items) => items.iter().for_each(|v| collect_text(v, out, depth + 1)),
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                out.push_str(text);
            } else if let Some(key) = map.get("translate").and_then(Value::as_str) {
                out.push_str(key);
            }
            if let Some(extra) = map.get("extra") {
                collect_text(extra, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// Strips color codes (`§` + character), control characters and excess length.
fn clean(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else if c == '\n' {
            out.push(' ');
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out.trim().chars().take(MAX_TEXT_CHARS).collect()
}

fn write_varint(buf: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    loop {
        if v & !0x7F == 0 {
            buf.push(v as u8);
            return;
        }
        buf.push((v as u8 & 0x7F) | 0x80);
        v >>= 7;
    }
}

async fn read_varint<R: AsyncRead + Unpin>(r: &mut R) -> Result<i32> {
    let mut value: u32 = 0;
    for i in 0..5 {
        let byte = r.read_u8().await?;
        value |= u32::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok(value as i32);
        }
    }
    bail!(tr!(
        "слишком длинное число (VarInt)",
        "the number is too long (VarInt)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn varint_roundtrip() {
        for v in [0, 1, 127, 128, 255, 25565, 2_097_151, i32::MAX, -1] {
            let mut buf = Vec::new();
            write_varint(&mut buf, v);
            assert_eq!(read_varint(&mut buf.as_slice()).await.unwrap(), v);
        }
        let mut buf = Vec::new();
        write_varint(&mut buf, -1);
        assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF, 0x0F]);
        // Six bytes with the continuation flag are an error, not a hang.
        assert!(read_varint(&mut [0x80u8; 6].as_slice()).await.is_err());
    }

    #[test]
    fn handshake_matches_protocol() {
        let packet = handshake_packet("127.0.0.1:25565".parse().unwrap());
        let mut expected = vec![0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F, 9];
        expected.extend_from_slice(b"127.0.0.1");
        expected.extend_from_slice(&[0x63, 0xDD, 0x01]);
        assert_eq!(packet[0] as usize, expected.len());
        assert_eq!(&packet[1..], expected.as_slice());
    }

    #[test]
    fn parses_modern_json() {
        let json = r#"{"version":{"name":"1.20.1","protocol":763},
            "players":{"max":8,"online":1},
            "description":{"text":"","extra":[{"text":"Steve - "},{"text":"§aМой мир"}]}}"#;
        let status = parse_status_json(json).unwrap();
        assert_eq!(status.version, "1.20.1");
        assert_eq!(status.protocol, Some(763));
        assert_eq!(status.motd, "Steve - Мой мир");
        assert_eq!(
            (status.players_online, status.players_max),
            (Some(1), Some(8))
        );
        assert_eq!(status.summary(), "1.20.1 · игроков 1/8");

        let plain =
            parse_status_json(r#"{"version":{"name":"1.8.9","protocol":47},"description":"Hi"}"#)
                .unwrap();
        assert_eq!(plain.motd, "Hi");
        assert!(parse_status_json(r#"{"hello":"world"}"#).is_err());
        assert!(parse_status_json("<html>").is_err());
    }

    #[test]
    fn parses_legacy_answers() {
        let status = parse_legacy("§1\x0074\x001.6.4\x00A Minecraft Server\x002\x0020").unwrap();
        assert_eq!(status.version, "1.6.4");
        assert_eq!(status.protocol, Some(74));
        assert_eq!(
            (status.players_online, status.players_max),
            (Some(2), Some(20))
        );
        assert!(status.legacy);

        let old = parse_legacy("Old server§3§10").unwrap();
        assert_eq!(old.motd, "Old server");
        assert_eq!(old.players_max, Some(10));
        assert!(parse_legacy("HTTP/1.1 400 Bad Request").is_err());
    }

    #[test]
    fn clean_strips_colors_and_controls() {
        assert_eq!(clean("§l§cКрасный\x1b[2J\nмир"), "Красный[2J мир");
    }
}
