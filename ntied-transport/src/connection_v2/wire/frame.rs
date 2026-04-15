/// Frame type bytes.
pub(crate) const PADDING: u8 = 0x00;
pub(crate) const ACK: u8 = 0x01;
pub(crate) const PING: u8 = 0x02;
pub(crate) const PONG: u8 = 0x03;
pub(crate) const AUTH_COMPLETE: u8 = 0x04;
pub(crate) const CONNECTION_CLOSE: u8 = 0x05;
pub(crate) const WINDOW_UPDATE: u8 = 0x06;
pub(crate) const CHANNEL_CLOSE: u8 = 0x07;
pub(crate) const STREAM_BASE: u8 = 0x08; // bit0 = FIN
pub(crate) const CHANNEL_OPEN: u8 = 0x0A;
pub(crate) const CHANNEL_BASE: u8 = 0x10; // bit0 = FIN
pub(crate) const AUTH_BASE: u8 = 0x18; // bit0 = FIN
pub(crate) const REKEY_BASE: u8 = 0x20; // bit0 = FIN
pub(crate) const REKEY_ACK_BASE: u8 = 0x28; // bit0 = FIN

const FIN_BIT: u8 = 0x01;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    UnexpectedEnd,
    InvalidType(u8),
}

/// Zero-copy decoded frame.  Data fields borrow from the input buffer.
#[derive(Debug, PartialEq)]
pub enum Frame<'a> {
    Ack {
        largest: u64,
        delay: u16,
        ranges: &'a [u8],
    },
    Ping {
        id: u32,
    },
    Pong {
        id: u32,
    },
    AuthComplete,
    ConnectionClose {
        error_code: u32,
        reason: &'a [u8],
    },
    WindowUpdate {
        stream_id: u64,
        max_offset: u64,
    },
    ChannelOpen {
        channel_id: u64,
    },
    ChannelClose {
        channel_id: u64,
    },
    Stream {
        stream_id: u64,
        offset: u64,
        fin: bool,
        data: &'a [u8],
    },
    Channel {
        channel_id: u64,
        message_id: u64,
        offset: u64,
        fin: bool,
        data: &'a [u8],
    },
    Auth {
        offset: u64,
        fin: bool,
        data: &'a [u8],
    },
    Rekey {
        offset: u64,
        fin: bool,
        data: &'a [u8],
    },
    RekeyAck {
        offset: u64,
        fin: bool,
        data: &'a [u8],
    },
}

/// Zero-alloc iterator over frames in a decrypted payload.
pub struct FrameIter<'a> {
    buf: &'a [u8],
}

impl<'a> FrameIter<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }
}

impl<'a> Iterator for FrameIter<'a> {
    type Item = Result<Frame<'a>, FrameError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.buf.is_empty() {
                return None;
            }
            let frame_type = self.buf[0];
            self.buf = &self.buf[1..];

            if frame_type == PADDING {
                continue;
            }

            return Some(match decode_frame(frame_type, self.buf) {
                Ok((frame, rest)) => {
                    self.buf = rest;
                    Ok(frame)
                }
                Err(e) => {
                    self.buf = &[]; // stop iteration on error
                    Err(e)
                }
            });
        }
    }
}

/// Decode all frames from a decrypted payload.  Zero-copy, zero-alloc.
pub fn decode_frames(buf: &[u8]) -> FrameIter<'_> {
    FrameIter::new(buf)
}

fn decode_frame<'a>(frame_type: u8, buf: &'a [u8]) -> Result<(Frame<'a>, &'a [u8]), FrameError> {
    match frame_type {
        ACK => decode_ack(buf),
        PING => decode_ping(buf),
        PONG => decode_pong(buf),
        AUTH_COMPLETE => Ok((Frame::AuthComplete, buf)),
        CONNECTION_CLOSE => decode_connection_close(buf),
        WINDOW_UPDATE => decode_window_update(buf),
        CHANNEL_OPEN => decode_channel_open(buf),
        CHANNEL_CLOSE => decode_channel_close(buf),
        t if (t & 0xFE) == STREAM_BASE => decode_stream(t, buf),
        t if (t & 0xFE) == CHANNEL_BASE => decode_channel(t, buf),
        t if (t & 0xFE) == AUTH_BASE => decode_auth(t, buf),
        t if (t & 0xFE) == REKEY_BASE => decode_rekey(t, buf),
        t if (t & 0xFE) == REKEY_ACK_BASE => decode_rekey_ack(t, buf),
        t => Err(FrameError::InvalidType(t)),
    }
}

fn read_u16(buf: &[u8]) -> Result<(u16, &[u8]), FrameError> {
    if buf.len() < 2 {
        return Err(FrameError::UnexpectedEnd);
    }
    let v = u16::from_be_bytes(buf[..2].try_into().unwrap());
    Ok((v, &buf[2..]))
}

fn read_u32(buf: &[u8]) -> Result<(u32, &[u8]), FrameError> {
    if buf.len() < 4 {
        return Err(FrameError::UnexpectedEnd);
    }
    let v = u32::from_be_bytes(buf[..4].try_into().unwrap());
    Ok((v, &buf[4..]))
}

fn read_u64(buf: &[u8]) -> Result<(u64, &[u8]), FrameError> {
    if buf.len() < 8 {
        return Err(FrameError::UnexpectedEnd);
    }
    let v = u64::from_be_bytes(buf[..8].try_into().unwrap());
    Ok((v, &buf[8..]))
}

fn read_bytes<'a>(buf: &'a [u8], len: usize) -> Result<(&'a [u8], &'a [u8]), FrameError> {
    if buf.len() < len {
        return Err(FrameError::UnexpectedEnd);
    }
    Ok(buf.split_at(len))
}

// --- Decoders ---------------------------------------------------------------

// ACK: [largest:8] [delay:2] [range_count:1] [ranges: count * 16]
fn decode_ack<'a>(buf: &'a [u8]) -> Result<(Frame<'a>, &'a [u8]), FrameError> {
    let (largest, buf) = read_u64(buf)?;
    let (delay, buf) = read_u16(buf)?;
    if buf.is_empty() {
        return Err(FrameError::UnexpectedEnd);
    }
    let count = buf[0] as usize;
    let buf = &buf[1..];
    let range_bytes = count * 16; // each range: gap:8 + length:8
    let (ranges, buf) = read_bytes(buf, range_bytes)?;
    Ok((Frame::Ack { largest, delay, ranges }, buf))
}

fn decode_ping<'a>(buf: &'a [u8]) -> Result<(Frame<'a>, &'a [u8]), FrameError> {
    let (id, buf) = read_u32(buf)?;
    Ok((Frame::Ping { id }, buf))
}

fn decode_pong<'a>(buf: &'a [u8]) -> Result<(Frame<'a>, &'a [u8]), FrameError> {
    let (id, buf) = read_u32(buf)?;
    Ok((Frame::Pong { id }, buf))
}

// CONNECTION_CLOSE: [error_code:4] [reason_len:2] [reason:reason_len]
fn decode_connection_close<'a>(buf: &'a [u8]) -> Result<(Frame<'a>, &'a [u8]), FrameError> {
    let (error_code, buf) = read_u32(buf)?;
    let (reason_len, buf) = read_u16(buf)?;
    let (reason, buf) = read_bytes(buf, reason_len as usize)?;
    Ok((Frame::ConnectionClose { error_code, reason }, buf))
}

// WINDOW_UPDATE: [stream_id:8] [max_offset:8]
fn decode_window_update<'a>(buf: &'a [u8]) -> Result<(Frame<'a>, &'a [u8]), FrameError> {
    let (stream_id, buf) = read_u64(buf)?;
    let (max_offset, buf) = read_u64(buf)?;
    Ok((Frame::WindowUpdate { stream_id, max_offset }, buf))
}

// CHANNEL_OPEN: [channel_id:8]
fn decode_channel_open<'a>(buf: &'a [u8]) -> Result<(Frame<'a>, &'a [u8]), FrameError> {
    let (channel_id, buf) = read_u64(buf)?;
    Ok((Frame::ChannelOpen { channel_id }, buf))
}

// CHANNEL_CLOSE: [channel_id:8]
fn decode_channel_close<'a>(buf: &'a [u8]) -> Result<(Frame<'a>, &'a [u8]), FrameError> {
    let (channel_id, buf) = read_u64(buf)?;
    Ok((Frame::ChannelClose { channel_id }, buf))
}

// STREAM: [stream_id:8] [offset:8] [len:2] [data:len]
fn decode_stream<'a>(type_byte: u8, buf: &'a [u8]) -> Result<(Frame<'a>, &'a [u8]), FrameError> {
    let fin = type_byte & FIN_BIT != 0;
    let (stream_id, buf) = read_u64(buf)?;
    let (offset, buf) = read_u64(buf)?;
    let (len, buf) = read_u16(buf)?;
    let (data, buf) = read_bytes(buf, len as usize)?;
    Ok((Frame::Stream { stream_id, offset, fin, data }, buf))
}

// CHANNEL: [channel_id:8] [message_id:8] [offset:8] [len:2] [data:len]
fn decode_channel<'a>(type_byte: u8, buf: &'a [u8]) -> Result<(Frame<'a>, &'a [u8]), FrameError> {
    let fin = type_byte & FIN_BIT != 0;
    let (channel_id, buf) = read_u64(buf)?;
    let (message_id, buf) = read_u64(buf)?;
    let (offset, buf) = read_u64(buf)?;
    let (len, buf) = read_u16(buf)?;
    let (data, buf) = read_bytes(buf, len as usize)?;
    Ok((Frame::Channel { channel_id, message_id, offset, fin, data }, buf))
}

// AUTH: [offset:8] [len:2] [data:len]
fn decode_auth<'a>(type_byte: u8, buf: &'a [u8]) -> Result<(Frame<'a>, &'a [u8]), FrameError> {
    let fin = type_byte & FIN_BIT != 0;
    let (offset, buf) = read_u64(buf)?;
    let (len, buf) = read_u16(buf)?;
    let (data, buf) = read_bytes(buf, len as usize)?;
    Ok((Frame::Auth { offset, fin, data }, buf))
}

// REKEY: [offset:8] [len:2] [data:len]
fn decode_rekey<'a>(type_byte: u8, buf: &'a [u8]) -> Result<(Frame<'a>, &'a [u8]), FrameError> {
    let fin = type_byte & FIN_BIT != 0;
    let (offset, buf) = read_u64(buf)?;
    let (len, buf) = read_u16(buf)?;
    let (data, buf) = read_bytes(buf, len as usize)?;
    Ok((Frame::Rekey { offset, fin, data }, buf))
}

// REKEY_ACK: [offset:8] [len:2] [data:len]
fn decode_rekey_ack<'a>(type_byte: u8, buf: &'a [u8]) -> Result<(Frame<'a>, &'a [u8]), FrameError> {
    let fin = type_byte & FIN_BIT != 0;
    let (offset, buf) = read_u64(buf)?;
    let (len, buf) = read_u16(buf)?;
    let (data, buf) = read_bytes(buf, len as usize)?;
    Ok((Frame::RekeyAck { offset, fin, data }, buf))
}

// --- Encoders ---------------------------------------------------------------

/// Encode a STREAM frame header. Returns header length.
/// Caller writes payload data at `out[header_len..header_len + data_len]`.
///
/// Header: [type:1] [stream_id:8] [offset:8] [len:2] = 19 bytes
pub const STREAM_HEADER_SIZE: usize = 1 + 8 + 8 + 2;

pub fn encode_stream_header(
    out: &mut [u8],
    stream_id: u64,
    offset: u64,
    data_len: u16,
    fin: bool,
) -> usize {
    out[0] = STREAM_BASE | if fin { FIN_BIT } else { 0 };
    out[1..9].copy_from_slice(&stream_id.to_be_bytes());
    out[9..17].copy_from_slice(&offset.to_be_bytes());
    out[17..19].copy_from_slice(&data_len.to_be_bytes());
    STREAM_HEADER_SIZE
}

/// Encode a CHANNEL frame header. Returns header length.
///
/// Header: [type:1] [channel_id:8] [message_id:8] [offset:8] [len:2] = 27 bytes
pub const CHANNEL_HEADER_SIZE: usize = 1 + 8 + 8 + 8 + 2;

pub fn encode_channel_header(
    out: &mut [u8],
    channel_id: u64,
    message_id: u64,
    offset: u64,
    data_len: u16,
    fin: bool,
) -> usize {
    out[0] = CHANNEL_BASE | if fin { FIN_BIT } else { 0 };
    out[1..9].copy_from_slice(&channel_id.to_be_bytes());
    out[9..17].copy_from_slice(&message_id.to_be_bytes());
    out[17..25].copy_from_slice(&offset.to_be_bytes());
    out[25..27].copy_from_slice(&data_len.to_be_bytes());
    CHANNEL_HEADER_SIZE
}

/// Encode an AUTH frame header. Returns header length.
///
/// Header: [type:1] [offset:8] [len:2] = 11 bytes
pub const AUTH_HEADER_SIZE: usize = 1 + 8 + 2;

pub fn encode_auth_header(
    out: &mut [u8],
    offset: u64,
    data_len: u16,
    fin: bool,
) -> usize {
    out[0] = AUTH_BASE | if fin { FIN_BIT } else { 0 };
    out[1..9].copy_from_slice(&offset.to_be_bytes());
    out[9..11].copy_from_slice(&data_len.to_be_bytes());
    AUTH_HEADER_SIZE
}

/// Encode a REKEY frame header. Same layout as AUTH.
pub const REKEY_HEADER_SIZE: usize = AUTH_HEADER_SIZE;

pub fn encode_rekey_header(out: &mut [u8], offset: u64, data_len: u16, fin: bool) -> usize {
    out[0] = REKEY_BASE | if fin { FIN_BIT } else { 0 };
    out[1..9].copy_from_slice(&offset.to_be_bytes());
    out[9..11].copy_from_slice(&data_len.to_be_bytes());
    REKEY_HEADER_SIZE
}

/// Encode a REKEY_ACK frame header. Same layout as AUTH.
pub const REKEY_ACK_HEADER_SIZE: usize = AUTH_HEADER_SIZE;

pub fn encode_rekey_ack_header(out: &mut [u8], offset: u64, data_len: u16, fin: bool) -> usize {
    out[0] = REKEY_ACK_BASE | if fin { FIN_BIT } else { 0 };
    out[1..9].copy_from_slice(&offset.to_be_bytes());
    out[9..11].copy_from_slice(&data_len.to_be_bytes());
    REKEY_ACK_HEADER_SIZE
}

/// Encode an ACK frame. Returns total frame length.
///
/// [type:1] [largest:8] [delay:2] [count:1] [ranges: count * (gap:8 + length:8)]
pub fn encode_ack(out: &mut [u8], largest: u64, delay: u16, ranges: &[(u64, u64)]) -> usize {
    out[0] = ACK;
    out[1..9].copy_from_slice(&largest.to_be_bytes());
    out[9..11].copy_from_slice(&delay.to_be_bytes());
    out[11] = ranges.len() as u8;
    let mut pos = 12;
    for &(gap, length) in ranges {
        out[pos..pos + 8].copy_from_slice(&gap.to_be_bytes());
        out[pos + 8..pos + 16].copy_from_slice(&length.to_be_bytes());
        pos += 16;
    }
    pos
}

/// Encode a PING frame. Returns frame length (5).
pub fn encode_ping(out: &mut [u8], id: u32) -> usize {
    out[0] = PING;
    out[1..5].copy_from_slice(&id.to_be_bytes());
    5
}

/// Encode a PONG frame. Returns frame length (5).
pub fn encode_pong(out: &mut [u8], id: u32) -> usize {
    out[0] = PONG;
    out[1..5].copy_from_slice(&id.to_be_bytes());
    5
}

/// Encode AUTH_COMPLETE frame. Returns frame length (1).
pub fn encode_auth_complete(out: &mut [u8]) -> usize {
    out[0] = AUTH_COMPLETE;
    1
}

/// Encode CONNECTION_CLOSE frame. Returns frame length.
///
/// [type:1] [error_code:4] [reason_len:2] [reason:reason_len]
pub fn encode_connection_close(out: &mut [u8], error_code: u32, reason: &[u8]) -> usize {
    out[0] = CONNECTION_CLOSE;
    out[1..5].copy_from_slice(&error_code.to_be_bytes());
    out[5..7].copy_from_slice(&(reason.len() as u16).to_be_bytes());
    out[7..7 + reason.len()].copy_from_slice(reason);
    7 + reason.len()
}

/// Encode CHANNEL_OPEN frame. Returns frame length (9).
pub fn encode_channel_open(out: &mut [u8], channel_id: u64) -> usize {
    out[0] = CHANNEL_OPEN;
    out[1..9].copy_from_slice(&channel_id.to_be_bytes());
    9
}

/// Encode CHANNEL_CLOSE frame. Returns frame length (9).
pub fn encode_channel_close(out: &mut [u8], channel_id: u64) -> usize {
    out[0] = CHANNEL_CLOSE;
    out[1..9].copy_from_slice(&channel_id.to_be_bytes());
    9
}

/// Encode WINDOW_UPDATE frame. Returns frame length (17).
pub fn encode_window_update(out: &mut [u8], stream_id: u64, max_offset: u64) -> usize {
    out[0] = WINDOW_UPDATE;
    out[1..9].copy_from_slice(&stream_id.to_be_bytes());
    out[9..17].copy_from_slice(&max_offset.to_be_bytes());
    17
}
