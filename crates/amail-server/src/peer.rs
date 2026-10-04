//! Who is on the other end of a request: the client address (for rate
//! limits) and whether the request came through a trusted reverse proxy.
//!
//! `X-Forwarded-For`, `X-Forwarded-Host` and `X-Forwarded-Proto` are
//! believed only when the direct peer is one of the configured trusted
//! proxies (`amail serve --trusted-proxy`). Then the client is the
//! right-most address in `X-Forwarded-For` that is not itself a trusted
//! proxy.

use crate::AppState;
use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Peer {
    /// The client's address, if known.
    pub ip: Option<IpAddr>,
    /// The direct peer is a trusted proxy: forwarded headers count.
    pub proxied: bool,
}

impl Peer {
    /// The client address as a rate-limit key. IPv6 clients are keyed by
    /// their /64 network: one host usually holds a whole /64, so keying by
    /// the full address would give it billions of separate limits.
    pub fn key(&self) -> String {
        match self.ip.map(crate::config::canonical) {
            Some(IpAddr::V6(v6)) => {
                let net = u128::from(v6) & (u128::MAX << 64);
                format!("{}/64", std::net::Ipv6Addr::from(net))
            }
            Some(ip) => ip.to_string(),
            None => "unknown".into(),
        }
    }

    /// The /48 network of an IPv6 client, as a key for a limit shared by
    /// all /64 networks inside it; `None` for other clients.
    pub fn net48_key(&self) -> Option<String> {
        match self.ip.map(crate::config::canonical) {
            Some(IpAddr::V6(v6)) => {
                let net = u128::from(v6) & (u128::MAX << 80);
                Some(format!("{}/48", std::net::Ipv6Addr::from(net)))
            }
            _ => None,
        }
    }
}

/// Work out the [`Peer`] of a request (see the module docs).
pub fn resolve(
    cfg: &crate::config::Config,
    direct: Option<IpAddr>,
    headers: &axum::http::HeaderMap,
) -> Peer {
    let Some(direct) = direct else {
        return Peer::default();
    };
    if !cfg.trusts(direct) {
        return Peer {
            ip: Some(direct),
            proxied: false,
        };
    }
    let hops: Vec<IpAddr> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|h| h.trim().parse::<IpAddr>().ok())
        .collect();
    let client = hops
        .iter()
        .rev()
        .find(|ip| !cfg.trusts(**ip))
        .or(hops.first())
        .copied()
        .unwrap_or(direct);
    Peer {
        ip: Some(client),
        proxied: true,
    }
}

/// Middleware: put the request's [`Peer`] into its extensions.
pub async fn layer(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let direct = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    let peer = resolve(&st.config, direct, req.headers());
    req.extensions_mut().insert(peer);
    next.run(req).await
}

impl<S: Send + Sync> FromRequestParts<S> for Peer {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Infallible> {
        Ok(parts.extensions.get::<Peer>().copied().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_group_ipv6_by_64() {
        let key = |s: &str| {
            Peer {
                ip: Some(s.parse().unwrap()),
                proxied: false,
            }
            .key()
        };
        assert_eq!(key("2001:db8:1:2::1"), "2001:db8:1:2::/64");
        assert_eq!(key("2001:db8:1:2:ffff::9"), key("2001:db8:1:2::1"));
        assert_ne!(key("2001:db8:1:3::1"), key("2001:db8:1:2::1"));
        assert_eq!(key("192.0.2.7"), "192.0.2.7");
        assert_eq!(key("::ffff:192.0.2.7"), "192.0.2.7");
        let net = |s: &str| {
            Peer {
                ip: Some(s.parse().unwrap()),
                proxied: false,
            }
            .net48_key()
        };
        assert_eq!(net("2001:db8:1:2::1").unwrap(), "2001:db8:1::/48");
        assert_eq!(net("2001:db8:1:ffff::1"), net("2001:db8:1:2::1"));
        assert_eq!(net("192.0.2.7"), None);
        assert_eq!(net("::ffff:192.0.2.7"), None);
    }
    use crate::config::Config;
    use axum::http::HeaderMap;

    #[test]
    fn forwarded_for_only_from_trusted_proxies() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.9, 10.0.0.2".parse().unwrap());
        let direct: IpAddr = "127.0.0.1".parse().unwrap();
        let cfg = Config::default();
        let p = resolve(&cfg, Some(direct), &h);
        assert_eq!(p.ip, Some(direct));
        assert!(!p.proxied);

        let cfg = Config {
            trusted_proxies: vec!["127.0.0.1".parse().unwrap(), "10.0.0.0/8".parse().unwrap()],
            ..Config::default()
        };
        let p = resolve(&cfg, Some(direct), &h);
        assert_eq!(p.ip, Some("203.0.113.9".parse().unwrap()));
        assert!(p.proxied);
        // A client cannot pick its address by prepending to the header: the
        // right-most untrusted hop counts.
        h.insert("x-forwarded-for", "1.2.3.4, 198.51.100.7".parse().unwrap());
        assert_eq!(
            resolve(&cfg, Some(direct), &h).ip,
            Some("198.51.100.7".parse().unwrap())
        );
        // An untrusted peer's header is ignored.
        let other: IpAddr = "192.0.2.50".parse().unwrap();
        assert_eq!(resolve(&cfg, Some(other), &h).ip, Some(other));
        // No header: the proxy itself.
        assert_eq!(
            resolve(&cfg, Some(direct), &HeaderMap::new()).ip,
            Some(direct)
        );
    }
}
