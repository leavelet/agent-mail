//! Where the server's outgoing requests (notification webhooks) may go.
//!
//! In public mode a webhook must not reach the server's own network:
//! loopback, private, link-local, carrier-grade NAT, multicast and other
//! reserved addresses are refused. The check happens on the addresses the
//! name resolves to, in the HTTP client's resolver, and the client then
//! connects to exactly those addresses: a name that resolves to a public
//! address when the URL is saved and to `127.0.0.1` when the webhook fires
//! (DNS rebinding) is refused at that moment. URLs with a literal address
//! are checked before the request. Redirects are never followed and no
//! proxy is used.
//!
//! The server's own interface addresses, and the networks they are on, are
//! refused too: an address can be public and still be the server itself
//! (a host with global IPv6 addresses, say). In that mode only ports 80 and
//! 443 may be used, so that webhooks cannot be aimed at other services of
//! a public host.

use crate::config::Cidr;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Ports a webhook may use when private targets are refused.
pub const WEBHOOK_PORTS: &[u16] = &[80, 443];

/// Whether a webhook may connect to `ip`: a public address that is not on
/// one of the server's own networks.
pub fn allowed(ip: IpAddr) -> bool {
    allowed_with(ip, &local_networks())
}

fn allowed_with(ip: IpAddr, own: &[Cidr]) -> bool {
    is_public(ip) && !own.iter().any(|n| n.contains(ip))
}

/// How long the list of the host's own networks is reused.
const LOCAL_TTL: Duration = Duration::from_secs(30);

/// The networks of this host's interfaces (address and netmask), looked up
/// again at most every 30 seconds. A network wider than a /16 (IPv4) or a
/// /48 (IPv6) counts as its address alone, so that an odd netmask cannot
/// refuse half the Internet.
pub fn local_networks() -> Vec<Cidr> {
    static CACHE: Mutex<Option<(Instant, Vec<Cidr>)>> = Mutex::new(None);
    let mut c = CACHE.lock().unwrap();
    if let Some((at, nets)) = &*c
        && at.elapsed() < LOCAL_TTL
    {
        return nets.clone();
    }
    let nets = interface_networks();
    *c = Some((Instant::now(), nets.clone()));
    nets
}

#[cfg(unix)]
fn interface_networks() -> Vec<Cidr> {
    fn ip_of(sa: *const libc::sockaddr) -> Option<IpAddr> {
        if sa.is_null() {
            return None;
        }
        // SAFETY: getifaddrs gives sockaddrs of the size their family says.
        unsafe {
            match i32::from((*sa).sa_family) {
                libc::AF_INET => {
                    let a = &*(sa as *const libc::sockaddr_in);
                    Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr))))
                }
                libc::AF_INET6 => {
                    let a = &*(sa as *const libc::sockaddr_in6);
                    Some(IpAddr::V6(Ipv6Addr::from(a.sin6_addr.s6_addr)))
                }
                _ => None,
            }
        }
    }
    let mut out = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: the list is freed below and not used afterwards.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        tracing::warn!(
            "listing network interfaces: {}",
            std::io::Error::last_os_error()
        );
        return out;
    }
    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: `cur` is a node of the list getifaddrs returned.
        let ifa = unsafe { &*cur };
        if let Some(addr) = ip_of(ifa.ifa_addr) {
            let bits = match ip_of(ifa.ifa_netmask) {
                Some(IpAddr::V4(m)) => u32::from(m).count_ones() as u8,
                Some(IpAddr::V6(m)) => u128::from(m).count_ones() as u8,
                None => 0,
            };
            let (max, min) = if addr.is_ipv4() { (32, 16) } else { (128, 48) };
            let prefix = if bits >= min && bits <= max {
                bits
            } else {
                max
            };
            out.push(Cidr { addr, prefix });
        }
        cur = ifa.ifa_next;
    }
    // SAFETY: `head` came from getifaddrs.
    unsafe { libc::freeifaddrs(head) };
    out
}

#[cfg(not(unix))]
fn interface_networks() -> Vec<Cidr> {
    Vec::new()
}

/// Whether `ip` is an ordinary public unicast address.
pub fn is_public(ip: IpAddr) -> bool {
    match crate::config::canonical(ip) {
        IpAddr::V4(a) => public_v4(a),
        IpAddr::V6(a) => public_v6(a),
    }
}

fn public_v4(a: Ipv4Addr) -> bool {
    let o = a.octets();
    !(a.is_unspecified()
        || a.is_loopback()
        || a.is_private()
        || a.is_link_local()
        || a.is_broadcast()
        || a.is_multicast()
        || a.is_documentation()
        || o[0] == 0
        // Carrier-grade NAT 100.64.0.0/10.
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        // IETF protocol assignments 192.0.0.0/24.
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        // Benchmarking 198.18.0.0/15.
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
        // Reserved 240.0.0.0/4.
        || o[0] >= 240)
}

fn public_v6(a: Ipv6Addr) -> bool {
    let s = a.segments();
    !(a.is_unspecified()
        || a.is_loopback()
        || a.is_multicast()
        // Unique local fc00::/7, link-local fe80::/10, site-local fec0::/10.
        || (s[0] & 0xfe00) == 0xfc00
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] & 0xffc0) == 0xfec0
        // Documentation 2001:db8::/32.
        || (s[0] == 0x2001 && s[1] == 0x0db8)
        // NAT64 64:ff9b::/96 and IPv4-compatible ::/96 may reach IPv4
        // space; 6to4 2002::/16 embeds an IPv4 address.
        || (s[0] == 0x64 && s[1] == 0xff9b)
        || s[..6].iter().all(|x| *x == 0)
        || s[0] == 0x2002
        // Discard-only 100::/64.
        || (s[0] == 0x100 && s[1..4].iter().all(|x| *x == 0)))
}

/// A resolver that drops every non-public address and fails when none is
/// left. `overrides` answer for some names without asking DNS (tests).
#[derive(Default)]
pub struct GuardedResolver {
    pub overrides: HashMap<String, Vec<IpAddr>>,
}

#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} resolves only to addresses that webhooks may not reach",
            self.0
        )
    }
}

impl std::error::Error for Refused {}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        let fixed = self.overrides.get(&host).cloned();
        Box::pin(async move {
            let found: Vec<SocketAddr> = match fixed {
                Some(ips) => ips.into_iter().map(|ip| SocketAddr::new(ip, 0)).collect(),
                None => tokio::net::lookup_host((host.as_str(), 0)).await?.collect(),
            };
            let ok: Vec<SocketAddr> = found.into_iter().filter(|a| allowed(a.ip())).collect();
            if ok.is_empty() {
                return Err(Box::new(Refused(host)) as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(ok.into_iter()) as Addrs)
        })
    }
}

/// The client for webhooks: no redirects, no proxy, and with
/// `private_ok == false` the guarded resolver.
pub fn webhook_client(private_ok: bool, resolver: Option<GuardedResolver>) -> reqwest::Client {
    let mut b = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy();
    if !private_ok {
        b = b.dns_resolver(Arc::new(resolver.unwrap_or_default()));
    }
    b.build().expect("building the HTTP client")
}

/// Check a URL's host when it is a literal address (names are checked when
/// they are resolved). `Err` names the reason.
pub fn check_literal(url: &str, private_ok: bool) -> Result<(), String> {
    if private_ok {
        return Ok(());
    }
    let u = reqwest::Url::parse(url).map_err(|e| format!("invalid URL: {e}"))?;
    if let Some(port) = u.port_or_known_default()
        && !WEBHOOK_PORTS.contains(&port)
    {
        return Err(format!(
            "webhooks may only use ports 80 and 443 on this server, not {port}"
        ));
    }
    let ip = match u.host() {
        Some(url::Host::Ipv4(a)) => IpAddr::V4(a),
        Some(url::Host::Ipv6(a)) => IpAddr::V6(a),
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if d == "localhost" || d.ends_with(".localhost") {
                return Err("webhooks may not target localhost".into());
            }
            return Ok(());
        }
        None => return Err("the URL has no host".into()),
    };
    if allowed(ip) {
        Ok(())
    } else {
        Err(format!("webhooks may not target {ip}"))
    }
}

/// The URL without path, query and credentials, for logs (webhook URLs
/// often carry secrets).
pub fn redact(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(u) => format!(
            "{}://{}{}",
            u.scheme(),
            u.host_str().unwrap_or("?"),
            u.port().map(|p| format!(":{p}")).unwrap_or_default()
        ),
        Err(_) => "<invalid url>".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The server's own addresses and the networks they are on are not
    /// webhook targets, even when public.
    #[test]
    fn own_networks_refused() {
        let own: Vec<Cidr> = vec![
            "2001:4860:1:2::5/64".parse().unwrap(),
            "203.0.114.10/24".parse().unwrap(),
        ];
        for a in ["2001:4860:1:2::5", "2001:4860:1:2::99", "203.0.114.200"] {
            assert!(!allowed_with(a.parse().unwrap(), &own), "{a}");
        }
        for a in ["2001:4860:1:3::1", "203.0.115.1", "8.8.8.8"] {
            assert!(allowed_with(a.parse().unwrap(), &own), "{a}");
        }
        // Whatever this host's interfaces are, none of their addresses is
        // allowed.
        let nets = local_networks();
        assert!(nets.iter().any(|n| n.addr.is_loopback()), "{nets:?}");
        for n in &nets {
            assert!(!allowed(n.addr), "{n:?}");
        }
    }

    #[test]
    fn ports() {
        assert!(check_literal("https://8.8.8.8/hook", false).is_ok());
        assert!(check_literal("http://8.8.8.8:80/hook", false).is_ok());
        assert!(check_literal("https://hooks.example.com/x", false).is_ok());
        assert!(check_literal("http://hooks.example.com:8080/x", false).is_err());
        assert!(check_literal("https://8.8.8.8:22/", false).is_err());
        assert!(check_literal("http://hooks.example.com:8080/x", true).is_ok());
    }

    #[test]
    fn classification() {
        for a in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "198.18.0.1",
            "240.0.0.1",
            "192.0.2.1",
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::7f00:1",
            "2002:7f00:1::",
            "::127.0.0.1",
            "2001:db8::1",
            "ff02::1",
        ] {
            assert!(!is_public(a.parse().unwrap()), "{a} should not be public");
        }
        for a in [
            "8.8.8.8",
            "1.1.1.1",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(is_public(a.parse().unwrap()), "{a} should be public");
        }
    }

    #[test]
    fn literals() {
        assert!(check_literal("http://127.0.0.1:9/x", false).is_err());
        assert!(check_literal("http://[::1]/x", false).is_err());
        assert!(check_literal("http://localhost:80/", false).is_err());
        assert!(check_literal("http://a.localhost/", false).is_err());
        assert!(check_literal("http://169.254.169.254/latest", false).is_err());
        assert!(check_literal("https://8.8.8.8/x", false).is_ok());
        assert!(check_literal("https://example.com/x", false).is_ok());
        assert!(check_literal("http://127.0.0.1:9/x", true).is_ok());
    }

    #[test]
    fn redaction() {
        assert_eq!(
            redact("https://user:pw@hooks.example.com:8443/T0/B1/secret?x=1"),
            "https://hooks.example.com:8443"
        );
    }

    #[tokio::test]
    async fn resolver_drops_private_answers() {
        let mut r = GuardedResolver::default();
        r.overrides
            .insert("rebind.test".into(), vec!["127.0.0.1".parse().unwrap()]);
        r.overrides.insert(
            "mixed.test".into(),
            vec!["10.0.0.1".parse().unwrap(), "8.8.8.8".parse().unwrap()],
        );
        let name = |s: &str| s.parse::<Name>().unwrap();
        assert!(r.resolve(name("rebind.test")).await.is_err());
        let ok: Vec<SocketAddr> = r.resolve(name("mixed.test")).await.unwrap().collect();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].ip(), "8.8.8.8".parse::<IpAddr>().unwrap());
        // A real lookup of localhost resolves to loopback only.
        assert!(r.resolve(name("localhost")).await.is_err());
    }
}
