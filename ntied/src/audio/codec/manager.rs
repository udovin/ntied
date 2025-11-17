use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use super::{CodecCapabilities, CodecType, NegotiatedCodec};

/// Simplified codec manager for backward compatibility
/// In new architecture, Encoder/Decoder handle codec management directly
#[derive(Clone)]
pub struct CodecManager {
    capabilities: Arc<RwLock<CodecCapabilities>>,
}

impl CodecManager {
    /// Create a new codec manager with default codecs
    pub fn new() -> Self {
        Self {
            capabilities: Arc::new(RwLock::new(CodecCapabilities::default())),
        }
    }

    /// Get codec capabilities
    pub async fn capabilities(&self) -> CodecCapabilities {
        self.capabilities.read().await.clone()
    }

    /// Create a codec offer (simplified - just returns default codec)
    pub fn create_offer(&self) -> NegotiatedCodec {
        NegotiatedCodec {
            codec: CodecType::ADPCM,
            params: super::CodecParams::adpcm(),
            is_offerer: true,
        }
    }

    /// Create a codec answer based on peer capabilities
    pub fn create_answer(&self, _peer_caps: &CodecCapabilities) -> Result<NegotiatedCodec> {
        // Simplified: just accept ADPCM
        Ok(NegotiatedCodec {
            codec: CodecType::ADPCM,
            params: super::CodecParams::adpcm(),
            is_offerer: false,
        })
    }

    /// Initialize codec (no-op in new architecture)
    pub async fn initialize(&self, _negotiated: &NegotiatedCodec) -> Result<()> {
        // No-op: encoder/decoder create their own codec instances
        Ok(())
    }
}

impl Default for CodecManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_codec_manager_initialization() {
        let manager = CodecManager::new();
        let caps = manager.capabilities().await;
        assert!(!caps.codecs.is_empty());
    }

    #[tokio::test]
    async fn test_create_offer() {
        let manager = CodecManager::new();
        let offer = manager.create_offer();
        assert_eq!(offer.codec, CodecType::ADPCM);
    }

    #[tokio::test]
    async fn test_create_answer() {
        let manager = CodecManager::new();
        let caps = CodecCapabilities::default();
        let answer = manager.create_answer(&caps).unwrap();
        assert_eq!(answer.codec, CodecType::ADPCM);
    }
}
