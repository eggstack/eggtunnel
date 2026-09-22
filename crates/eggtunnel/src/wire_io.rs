use eggress_core::BoxStream;
use eggtunnel_proto::{
    HEADER_LEN, MAX_FRAME_BYTES, Message, ProtocolError, decode_frame, encode_frame,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(crate) async fn read_message<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Message, ProtocolError> {
    let mut header = [0u8; HEADER_LEN];
    reader
        .read_exact(&mut header)
        .await
        .map_err(|_| ProtocolError::TruncatedFrame)?;
    match decode_frame(&header) {
        Err(ProtocolError::TruncatedFrame) => {}
        Err(error) => return Err(error),
        Ok(_) => unreachable!("a complete protocol frame cannot fit in its header"),
    }
    let len = u32::from_be_bytes([header[10], header[11], header[12], header[13]]) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    let mut frame = Vec::with_capacity(HEADER_LEN + len);
    frame.extend_from_slice(&header);
    frame.resize(HEADER_LEN + len, 0);
    reader
        .read_exact(&mut frame[HEADER_LEN..])
        .await
        .map_err(|_| ProtocolError::TruncatedFrame)?;
    let (message, consumed) = decode_frame(&frame)?;
    if consumed != frame.len() {
        return Err(ProtocolError::InvalidPayload);
    }
    Ok(message)
}

pub(crate) async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Message,
) -> Result<(), ProtocolError> {
    let frame = encode_frame(message)?;
    writer
        .write_all(&frame)
        .await
        .map_err(|_| ProtocolError::TruncatedFrame)
}

pub(crate) async fn read_boxed(stream: &mut BoxStream) -> Result<Message, ProtocolError> {
    read_message(stream).await
}

pub(crate) async fn write_boxed(
    stream: &mut BoxStream,
    message: &Message,
) -> Result<(), ProtocolError> {
    write_message(stream, message).await
}
