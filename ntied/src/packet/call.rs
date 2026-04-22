use crate::audio::{CodecCapabilities, CodecType, NegotiatedCodec};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CallPacket {
    Start(CallStartPacket),
    Accept(CallAcceptPacket),
    Reject(CallRejectPacket),
    End(CallEndPacket),
    AudioData(AudioDataPacket),
    VideoData(VideoDataPacket),
    CodecOffer(CodecOfferPacket),
    CodecAnswer(CodecAnswerPacket),
    VideoOffer(VideoOfferPacket),
    VideoAnswer(VideoAnswerPacket),
    VideoStop(VideoStopPacket),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallStartPacket {
    pub call_id: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallAcceptPacket {
    pub call_id: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallRejectPacket {
    pub call_id: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallEndPacket {
    pub call_id: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioDataPacket {
    pub call_id: Uuid,
    pub sequence: u32,    // Sequence number for packet ordering
    pub timestamp: u64,   // Unix timestamp in microseconds
    pub codec: CodecType, // Codec used for encoding
    pub channels: u16,    // Number of channels (e.g., 1 for mono)
    pub data: Vec<u8>,    // Encoded audio data
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodecOfferPacket {
    pub call_id: Uuid,
    pub capabilities: CodecCapabilities,
    pub preferred_codec: NegotiatedCodec,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodecAnswerPacket {
    pub call_id: Uuid,
    pub negotiated_codec: NegotiatedCodec,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoDataPacket {
    pub call_id: Uuid,
    pub timestamp: u64,
    pub frame: Vec<u8>,
}

/// Initiator announces intent to start sending screen video. Carries the
/// encoder parameters so the responder can allocate a decoder with the
/// right config before the first frame arrives.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoOfferPacket {
    pub call_id: Uuid,
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    pub codec: VideoCodec,
}

/// Responder accepts or declines the video offer. On `accepted = true`
/// both sides proceed to open/accept the separate video call channel.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoAnswerPacket {
    pub call_id: Uuid,
    pub accepted: bool,
}

/// Either side requests the other to stop sending video. Does not end
/// the call itself — the audio channel stays up.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoStopPacket {
    pub call_id: Uuid,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
}
