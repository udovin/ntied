use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Supported audio codec types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CodecType {
    /// No compression - raw PCM samples
    Raw,
    /// Opus codec - best for voice over IP
    Opus,
    /// G.722 codec - fallback option with lower complexity
    G722,
    /// μ-law (PCMU) - simple compression for telephony
    PCMU,
    /// A-law (PCMA) - simple compression for telephony
    PCMA,
}

impl CodecType {
    /// Get the priority of this codec (higher is better)
    pub fn priority(&self) -> u8 {
        match self {
            CodecType::Opus => 100, // Preferred
            CodecType::G722 => 80,  // Good fallback
            CodecType::PCMU => 60,  // Basic fallback
            CodecType::PCMA => 50,  // Basic fallback
            CodecType::Raw => 10,   // Last resort
        }
    }

    /// Get typical bitrate in kbps for this codec
    pub fn typical_bitrate(&self) -> u32 {
        match self {
            CodecType::Opus => 32, // Variable, but typical for voice
            CodecType::G722 => 64,
            CodecType::PCMU => 64,
            CodecType::PCMA => 64,
            CodecType::Raw => 768, // For 48kHz mono f32
        }
    }

    /// Check if this codec supports Forward Error Correction
    pub fn supports_fec(&self) -> bool {
        matches!(self, CodecType::Opus)
    }

    /// Check if this codec supports Discontinuous Transmission
    pub fn supports_dtx(&self) -> bool {
        matches!(self, CodecType::Opus)
    }
}

impl Default for CodecType {
    fn default() -> Self {
        CodecType::Opus
    }
}

/// Parameters for configuring an audio codec
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodecParams {
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Number of channels (1 = mono, 2 = stereo)
    pub channels: u16,
    /// Target bitrate in bits per second (0 for auto)
    pub bitrate: u32,
    /// Enable Forward Error Correction if supported
    pub fec: bool,
    /// Enable Discontinuous Transmission if supported
    pub dtx: bool,
    /// Packet loss percentage hint for optimization (0-100)
    pub expected_packet_loss: u8,
    /// Complexity/quality trade-off (0-10, 10 = best quality)
    pub complexity: u8,
}

impl Default for CodecParams {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 1,
            bitrate: 32000,
            fec: true,
            dtx: true,
            expected_packet_loss: 5,
            complexity: 10,
        }
    }
}

impl CodecParams {
    /// Create parameters optimized for voice
    pub fn voice() -> Self {
        Self {
            sample_rate: 48000,
            channels: 1,
            bitrate: 32000,
            fec: true,
            dtx: true,
            expected_packet_loss: 5,
            complexity: 10,
        }
    }

    /// Create parameters optimized for music
    pub fn music() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            bitrate: 128000,
            fec: false,
            dtx: false,
            expected_packet_loss: 0,
            complexity: 10,
        }
    }

    /// Create parameters optimized for low bandwidth
    pub fn low_bandwidth() -> Self {
        Self {
            sample_rate: 16000,
            channels: 1,
            bitrate: 16000,
            fec: true,
            dtx: true,
            expected_packet_loss: 10,
            complexity: 5,
        }
    }
}

/// Statistics about codec performance
#[derive(Debug, Clone, Default)]
pub struct CodecStats {
    /// Number of frames encoded
    pub frames_encoded: u64,
    /// Number of frames decoded
    pub frames_decoded: u64,
    /// Number of decode errors
    pub decode_errors: u64,
    /// Total bytes encoded
    pub bytes_encoded: u64,
    /// Total bytes decoded
    pub bytes_decoded: u64,
    /// Average encoding time in microseconds
    pub avg_encode_time_us: f64,
    /// Average decoding time in microseconds
    pub avg_decode_time_us: f64,
    /// Current bitrate in bits per second
    pub current_bitrate: u32,
    /// Number of FEC recoveries
    pub fec_recoveries: u64,
    /// Number of packets with DTX (silence)
    pub dtx_packets: u64,
}

/// Trait for audio encoders
pub trait AudioEncoder: Send + Sync {
    /// Encode raw audio samples into compressed data
    ///
    /// # Arguments
    /// * `samples` - Raw audio samples (normalized to -1.0 to 1.0)
    ///
    /// # Returns
    /// Encoded audio data
    fn encode(&mut self, samples: &[f32]) -> Result<Vec<u8>>;

    /// Reset the encoder state
    fn reset(&mut self) -> Result<()>;

    /// Get current codec parameters
    fn params(&self) -> &CodecParams;

    /// Update codec parameters (may require reset)
    fn set_params(&mut self, params: CodecParams) -> Result<()>;

    /// Set target bitrate dynamically
    fn set_bitrate(&mut self, bitrate: u32) -> Result<()>;

    /// Set expected packet loss for FEC optimization
    fn set_packet_loss(&mut self, percentage: u8) -> Result<()>;

    /// Get encoder statistics
    fn stats(&self) -> &CodecStats;
}

/// Trait for audio decoders
pub trait AudioDecoder: Send + Sync {
    /// Decode compressed audio data into raw samples
    ///
    /// # Arguments
    /// * `data` - Compressed audio data
    ///
    /// # Returns
    /// Decoded audio samples (normalized to -1.0 to 1.0)
    fn decode(&mut self, data: &[u8]) -> Result<Vec<f32>>;

    /// Generate a frame for packet loss concealment
    ///
    /// # Returns
    /// Generated audio samples to fill the gap
    fn conceal_packet_loss(&mut self) -> Result<Vec<f32>>;

    /// Reset the decoder state
    fn reset(&mut self) -> Result<()>;

    /// Get current codec parameters
    fn params(&self) -> &CodecParams;

    /// Update codec parameters (may require reset)
    fn set_params(&mut self, params: CodecParams) -> Result<()>;

    /// Get decoder statistics
    fn stats(&self) -> &CodecStats;
}

/// Factory for creating codec instances
pub trait CodecFactory: Send + Sync {
    /// Get the codec type this factory creates
    fn codec_type(&self) -> CodecType;

    /// Check if this codec is available on the system
    fn is_available(&self) -> bool;

    /// Create a new encoder instance
    fn create_encoder(&self, params: CodecParams) -> Result<Box<dyn AudioEncoder>>;

    /// Create a new decoder instance
    fn create_decoder(&self, params: CodecParams) -> Result<Box<dyn AudioDecoder>>;
}

/// Result of codec negotiation between peers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NegotiatedCodec {
    /// The selected codec type
    pub codec: CodecType,
    /// Negotiated parameters
    pub params: CodecParams,
    /// Whether this peer is the offerer (initiator)
    pub is_offerer: bool,
}

/// Codec capabilities for negotiation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodecCapabilities {
    /// List of supported codecs in preference order
    pub codecs: Vec<CodecType>,
    /// Supported sample rates
    pub sample_rates: Vec<u32>,
    /// Maximum supported channels
    pub max_channels: u16,
    /// Maximum supported bitrate
    pub max_bitrate: u32,
    /// Whether FEC is supported
    pub supports_fec: bool,
    /// Whether DTX is supported
    pub supports_dtx: bool,
}

impl Default for CodecCapabilities {
    fn default() -> Self {
        Self {
            codecs: vec![CodecType::Opus, CodecType::Raw],
            sample_rates: vec![48000, 44100, 32000, 24000, 16000, 8000],
            max_channels: 2,
            max_bitrate: 510000,
            supports_fec: true,
            supports_dtx: true,
        }
    }
}
