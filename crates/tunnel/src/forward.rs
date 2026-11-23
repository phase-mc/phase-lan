//! Pumping bytes between a local TCP connection and a QUIC stream.

use phase_lang::tr;
use std::io::ErrorKind;

use anyhow::{Context, Result};
use iroh::endpoint::{ReadError, RecvStream, SendStream, VarInt};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
};

use crate::wire;

/// Pump buffer size.
const BUF_LEN: usize = 16 * 1024;

/// Links a TCP connection with a QUIC stream in both directions and waits until both
/// directions close.
///
/// - A normal close (EOF / TCP FIN) is passed to the other side as `finish()`.
/// - An abrupt close by the local program (TCP RST) is passed **the same way** as a normal one.
///   For example, a Minecraft server sends "you are disconnected: reason" and closes the
///   socket right away. If we reset the stream (`reset`), QUIC would drop the not yet delivered
///   bytes, and the player would not see the disconnect reason.
/// - A channel failure (QUIC) ends the pump with an error. The local socket is
///   closed in that case, so no "ghost" players remain.
pub async fn pipe(tcp: TcpStream, mut send: SendStream, mut recv: RecvStream) -> Result<()> {
    let (mut tcp_read, mut tcp_write) = tcp.into_split();
    tokio::try_join!(
        tcp_to_peer(&mut tcp_read, &mut send),
        peer_to_tcp(&mut recv, &mut tcp_write),
    )
    .map(|_| ())
}

async fn tcp_to_peer(tcp: &mut OwnedReadHalf, send: &mut SendStream) -> Result<()> {
    let mut buf = vec![0u8; BUF_LEN];
    loop {
        let n = match tcp.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            // The local program closed the connection abruptly: deliver everything read.
            Err(err)
                if matches!(
                    err.kind(),
                    ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
                ) =>
            {
                break;
            }
            Err(err) => {
                let _ = send.reset(VarInt::from_u32(wire::code::TCP_ERROR));
                return Err(err).context(tr!(
                    "ошибка чтения локального подключения",
                    "error reading from the local connection"
                ));
            }
        };
        send.write_all(&buf[..n]).await.context(tr!(
            "канал с другой стороной оборвался",
            "the connection to the other side dropped"
        ))?;
    }
    let _ = send.finish();
    Ok(())
}

async fn peer_to_tcp(recv: &mut RecvStream, tcp: &mut OwnedWriteHalf) -> Result<()> {
    let mut buf = vec![0u8; BUF_LEN];
    loop {
        let n = match recv.read(&mut buf).await {
            Ok(Some(n)) => n,
            Ok(None) => break,
            // The other side closed the stream abruptly, so close the local connection too.
            Err(ReadError::Reset(_)) => return Ok(()),
            Err(err) => {
                return Err(err).context(tr!(
                    "канал с другой стороной оборвался",
                    "the connection to the other side dropped"
                ));
            }
        };
        if let Err(err) = tcp.write_all(&buf[..n]).await {
            let _ = recv.stop(VarInt::from_u32(wire::code::TCP_ERROR));
            return Err(err).context(tr!(
                "ошибка записи в локальное подключение",
                "error writing to the local connection"
            ));
        }
    }
    let _ = tcp.shutdown().await;
    Ok(())
}
