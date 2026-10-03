use std::fmt;
use std::str::FromStr;

/// A mail address: `name` (a seat or list in the sender's org) or `name@org`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Address {
    pub name: String,
    pub org: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressError(pub String);

impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AddressError {}

/// Names of seats, lists and orgs match `[a-z0-9][a-z0-9._-]{0,62}`.
pub fn valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 63
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b[1..].iter().all(|&c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

impl Address {
    pub fn local(name: impl Into<String>) -> Self {
        Address {
            name: name.into(),
            org: None,
        }
    }

    /// Resolve the org part against the given home org: `None` means home.
    pub fn org_or<'a>(&'a self, home: &'a str) -> &'a str {
        self.org.as_deref().unwrap_or(home)
    }

    /// Render relative to `viewer_org`: drop the org part when it is the
    /// viewer's own org.
    pub fn relative_to(&self, viewer_org: &str) -> String {
        match &self.org {
            Some(org) if org != viewer_org => format!("{}@{}", self.name, org),
            _ => self.name.clone(),
        }
    }
}

impl FromStr for Address {
    type Err = AddressError;

    fn from_str(s: &str) -> Result<Self, AddressError> {
        let s = s.trim();
        let (name, org) = match s.split_once('@') {
            Some((n, o)) => (n, Some(o)),
            None => (s, None),
        };
        if !valid_name(name) {
            return Err(AddressError(format!("invalid name in address {s:?}")));
        }
        if let Some(o) = org
            && !valid_name(o)
        {
            return Err(AddressError(format!("invalid org in address {s:?}")));
        }
        Ok(Address {
            name: name.to_string(),
            org: org.map(str::to_string),
        })
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.org {
            Some(o) => write!(f, "{}@{}", self.name, o),
            None => f.write_str(&self.name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("a"));
        assert!(valid_name("0abc.def_g-h"));
        assert!(valid_name(&"a".repeat(63)));
        assert!(!valid_name(&"a".repeat(64)));
        assert!(!valid_name(""));
        assert!(!valid_name("-a"));
        assert!(!valid_name(".a"));
        assert!(!valid_name("Abc"));
        assert!(!valid_name("a b"));
        assert!(!valid_name("a@b"));
    }

    #[test]
    fn parse_and_render() {
        let a: Address = "alice".parse().unwrap();
        assert_eq!(a, Address::local("alice"));
        let b: Address = "bob@acme".parse().unwrap();
        assert_eq!(b.org.as_deref(), Some("acme"));
        assert_eq!(b.to_string(), "bob@acme");
        assert_eq!(b.relative_to("acme"), "bob");
        assert_eq!(b.relative_to("other"), "bob@acme");
        assert!("bob@".parse::<Address>().is_err());
        assert!("@acme".parse::<Address>().is_err());
        assert!("a@b@c".parse::<Address>().is_err());
    }
}
