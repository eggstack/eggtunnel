#![forbid(unsafe_code)]
//! Runtime-neutral, bounded native Eggtunnel wire protocol.
//!
//! A frame is `ETUN` + major (u16 BE) + minor (u16 BE) + message ID (u16 BE)
//! + bounded payload length (u32 BE) + one postcard payload.
//!
//! The payload maximum is checked before a frame payload is copied or deserialized.

use core::fmt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAGIC: [u8; 4] = *b"ETUN";
pub const HEADER_LEN: usize = 14;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_NAME_BYTES: usize = 128;
pub const MAX_DIAGNOSTIC_BYTES: usize = 256;
pub const MAX_CAPABILITIES: usize = 32;
pub const MAX_TARGET_HOST_BYTES: usize = 253;
pub const MAX_AUTH_TOKEN_BYTES: usize = 4096;
pub const PROTOCOL_MAJOR: u16 = 1;
pub const PROTOCOL_MINOR: u16 = 1;

/// Capability 1 (ADR-0002): correlated registration rejection. When
/// negotiated, registration failures arrive as `RegisterReject` (message
/// 15) instead of the generic `Error`, so multiple dynamic registrations
/// may be safely in flight.
pub const CAPABILITY_REGISTER_REJECT: u16 = 1;
/// Capability 2 (ADR-0002): drain deadline semantics. When negotiated,
/// `Drain.deadline_ms` is a relative grace duration honoured as
/// `min(peer, local ceiling)`; otherwise receivers use local-only timing.
pub const CAPABILITY_DRAIN_DEADLINE: u16 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    pub const CURRENT: Self = Self {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
    };
}

#[derive(Clone, Copy, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct SessionId(pub [u8; 16]);

impl SessionId {
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes))
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A Session ID is a capability in `DataHello`; it is redacted exactly
        // like `ConnectionId` so no prefix is recoverable from logs.
        f.write_str("SessionId([REDACTED])")
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ServiceId(pub u64);

#[derive(Clone, Copy, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ConnectionId(pub [u8; 16]);

impl ConnectionId {
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes))
    }

    /// Constant-time byte comparison, useful when IDs are treated as capabilities.
    pub fn constant_time_eq(&self, other: &Self) -> bool {
        self.0
            .iter()
            .zip(other.0.iter())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
    }
}

impl fmt::Debug for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConnectionId([REDACTED])")
    }
}

#[derive(Clone, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct ServiceName(String);

impl ServiceName {
    pub fn new(value: impl Into<String>) -> Result<Self, ProtocolError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_NAME_BYTES
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return Err(ProtocolError::InvalidName);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ServiceName {
    type Error = ProtocolError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Debug for ServiceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for ServiceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RequestedBind {
    Loopback { port: u16 },
    Ip { address: [u8; 16], port: u16 },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EffectiveBind {
    pub address: [u8; 16],
    pub port: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "WireTcpTarget")]
pub struct TcpTarget {
    host: String,
    port: u16,
}

#[derive(Deserialize)]
struct WireTcpTarget {
    host: String,
    port: u16,
}

impl TryFrom<WireTcpTarget> for TcpTarget {
    type Error = ProtocolError;
    fn try_from(value: WireTcpTarget) -> Result<Self, Self::Error> {
        Self::new(value.host, value.port)
    }
}

impl TcpTarget {
    pub fn new(host: impl Into<String>, port: u16) -> Result<Self, ProtocolError> {
        let host = host.into();
        validate_target_host(&host)?;
        if port == 0 {
            return Err(ProtocolError::InvalidTarget);
        }
        Ok(Self { host, port })
    }
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> u16 {
        self.port
    }
}

/// The single host-text validator for every Eggtunnel surface that carries a
/// bare host: the wire `TcpTarget` and the client `host:port` endpoint both
/// lower their text through this, so a peer cannot register a Service whose
/// target host shape local configuration would reject.
///
/// Rejects empty text, text over [`MAX_TARGET_HOST_BYTES`], control characters,
/// whitespace, and the characters that would be ambiguous once the host is
/// embedded in a URL authority. A colon is accepted only for a valid IPv6
/// literal; anything else containing `:` (e.g. `":"`, `":::"`, `"foo:bar"`)
/// fails closed. Unicode format characters (`Cf`, e.g. bidi overrides) are
/// rejected alongside `Cc`/`Zl`/`Zp` (the latter two already covered by
/// `is_whitespace`/`is_control`, checked explicitly for clarity).
pub fn validate_target_host(host: &str) -> Result<(), ProtocolError> {
    if host.is_empty()
        || host.len() > MAX_TARGET_HOST_BYTES
        || host.chars().any(|c| {
            c.is_whitespace() || c.is_control() || is_format_char(c) || "/?#@[]\\\"'<>".contains(c)
        })
    {
        return Err(ProtocolError::InvalidTarget);
    }
    if host.contains(':') && host.parse::<core::net::Ipv6Addr>().is_err() {
        return Err(ProtocolError::InvalidTarget);
    }
    Ok(())
}

/// Unicode format characters (`Cf` general category + `Zl`/`Zp` separators):
/// bidi overrides, zero-width joiners, word joiners, BOM, etc. These pass
/// `is_whitespace`/`is_control` checks but enable spoofing, so they fail
/// closed here and in [`BoundedDiagnostic`].
fn is_format_char(c: char) -> bool {
    matches!(c,
        '\u{00AD}'
        | '\u{200B}'..='\u{200F}'
        | '\u{2028}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}'
        | '\u{FEFF}')
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Vec<u16>")]
pub struct Capabilities(Vec<u16>);

impl Capabilities {
    pub fn new(ids: Vec<u16>) -> Result<Self, ProtocolError> {
        if ids.len() > MAX_CAPABILITIES {
            return Err(ProtocolError::InvalidPayload);
        }
        // Normalized set semantics: sorted + deduped at construction so
        // `new(vec![2,1]) == new(vec![1,2])`. `intersect()` was already
        // deterministic; now construction is too.
        let mut ids = ids;
        ids.sort_unstable();
        ids.dedup();
        Ok(Self(ids))
    }

    /// The full capability set this implementation supports, emitted in
    /// deterministic (sorted, unique) order.
    pub fn supported() -> Self {
        Self(vec![CAPABILITY_REGISTER_REJECT, CAPABILITY_DRAIN_DEADLINE])
    }

    /// Bilateral intersection: only capabilities present in both lists are
    /// negotiated. Unknown IDs are ignored; duplicates collapse. Emission
    /// order is deterministic (sorted).
    pub fn intersect(&self, other: &Self) -> Self {
        let mut ids: Vec<u16> = self
            .0
            .iter()
            .filter(|id| other.0.contains(id))
            .copied()
            .collect();
        ids.sort_unstable();
        ids.dedup();
        Self(ids)
    }

    pub fn has(&self, id: u16) -> bool {
        self.0.contains(&id)
    }

    pub fn as_slice(&self) -> &[u16] {
        &self.0
    }
}

impl TryFrom<Vec<u16>> for Capabilities {
    type Error = ProtocolError;
    fn try_from(ids: Vec<u16>) -> Result<Self, Self::Error> {
        Self::new(ids)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct BoundedDiagnostic(String);

impl BoundedDiagnostic {
    pub fn new(text: impl Into<String>) -> Result<Self, ProtocolError> {
        let text = text.into();
        if text.len() > MAX_DIAGNOSTIC_BYTES {
            return Err(ProtocolError::InvalidPayload);
        }
        // Bound charset as well as length: controls/newlines/ANSI/bidi
        // enable log injection/spoofing in any sink that logs `as_str()`
        // verbatim. Fail closed on `Cc`/`Cf`/`Zl`/`Zp` (`ESC` is `Cc`).
        if text.chars().any(|c| c.is_control() || is_format_char(c)) {
            return Err(ProtocolError::InvalidPayload);
        }
        Ok(Self(text))
    }
    /// Raw diagnostic text. Sinks must not log this verbatim without
    /// sanitization: construction rejects controls/format chars, but defense
    /// in depth still applies at the logging layer.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BoundedDiagnostic {
    type Error = ProtocolError;
    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::new(text)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum MessageType {
    ClientHello = 1,
    ServerHello = 2,
    Auth = 3,
    AuthOk = 4,
    RegisterService = 5,
    RegisterAck = 6,
    UnregisterService = 7,
    Open = 8,
    OpenReject = 9,
    Ping = 10,
    Pong = 11,
    Drain = 12,
    Error = 13,
    DataHello = 14,
    RegisterReject = 15,
}

impl TryFrom<u16> for MessageType {
    type Error = ProtocolError;
    fn try_from(value: u16) -> Result<Self, ProtocolError> {
        Ok(match value {
            1 => Self::ClientHello,
            2 => Self::ServerHello,
            3 => Self::Auth,
            4 => Self::AuthOk,
            5 => Self::RegisterService,
            6 => Self::RegisterAck,
            7 => Self::UnregisterService,
            8 => Self::Open,
            9 => Self::OpenReject,
            10 => Self::Ping,
            11 => Self::Pong,
            12 => Self::Drain,
            13 => Self::Error,
            14 => Self::DataHello,
            15 => Self::RegisterReject,
            _ => return Err(ProtocolError::UnknownMessage(value)),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClientHello {
    pub version: ProtocolVersion,
    pub capabilities: Capabilities,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServerHello {
    pub version: ProtocolVersion,
    pub capabilities: Capabilities,
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Auth {
    #[serde(deserialize_with = "bounded_bytes")]
    token: Vec<u8>,
}

impl Auth {
    pub fn new(token: Vec<u8>) -> Result<Self, ProtocolError> {
        if token.len() > MAX_AUTH_TOKEN_BYTES {
            return Err(ProtocolError::InvalidPayload);
        }
        Ok(Self { token })
    }

    pub fn token(&self) -> &[u8] {
        &self.token
    }
}

impl Drop for Auth {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.token.zeroize();
    }
}

fn bounded_bytes<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Vec::<u8>::deserialize(deserializer)?;
    if value.len() > MAX_AUTH_TOKEN_BYTES {
        return Err(serde::de::Error::custom("token exceeds maximum"));
    }
    Ok(value)
}
impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Auth")
            .field("token", &"[REDACTED]")
            .finish()
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuthOk {
    pub session_id: SessionId,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegisterService {
    pub service_id: ServiceId,
    pub name: ServiceName,
    pub requested_bind: RequestedBind,
    pub target: TcpTarget,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegisterAck {
    pub service_id: ServiceId,
    pub effective_bind: EffectiveBind,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UnregisterService {
    pub service_id: ServiceId,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Open {
    pub service_id: ServiceId,
    pub connection_id: ConnectionId,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OpenReject {
    pub connection_id: ConnectionId,
    pub code: u16,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Ping {
    pub nonce: u64,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Pong {
    pub nonce: u64,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Drain {
    pub deadline_ms: u32,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ErrorMessage {
    pub code: u16,
    pub diagnostic: BoundedDiagnostic,
}
/// Correlated registration rejection (capability 1, ADR-0002). Unlike the
/// generic `Error`, this carries the `ServiceId`, so the client can match
/// the failure to one of several in-flight registration transactions.
/// The numeric `code` reuses the registration-category vocabulary also
/// used with generic `Error` responses.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegisterReject {
    pub service_id: ServiceId,
    pub code: u16,
    pub diagnostic: BoundedDiagnostic,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DataHello {
    pub session_id: SessionId,
    pub service_id: ServiceId,
    pub connection_id: ConnectionId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    ClientHello(ClientHello),
    ServerHello(ServerHello),
    Auth(Auth),
    AuthOk(AuthOk),
    RegisterService(RegisterService),
    RegisterAck(RegisterAck),
    UnregisterService(UnregisterService),
    Open(Open),
    OpenReject(OpenReject),
    Ping(Ping),
    Pong(Pong),
    Drain(Drain),
    Error(ErrorMessage),
    DataHello(DataHello),
    RegisterReject(RegisterReject),
}

impl Message {
    pub fn kind(&self) -> MessageType {
        match self {
            Self::ClientHello(_) => MessageType::ClientHello,
            Self::ServerHello(_) => MessageType::ServerHello,
            Self::Auth(_) => MessageType::Auth,
            Self::AuthOk(_) => MessageType::AuthOk,
            Self::RegisterService(_) => MessageType::RegisterService,
            Self::RegisterAck(_) => MessageType::RegisterAck,
            Self::UnregisterService(_) => MessageType::UnregisterService,
            Self::Open(_) => MessageType::Open,
            Self::OpenReject(_) => MessageType::OpenReject,
            Self::Ping(_) => MessageType::Ping,
            Self::Pong(_) => MessageType::Pong,
            Self::Drain(_) => MessageType::Drain,
            Self::Error(_) => MessageType::Error,
            Self::DataHello(_) => MessageType::DataHello,
            Self::RegisterReject(_) => MessageType::RegisterReject,
        }
    }
    fn encode_payload(&self) -> Result<Vec<u8>, ProtocolError> {
        macro_rules! enc {
            ($v:expr) => {
                postcard::to_allocvec($v).map_err(|_| ProtocolError::InvalidPayload)?
            };
        }
        Ok(match self {
            Self::ClientHello(v) => enc!(v),
            Self::ServerHello(v) => enc!(v),
            Self::Auth(v) => enc!(v),
            Self::AuthOk(v) => enc!(v),
            Self::RegisterService(v) => enc!(v),
            Self::RegisterAck(v) => enc!(v),
            Self::UnregisterService(v) => enc!(v),
            Self::Open(v) => enc!(v),
            Self::OpenReject(v) => enc!(v),
            Self::Ping(v) => enc!(v),
            Self::Pong(v) => enc!(v),
            Self::Drain(v) => enc!(v),
            Self::Error(v) => enc!(v),
            Self::DataHello(v) => enc!(v),
            Self::RegisterReject(v) => enc!(v),
        })
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProtocolError {
    #[error("invalid protocol magic")]
    InvalidMagic,
    #[error("unsupported protocol version {0}.{1}")]
    UnsupportedVersion(u16, u16),
    #[error("unknown message type {0}")]
    UnknownMessage(u16),
    #[error("truncated frame")]
    TruncatedFrame,
    #[error("frame exceeds maximum size")]
    FrameTooLarge,
    #[error("malformed protocol payload")]
    InvalidPayload,
    #[error("invalid service name")]
    InvalidName,
    #[error("invalid bind specification")]
    InvalidBind,
    #[error("invalid target descriptor")]
    InvalidTarget,
    #[error("unexpected message for protocol state")]
    UnexpectedMessage,
}

pub fn encode_frame(message: &Message) -> Result<Vec<u8>, ProtocolError> {
    let payload = message.encode_payload()?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    let payload_len = u32::try_from(payload.len()).map_err(|_| ProtocolError::FrameTooLarge)?;
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&PROTOCOL_MAJOR.to_be_bytes());
    out.extend_from_slice(&PROTOCOL_MINOR.to_be_bytes());
    out.extend_from_slice(&(message.kind() as u16).to_be_bytes());
    out.extend_from_slice(&payload_len.to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Decode exactly one frame; bytes after that frame are left for the caller.
/// `TruncatedFrame` means the caller should read more bytes.
pub fn decode_frame(input: &[u8]) -> Result<(Message, usize), ProtocolError> {
    if input.len() < HEADER_LEN {
        return Err(ProtocolError::TruncatedFrame);
    }
    if input[..4] != MAGIC {
        return Err(ProtocolError::InvalidMagic);
    }
    let major = u16::from_be_bytes([input[4], input[5]]);
    let minor = u16::from_be_bytes([input[6], input[7]]);
    if major != PROTOCOL_MAJOR {
        return Err(ProtocolError::UnsupportedVersion(major, minor));
    }
    let kind = MessageType::try_from(u16::from_be_bytes([input[8], input[9]]))?;
    let len = u32::from_be_bytes([input[10], input[11], input[12], input[13]]) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    let total = HEADER_LEN
        .checked_add(len)
        .ok_or(ProtocolError::FrameTooLarge)?;
    if input.len() < total {
        return Err(ProtocolError::TruncatedFrame);
    }
    let payload = &input[HEADER_LEN..total];
    macro_rules! dec {
        ($ty:ty, $variant:ident) => {{
            let (value, trailing): ($ty, &[u8]) =
                postcard::take_from_bytes(payload).map_err(|_| ProtocolError::InvalidPayload)?;
            if !trailing.is_empty() {
                return Err(ProtocolError::InvalidPayload);
            }
            Message::$variant(value)
        }};
    }
    let message = match kind {
        MessageType::ClientHello => dec!(ClientHello, ClientHello),
        MessageType::ServerHello => dec!(ServerHello, ServerHello),
        MessageType::Auth => dec!(Auth, Auth),
        MessageType::AuthOk => dec!(AuthOk, AuthOk),
        MessageType::RegisterService => dec!(RegisterService, RegisterService),
        MessageType::RegisterAck => dec!(RegisterAck, RegisterAck),
        MessageType::UnregisterService => dec!(UnregisterService, UnregisterService),
        MessageType::Open => dec!(Open, Open),
        MessageType::OpenReject => dec!(OpenReject, OpenReject),
        MessageType::Ping => dec!(Ping, Ping),
        MessageType::Pong => dec!(Pong, Pong),
        MessageType::Drain => dec!(Drain, Drain),
        MessageType::Error => dec!(ErrorMessage, Error),
        MessageType::DataHello => dec!(DataHello, DataHello),
        MessageType::RegisterReject => dec!(RegisterReject, RegisterReject),
    };
    Ok((message, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_messages() -> Vec<Message> {
        let name = ServiceName::new("web-main").unwrap();
        let id = ConnectionId([7; 16]);
        vec![
            Message::ClientHello(ClientHello {
                version: ProtocolVersion::CURRENT,
                capabilities: Capabilities::default(),
            }),
            Message::ServerHello(ServerHello {
                version: ProtocolVersion::CURRENT,
                capabilities: Capabilities::default(),
            }),
            Message::Auth(Auth::new(b"secret".to_vec()).unwrap()),
            Message::AuthOk(AuthOk {
                session_id: SessionId([1; 16]),
            }),
            Message::RegisterService(RegisterService {
                service_id: ServiceId(1),
                name,
                requested_bind: RequestedBind::Loopback { port: 0 },
                target: TcpTarget::new("127.0.0.1", 8080).unwrap(),
            }),
            Message::RegisterAck(RegisterAck {
                service_id: ServiceId(1),
                effective_bind: EffectiveBind {
                    address: [0; 16],
                    port: 80,
                },
            }),
            Message::UnregisterService(UnregisterService {
                service_id: ServiceId(1),
            }),
            Message::Open(Open {
                service_id: ServiceId(1),
                connection_id: id,
            }),
            Message::OpenReject(OpenReject {
                connection_id: id,
                code: 1,
            }),
            Message::Ping(Ping { nonce: 9 }),
            Message::Pong(Pong { nonce: 9 }),
            Message::Drain(Drain { deadline_ms: 1000 }),
            Message::Error(ErrorMessage {
                code: 2,
                diagnostic: BoundedDiagnostic::new("denied").unwrap(),
            }),
            Message::DataHello(DataHello {
                session_id: SessionId([1; 16]),
                service_id: ServiceId(1),
                connection_id: id,
            }),
            Message::RegisterReject(RegisterReject {
                service_id: ServiceId(2),
                code: 1,
                diagnostic: BoundedDiagnostic::new("duplicate").unwrap(),
            }),
        ]
    }

    #[test]
    fn every_message_round_trips_and_concatenation_is_exact() {
        let mut joined = Vec::new();
        for message in sample_messages() {
            let encoded = encode_frame(&message).unwrap();
            let (decoded, used) = decode_frame(&encoded).unwrap();
            assert_eq!(decoded, message);
            assert_eq!(used, encoded.len());
            joined.extend(encoded);
        }
        let mut rest = joined.as_slice();
        let mut count = 0;
        while !rest.is_empty() {
            let (_, used) = decode_frame(rest).unwrap();
            rest = &rest[used..];
            count += 1;
        }
        assert_eq!(count, sample_messages().len());
    }

    #[test]
    fn documented_wire_version_and_message_ids_are_pinned() {
        // Guard matching docs/PROTOCOL.md: wire version 1.1 (major 1 keeps
        // the 1.0 incompatibility boundary) and the stable numeric message
        // IDs. Any change here is a compatibility event and must update the
        // protocol document alongside the constants.
        assert_eq!(PROTOCOL_MAJOR, 1);
        assert_eq!(PROTOCOL_MINOR, 1);
        assert_eq!(
            ProtocolVersion::CURRENT,
            ProtocolVersion { major: 1, minor: 1 }
        );
        let ids = [
            (MessageType::ClientHello, 1u16),
            (MessageType::ServerHello, 2),
            (MessageType::Auth, 3),
            (MessageType::AuthOk, 4),
            (MessageType::RegisterService, 5),
            (MessageType::RegisterAck, 6),
            (MessageType::UnregisterService, 7),
            (MessageType::Open, 8),
            (MessageType::OpenReject, 9),
            (MessageType::Ping, 10),
            (MessageType::Pong, 11),
            (MessageType::Drain, 12),
            (MessageType::Error, 13),
            (MessageType::DataHello, 14),
            (MessageType::RegisterReject, 15),
        ];
        for (message_type, expected) in ids {
            assert_eq!(message_type as u16, expected);
            assert_eq!(MessageType::try_from(expected).unwrap(), message_type);
        }
        assert!(MessageType::try_from(16).is_err());
        assert!(MessageType::try_from(0).is_err());
    }

    #[test]
    fn capability_registry_is_pinned_and_intersection_is_a_set() {
        // ADR-0002 capability IDs. Never silently reassign.
        assert_eq!(CAPABILITY_REGISTER_REJECT, 1);
        assert_eq!(CAPABILITY_DRAIN_DEADLINE, 2);
        let supported = Capabilities::supported();
        assert_eq!(
            supported.as_slice(),
            &[CAPABILITY_REGISTER_REJECT, CAPABILITY_DRAIN_DEADLINE]
        );
        // Intersection only: extras on either side never negotiate.
        let client = Capabilities::new(vec![CAPABILITY_REGISTER_REJECT, 9]).unwrap();
        let server = Capabilities::new(vec![
            CAPABILITY_DRAIN_DEADLINE,
            CAPABILITY_REGISTER_REJECT,
            CAPABILITY_REGISTER_REJECT,
        ])
        .unwrap();
        let negotiated = client.intersect(&server);
        assert_eq!(negotiated.as_slice(), &[CAPABILITY_REGISTER_REJECT]);
        assert!(negotiated.has(CAPABILITY_REGISTER_REJECT));
        assert!(!negotiated.has(CAPABILITY_DRAIN_DEADLINE));
        assert!(!negotiated.has(9));
        // Empty capabilities (a 1.0 peer) negotiate nothing.
        assert!(
            Capabilities::default()
                .intersect(&supported)
                .as_slice()
                .is_empty()
        );
        assert!(
            supported
                .intersect(&Capabilities::default())
                .as_slice()
                .is_empty()
        );
        // Emission is deterministic regardless of input order.
        let shuffled =
            Capabilities::new(vec![CAPABILITY_DRAIN_DEADLINE, CAPABILITY_REGISTER_REJECT]).unwrap();
        assert_eq!(
            shuffled.intersect(&shuffled).as_slice(),
            supported.as_slice()
        );
    }

    #[test]
    fn register_reject_round_trips_and_oversized_diagnostics_fail_closed() {
        let message = Message::RegisterReject(RegisterReject {
            service_id: ServiceId(7),
            code: 5,
            diagnostic: BoundedDiagnostic::new("saturated").unwrap(),
        });
        assert_eq!(message.kind(), MessageType::RegisterReject);
        let encoded = encode_frame(&message).unwrap();
        let (decoded, used) = decode_frame(&encoded).unwrap();
        assert_eq!(decoded, message);
        assert_eq!(used, encoded.len());
        // A 1.0 frame fixture (minor 0, empty capabilities) still decodes.
        let legacy = Message::Ping(Ping { nonce: 3 });
        let mut frame = encode_frame(&legacy).unwrap();
        frame[6] = 0;
        frame[7] = 0;
        assert_eq!(decode_frame(&frame).unwrap().0, legacy);
        // Oversized diagnostic text cannot be constructed, and a hostile
        // payload carrying one fails decode through the revalidating
        // constructor.
        assert!(BoundedDiagnostic::new("x".repeat(MAX_DIAGNOSTIC_BYTES + 1)).is_err());
        let hostile =
            postcard::to_allocvec(&(ServiceId(1), 1u16, "x".repeat(MAX_DIAGNOSTIC_BYTES + 1)))
                .unwrap();
        assert!(postcard::from_bytes::<RegisterReject>(&hostile).is_err());
    }

    #[test]
    fn rejects_bad_headers_lengths_and_unknown_ids() {
        let frame = encode_frame(&Message::Ping(Ping { nonce: 1 })).unwrap();
        assert_eq!(
            decode_frame(&frame[..3]),
            Err(ProtocolError::TruncatedFrame)
        );
        assert_eq!(
            decode_frame(&frame[..frame.len() - 1]),
            Err(ProtocolError::TruncatedFrame)
        );
        let mut trailing = frame.clone();
        trailing.push(0);
        let payload_len = (trailing.len() - HEADER_LEN) as u32;
        trailing[10..14].copy_from_slice(&payload_len.to_be_bytes());
        assert_eq!(decode_frame(&trailing), Err(ProtocolError::InvalidPayload));
        let mut bad = frame.clone();
        bad[0] = 0;
        assert_eq!(decode_frame(&bad), Err(ProtocolError::InvalidMagic));
        let mut bad = frame.clone();
        bad[4] = 0;
        bad[5] = 2;
        assert_eq!(
            decode_frame(&bad),
            Err(ProtocolError::UnsupportedVersion(2, PROTOCOL_MINOR))
        );
        let mut bad = frame.clone();
        bad[8] = 0xff;
        bad[9] = 0xff;
        assert_eq!(
            decode_frame(&bad),
            Err(ProtocolError::UnknownMessage(65535))
        );
        let mut bad = frame;
        bad[10..14].copy_from_slice(&((MAX_FRAME_BYTES as u32) + 1).to_be_bytes());
        assert_eq!(decode_frame(&bad), Err(ProtocolError::FrameTooLarge));
    }

    #[test]
    fn maximum_frame_length_is_checked_before_payload_decode() {
        for length in [MAX_FRAME_BYTES - 1, MAX_FRAME_BYTES] {
            let mut frame = vec![0; HEADER_LEN + length];
            frame[..4].copy_from_slice(&MAGIC);
            frame[4..6].copy_from_slice(&PROTOCOL_MAJOR.to_be_bytes());
            frame[6..8].copy_from_slice(&PROTOCOL_MINOR.to_be_bytes());
            frame[8..10].copy_from_slice(&(MessageType::Ping as u16).to_be_bytes());
            frame[10..14].copy_from_slice(&(length as u32).to_be_bytes());
            assert_eq!(decode_frame(&frame), Err(ProtocolError::InvalidPayload));
        }
        let mut over = vec![0; HEADER_LEN];
        over[..4].copy_from_slice(&MAGIC);
        over[4..6].copy_from_slice(&PROTOCOL_MAJOR.to_be_bytes());
        over[8..10].copy_from_slice(&(MessageType::Ping as u16).to_be_bytes());
        over[10..14].copy_from_slice(&((MAX_FRAME_BYTES as u32) + 1).to_be_bytes());
        assert_eq!(decode_frame(&over), Err(ProtocolError::FrameTooLarge));
    }

    #[test]
    fn hostile_wire_strings_vectors_and_tokens_are_revalidated() {
        let oversized_name = postcard::to_allocvec(&"x".repeat(MAX_NAME_BYTES + 1)).unwrap();
        assert!(postcard::from_bytes::<ServiceName>(&oversized_name).is_err());
        let oversized_diagnostic =
            postcard::to_allocvec(&"x".repeat(MAX_DIAGNOSTIC_BYTES + 1)).unwrap();
        assert!(postcard::from_bytes::<BoundedDiagnostic>(&oversized_diagnostic).is_err());

        let excessive_caps = vec![0u16; MAX_CAPABILITIES + 1];
        assert!(Capabilities::new(excessive_caps).is_err());
        let oversized_token = postcard::to_allocvec(&vec![0; MAX_AUTH_TOKEN_BYTES + 1]).unwrap();
        assert!(postcard::from_bytes::<Auth>(&oversized_token).is_err());
        assert!(Auth::new(vec![0; MAX_AUTH_TOKEN_BYTES + 1]).is_err());
    }

    #[test]
    fn target_host_validation_is_shared_with_endpoint_parsing() {
        // One validator: a host shape a peer may register is exactly the shape
        // local configuration accepts.
        for host in [
            "localhost",
            "example.internal",
            "127.0.0.1",
            "::1",
            "a-b_c.d",
        ] {
            assert!(validate_target_host(host).is_ok(), "{host}");
            assert!(TcpTarget::new(host, 443).is_ok(), "{host}");
        }
        for host in [
            "",
            "a b",
            "a/b",
            "a?b",
            "a#b",
            "u@h",
            "a[b]",
            "a\\b",
            "a\"b",
            "a'b",
            "a<b>",
            "a\u{0003}b",
            "a\u{00a0}b",
        ] {
            assert!(validate_target_host(host).is_err(), "{host:?}");
            assert!(TcpTarget::new(host, 443).is_err(), "{host:?}");
        }
        let over_length = "x".repeat(MAX_TARGET_HOST_BYTES + 1);
        assert!(TcpTarget::new(over_length, 443).is_err());
        assert!(TcpTarget::new("x".repeat(MAX_TARGET_HOST_BYTES), 443).is_ok());
        // A wire payload carrying a hostile host revalidates on decode.
        let hostile = postcard::to_allocvec(&("a b".to_owned(), 80u16)).unwrap();
        assert!(postcard::from_bytes::<TcpTarget>(&hostile).is_err());
        let hostile = postcard::to_allocvec(&("::1".to_owned(), 0u16)).unwrap();
        assert!(postcard::from_bytes::<TcpTarget>(&hostile).is_err());
    }

    #[test]
    fn validates_bounded_types_and_redacts_secrets() {
        assert!(ServiceName::new("").is_err());
        assert!(ServiceName::new("x".repeat(MAX_NAME_BYTES)).is_ok());
        assert!(ServiceName::new("x".repeat(MAX_NAME_BYTES + 1)).is_err());
        assert!(TcpTarget::new("host", 0).is_err());
        assert!(BoundedDiagnostic::new("x".repeat(MAX_DIAGNOSTIC_BYTES + 1)).is_err());
        assert!(!format!("{:?}", Auth::new(b"secret".to_vec()).unwrap()).contains("secret"));
        assert!(!format!("{:?}", ConnectionId([0xab; 16])).contains("ab"));
        // Session IDs are capabilities in `DataHello` and are redacted
        // exactly like `ConnectionId`.
        let session_debug = format!("{:?}", SessionId([0xab; 16]));
        assert_eq!(session_debug, "SessionId([REDACTED])");
        assert!(!session_debug.contains("ab"));
    }

    #[test]
    fn ids_have_separate_types_and_connection_comparison_is_correct() {
        let session = SessionId::generate().unwrap();
        let other_session = SessionId::generate().unwrap();
        let connection = ConnectionId::generate().unwrap();
        assert_ne!(session, other_session);
        assert!(connection.constant_time_eq(&connection));
        assert!(!connection.constant_time_eq(&ConnectionId([0; 16])));
        assert_eq!(core::mem::size_of::<ConnectionId>(), 16);
        let _: ServiceId = ServiceId(42);
    }

    #[test]
    fn arbitrary_input_never_panics() {
        let mut state = 0xD1CE_BA5E_F00D_u64;
        for sample in 0..10_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let len = (state as usize ^ sample) % 4097;
            let mut bytes = Vec::with_capacity(len);
            for _ in 0..len {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                bytes.push(state as u8);
            }
            let _ = decode_frame(&bytes);
        }
    }
}
