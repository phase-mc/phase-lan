//! Shared helpers of end-to-end tests: a "game", a "voice server", a UDP "cable".

#![allow(dead_code)] // each test file uses its own subset of helpers

use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
};

/// A UDP middlebox: guest → `addr` → host. The test can "cut" it (packets
/// are dropped both ways) and switch it to another host.
///
/// On the host side the middlebox is IPv6 (`[::1]`): in tests the host lives on `[::1]`, and the guest
/// only on `127.0.0.1`. iroh learns the host's real address itself, but an IPv4 socket cannot send
/// to an IPv6 address, so the direct path goes only through the middlebox.
pub struct Cable {
    pub addr: SocketAddr,
    upstream: Arc<Mutex<SocketAddr>>,
    cut: Arc<AtomicBool>,
    /// How many packets were dropped while the cable was cut.
    pub dropped: Arc<AtomicUsize>,
}

impl Cable {
    pub async fn new(upstream: SocketAddr) -> Self {
        let front = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let back = Arc::new(UdpSocket::bind("[::1]:0").await.unwrap());
        let cable = Self {
            addr: front.local_addr().unwrap(),
            upstream: Arc::new(Mutex::new(upstream)),
            cut: Arc::default(),
            dropped: Arc::default(),
        };
        let client: Arc<Mutex<Option<SocketAddr>>> = Arc::default();

        // Guest → host.
        tokio::spawn({
            let (front, back, client) = (front.clone(), back.clone(), client.clone());
            let (upstream, cut, dropped) = (
                cable.upstream.clone(),
                cable.cut.clone(),
                cable.dropped.clone(),
            );
            async move {
                let mut buf = vec![0u8; 65536];
                loop {
                    let Ok((len, from)) = front.recv_from(&mut buf).await else {
                        continue;
                    };
                    *client.lock().unwrap() = Some(from);
                    if cut.load(Ordering::SeqCst) {
                        dropped.fetch_add(1, Ordering::SeqCst);
                        continue;
                    }
                    let to = *upstream.lock().unwrap();
                    let _ = back.send_to(&buf[..len], to).await;
                }
            }
        });
        // Host → guest. Packets from the previous host (after a switch) are not let through.
        tokio::spawn({
            let (upstream, cut, dropped) = (
                cable.upstream.clone(),
                cable.cut.clone(),
                cable.dropped.clone(),
            );
            async move {
                let mut buf = vec![0u8; 65536];
                loop {
                    let Ok((len, from)) = back.recv_from(&mut buf).await else {
                        continue;
                    };
                    if from != *upstream.lock().unwrap() {
                        continue;
                    }
                    if cut.load(Ordering::SeqCst) {
                        dropped.fetch_add(1, Ordering::SeqCst);
                        continue;
                    }
                    let to = *client.lock().unwrap();
                    if let Some(to) = to {
                        let _ = front.send_to(&buf[..len], to).await;
                    }
                }
            }
        });
        cable
    }

    pub fn set_cut(&self, cut: bool) {
        self.cut.store(cut, Ordering::SeqCst);
    }

    pub fn switch_to(&self, upstream: SocketAddr) {
        *self.upstream.lock().unwrap() = upstream;
    }
}

/// A TCP echo server instead of Minecraft. Returns its port.
pub async fn spawn_fake_game() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let (mut r, mut w) = sock.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    port
}

/// A UDP echo server instead of a voice mod. Returns its port.
pub async fn spawn_fake_voice() -> u16 {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = socket.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            if let Ok((len, from)) = socket.recv_from(&mut buf).await {
                let _ = socket.send_to(&buf[..len], from).await;
            }
        }
    });
    port
}

pub async fn roundtrip(sock: &mut TcpStream, data: &[u8], within: Duration) {
    sock.write_all(data).await.unwrap();
    read_exactly(sock, data, within).await;
}

pub async fn read_exactly(sock: &mut TcpStream, expected: &[u8], within: Duration) {
    let mut back = vec![0u8; expected.len()];
    tokio::time::timeout(within, sock.read_exact(&mut back))
        .await
        .expect("the game's answer did not arrive in time")
        .expect("the game connection broke");
    assert_eq!(back, expected);
}

pub async fn voice_roundtrip(client: &UdpSocket, to: SocketAddr, payload: &[u8]) {
    let mut buf = vec![0u8; 4096];
    // Voice is UDP: right after reconnecting the first packet may be lost, so try again.
    for _ in 0..10 {
        client.send_to(payload, to).await.unwrap();
        if let Ok(Ok((len, _))) =
            tokio::time::timeout(Duration::from_millis(500), client.recv_from(&mut buf)).await
        {
            assert_eq!(&buf[..len], payload);
            return;
        }
    }
    panic!("voice did not get through the tunnel");
}
