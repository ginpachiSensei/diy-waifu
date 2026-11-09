//! Serves the phone "body" page (`phone/` at the repo root) over plain HTTP so
//! an Android phone on the same network can open it in a browser. The page then
//! connects to the WebSocket server like the ESP32 does.
//!
//! Files are compiled into the binary, so there's nothing to bundle and the
//! only routes that exist are the ones listed in `FILES`.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const PHONE_PORT: u16 = 8081;

/// (path, content type, bytes). Add new GIFs here and in `phone/app.js`.
const FILES: &[(&str, &str, &[u8])] = &[
    ("/", "text/html; charset=utf-8", include_bytes!("../../phone/index.html")),
    ("/index.html", "text/html; charset=utf-8", include_bytes!("../../phone/index.html")),
    ("/style.css", "text/css; charset=utf-8", include_bytes!("../../phone/style.css")),
    ("/app.js", "text/javascript; charset=utf-8", include_bytes!("../../phone/app.js")),
    ("/gifs/mai-san-wave.gif", "image/gif", include_bytes!("../../gifs/mai-san-wave.gif")),
];

pub async fn start_server(host: String, port: u16) -> std::io::Result<()> {
    let addr = format!("{}:{}", host, port);
    let listener = TcpListener::bind(&addr).await?;
    println!("Phone page served on http://{}", addr);

    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = handle(stream).await {
                eprintln!("Phone HTTP error: {}", e);
            }
        });
    }
}

async fn handle(mut stream: TcpStream) -> std::io::Result<()> {
    // Read just the request head. We only need the first line.
    let mut buf = vec![0u8; 8192];
    let mut len = 0;
    loop {
        let n = stream.read(&mut buf[len..]).await?;
        if n == 0 {
            return Ok(());
        }
        len += n;
        if buf[..len].windows(4).any(|w| w == b"\r\n\r\n") || len == buf.len() {
            break;
        }
    }

    let head = String::from_utf8_lossy(&buf[..len]);
    let mut parts = head.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let path = target.split(['?', '#']).next().unwrap_or("/");

    let (status, content_type, body): (&str, &str, &[u8]) = if method != "GET" && method != "HEAD" {
        ("405 Method Not Allowed", "text/plain", b"Method not allowed")
    } else {
        match FILES.iter().find(|(p, _, _)| *p == path) {
            Some((_, ct, bytes)) => ("200 OK", *ct, *bytes),
            None => ("404 Not Found", "text/plain", b"Not found"),
        }
    };

    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    if method != "HEAD" {
        stream.write_all(body).await?;
    }
    stream.shutdown().await
}

/// This PC's address on the local network, for showing the phone URL in the UI.
/// Connecting a UDP socket sends no packets; it just picks the outgoing interface.
pub fn lan_ip() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_every_listed_file() {
        assert!(FILES.iter().all(|(p, _, b)| p.starts_with('/') && !b.is_empty()));
        assert!(FILES.iter().any(|(p, _, _)| *p == "/"));
    }

    async fn get(port: u16, path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        String::from_utf8_lossy(&out).into_owned()
    }

    #[tokio::test]
    async fn http_server_serves_page_and_404s() {
        let port = 18_081;
        tokio::spawn(start_server("127.0.0.1".into(), port));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let index = get(port, "/?ws=ws://x:1").await;
        assert!(index.starts_with("HTTP/1.1 200 OK"));
        assert!(index.contains("<title>Companion</title>"));
        assert!(get(port, "/gifs/mai-san-wave.gif").await.contains("image/gif"));
        assert!(get(port, "/../Cargo.toml").await.starts_with("HTTP/1.1 404"));
    }
}
