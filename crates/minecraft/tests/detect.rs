//! End-to-end tests of world detection: fake Minecraft servers (modern and legacy),
//! a foreign program on the port, and forged LAN announcements.
//! Everything opens on 127.0.0.1 only, so there will be no firewall prompt.

use std::{net::SocketAddr, time::Duration};

use phase_minecraft::{lan, status};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
};

const STATUS_JSON: &str = r#"{"version":{"name":"1.20.1","protocol":763},"players":{"max":8,"online":1},"description":{"text":"Мой мир"}}"#;

#[derive(Clone, Copy)]
enum Fake {
    /// A 1.7+ server: understands the modern query.
    Modern,
    /// A 1.4–1.6 server: closes the connection on the modern query, answers the legacy one.
    Legacy,
    /// Not Minecraft: answers with something of its own.
    Http,
    /// Accepts the connection and stays silent.
    Silent,
}

async fn spawn_fake(kind: Fake) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            tokio::spawn(serve(sock, kind));
        }
    });
    addr
}

async fn read_varint(sock: &mut TcpStream) -> Option<u32> {
    let mut value = 0u32;
    for i in 0..5 {
        let byte = sock.read_u8().await.ok()?;
        value |= u32::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn varint(mut v: u32, out: &mut Vec<u8>) {
    loop {
        if v < 0x80 {
            out.push(v as u8);
            return;
        }
        out.push((v as u8 & 0x7F) | 0x80);
        v >>= 7;
    }
}

async fn serve(mut sock: TcpStream, kind: Fake) {
    let mut first = [0u8; 1];
    if sock.peek(&mut first).await.is_err() {
        return;
    }
    match kind {
        Fake::Silent => {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        Fake::Http => {
            let mut buf = [0u8; 64];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
                .await;
        }
        Fake::Legacy if first[0] == 0xFE => {
            let mut req = [0u8; 2];
            let _ = sock.read_exact(&mut req).await;
            let text = "§1\x0074\x001.6.4\x00Старый мир\x000\x0010";
            let units: Vec<u16> = text.encode_utf16().collect();
            let mut out = vec![0xFF];
            out.extend_from_slice(&(units.len() as u16).to_be_bytes());
            for u in units {
                out.extend_from_slice(&u.to_be_bytes());
            }
            let _ = sock.write_all(&out).await;
        }
        Fake::Legacy => {} // does not understand the modern query: closes the connection
        Fake::Modern => {
            // Handshake: length, id 0, protocol, address, port, state 1.
            let Some(len) = read_varint(&mut sock).await else {
                return;
            };
            let mut handshake = vec![0u8; len as usize];
            if sock.read_exact(&mut handshake).await.is_err() {
                return;
            }
            assert_eq!(handshake[0], 0x00, "Handshake packet id");
            assert_eq!(*handshake.last().unwrap(), 0x01, "next state is status");
            // Status Request: length 1, id 0.
            let mut req = [0u8; 2];
            if sock.read_exact(&mut req).await.is_err() {
                return;
            }
            assert_eq!(req, [0x01, 0x00]);
            let mut body = vec![0x00];
            varint(STATUS_JSON.len() as u32, &mut body);
            body.extend_from_slice(STATUS_JSON.as_bytes());
            let mut out = Vec::new();
            varint(body.len() as u32, &mut out);
            out.extend_from_slice(&body);
            // We send it in pieces, as happens on a real network.
            let (a, b) = out.split_at(out.len() / 2);
            let _ = sock.write_all(a).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = sock.write_all(b).await;
        }
    }
}

const QUICK: Duration = Duration::from_millis(700);

#[tokio::test]
async fn status_of_modern_server() {
    let addr = spawn_fake(Fake::Modern).await;
    let found = status::query(addr, QUICK).await.unwrap();
    assert_eq!(found.version, "1.20.1");
    assert_eq!(found.protocol, Some(763));
    assert_eq!(found.motd, "Мой мир");
    assert!(!found.legacy);
}

#[tokio::test]
async fn status_of_old_server_uses_legacy_ping() {
    let addr = spawn_fake(Fake::Legacy).await;
    let found = status::query(addr, QUICK).await.unwrap();
    assert_eq!(found.version, "1.6.4");
    assert_eq!(found.motd, "Старый мир");
    assert!(found.legacy);
}

#[tokio::test]
async fn other_programs_are_not_minecraft() {
    for kind in [Fake::Http, Fake::Silent] {
        let addr = spawn_fake(kind).await;
        let started = std::time::Instant::now();
        assert!(status::query(addr, QUICK).await.is_err());
        assert!(
            started.elapsed() < QUICK * 3,
            "a silent port must not delay the search for long"
        );
    }
    // Closed port.
    let closed = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap()
    };
    assert!(status::query(closed, QUICK).await.is_err());
}

/// Sends announcements to `to` from 127.0.0.1, in a loop, until the test ends.
fn spawn_announcer(to: SocketAddr, texts: Vec<String>) {
    tokio::spawn(async move {
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        loop {
            for text in &texts {
                let _ = sender.send_to(text.as_bytes(), to).await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    });
}

#[tokio::test]
async fn detects_only_real_minecraft_world() {
    let http = spawn_fake(Fake::Http).await;
    let ours = spawn_fake(Fake::Modern).await;
    let real = spawn_fake(Fake::Modern).await;

    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let to = socket.local_addr().unwrap();
    spawn_announcer(
        to,
        vec![
            // A foreign service posing as a world: must be rejected by the status query.
            format!("[MOTD]Подделка[/MOTD][AD]{}[/AD]", http.port()),
            // Our own announcement (a guest on this same PC): skipped.
            lan::format_announcement("Мир друга", ours.port()),
            "мусор".to_string(),
            format!("[MOTD]Steve - New World[/MOTD][AD]{}[/AD]", real.port()),
        ],
    );

    let world = lan::detect_on(socket, Duration::from_secs(10))
        .await
        .unwrap()
        .expect("the real world must be found");
    assert_eq!(world.announcement.port, real.port());
    assert_eq!(world.announcement.motd, "Steve - New World");
    assert_eq!(world.status.version, "1.20.1");
}

#[tokio::test]
async fn detect_gives_up_after_wait() {
    let http = spawn_fake(Fake::Http).await;
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    spawn_announcer(
        socket.local_addr().unwrap(),
        vec![format!("[MOTD]Подделка[/MOTD][AD]{}[/AD]", http.port())],
    );
    let found = lan::detect_on(socket, Duration::from_millis(1500))
        .await
        .unwrap();
    assert!(
        found.is_none(),
        "a foreign service must not count as a world"
    );
}

/// A world that drops the first `fail_first` connections (just opened, busy with
/// autosave), then answers as usual.
async fn spawn_slow_starting_world(fail_first: usize) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut seen = 0;
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            seen += 1;
            if seen <= fail_first {
                drop(sock);
                continue;
            }
            tokio::spawn(serve(sock, Fake::Modern));
        }
    });
    addr
}

#[tokio::test]
async fn world_that_did_not_answer_at_first_is_retried() {
    // The first check (modern + legacy query) fails.
    let world = spawn_slow_starting_world(2).await;
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    spawn_announcer(
        socket.local_addr().unwrap(),
        vec![format!("[MOTD]Новый мир[/MOTD][AD]{}[/AD]", world.port())],
    );
    let found = lan::detect_on(socket, Duration::from_secs(10))
        .await
        .unwrap()
        .expect("the world must be found on the recheck");
    assert_eq!(found.announcement.port, world.port());
}
