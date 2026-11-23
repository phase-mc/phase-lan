//! The first two packets of a game connection: `Handshake` and `Login Start`.
//!
//! They come **before** encryption and compression, and their format has been stable since 1.7. From them the host
//! learns which real in-game nickname the guest's device joined with: the nickname in the Phase greeting
//! is chosen by the player, while this one is what the game server sees.
//!
//! This module only **reads** the start of the stream. Bytes go to the game unchanged; after that the stream
//! is encrypted between the guest's game and the host's server, and Phase does not look into it.
//!
//! ```text
//! Handshake:   VarInt length | VarInt id=0 | VarInt protocol | String address | u16 port | VarInt next
//! Login Start: VarInt length | VarInt id=0 | String nickname (≤16) | …version-dependent (UUID)
//! ```
//!
//! `next`: 1 is a status request (server list), 2 is login, 3 is a transfer from another server
//! (1.20.5+). Forge appends a `\0FML…\0` marker to the address; the address is not needed here and
//! is skipped entirely.

/// The two packets never take more than this (in 1.19 `Login Start` also carried a key signature,
/// about 840 bytes; the nickname is at the start of the packet and is read earlier).
pub const SNIFF_MAX: usize = 1024;

/// From this protocol version (1.20.2) the UUID in `Login Start` is mandatory.
const PROTOCOL_UUID_REQUIRED: i32 = 764;
/// In 1.19.3–1.20.1 the UUID is optional: a flag precedes it.
const PROTOCOL_UUID_OPTIONAL: i32 = 761;
/// Snapshot flag in the protocol number.
const SNAPSHOT_BIT: i32 = 0x4000_0000;
/// Maximum nickname length in the game.
const NAME_MAX_CHARS: usize = 16;

/// A game login read from the start of the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameLogin {
    /// Nickname the game joins the server with.
    pub name: String,
    /// Player UUID (`None` means the game version does not send it).
    pub uuid: Option<String>,
    /// Protocol number of the guest's game (763 is 1.20.1).
    pub protocol: i32,
}

/// What is visible at the start of the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sniff {
    /// Not enough bytes yet: read more and ask again.
    NeedMore,
    /// Not a game login: a status request, an old ping, or not Minecraft at all.
    NotLogin,
    /// The game is joining a server.
    Login(GameLogin),
}

/// The nickname is valid for game commands: Latin letters, digits and `_`, 1 to 16 characters. Nicknames with
/// other characters (seen with unofficial launchers) are not put into commands.
pub fn is_valid_name(name: &str) -> bool {
    (1..=NAME_MAX_CHARS).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Reading failed: not enough bytes, or the data does not look like the protocol.
#[derive(Debug)]
enum Stop {
    Short,
    Bad,
}

struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    fn left(&self) -> usize {
        self.buf.len() - self.at
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], Stop> {
        if self.left() < len {
            return Err(Stop::Short);
        }
        let bytes = &self.buf[self.at..self.at + len];
        self.at += len;
        Ok(bytes)
    }

    /// VarInt: up to five bytes, seven bits each, least significant first.
    fn varint(&mut self) -> Result<i32, Stop> {
        let mut value = 0u32;
        for shift in (0..35).step_by(7) {
            let byte = self.take(1)?[0];
            value |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value as i32);
            }
        }
        Err(Stop::Bad)
    }

    /// Length as a non-negative number not greater than `max`.
    fn length(&mut self, max: usize) -> Result<usize, Stop> {
        match usize::try_from(self.varint()?) {
            Ok(len) if len <= max => Ok(len),
            _ => Err(Stop::Bad),
        }
    }
}

/// Parses the start of the game stream. Called again with a larger buffer until it
/// returns something other than [`Sniff::NeedMore`]; there is no point reading more than [`SNIFF_MAX`] bytes.
pub fn sniff(buf: &[u8]) -> Sniff {
    // A ping from versions before 1.7 starts with 0xFE and has nothing to do with login.
    if buf.first() == Some(&0xFE) {
        return Sniff::NotLogin;
    }
    match parse(buf) {
        Ok(login) => login.map_or(Sniff::NotLogin, Sniff::Login),
        Err(Stop::Short) if buf.len() < SNIFF_MAX => Sniff::NeedMore,
        Err(_) => Sniff::NotLogin,
    }
}

fn parse(buf: &[u8]) -> Result<Option<GameLogin>, Stop> {
    let mut stream = Reader::new(buf);

    // The whole Handshake is needed: its end says what the game is going to do next.
    let len = stream.length(SNIFF_MAX)?;
    if len == 0 {
        return Err(Stop::Bad);
    }
    if stream.left() < len {
        // The packet has not fully arrived, but its number is already visible: for Handshake it is 0. Any other
        // start (for example HTTP) is not Minecraft, and there is no point reading more.
        return Err(match buf.get(stream.at) {
            Some(&id) if id != 0 => Stop::Bad,
            _ => Stop::Short,
        });
    }
    let mut packet = Reader::new(stream.take(len)?);
    let complete = |result: Result<i32, Stop>| result.map_err(|_| Stop::Bad);
    if complete(packet.varint())? != 0 {
        return Err(Stop::Bad);
    }
    let protocol = complete(packet.varint())?;
    let address = packet.length(SNIFF_MAX).map_err(|_| Stop::Bad)?;
    packet.take(address).map_err(|_| Stop::Bad)?;
    packet.take(2).map_err(|_| Stop::Bad)?;
    let next_state = complete(packet.varint())?;
    if !matches!(next_state, 2 | 3) {
        return Ok(None);
    }

    // Login Start: the nickname is at the start, the rest depends on the version.
    let len = stream.length(SNIFF_MAX)?;
    let body_at = stream.at;
    // The packet has fully arrived: a lack of bytes inside it means "this field is absent", not "read more".
    let whole = stream.left() >= len;
    if stream.varint()? != 0 {
        return Err(Stop::Bad);
    }
    let name_len = stream.length(NAME_MAX_CHARS * 4)?;
    let Ok(name) = std::str::from_utf8(stream.take(name_len)?) else {
        return Err(Stop::Bad);
    };
    if name.is_empty() || name.chars().count() > NAME_MAX_CHARS || name.contains(char::is_control) {
        return Err(Stop::Bad);
    }
    let uuid = match read_uuid(&mut stream, protocol) {
        Ok(uuid) => uuid,
        Err(Stop::Short) if !whole || stream.at - body_at < len => {
            if whole {
                None
            } else {
                return Err(Stop::Short);
            }
        }
        Err(_) => None,
    };
    Ok(Some(GameLogin {
        name: name.to_string(),
        uuid,
        protocol,
    }))
}

fn read_uuid(stream: &mut Reader<'_>, protocol: i32) -> Result<Option<String>, Stop> {
    let modern = protocol >= PROTOCOL_UUID_REQUIRED || protocol & SNAPSHOT_BIT != 0;
    if !modern {
        if protocol < PROTOCOL_UUID_OPTIONAL {
            // Before 1.19.3 this packet has no UUID (in 1.19–1.19.2 it followed the key signature;
            // we do not read that far: the nickname is enough).
            return Ok(None);
        }
        if stream.take(1)?[0] == 0 {
            return Ok(None);
        }
    }
    let bytes = stream.take(16)?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )))
}

fn write_varint(out: &mut Vec<u8>, value: usize) {
    let mut value = value as u32;
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Disconnect packet in the login phase (Login Disconnect, number 0): the guest's game closes
/// the connection and shows `text` on screen. The format is the same in all versions since 1.7:
/// the packet comes before compression and encryption, the reason is a text component in a JSON string.
pub fn login_disconnect(text: &str) -> Vec<u8> {
    let json = serde_json::json!({ "text": text }).to_string();
    let mut body = vec![0x00];
    write_varint(&mut body, json.len());
    body.extend_from_slice(json.as_bytes());
    let mut packet = Vec::with_capacity(body.len() + 3);
    write_varint(&mut packet, body.len());
    packet.extend(body);
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_disconnect_packet() {
        let packet = login_disconnect("Ник «Steve» уже занят");
        let json = r#"{"text":"Ник «Steve» уже занят"}"#;
        let mut reader = Reader::new(&packet);
        assert_eq!(
            reader.varint().unwrap() as usize,
            packet.len() - 1,
            "packet length"
        );
        assert_eq!(reader.varint().unwrap(), 0, "packet number");
        assert_eq!(reader.varint().unwrap() as usize, json.len());
        assert_eq!(reader.take(json.len()).unwrap(), json.as_bytes());
        assert_eq!(reader.left(), 0);
        // Long reason: lengths take two VarInt bytes (body 314 bytes = 0xBA 0x02).
        let long = login_disconnect(&"x".repeat(300));
        assert_eq!(long[0..2], [0xBA, 0x02]);
        let mut reader = Reader::new(&long);
        assert_eq!(reader.varint().unwrap() as usize, long.len() - 2);
        // JSON escapes quotes and line breaks.
        let tricky = login_disconnect("a\"b\nc");
        assert!(String::from_utf8_lossy(&tricky).contains(r#""a\"b\nc""#));
    }

    fn varint(mut value: u32) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }

    fn packet(body: &[u8]) -> Vec<u8> {
        [varint(body.len() as u32), body.to_vec()].concat()
    }

    fn string(text: &str) -> Vec<u8> {
        [varint(text.len() as u32), text.as_bytes().to_vec()].concat()
    }

    fn handshake(protocol: u32, address: &str, next: u32) -> Vec<u8> {
        packet(
            &[
                varint(0),
                varint(protocol),
                string(address),
                25565u16.to_be_bytes().to_vec(),
                varint(next),
            ]
            .concat(),
        )
    }

    const UUID: [u8; 16] = [
        0x06, 0x9a, 0x79, 0xf4, 0x44, 0xe9, 0x47, 0x26, 0xa5, 0xbe, 0xfc, 0xa9, 0x0e, 0x38, 0xaa,
        0xf5,
    ];
    const UUID_TEXT: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";

    fn login(body_after_name: &[u8], name: &str) -> Vec<u8> {
        packet(&[varint(0), string(name), body_after_name.to_vec()].concat())
    }

    fn expect_login(bytes: &[u8]) -> GameLogin {
        match sniff(bytes) {
            Sniff::Login(login) => login,
            other => panic!("expected a login, got {other:?}"),
        }
    }

    #[test]
    fn recorded_1_20_1_login() {
        // The game 1.20.1 starts a connection to localhost:25565 as Steve like this.
        let mut bytes = vec![
            0x10, 0x00, 0xfb, 0x05, 0x09, b'l', b'o', b'c', b'a', b'l', b'h', b'o', b's', b't',
            0x63, 0xdd, 0x02,
        ];
        bytes.extend([0x18, 0x00, 0x05, b'S', b't', b'e', b'v', b'e', 0x01]);
        bytes.extend(UUID);
        let login = expect_login(&bytes);
        assert_eq!(login.name, "Steve");
        assert_eq!(login.uuid.as_deref(), Some(UUID_TEXT));
        assert_eq!(login.protocol, 763);
    }

    #[test]
    fn versions_from_1_16_to_modern() {
        // 1.16.5 (754): nickname only.
        let old = [handshake(754, "127.0.0.1", 2), login(&[], "Alex_01")].concat();
        assert_eq!(
            expect_login(&old),
            GameLogin {
                name: "Alex_01".into(),
                uuid: None,
                protocol: 754
            }
        );

        // 1.19.3–1.20.1 without UUID (flag 0).
        let no_uuid = [handshake(761, "127.0.0.1", 2), login(&[0], "Steve")].concat();
        assert_eq!(expect_login(&no_uuid).uuid, None);

        // 1.21.x (767): UUID is mandatory; a login by transfer (next = 3) is a login too.
        for next in [2, 3] {
            let modern = [
                handshake(767, "play.example", next),
                login(&UUID, "GrayIsko1337"),
            ]
            .concat();
            let got = expect_login(&modern);
            assert_eq!(got.name, "GrayIsko1337");
            assert_eq!(got.uuid.as_deref(), Some(UUID_TEXT));
        }

        // Snapshot: protocol number with the snapshot flag, modern format.
        let snapshot = [
            handshake(0x4000_0000 | 250, "127.0.0.1", 2),
            login(&UUID, "Steve"),
        ]
        .concat();
        assert_eq!(expect_login(&snapshot).uuid.as_deref(), Some(UUID_TEXT));
    }

    #[test]
    fn forge_marker_in_the_address_is_skipped() {
        for address in ["127.0.0.1\0FML\0", "127.0.0.1\0FML2\0", "127.0.0.1\0FML3\0"] {
            let bytes = [handshake(763, address, 2), login(&[0], "Steve")].concat();
            assert_eq!(expect_login(&bytes).name, "Steve", "{address:?}");
        }
    }

    #[test]
    fn name_is_enough_when_the_rest_of_1_19_login_has_not_arrived() {
        // 1.19 (759): after the nickname comes a key signature of hundreds of bytes; no need to wait for it.
        let mut bytes = [handshake(759, "127.0.0.1", 2)].concat();
        let body = [varint(0), string("Steve"), vec![1; 12]].concat();
        bytes.extend(varint(840)); // declared packet length is larger than what arrived
        bytes.extend(body);
        assert_eq!(expect_login(&bytes).name, "Steve");
    }

    #[test]
    fn partial_data_asks_for_more() {
        let full = [handshake(767, "127.0.0.1", 2), login(&UUID, "Steve")].concat();
        for cut in 0..full.len() {
            assert_eq!(sniff(&full[..cut]), Sniff::NeedMore, "bytes: {cut}");
        }
        assert!(matches!(sniff(&full), Sniff::Login(_)));
        // Bytes after the login follow; they do not interfere.
        let more = [full, vec![0xAB; 64]].concat();
        assert!(matches!(sniff(&more), Sniff::Login(_)));
    }

    #[test]
    fn status_ping_legacy_ping_and_garbage_are_not_logins() {
        // Status request: handshake with next = 1 and an empty request packet.
        let status = [handshake(767, "127.0.0.1", 1), packet(&varint(0))].concat();
        assert_eq!(sniff(&status), Sniff::NotLogin);
        // The decision is made right after the handshake, without waiting for the rest.
        assert_eq!(sniff(&handshake(767, "127.0.0.1", 1)), Sniff::NotLogin);

        assert_eq!(sniff(&[0xFE, 0x01, 0xFA]), Sniff::NotLogin);
        assert_eq!(sniff(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"), Sniff::NotLogin);
        assert_eq!(sniff(&[0x00]), Sniff::NotLogin, "zero-length packet");
        assert_eq!(
            sniff(&[0xff; 8]),
            Sniff::NotLogin,
            "VarInt longer than five bytes"
        );
        // A declared length larger than we are ready to read.
        assert_eq!(sniff(&varint(5000)), Sniff::NotLogin);

        // A nickname longer than 16 characters, empty, or with control characters is not a login.
        for name in ["", "ThisNameIsWayTooLong", "Ste\u{7}ve"] {
            let bytes = [handshake(754, "127.0.0.1", 2), login(&[], name)].concat();
            assert_eq!(sniff(&bytes), Sniff::NotLogin, "{name:?}");
        }
        // The buffer is full and the packets never formed: nothing more to wait for.
        let mut endless = handshake(767, "127.0.0.1", 2);
        endless.extend(varint(900));
        endless.resize(SNIFF_MAX, 0x80);
        assert_eq!(sniff(&endless), Sniff::NotLogin);
    }

    #[test]
    fn names_for_commands() {
        for ok in ["Steve", "Alex_01", "a", "GrayIsko1337", "x234567890123456"] {
            assert!(is_valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            "x2345678901234567",
            "Серый",
            "St eve",
            "a-b",
            "a\"b",
            "@a",
            "a.b",
        ] {
            assert!(!is_valid_name(bad), "{bad}");
        }
    }
}
