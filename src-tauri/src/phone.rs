//! Serves the phone "body" page (`phone/` at the repo root) over plain HTTP so
//! an Android phone on the same network can open it in a browser. The page then
//! connects to the WebSocket server like the ESP32 does.
//!
//! Files are compiled into the binary, so there's nothing to bundle. Routes are
//! the pages listed in `FILES`, plus everything in the repo's `gifs/` folder
//! under `/gifs/` (including `manifest.json`, which maps avatar states to GIFs
//! for the phone and the desktop pet).

use include_dir::{include_dir, Dir};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const PHONE_PORT: u16 = 8081;

static GIFS: Dir = include_dir!("$CARGO_MANIFEST_DIR/../gifs");

/// (path, content type, bytes) for the phone page itself.
const FILES: &[(&str, &str, &[u8])] = &[
    ("/", "text/html; charset=utf-8", include_bytes!("../../phone/index.html")),
    ("/index.html", "text/html; charset=utf-8", include_bytes!("../../phone/index.html")),
    ("/style.css", "text/css; charset=utf-8", include_bytes!("../../phone/style.css")),
    ("/app.js", "text/javascript; charset=utf-8", include_bytes!("../../phone/app.js")),
    ("/avatar.js", "text/javascript; charset=utf-8", include_bytes!("../../phone/avatar.js")),
];

/// A file from the repo's gifs/ folder.
pub fn gif_bytes(name: &str) -> Option<&'static [u8]> {
    GIFS.get_file(name).map(|f| f.contents())
}

/// The GIF manifest, for the desktop pet.
pub fn gif_manifest() -> &'static str {
    GIFS.get_file("manifest.json")
        .and_then(|f| f.contents_utf8())
        .unwrap_or("{}")
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "gif" => "image/gif",
        "png" => "image/png",
        "webp" => "image/webp",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

fn lookup(path: &str) -> Option<(&'static str, &'static [u8])> {
    if let Some((_, ct, bytes)) = FILES.iter().find(|(p, _, _)| *p == path) {
        return Some((ct, bytes));
    }
    // include_dir only holds files under gifs/, and ".." never matches one
    let name = path.strip_prefix("/gifs/")?;
    let file = GIFS.get_file(name)?;
    Some((content_type(name), file.contents()))
}

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

    // ESP32 copies of the GIFs are made on demand (see esp32.rs)
    let esp32: Option<std::sync::Arc<Vec<u8>>> = if path == "/esp32/manifest.json" {
        Some(std::sync::Arc::new(crate::esp32::manifest().await.to_string().into_bytes()))
    } else if let Some(name) = path.strip_prefix("/esp32/gifs/") {
        crate::esp32::gif(name).await
    } else {
        None
    };

    let (status, content_type, body): (&str, &str, &[u8]) = if method != "GET" && method != "HEAD" {
        ("405 Method Not Allowed", "text/plain", b"Method not allowed")
    } else if let Some(data) = &esp32 {
        ("200 OK", content_type(path), data.as_slice())
    } else {
        match lookup(path) {
            Some((ct, bytes)) => ("200 OK", ct, bytes),
            None => ("404 Not Found", "text/plain", b"Not found"),
        }
    };

    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
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
        // Every GIF the manifest names must exist
        let manifest: serde_json::Value = serde_json::from_str(gif_manifest()).unwrap();
        let mut names: Vec<&str> = vec![manifest["default"].as_str().unwrap()];
        names.extend(manifest["states"].as_object().unwrap().values().filter_map(|v| v.as_str()));
        for n in names {
            assert!(GIFS.get_file(n).is_some(), "manifest names missing gif {n}");
        }
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
        assert!(get(port, "/gifs/manifest.json").await.contains("application/json"));
        assert!(get(port, "/avatar.js").await.starts_with("HTTP/1.1 200 OK"));
        assert!(get(port, "/gifs/../../Cargo.toml").await.starts_with("HTTP/1.1 404"));
        assert!(get(port, "/../Cargo.toml").await.starts_with("HTTP/1.1 404"));
        let m = get(port, "/esp32/manifest.json").await;
        assert!(m.contains("\"files\"") && m.contains("mai-san-wave.gif"), "{m}");
        assert!(get(port, "/esp32/gifs/mai-san-wave.gif").await.contains("image/gif"));
        assert!(get(port, "/esp32/gifs/nope.gif").await.starts_with("HTTP/1.1 404"));
    }
}
