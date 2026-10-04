//! Saved logins (`amail login`): one or more profiles, each a server URL
//! and a token, in `<config dir>/config.toml` (mode 0600). The config
//! directory is `$AMAIL_CONFIG_DIR`, else `$XDG_CONFIG_HOME/amail`, else
//! `~/.config/amail`.
//!
//! ```toml
//! current = "acme@mail.example.com"
//!
//! [profiles."acme@mail.example.com"]
//! url = "https://mail.example.com"
//! token = "amail_..."
//! org = "acme"
//! ```

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub url: String,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("AMAIL_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(d);
    }
    if let Some(d) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(d).join("amail");
    }
    amail_node::paths::home().join(".config/amail")
}

pub fn path() -> PathBuf {
    dir().join("config.toml")
}

impl Config {
    pub fn load() -> Result<Config> {
        let p = path();
        match std::fs::read_to_string(&p) {
            Ok(t) => toml::from_str(&t).with_context(|| format!("parsing {}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
        }
    }

    /// Write the file (mode 0600, its directory 0700).
    pub fn save(&self) -> Result<PathBuf> {
        let d = dir();
        std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
        }
        let p = path();
        amail_node::persist::write_atomic(&p, toml::to_string(self)?.as_bytes(), 0o600)
            .with_context(|| format!("writing {}", p.display()))?;
        Ok(p)
    }

    /// The profile in use: `name`, else the current one.
    pub fn get(&self, name: Option<&str>) -> Option<(&str, &Profile)> {
        let n = name.or(self.current.as_deref())?;
        self.profiles.get_key_value(n).map(|(k, v)| (k.as_str(), v))
    }

    /// Use `name` from now on.
    pub fn select(&mut self, name: &str) -> Result<()> {
        if !self.profiles.contains_key(name) {
            bail!(
                "no profile {name:?} (have: {})",
                self.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        self.current = Some(name.to_string());
        Ok(())
    }
}

/// The default profile name: `<org>@<host>`.
pub fn default_name(url: &str, org: &str) -> String {
    let host = url
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();
    if org.is_empty() {
        host
    } else {
        format!("{org}@{host}")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn names() {
        assert_eq!(
            super::default_name("https://mail.example.com/", "acme"),
            "acme@mail.example.com"
        );
        assert_eq!(
            super::default_name("http://127.0.0.1:8025", ""),
            "127.0.0.1:8025"
        );
    }
}
