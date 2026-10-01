use eggress_core::BoxStream;
use eggtunnel_proto::{
    HEADER_LEN, MAX_FRAME_BYTES, Message, ProtocolError, decode_frame, encode_frame,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::common::TunnelError;

/// Classify a transport I/O failure. Only a genuinely short frame is a
/// protocol truncation; resets, refusals, TLS alerts, and broken pipes are
/// transport faults and must not be reported as `Protocol` to the
/// termination-category and reconnect accounting.
fn io_error(error: std::io::Error) -> TunnelError {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        TunnelError::Protocol(ProtocolError::TruncatedFrame)
    } else {
        TunnelError::Io(error)
    }
}

pub(crate) async fn read_message<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Message, TunnelError> {
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header).await.map_err(io_error)?;
    match decode_frame(&header) {
        Err(ProtocolError::TruncatedFrame) => {}
        Err(error) => return Err(TunnelError::Protocol(error)),
        // Fail closed: a header that decodes to a complete frame is never
        // trusted, so a future zero-byte message type cannot turn a crafted
        // header into a task panic.
        Ok(_) => return Err(TunnelError::Protocol(ProtocolError::InvalidPayload)),
    }
    let len = u32::from_be_bytes([header[10], header[11], header[12], header[13]]) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(TunnelError::Protocol(ProtocolError::FrameTooLarge));
    }
    // The payload is buffered incrementally under the declared ceiling, so a
    // peer that announces the maximum frame and then stalls holds only what it
    // actually sent instead of a pre-committed 1 MiB allocation.
    let mut payload = Vec::new();
    (&mut *reader)
        .take(len as u64)
        .read_to_end(&mut payload)
        .await
        .map_err(io_error)?;
    if payload.len() != len {
        return Err(TunnelError::Protocol(ProtocolError::TruncatedFrame));
    }
    let mut frame = Vec::with_capacity(HEADER_LEN + len);
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&payload);
    let (message, consumed) = decode_frame(&frame)?;
    if consumed != frame.len() {
        return Err(TunnelError::Protocol(ProtocolError::InvalidPayload));
    }
    Ok(message)
}

pub(crate) async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Message,
) -> Result<(), TunnelError> {
    let frame = encode_frame(message)?;
    writer.write_all(&frame).await.map_err(io_error)
}

pub(crate) async fn read_boxed(stream: &mut BoxStream) -> Result<Message, TunnelError> {
    read_message(stream).await
}

pub(crate) async fn write_boxed(
    stream: &mut BoxStream,
    message: &Message,
) -> Result<(), TunnelError> {
    write_message(stream, message).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use eggtunnel_proto::{MessageType, Ping};
    use std::io::ErrorKind;
    use std::task::{Context, Poll};

    struct FailingReader;

    impl AsyncRead for FailingReader {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(std::io::Error::from(ErrorKind::ConnectionReset)))
        }
    }

    struct FailingWriter;

    impl AsyncWrite for FailingWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Err(std::io::Error::from(ErrorKind::BrokenPipe)))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(std::io::Error::from(ErrorKind::BrokenPipe)))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn transport_io_failures_are_not_reported_as_protocol_truncation() {
        assert!(matches!(
            read_message(&mut FailingReader).await,
            Err(TunnelError::Io(_))
        ));
        assert!(matches!(
            write_message(&mut FailingWriter, &Message::Ping(Ping { nonce: 1 })).await,
            Err(TunnelError::Io(_))
        ));
        assert_eq!(
            TunnelError::Io(std::io::Error::from(ErrorKind::ConnectionReset))
                .termination_category(),
            crate::common::TerminationCategory::Transport
        );
    }

    #[tokio::test]
    async fn a_peer_that_closes_mid_frame_is_a_truncation() {
        let frame = encode_frame(&Message::Ping(Ping { nonce: 7 })).unwrap();
        let mut short = std::io::Cursor::new(frame[..frame.len() - 1].to_vec());
        assert!(matches!(
            read_message(&mut short).await,
            Err(TunnelError::Protocol(ProtocolError::TruncatedFrame))
        ));
        let mut empty = tokio::io::empty();
        assert!(matches!(
            read_message(&mut empty).await,
            Err(TunnelError::Protocol(ProtocolError::TruncatedFrame))
        ));
    }

    #[tokio::test]
    async fn a_zero_length_payload_fails_closed_instead_of_panicking() {
        // Every announced length is validated before the payload is buffered,
        // and no header can drive the control or data task into a panic.
        for kind in 1u16..=(MessageType::RegisterReject as u16) {
            let mut header = Vec::from(eggtunnel_proto::MAGIC.as_slice());
            header.extend_from_slice(&eggtunnel_proto::PROTOCOL_MAJOR.to_be_bytes());
            header.extend_from_slice(&eggtunnel_proto::PROTOCOL_MINOR.to_be_bytes());
            header.extend_from_slice(&kind.to_be_bytes());
            header.extend_from_slice(&0u32.to_be_bytes());
            let mut reader = std::io::Cursor::new(header);
            assert!(
                matches!(
                    read_message(&mut reader).await,
                    Err(TunnelError::Protocol(
                        ProtocolError::InvalidPayload | ProtocolError::UnknownMessage(_)
                    ))
                ),
                "message type {kind} must fail closed"
            );
        }
    }

    #[tokio::test]
    async fn an_oversized_announced_length_is_rejected_before_buffering() {
        let mut header = Vec::from(eggtunnel_proto::MAGIC.as_slice());
        header.extend_from_slice(&eggtunnel_proto::PROTOCOL_MAJOR.to_be_bytes());
        header.extend_from_slice(&eggtunnel_proto::PROTOCOL_MINOR.to_be_bytes());
        header.extend_from_slice(&(MessageType::Ping as u16).to_be_bytes());
        header.extend_from_slice(&u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes());
        let mut reader = std::io::Cursor::new(header);
        assert!(matches!(
            read_message(&mut reader).await,
            Err(TunnelError::Protocol(ProtocolError::FrameTooLarge))
        ));
    }

    #[tokio::test]
    async fn consecutive_frames_decode_one_at_a_time() {
        let mut joined = Vec::new();
        for nonce in 0..3u64 {
            joined.extend_from_slice(
                &encode_frame(&Message::Ping(Ping { nonce })).expect("ping encodes"),
            );
        }
        let mut reader = std::io::Cursor::new(joined);
        for nonce in 0..3u64 {
            assert_eq!(
                read_message(&mut reader).await.expect("ping decodes"),
                Message::Ping(Ping { nonce })
            );
        }
    }
}
