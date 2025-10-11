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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallStartPacket {
    pub call_id: Uuid,
    pub video_enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallAcceptPacket {
    pub call_id: Uuid,
    pub video_enabled: bool,
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
    pub sequence: u32,     // Sequence number for packet ordering
    pub timestamp: u64,    // Timestamp in milliseconds
    pub samples: Vec<f32>, // Raw audio samples instead of encoded data
    pub sample_rate: u32,  // Sample rate (e.g., 48000)
    pub channels: u16,     // Number of channels (e.g., 1 for mono)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoDataPacket {
    pub call_id: Uuid,
    pub timestamp: u64,
    pub frame: Vec<u8>,
}
