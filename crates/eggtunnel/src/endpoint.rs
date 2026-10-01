//! Canonical `host:port` endpoint representation.
//!
//! This module is the single semantic owner of endpoint shape for the client
//! runtime. Configuration adapters (including the standalone CLI) must lower
//! their text into [`Endpoint`] rather than re-implementing host/port parsing.
//!
//! Accepted shapes:
//!
//! - `host:port` — DNS-like host text or an IPv4 literal;
//! - `[ipv6]:port` — a bracketed IPv6 literal.
//!
//! Rejected shapes include a missing or zero port, an empty host, an unbracketed
//! IPv6 literal, and any host text that would be ambiguous once the endpoint is
//! embedded in a URL authority (`/`, `?`, `#`, `@`, `[`, `]`), whitespace, or
//! control characters.

/// Why a `host:port` endpoint was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EndpointError {
    #[error("endpoint must use host:port syntax")]
    MissingPort,
    #[error("endpoint host must not be empty")]
    EmptyHost,
    #[error("endpoint must use [address]:port syntax for IPv6 literals")]
    UnbracketedIpv6,
    #[error("endpoint host contains characters that are ambiguous in a URL authority")]
    AmbiguousHost,
    #[error("endpoint port must be a decimal number in 1..=65535")]
    InvalidPort,
}

/// A validated `host:port` endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoint {
    text: String,
    host: String,
    port: u16,
}

impl Endpoint {
    /// Parse and validate one endpoint.
    pub fn parse(value: &str) -> Result<Self, EndpointError> {
        let (host, port_text) = if let Some(rest) = value.strip_prefix('[') {
            let Some(end) = rest.find(']') else {
                return Err(EndpointError::MissingPort);
            };
            let host = &rest[..end];
            let Some(port_text) = rest[end + 1..].strip_prefix(':') else {
                return Err(EndpointError::MissingPort);
            };
            if host.is_empty() {
                return Err(EndpointError::EmptyHost);
            }
            if host.parse::<std::net::Ipv6Addr>().is_err() {
                return Err(EndpointError::AmbiguousHost);
            }
            (host, port_text)
        } else {
            let Some((host, port_text)) = value.rsplit_once(':') else {
                return Err(EndpointError::MissingPort);
            };
            if host.is_empty() {
                return Err(EndpointError::EmptyHost);
            }
            // A bare `::1:443` is ambiguous: only the bracketed form is a
            // host/port split.
            if host.contains(':') {
                return Err(EndpointError::UnbracketedIpv6);
            }
            (host, port_text)
        };
        validate_host(host)?;
        if port_text.is_empty() || !port_text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(EndpointError::InvalidPort);
        }
        let port: u16 = port_text.parse().map_err(|_| EndpointError::InvalidPort)?;
        if port == 0 {
            return Err(EndpointError::InvalidPort);
        }
        Ok(Self {
            text: value.to_owned(),
            host: host.to_owned(),
            port,
        })
    }

    /// Host text without IPv6 brackets.
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The original text, suitable for reconnecting and for URL authority use.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// WebSocket URL authority and root path for this validated endpoint.
    #[cfg(feature = "websocket-client")]
    pub(crate) fn websocket_url(&self) -> String {
        format!("wss://{}/", self.text)
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

fn validate_host(host: &str) -> Result<(), EndpointError> {
    // One validator shared with the wire `TcpTarget`: a host shape accepted
    // here is exactly the shape a peer may register.
    eggtunnel_proto::validate_target_host(host).map_err(|_| EndpointError::AmbiguousHost)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_dns_ipv4_and_bracketed_ipv6_forms() {
        for value in [
            "localhost:443",
            "example.internal:1",
            "127.0.0.1:8443",
            "[::1]:443",
            "[2001:db8::1]:65535",
        ] {
            let endpoint = Endpoint::parse(value).unwrap();
            assert_eq!(endpoint.as_str(), value);
            assert!(!endpoint.host().is_empty());
            assert_ne!(endpoint.port(), 0);
        }
        assert_eq!(Endpoint::parse("[::1]:443").unwrap().host(), "::1");
        assert_eq!(Endpoint::parse("example.com:443").unwrap().port(), 443);
    }

    #[test]
    fn rejects_malformed_zero_port_and_ambiguous_input() {
        for value in [
            "",
            "example.com",
            "example.com:",
            "example.com:0",
            "example.com:65536",
            "example.com:-1",
            "example.com:443x",
            ":443",
            "exa mple.com:443",
            "example.com:44\u{0003}3",
            "::1:443",
            "[::1]",
            "[::1]443",
            "[not-an-address]:443",
            "example.com/evil:443",
            "user@example.com:443",
            "example.com?x:443",
            "[::1]:0",
        ] {
            assert!(
                Endpoint::parse(value).is_err(),
                "endpoint {value:?} must be rejected"
            );
        }
    }

    #[cfg(feature = "websocket-client")]
    #[test]
    fn websocket_url_has_root_path_and_brackets_ipv6_authority() {
        assert_eq!(
            Endpoint::parse("[::1]:443").unwrap().websocket_url(),
            "wss://[::1]:443/"
        );
        assert_eq!(
            Endpoint::parse("example.com:443").unwrap().websocket_url(),
            "wss://example.com:443/"
        );
    }
}
