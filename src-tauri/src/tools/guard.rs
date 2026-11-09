//! Keeps model-chosen URLs on the public internet. Without this, "fetch
//! http://localhost:11434/..." would let a web page or the model poke at Ollama,
//! the app's own servers, or the home router.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use url::{Host, Url};

pub fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || o[0] == 0                                   // 0.0.0.0/8
        || (o[0] == 100 && (64..128).contains(&o[1]))  // 100.64.0.0/10 carrier NAT
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // 198.18.0.0/15 benchmarking
        || o[0] >= 240) // reserved
}

pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00 // fc00::/7 unique local
                || (first & 0xffc0) == 0xfe80 // fe80::/10 link local
                || v6 == Ipv6Addr::from(0u128))
        }
    }
}

/// Checks the URL and resolves its host. Returns an address to connect to
/// (so the request uses exactly the address that was checked), or why not.
pub async fn resolve_public(url: &Url) -> Result<SocketAddr, String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Only http and https links can be opened".into());
    }
    let port = url.port_or_known_default().ok_or("URL has no port")?;
    let addrs: Vec<SocketAddr> = match url.host() {
        None => return Err("URL has no host".into()),
        Some(Host::Ipv4(ip)) => vec![SocketAddr::new(IpAddr::V4(ip), port)],
        Some(Host::Ipv6(ip)) => vec![SocketAddr::new(IpAddr::V6(ip), port)],
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_lowercase();
            if d == "localhost" || [".localhost", ".local", ".internal", ".lan", ".home"].iter().any(|s| d.ends_with(s)) {
                return Err("That address is on this computer or the local network".into());
            }
            let found: Vec<SocketAddr> = tokio::net::lookup_host((d.as_str(), port))
                .await
                .map_err(|e| format!("Couldn't look up {d}: {e}"))?
                .collect();
            found
        }
    };
    if addrs.is_empty() {
        return Err("The host didn't resolve to any address".into());
    }
    // Every address must be public, or a DNS name could mix in a private one
    if addrs.iter().any(|a| !is_public(a.ip())) {
        return Err("That address is on this computer or the local network".into());
    }
    Ok(addrs[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn check(u: &str) -> Result<SocketAddr, String> {
        resolve_public(&Url::parse(u).unwrap()).await
    }

    #[tokio::test]
    async fn blocks_local_and_private_addresses() {
        for u in [
            "http://localhost:11434/api/tags",
            "http://127.0.0.1:8080",
            "http://10.0.0.5",
            "http://192.168.1.1",
            "http://172.16.3.4",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]:8080",
            "http://[fd00::1]",
            "http://[::ffff:127.0.0.1]",
            "http://0.0.0.0:8081",
            "http://printer.local",
            "http://host.wokwi.internal",
            "file:///C:/Windows/win.ini",
            "ftp://example.com",
        ] {
            assert!(check(u).await.is_err(), "should block {u}");
        }
    }

    #[tokio::test]
    async fn allows_public_ip_literals() {
        assert!(check("https://1.1.1.1/").await.is_ok());
        assert!(check("http://[2606:4700:4700::1111]/").await.is_ok());
    }

    #[test]
    fn classifies_ranges() {
        assert!(!is_public("100.64.0.1".parse().unwrap()));
        assert!(is_public("8.8.8.8".parse().unwrap()));
        assert!(!is_public("fe80::1".parse().unwrap()));
    }
}
