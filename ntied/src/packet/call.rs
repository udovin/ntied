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
    pub timestamp: u64,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoDataPacket {
    pub call_id: Uuid,
    pub timestamp: u64,
    pub frame: Vec<u8>,
}
