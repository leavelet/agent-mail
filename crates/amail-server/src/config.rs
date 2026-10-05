//! Server settings beyond the store: public mode, protective limits,
//! trusted reverse proxies, webhook targets and retention.
//!
//! A self-hosted server (the default) has no limits beyond the per-org
//! settings and trusts no forwarded headers. `amail serve --public` turns on
//! registration with invite codes, the protective limits below (defaults
//! from the capacity measurements in the operations guide), reserved names,
//! always-`Secure` session cookies and the webhook target restriction.

use std::net::IpAddr;
use std::time::Duration;

/// Protective limits. `None` means unlimited. They apply per server
/// instance (with several instances, a global limit is per instance).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Limits {
    /// Live agent seats per org.
    pub seats_per_org: Option<i64>,
    /// Lists per org.
    pub lists_per_org: Option<i64>,
    /// Live tokens per org (all scopes; launch tokens replace each other).
    pub tokens_per_org: Option<i64>,
    /// Messages per second over all orgs.
    pub global_send_per_sec: Option<f64>,
    /// Messages per second per org (on top of each seat's send budget).
    pub org_send_per_sec: Option<f64>,
    /// Open event streams (`/events` and conversation mirrors) per org.
    pub org_streams: Option<usize>,
    /// Requests per second per token (or browser session); bursts of twice
    /// as many.
    pub token_requests_per_sec: Option<f64>,
    /// Requests per second per org, all its tokens and browser sessions
    /// together; bursts of twice as many.
    pub org_requests_per_sec: Option<f64>,
    /// Link requests (`POST /v1/links`) per hour per org.
    pub link_requests_per_hour: Option<f64>,
    /// Registrations (`POST /v1/register`, also failed ones) per hour per
    /// client address.
    pub registrations_per_hour: Option<f64>,
    /// Webhook notices of one org being sent at the same time; further
    /// notices are dropped while that many are in flight. The cap counts the
    /// org's own mail; notices of mail from other orgs have a cap of the
    /// same size, of which each sending org may hold a quarter.
    pub org_webhooks_in_flight: Option<usize>,
    /// Bytes held by all conversation mirrors of the instance (replay
    /// events and what waits to be sent to watchers). Beyond it, the
    /// watcher furthest behind is disconnected or the largest replay buffer
    /// shortened, whichever holds more.
    pub mirror_bytes: Option<usize>,
    /// Bytes of mail an org may store: every message it sent with all its
    /// deliveries, and the mail of deleted orgs it keeps
    /// (`amail_store::message_bytes`). A send that would go over it is
    /// refused, so an org at its quota can send nothing.
    pub org_storage_bytes: Option<i64>,
    /// Requests whose database cost depends on the data (search, archive,
    /// threads, attention, overview, delivery status, lists) of one org in
    /// progress at the same time, in each pool of
    /// [`crate::limits::Pool`]; a further one waits briefly for a slot,
    /// then gets 429.
    pub org_queries: Option<usize>,
    /// The same over all orgs, in each of the pools
    /// [`crate::limits::Pool::Scan`] and [`crate::limits::Pool::Other`],
    /// so that several orgs together cannot take every database
    /// connection.
    pub queries: Option<usize>,
    /// Failed authentications (unknown, revoked or expired tokens and
    /// sessions) per minute per client address (IPv6: per /64), with bursts
    /// of [`amail_proto::limits::AUTH_FAILURE_BURST_MINUTES`] minutes'
    /// worth. A client over it gets 429 for requests with a credential the
    /// server has not seen valid lately, without a database lookup.
    pub auth_failures_per_min: Option<f64>,
}

impl Limits {
    /// The defaults of public mode.
    pub fn public() -> Self {
        Limits {
            seats_per_org: Some(200),
            lists_per_org: Some(500),
            tokens_per_org: Some(1000),
            global_send_per_sec: Some(500.0),
            org_send_per_sec: Some(50.0),
            org_streams: Some(32),
            token_requests_per_sec: Some(200.0),
            org_requests_per_sec: Some(500.0),
            link_requests_per_hour: Some(20.0),
            registrations_per_hour: Some(10.0),
            org_webhooks_in_flight: Some(16),
            mirror_bytes: Some(256 * 1024 * 1024),
            org_storage_bytes: Some(1 << 30),
            org_queries: Some(1),
            queries: Some(8),
            auth_failures_per_min: Some(10.0),
        }
    }
}

/// An address range: `10.0.0.0/8`, `::1/128`, or a single address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl std::str::FromStr for Cidr {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let (a, p) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr = a
            .parse()
            .map_err(|_| format!("invalid address {a:?} in {s:?}"))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match p {
            None => max,
            Some(p) => p
                .parse::<u8>()
                .ok()
                .filter(|n| *n <= max)
                .ok_or_else(|| format!("invalid prefix length in {s:?}"))?,
        };
        Ok(Cidr { addr, prefix })
    }
}

impl Cidr {
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        match (canonical(self.addr), ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let bits = 32 - u32::from(self.prefix.min(32));
                let mask = if bits == 32 { 0 } else { u32::MAX << bits };
                u32::from(n) & mask == u32::from(a) & mask
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let bits = 128 - u32::from(self.prefix.min(128));
                let mask = if bits == 128 { 0 } else { u128::MAX << bits };
                u128::from(n) & mask == u128::from(a) & mask
            }
            _ => false,
        }
    }
}

/// IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) as IPv4.
pub fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => ip,
        },
        v4 => v4,
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub public: bool,
    /// Invite-code registration (on in public mode).
    pub registration: bool,
    pub limits: Limits,
    /// Peers whose `X-Forwarded-For`, `-Host` and `-Proto` are believed.
    pub trusted_proxies: Vec<Cidr>,
    /// Webhooks may target loopback, private, link-local and reserved
    /// addresses (self-hosted default); public mode refuses them unless
    /// this is set.
    pub webhook_private_targets: bool,
    /// Session cookies are always `Secure` (public mode), not only when the
    /// request came over HTTPS.
    pub secure_cookies: bool,
    /// Clear message bodies older than this many days.
    pub retention_days: Option<u32>,
    /// How often retention runs and how often streams of suspended or
    /// deleted orgs are looked for.
    pub retention_every: Duration,
    pub sweep_every: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            public: false,
            registration: false,
            limits: Limits::default(),
            trusted_proxies: Vec::new(),
            webhook_private_targets: true,
            secure_cookies: false,
            retention_days: None,
            retention_every: Duration::from_secs(600),
            sweep_every: Duration::from_secs(15),
        }
    }
}

impl Config {
    /// Public mode with its defaults.
    pub fn public() -> Self {
        Config {
            public: true,
            registration: true,
            limits: Limits::public(),
            webhook_private_targets: false,
            secure_cookies: true,
            ..Config::default()
        }
    }

    pub fn trusts(&self, ip: IpAddr) -> bool {
        self.trusted_proxies.iter().any(|c| c.contains(ip))
    }
}

/// Largest values of the org settings. `public` bounds apply in public
/// mode only; the others always (they are times, and keep arithmetic on
/// them in range).
pub struct SettingMax {
    pub name: &'static str,
    pub max: i64,
    pub public_only: bool,
}

pub const ORG_SETTING_MAX: &[SettingMax] = &[
    // The receiving orgs are protected by it: no org may raise it above
    // this, on any server.
    SettingMax {
        name: "new_threads_per_hour",
        max: amail_proto::limits::MAX_NEW_THREADS_PER_HOUR,
        public_only: false,
    },
    SettingMax {
        name: "send_rate_per_min",
        max: 600,
        public_only: true,
    },
    SettingMax {
        name: "send_burst",
        max: 600,
        public_only: true,
    },
    SettingMax {
        name: "max_recipients",
        max: 1000,
        public_only: true,
    },
    SettingMax {
        name: "max_thread_depth",
        max: 1000,
        public_only: true,
    },
    SettingMax {
        name: "ack_timeout_min",
        max: 10_080,
        public_only: false,
    },
    SettingMax {
        name: "focus_max_min",
        max: 10_080,
        public_only: false,
    },
    SettingMax {
        name: "digest_interval_min",
        max: 10_080,
        public_only: false,
    },
];

/// Names a seat or list may not take in public mode: they could pass for
/// the mail system, the server's operator or the org's user.
pub const RESERVED_SEAT_NAMES: &[&str] = &[
    "amail",
    "system",
    "admin",
    "administrator",
    "operator",
    "postmaster",
    "mailer-daemon",
    "noreply",
    "no-reply",
    "owner",
    "user",
    "human",
    "me",
    "security",
    "support",
    "abuse",
];

/// Names an org may not take in public mode.
pub const RESERVED_ORG_NAMES: &[&str] = &[
    "amail",
    "system",
    "admin",
    "administrator",
    "operator",
    "root",
    "postmaster",
    "security",
    "support",
    "abuse",
    "help",
    "official",
    "staff",
    "www",
    "api",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidrs() {
        let c: Cidr = "10.0.0.0/8".parse().unwrap();
        assert!(c.contains("10.1.2.3".parse().unwrap()));
        assert!(!c.contains("11.0.0.1".parse().unwrap()));
        assert!(c.contains("::ffff:10.0.0.1".parse().unwrap()));
        let one: Cidr = "127.0.0.1".parse().unwrap();
        assert!(one.contains("127.0.0.1".parse().unwrap()));
        assert!(!one.contains("127.0.0.2".parse().unwrap()));
        let v6: Cidr = "fd00::/8".parse().unwrap();
        assert!(v6.contains("fd12::1".parse().unwrap()));
        assert!(!v6.contains("fe80::1".parse().unwrap()));
        let all: Cidr = "0.0.0.0/0".parse().unwrap();
        assert!(all.contains("8.8.8.8".parse().unwrap()));
        assert!("10.0.0.0/33".parse::<Cidr>().is_err());
        assert!("nope".parse::<Cidr>().is_err());
    }
}
