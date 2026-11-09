//! Pairing code for bodies that connect over the network.
//!
//! The WebSocket server listens on every interface, so without this anyone on
//! the same Wi-Fi could chat with the companion and see what app you're using.
//! Rules for a new connection:
//! - from this PC (loopback): always allowed (the desktop pet, `adb reverse`, Wokwi),
//! - from the local network: needs `?token=<code>` in the WebSocket URL,
//! - from a public address: refused.
//!
//! The code lives in the settings table under `pair_token`. Making a new one
//! disconnects every body that isn't on this PC.

use crate::db::Db;
use std::net::IpAddr;
use tokio::sync::watch;

const KEY: &str = "pair_token";
/// No 0/O or 1/I/L, so it's easy to read off the screen and type on an ESP32 portal
const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const LEN: usize = 8;

/// The current code. Client loops subscribe so a new code kicks old clients.
pub struct Pairing(pub watch::Sender<String>);

impl Pairing {
    pub fn load(db: &Db) -> Self {
        let token = match db.get_setting(KEY) {
            Ok(Some(t)) if valid_format(&t) => t,
            _ => {
                let t = generate();
                if let Err(e) = db.set_setting(KEY, &t) {
                    log::error!("Couldn't save pairing code: {e}");
                }
                t
            }
        };
        Pairing(watch::channel(token).0)
    }

    pub fn current(&self) -> String {
        self.0.borrow().clone()
    }

    pub fn regenerate(&self, db: &Db) -> Result<String, String> {
        let t = generate();
        db.set_setting(KEY, &t)?;
        self.0.send_replace(t.clone());
        Ok(t)
    }
}

fn valid_format(t: &str) -> bool {
    t.len() == LEN && t.bytes().all(|b| ALPHABET.contains(&b))
}

fn generate() -> String {
    let mut bytes = [0u8; LEN];
    // Only fails if the OS has no randomness source, which would break far more than this
    getrandom::getrandom(&mut bytes).expect("no OS random source");
    // 256 % 31 leaves a tiny bias; irrelevant for a LAN pairing code
    bytes.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect()
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    NeedsToken,
    WrongToken,
    NotLocal,
}

/// Whether a connection from `peer` that sent `given` (from `?token=`) may in.
pub fn check(peer: IpAddr, given: Option<&str>, token: &str) -> Verdict {
    let peer = match peer {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(peer),
        v4 => v4,
    };
    if peer.is_loopback() {
        return Verdict::Ok;
    }
    if crate::tools::guard::is_public(peer) {
        return Verdict::NotLocal;
    }
    match given {
        None | Some("") => Verdict::NeedsToken,
        Some(g) if same(&g.trim().to_ascii_uppercase(), token) => Verdict::Ok,
        Some(_) => Verdict::WrongToken,
    }
}

/// Constant-time compare, so timing doesn't leak how much of the code was right.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Pulls `token` out of a query string like `a=1&token=ABC`.
pub fn token_from_query(query: Option<&str>) -> Option<String> {
    url::form_urlencoded::parse(query?.as_bytes())
        .find(|(k, _)| k == "token")
        .map(|(_, v)| v.into_owned())
}

/// QR code of `text` as an SVG string, for the Bodies tab.
pub fn qr_svg(text: &str) -> Result<String, String> {
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(200, 200)
        .quiet_zone(true)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_look_right() {
        let a = generate();
        assert!(valid_format(&a), "{a}");
        assert_ne!(a, generate());
    }

    #[test]
    fn checks_peers() {
        let t = "ABCD2345";
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        assert_eq!(check("127.0.0.1".parse().unwrap(), None, t), Verdict::Ok);
        assert_eq!(check("::1".parse().unwrap(), None, t), Verdict::Ok);
        assert_eq!(check("::ffff:127.0.0.1".parse().unwrap(), None, t), Verdict::Ok);
        assert_eq!(check(lan, None, t), Verdict::NeedsToken);
        assert_eq!(check(lan, Some("WRONG123"), t), Verdict::WrongToken);
        assert_eq!(check(lan, Some("abcd2345"), t), Verdict::Ok);
        assert_eq!(check("10.0.0.5".parse().unwrap(), Some(t), t), Verdict::Ok);
        assert_eq!(check("8.8.8.8".parse().unwrap(), Some(t), t), Verdict::NotLocal);
    }

    #[test]
    fn reads_token_from_query() {
        assert_eq!(token_from_query(Some("body=esp32&token=AB%20C")).as_deref(), Some("AB C"));
        assert_eq!(token_from_query(Some("x=1")), None);
        assert_eq!(token_from_query(None), None);
    }

    #[test]
    fn keeps_and_regenerates_code() {
        let db = Db::open_in_memory().unwrap();
        let p = Pairing::load(&db);
        let first = p.current();
        assert_eq!(Pairing::load(&db).current(), first);
        let second = p.regenerate(&db).unwrap();
        assert_ne!(first, second);
        assert_eq!(Pairing::load(&db).current(), second);
        assert!(qr_svg("http://192.168.1.2:8081/?token=X").unwrap().contains("<svg"));
    }
}
