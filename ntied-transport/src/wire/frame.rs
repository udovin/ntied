use super::codec::{CodecError, Reader, Writer};

pub const FRAME_ACK: u8 = 0x01;
pub const FRAME_PING: u8 = 0x02;
pub const FRAME_PONG: u8 = 0x03;
pub const FRAME_STREAM_OPEN: u8 = 0x04;
pub const FRAME_STREAM_DATA: u8 = 0x05;
pub const FRAME_STREAM_CLOSE: u8 = 0x06;
pub const FRAME_STREAM_RESET: u8 = 0x07;
pub const FRAME_WINDOW_UPDATE: u8 = 0x08;
pub const FRAME_DATAGRAM_FRAGMENT: u8 = 0x09;
pub const FRAME_DATAGRAM: u8 = 0x0A;
pub const FRAME_AUTH: u8 = 0x0B;
pub const FRAME_AUTH_COMPLETE: u8 = 0x0C;
pub const FRAME_REKEY: u8 = 0x0D;
pub const FRAME_REKEY_ACK: u8 = 0x0E;
pub const FRAME_CONNECTION_CLOSE: u8 = 0x0F;

pub const MAX_ACK_RANGES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    UnexpectedEnd,
    InvalidFrameType(u8),
    InvalidStreamType(u8),
}

impl From<CodecError> for FrameError {
    fn from(err: CodecError) -> Self {
        match err {
            CodecError::UnexpectedEnd => Self::UnexpectedEnd,
        }
    }
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnexpectedEnd => f.write_str("unexpected end of frame"),
            Self::InvalidFrameType(t) => write!(f, "invalid frame type: 0x{t:02X}"),
            Self::InvalidStreamType(t) => write!(f, "invalid stream type: 0x{t:02X}"),
        }
    }
}

impl std::error::Error for FrameError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StreamType {
    ReliableOrdered = 0x01,
    ReliableDatagram = 0x02,
    Unreliable = 0x03,
}

impl StreamType {
    pub fn from_u8(val: u8) -> Result<Self, FrameError> {
        match val {
            0x01 => Ok(Self::ReliableOrdered),
            0x02 => Ok(Self::ReliableDatagram),
            0x03 => Ok(Self::Unreliable),
            other => Err(FrameError::InvalidStreamType(other)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckRange {
    pub gap: u64,
    pub length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ack {
    pub largest_ack: u64,
    pub ack_delay: u16,
    pub ranges: Vec<AckRange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ping {
    pub ping_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pong {
    pub ping_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOpen {
    pub stream_id: u32,
    pub stream_type: StreamType,
    pub purpose: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamData {
    pub stream_id: u32,
    pub offset: u64,
    pub fin: bool,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamClose {
    pub stream_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamReset {
    pub stream_id: u32,
    pub error_code: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowUpdate {
    pub stream_id: u32,
    pub max_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatagramFragment {
    pub stream_id: u32,
    pub message_id: u32,
    pub fragment_index: u16,
    pub fragment_total: u16,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Datagram {
    pub stream_id: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Auth {
    pub fragment_index: u8,
    pub fragment_total: u8,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthComplete;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rekey {
    pub fragment_index: u8,
    pub fragment_total: u8,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RekeyAck {
    pub fragment_index: u8,
    pub fragment_total: u8,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionClose {
    pub error_code: u32,
    pub reason: Vec<u8>,
}

pub enum Frame {
    Ack(Ack),
    Ping(Ping),
    Pong(Pong),
    StreamOpen(StreamOpen),
    StreamData(StreamData),
    StreamClose(StreamClose),
    StreamReset(StreamReset),
    WindowUpdate(WindowUpdate),
    DatagramFragment(DatagramFragment),
    Datagram(Datagram),
    Auth(Auth),
    AuthComplete(AuthComplete),
    Rekey(Rekey),
    RekeyAck(RekeyAck),
    ConnectionClose(ConnectionClose),
}

impl Frame {
    pub fn decode(reader: &mut Reader) -> Result<Self, FrameError> {
        let frame_type = reader.read_u8()?;
        let frame_length = reader.read_u16()? as usize;
        let frame_data = reader.read_bytes(frame_length)?;
        let mut r = Reader::new(frame_data);
        match frame_type {
            FRAME_ACK => Ok(Self::Ack(Ack::decode_data(&mut r)?)),
            FRAME_PING => Ok(Self::Ping(Ping::decode_data(&mut r)?)),
            FRAME_PONG => Ok(Self::Pong(Pong::decode_data(&mut r)?)),
            FRAME_STREAM_OPEN => Ok(Self::StreamOpen(StreamOpen::decode_data(&mut r)?)),
            FRAME_STREAM_DATA => Ok(Self::StreamData(StreamData::decode_data(&mut r)?)),
            FRAME_STREAM_CLOSE => Ok(Self::StreamClose(StreamClose::decode_data(&mut r)?)),
            FRAME_STREAM_RESET => Ok(Self::StreamReset(StreamReset::decode_data(&mut r)?)),
            FRAME_WINDOW_UPDATE => Ok(Self::WindowUpdate(WindowUpdate::decode_data(&mut r)?)),
            FRAME_DATAGRAM_FRAGMENT => Ok(Self::DatagramFragment(DatagramFragment::decode_data(
                &mut r,
            )?)),
            FRAME_DATAGRAM => Ok(Self::Datagram(Datagram::decode_data(&mut r)?)),
            FRAME_AUTH => Ok(Self::Auth(Auth::decode_data(&mut r)?)),
            FRAME_AUTH_COMPLETE => Ok(Self::AuthComplete(AuthComplete)),
            FRAME_REKEY => Ok(Self::Rekey(Rekey::decode_data(&mut r)?)),
            FRAME_REKEY_ACK => Ok(Self::RekeyAck(RekeyAck::decode_data(&mut r)?)),
            FRAME_CONNECTION_CLOSE => {
                Ok(Self::ConnectionClose(ConnectionClose::decode_data(&mut r)?))
            }
            t => Err(FrameError::InvalidFrameType(t)),
        }
    }

    pub fn encode(&self, writer: &mut Writer) {
        let mut data = Writer::new();
        let frame_type = match self {
            Self::Ack(f) => {
                f.encode_data(&mut data);
                FRAME_ACK
            }
            Self::Ping(f) => {
                f.encode_data(&mut data);
                FRAME_PING
            }
            Self::Pong(f) => {
                f.encode_data(&mut data);
                FRAME_PONG
            }
            Self::StreamOpen(f) => {
                f.encode_data(&mut data);
                FRAME_STREAM_OPEN
            }
            Self::StreamData(f) => {
                f.encode_data(&mut data);
                FRAME_STREAM_DATA
            }
            Self::StreamClose(f) => {
                f.encode_data(&mut data);
                FRAME_STREAM_CLOSE
            }
            Self::StreamReset(f) => {
                f.encode_data(&mut data);
                FRAME_STREAM_RESET
            }
            Self::WindowUpdate(f) => {
                f.encode_data(&mut data);
                FRAME_WINDOW_UPDATE
            }
            Self::DatagramFragment(f) => {
                f.encode_data(&mut data);
                FRAME_DATAGRAM_FRAGMENT
            }
            Self::Datagram(f) => {
                f.encode_data(&mut data);
                FRAME_DATAGRAM
            }
            Self::Auth(f) => {
                f.encode_data(&mut data);
                FRAME_AUTH
            }
            Self::AuthComplete(_) => FRAME_AUTH_COMPLETE,
            Self::Rekey(f) => {
                f.encode_data(&mut data);
                FRAME_REKEY
            }
            Self::RekeyAck(f) => {
                f.encode_data(&mut data);
                FRAME_REKEY_ACK
            }
            Self::ConnectionClose(f) => {
                f.encode_data(&mut data);
                FRAME_CONNECTION_CLOSE
            }
        };
        writer.write_u8(frame_type);
        writer.write_u16(data.len() as u16);
        writer.write_bytes(data.as_bytes());
    }

    pub fn is_ack_eliciting(&self) -> bool {
        !matches!(self, Self::Ack(_) | Self::Pong(_) | Self::WindowUpdate(_))
    }
}

pub fn decode_frames(payload: &[u8]) -> Result<Vec<Frame>, FrameError> {
    let mut reader = Reader::new(payload);
    let mut frames = Vec::new();
    while !reader.is_empty() {
        frames.push(Frame::decode(&mut reader)?);
    }
    Ok(frames)
}

pub fn encode_frames(frames: &[Frame]) -> Vec<u8> {
    let mut writer = Writer::new();
    for frame in frames {
        frame.encode(&mut writer);
    }
    writer.into_vec()
}

impl Ack {
    fn decode_data(r: &mut Reader) -> Result<Self, FrameError> {
        let largest_ack = r.read_u64()?;
        let ack_delay = r.read_u16()?;
        let range_count = r.read_u8()? as usize;
        let mut ranges = Vec::with_capacity(range_count);
        for _ in 0..range_count {
            ranges.push(AckRange {
                gap: r.read_u64()?,
                length: r.read_u64()?,
            });
        }
        Ok(Self {
            largest_ack,
            ack_delay,
            ranges,
        })
    }

    fn encode_data(&self, w: &mut Writer) {
        w.write_u64(self.largest_ack);
        w.write_u16(self.ack_delay);
        w.write_u8(self.ranges.len() as u8);
        for range in &self.ranges {
            w.write_u64(range.gap);
            w.write_u64(range.length);
        }
    }
}

impl Ping {
    fn decode_data(r: &mut Reader) -> Result<Self, FrameError> {
        Ok(Self {
            ping_id: r.read_u32()?,
        })
    }

    fn encode_data(&self, w: &mut Writer) {
        w.write_u32(self.ping_id);
    }
}

impl Pong {
    fn decode_data(r: &mut Reader) -> Result<Self, FrameError> {
        Ok(Self {
            ping_id: r.read_u32()?,
        })
    }

    fn encode_data(&self, w: &mut Writer) {
        w.write_u32(self.ping_id);
    }
}

impl StreamOpen {
    fn decode_data(r: &mut Reader) -> Result<Self, FrameError> {
        let stream_id = r.read_u32()?;
        let stream_type = StreamType::from_u8(r.read_u8()?)?;
        let purpose = r.read_u16()?;
        Ok(Self {
            stream_id,
            stream_type,
            purpose,
        })
    }

    fn encode_data(&self, w: &mut Writer) {
        w.write_u32(self.stream_id);
        w.write_u8(self.stream_type as u8);
        w.write_u16(self.purpose);
    }
}

impl StreamData {
    fn decode_data(r: &mut Reader) -> Result<Self, FrameError> {
        let stream_id = r.read_u32()?;
        let offset = r.read_u64()?;
        let fin = r.read_u8()? != 0;
        let data = r.remaining().to_vec();
        Ok(Self {
            stream_id,
            offset,
            fin,
            data,
        })
    }

    fn encode_data(&self, w: &mut Writer) {
        w.write_u32(self.stream_id);
        w.write_u64(self.offset);
        w.write_u8(if self.fin { 0x01 } else { 0x00 });
        w.write_bytes(&self.data);
    }
}

impl StreamClose {
    fn decode_data(r: &mut Reader) -> Result<Self, FrameError> {
        Ok(Self {
            stream_id: r.read_u32()?,
        })
    }

    fn encode_data(&self, w: &mut Writer) {
        w.write_u32(self.stream_id);
    }
}

impl StreamReset {
    fn decode_data(r: &mut Reader) -> Result<Self, FrameError> {
        Ok(Self {
            stream_id: r.read_u32()?,
            error_code: r.read_u32()?,
        })
    }

    fn encode_data(&self, w: &mut Writer) {
        w.write_u32(self.stream_id);
        w.write_u32(self.error_code);
    }
}

impl WindowUpdate {
    fn decode_data(r: &mut Reader) -> Result<Self, FrameError> {
        Ok(Self {
            stream_id: r.read_u32()?,
            max_offset: r.read_u64()?,
        })
    }

    fn encode_data(&self, w: &mut Writer) {
        w.write_u32(self.stream_id);
        w.write_u64(self.max_offset);
    }
}

impl DatagramFragment {
    fn decode_data(r: &mut Reader) -> Result<Self, FrameError> {
        let stream_id = r.read_u32()?;
        let message_id = r.read_u32()?;
        let fragment_index = r.read_u16()?;
        let fragment_total = r.read_u16()?;
        let data = r.remaining().to_vec();
        Ok(Self {
            stream_id,
            message_id,
            fragment_index,
            fragment_total,
            data,
        })
    }

    fn encode_data(&self, w: &mut Writer) {
        w.write_u32(self.stream_id);
        w.write_u32(self.message_id);
        w.write_u16(self.fragment_index);
        w.write_u16(self.fragment_total);
        w.write_bytes(&self.data);
    }
}

impl Datagram {
    fn decode_data(r: &mut Reader) -> Result<Self, FrameError> {
        let stream_id = r.read_u32()?;
        let data = r.remaining().to_vec();
        Ok(Self { stream_id, data })
    }

    fn encode_data(&self, w: &mut Writer) {
        w.write_u32(self.stream_id);
        w.write_bytes(&self.data);
    }
}

impl Auth {
    fn decode_data(r: &mut Reader) -> Result<Self, FrameError> {
        let fragment_index = r.read_u8()?;
        let fragment_total = r.read_u8()?;
        let data = r.remaining().to_vec();
        Ok(Self {
            fragment_index,
            fragment_total,
            data,
        })
    }

    fn encode_data(&self, w: &mut Writer) {
        w.write_u8(self.fragment_index);
        w.write_u8(self.fragment_total);
        w.write_bytes(&self.data);
    }
}

impl Rekey {
    fn decode_data(r: &mut Reader) -> Result<Self, FrameError> {
        let fragment_index = r.read_u8()?;
        let fragment_total = r.read_u8()?;
        let data = r.remaining().to_vec();
        Ok(Self {
            fragment_index,
            fragment_total,
            data,
        })
    }

    fn encode_data(&self, w: &mut Writer) {
        w.write_u8(self.fragment_index);
        w.write_u8(self.fragment_total);
        w.write_bytes(&self.data);
    }
}

impl RekeyAck {
    fn decode_data(r: &mut Reader) -> Result<Self, FrameError> {
        let fragment_index = r.read_u8()?;
        let fragment_total = r.read_u8()?;
        let data = r.remaining().to_vec();
        Ok(Self {
            fragment_index,
            fragment_total,
            data,
        })
    }

    fn encode_data(&self, w: &mut Writer) {
        w.write_u8(self.fragment_index);
        w.write_u8(self.fragment_total);
        w.write_bytes(&self.data);
    }
}

impl ConnectionClose {
    fn decode_data(r: &mut Reader) -> Result<Self, FrameError> {
        let error_code = r.read_u32()?;
        let reason_length = r.read_u16()? as usize;
        let reason = r.read_bytes(reason_length)?.to_vec();
        Ok(Self { error_code, reason })
    }

    fn encode_data(&self, w: &mut Writer) {
        w.write_u32(self.error_code);
        w.write_u16(self.reason.len() as u16);
        w.write_bytes(&self.reason);
    }
}
