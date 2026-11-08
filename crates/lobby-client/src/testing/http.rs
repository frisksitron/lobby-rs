//! The little HTTP the fake Lobby and the chaos proxy speak over raw sockets.

use std::{io, time::Duration};

use lobby_protocol::{ErrorBody, ErrorCode, ErrorEnvelope};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};

/// Peeks the request line without consuming it, so the WebSocket handshake can still read it.
pub(super) async fn request_line(stream: &TcpStream) -> io::Result<String> {
    let mut buf = [0u8; 1024];
    loop {
        let n = stream.peek(&mut buf).await?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if let Some(end) = buf[..n].iter().position(|&b| b == b'\n') {
            return Ok(String::from_utf8_lossy(&buf[..end]).into_owned());
        }
        if n == buf.len() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

pub(super) fn bearer_of(head: &str) -> String {
    head.lines()
        .find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                .map(|(_, v)| v.trim().trim_start_matches("Bearer ").to_owned())
        })
        .unwrap_or_default()
}

pub(super) fn envelope(code: ErrorCode, message: &str) -> Vec<u8> {
    serde_json::to_vec(&ErrorEnvelope {
        error: ErrorBody::new(code, message, None),
    })
    .expect("envelope serializes")
}

pub(super) async fn read_request(stream: &mut TcpStream) -> io::Result<String> {
    Ok(read_request_and_body(stream).await?.0)
}

pub(super) async fn read_request_and_body(stream: &mut TcpStream) -> io::Result<(String, Vec<u8>)> {
    let mut data = Vec::new();
    let mut buf = [0u8; 1024];
    let head_end = loop {
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        data.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
    let length: usize = head
        .lines()
        .find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.trim().parse().ok())
        })
        .unwrap_or(0);
    while data.len() < head_end + length {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }
    let body = data[head_end..(head_end + length).min(data.len())].to_vec();
    Ok((head, body))
}

pub(super) async fn respond(stream: TcpStream, status: u16, body: &[u8]) -> io::Result<()> {
    respond_with(stream, status, "application/json", "", body).await
}

/// `extra` is whole header lines, each ending in CRLF.
pub(super) async fn respond_with(
    mut stream: TcpStream,
    status: u16,
    content_type: &str,
    extra: &str,
    body: &[u8],
) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await
}
