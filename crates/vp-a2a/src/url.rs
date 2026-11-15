//! Strict loopback URL and listen-address parsing.
//!
//! The facade deliberately accepts literal loopback IPs only. Hostnames such
//! as `localhost` are rejected rather than resolved: a name can be redirected
//! by DNS, hosts files or search domains, and this facade must never become a
//! public listener or proxy by accident.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

use crate::error::{A2aError, A2aResult};
use crate::limits::MAX_URL_BYTES;

/// Address family used by a loopback URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum A2aScheme {
    Http,
    Https,
}

impl A2aScheme {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    const fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

/// A validated `http(s)://127.0.0.1:port` or `http(s)://[::1]:port` base URL.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct A2aUrl {
    scheme: A2aScheme,
    host: IpAddr,
    port: u16,
}

impl A2aUrl {
    /// Parse and validate a literal-loopback base URL.
    ///
    /// A base URL must have a root path. Query strings, fragments, user info,
    /// hostnames, port zero and every public address are rejected.
    pub fn parse(input: &str) -> A2aResult<Self> {
        if input.is_empty() || input.len() > MAX_URL_BYTES {
            return Err(A2aError::ConfigInvalid {
                reason: format!("agent URL must be 1..={MAX_URL_BYTES} bytes"),
            });
        }
        if input.trim() != input || input.bytes().any(|b| b.is_ascii_whitespace()) {
            return Err(A2aError::ConfigInvalid {
                reason: "agent URL must not contain whitespace".into(),
            });
        }
        if input.contains('@') {
            return Err(A2aError::ConfigInvalid {
                reason: "agent URL must not contain user info".into(),
            });
        }
        if input.contains('?') || input.contains('#') {
            return Err(A2aError::ConfigInvalid {
                reason: "agent URL must not contain a query or fragment".into(),
            });
        }

        let (scheme_text, rest) =
            input
                .split_once("://")
                .ok_or_else(|| A2aError::ConfigInvalid {
                    reason: "agent URL must include a scheme".into(),
                })?;
        let scheme = match scheme_text.to_ascii_lowercase().as_str() {
            "http" => A2aScheme::Http,
            "https" => A2aScheme::Https,
            _ => {
                return Err(A2aError::ConfigInvalid {
                    reason: "agent URL scheme must be http or https".into(),
                });
            }
        };
        if rest.is_empty() {
            return Err(A2aError::ConfigInvalid {
                reason: "agent URL must include a loopback host".into(),
            });
        }

        let (host, port_text) = if let Some(after_bracket) = rest.strip_prefix('[') {
            let (inside, after) =
                after_bracket
                    .split_once(']')
                    .ok_or_else(|| A2aError::ConfigInvalid {
                        reason: "unterminated IPv6 host".into(),
                    })?;
            let after = after.strip_suffix('/').unwrap_or(after);
            let port = match after {
                "" => None,
                value => {
                    let port = value
                        .strip_prefix(':')
                        .ok_or_else(|| A2aError::ConfigInvalid {
                            reason: "invalid characters after IPv6 host".into(),
                        })?;
                    Some(port)
                }
            };
            let address = inside
                .parse::<Ipv6Addr>()
                .map_err(|_| A2aError::LoopbackRequired {
                    address: format!("[{inside}]"),
                })?;
            (IpAddr::V6(address), port)
        } else {
            let (authority, path) = match rest.split_once('/') {
                Some((authority, path)) => (authority, Some(path)),
                None => (rest, None),
            };
            if let Some(path) = path
                && !path.is_empty()
            {
                return Err(A2aError::ConfigInvalid {
                    reason: "agent URL must use the root path".into(),
                });
            }
            if authority.matches(':').count() > 1 {
                return Err(A2aError::ConfigInvalid {
                    reason: "IPv6 agent URLs must be bracketed".into(),
                });
            }
            let (host, port) = match authority.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            };
            if host.is_empty() {
                return Err(A2aError::ConfigInvalid {
                    reason: "agent URL must include a loopback host".into(),
                });
            }
            let address = host
                .parse::<IpAddr>()
                .map_err(|_| A2aError::LoopbackRequired {
                    address: host.to_string(),
                })?;
            (address, port)
        };

        if !is_exact_loopback(host) {
            return Err(A2aError::LoopbackRequired {
                address: host.to_string(),
            });
        }

        let port = match port_text {
            Some(text) => text.parse::<u16>().map_err(|_| A2aError::ConfigInvalid {
                reason: "agent URL port must be a number in 1..=65535".into(),
            })?,
            None => scheme.default_port(),
        };
        if port == 0 {
            return Err(A2aError::ConfigInvalid {
                reason: "agent URL port must not be zero".into(),
            });
        }

        Ok(Self { scheme, host, port })
    }

    /// Construct a URL from an already-bound loopback address.
    pub fn for_server(address: SocketAddr) -> A2aResult<Self> {
        if address.port() == 0 {
            return Err(A2aError::ConfigInvalid {
                reason: "a bound server address must have a concrete port".into(),
            });
        }
        if !is_exact_loopback(address.ip()) {
            return Err(A2aError::LoopbackRequired {
                address: address.to_string(),
            });
        }
        Ok(Self {
            scheme: A2aScheme::Http,
            host: address.ip(),
            port: address.port(),
        })
    }

    pub const fn scheme(&self) -> A2aScheme {
        self.scheme
    }

    pub const fn host(&self) -> IpAddr {
        self.host
    }

    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Append an absolute path to this base URL.
    pub fn join(&self, path: &str) -> String {
        let prefix = self.to_string();
        if path.starts_with('/') {
            format!("{prefix}{path}")
        } else {
            format!("{prefix}/{path}")
        }
    }

    /// The canonical base URL without a trailing slash.
    pub fn base(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for A2aUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.host {
            IpAddr::V4(address) => {
                write!(f, "{}://{}:{}", self.scheme.as_str(), address, self.port)
            }
            IpAddr::V6(address) => {
                write!(f, "{}://[{}]:{}", self.scheme.as_str(), address, self.port)
            }
        }
    }
}

impl FromStr for A2aUrl {
    type Err = A2aError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// A validated loopback listen address. Port zero requests an ephemeral port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct A2aListenAddr {
    address: SocketAddr,
}

impl A2aListenAddr {
    /// Loopback IPv4, with `port == 0` selecting an ephemeral port.
    pub const fn loopback_v4(port: u16) -> Self {
        Self {
            address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        }
    }

    /// Loopback IPv6, with `port == 0` selecting an ephemeral port.
    pub const fn loopback_v6(port: u16) -> Self {
        Self {
            address: SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
        }
    }

    /// Validate a socket address and retain it only if it is exactly
    /// `127.0.0.1` or `::1`.
    pub fn from_socket_addr(address: SocketAddr) -> A2aResult<Self> {
        if !is_exact_loopback(address.ip()) {
            return Err(A2aError::LoopbackRequired {
                address: address.to_string(),
            });
        }
        Ok(Self { address })
    }

    pub const fn socket_addr(self) -> SocketAddr {
        self.address
    }

    pub const fn port(self) -> u16 {
        self.address.port()
    }
}

impl Default for A2aListenAddr {
    fn default() -> Self {
        Self::loopback_v4(0)
    }
}

impl fmt::Display for A2aListenAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.address.fmt(f)
    }
}

impl FromStr for A2aListenAddr {
    type Err = A2aError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let address = s
            .parse::<SocketAddr>()
            .map_err(|_| A2aError::ConfigInvalid {
                reason: "listen address must be a socket address".into(),
            })?;
        Self::from_socket_addr(address)
    }
}

/// True only for the two literal loopback addresses this facade supports.
pub fn is_exact_loopback(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address == Ipv4Addr::LOCALHOST,
        IpAddr::V6(address) => address == Ipv6Addr::LOCALHOST,
    }
}

/// True when a URL is a literal loopback HTTP(S) URL, including an Agent Card
/// endpoint below a path.
pub fn is_loopback_url(input: &str) -> bool {
    validate_loopback_http_url(input).is_ok()
}

/// Validate a loopback HTTP(S) endpoint that may include a path. Agent Card
/// advertisements may point at an endpoint below a prefix; the facade still
/// never uses that path for its own client requests unless the operator passes
/// it as a base URL (which [`A2aUrl`] keeps root-only).
pub fn validate_loopback_http_url(input: &str) -> A2aResult<()> {
    if input.is_empty() || input.len() > MAX_URL_BYTES {
        return Err(A2aError::ConfigInvalid {
            reason: format!("agent URL must be 1..={MAX_URL_BYTES} bytes"),
        });
    }
    if input.trim() != input || input.bytes().any(|b| b.is_ascii_whitespace()) {
        return Err(A2aError::ConfigInvalid {
            reason: "agent URL must not contain whitespace".into(),
        });
    }
    if input.contains('@') || input.contains('?') || input.contains('#') {
        return Err(A2aError::ConfigInvalid {
            reason: "agent URL must not contain user info, a query, or a fragment".into(),
        });
    }
    let (scheme, rest) = input
        .split_once("://")
        .ok_or_else(|| A2aError::ConfigInvalid {
            reason: "agent URL must include a scheme".into(),
        })?;
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, Some(path)),
        None => (rest, None),
    };
    let base = format!("{scheme}://{authority}");
    A2aUrl::parse(&base)?;
    if let Some(path) = path
        && path.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(A2aError::ConfigInvalid {
            reason: "agent URL path contains control bytes".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ipv4_and_ipv6_loopback() {
        let v4 = A2aUrl::parse("http://127.0.0.1:43121").expect("valid v4 URL");
        assert_eq!(v4.host(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(v4.port(), 43121);
        assert_eq!(v4.to_string(), "http://127.0.0.1:43121");

        let v6 = A2aUrl::parse("http://[::1]:43122/").expect("valid v6 URL");
        assert_eq!(v6.host(), IpAddr::V6(Ipv6Addr::LOCALHOST));
        assert_eq!(v6.port(), 43122);
        assert_eq!(v6.to_string(), "http://[::1]:43122");
        assert_eq!(v6.join("/a2a"), "http://[::1]:43122/a2a");
    }

    #[test]
    fn applies_default_ports() {
        let url = A2aUrl::parse("http://127.0.0.1").expect("default port");
        assert_eq!(url.port(), 80);
        let url = A2aUrl::parse("https://[::1]").expect("default TLS port");
        assert_eq!(url.port(), 443);
    }

    #[test]
    fn rejects_public_hosts_and_hostnames() {
        for value in [
            "http://0.0.0.0:8080",
            "http://192.168.1.10:8080",
            "http://10.0.0.1:8080",
            "http://localhost:8080",
            "http://example.com:8080",
            "http://127.0.0.2:8080",
            "http://[::]:8080",
            "http://[fe80::1]:8080",
        ] {
            let error = A2aUrl::parse(value).expect_err(value);
            assert_eq!(error.code(), crate::error::A2aErrorCode::LoopbackRequired);
        }
    }

    #[test]
    fn rejects_public_shapes() {
        for value in [
            "127.0.0.1:8080",
            "ftp://127.0.0.1:8080",
            "http://user@127.0.0.1:8080",
            "http://127.0.0.1:8080/path",
            "http://127.0.0.1:8080?query=1",
            "http://127.0.0.1:8080#fragment",
            "http://127.0.0.1:0",
            "http://::1:8080",
            "http://127.0.0.1:not-a-port",
            " http://127.0.0.1:8080",
        ] {
            assert!(A2aUrl::parse(value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn listen_addresses_are_literal_loopback() {
        let address =
            A2aListenAddr::from_socket_addr("127.0.0.1:0".parse().expect("socket address"))
                .expect("loopback");
        assert_eq!(address.port(), 0);
        assert!(
            A2aListenAddr::from_socket_addr("0.0.0.0:1".parse().expect("socket address")).is_err()
        );
        assert!(
            A2aListenAddr::from_socket_addr("[::]:1".parse().expect("socket address")).is_err()
        );
        assert!(
            A2aListenAddr::from_socket_addr("127.0.0.2:1".parse().expect("socket address"))
                .is_err()
        );
    }

    #[test]
    fn endpoint_urls_may_have_paths_but_base_urls_may_not() {
        assert!(is_loopback_url("http://127.0.0.1:8080/a2a"));
        assert!(validate_loopback_http_url("http://127.0.0.1:8080/a2a").is_ok());
        assert!(A2aUrl::parse("http://127.0.0.1:8080/a2a").is_err());
        assert!(!is_loopback_url("http://example.com:8080/a2a"));
    }

    #[test]
    fn server_url_requires_concrete_port() {
        let address: SocketAddr = "127.0.0.1:0".parse().expect("socket address");
        assert!(A2aUrl::for_server(address).is_err());
        let address: SocketAddr = "127.0.0.1:9000".parse().expect("socket address");
        assert_eq!(
            A2aUrl::for_server(address).expect("loopback").to_string(),
            "http://127.0.0.1:9000"
        );
    }
}
