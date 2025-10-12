use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use tokio::sync::RwLock;

use super::{
    AdaptiveCodecManager, AdpcmCodecFactory, AudioDecoder, AudioEncoder, CodecCapabilities,
    CodecFactory, CodecNegotiator, CodecStats, CodecType, NegotiatedCodec, NetworkQuality,
    RawCodecFactory,
};

#[cfg(feature = "opus")]
use super::OpusCodecFactory;

/// Manages available codecs and provides encoding/decoding services
pub struct CodecManager {
    factories: HashMap<CodecType, Box<dyn CodecFactory>>,
    encoder: Arc<RwLock<Option<Box<dyn AudioEncoder>>>>,
    decoder: Arc<RwLock<Option<Box<dyn AudioDecoder>>>>,
    current_codec: Arc<RwLock<Option<CodecType>>>,
    negotiator: CodecNegotiator,
    adaptive_manager: Arc<RwLock<AdaptiveCodecManager>>,
}

impl CodecManager {
    /// Create a new codec manager with default codecs
    pub fn new() -> Self {
        let mut factories: HashMap<CodecType, Box<dyn CodecFactory>> = HashMap::new();

        // Register available codecs
        #[cfg(feature = "opus")]
        factories.insert(CodecType::Opus, Box::new(OpusCodecFactory));
        factories.insert(CodecType::PCMU, Box::new(AdpcmCodecFactory)); // Using PCMU for ADPCM
        factories.insert(CodecType::Raw, Box::new(RawCodecFactory));

        // Create capabilities based on available codecs
        let mut available_codecs = Vec::new();
        #[cfg(feature = "opus")]
        if factories
            .get(&CodecType::Opus)
            .map_or(false, |f| f.is_available())
        {
            available_codecs.push(CodecType::Opus);
        }
        available_codecs.push(CodecType::PCMU); // ADPCM compression
        available_codecs.push(CodecType::Raw); // Always available

        let capabilities = CodecCapabilities {
            codecs: available_codecs,
            ..Default::default()
        };

        let negotiator = CodecNegotiator::new(capabilities.clone());
        let adaptive_manager = AdaptiveCodecManager::new(CodecNegotiator::new(capabilities));

        Self {
            factories,
            encoder: Arc::new(RwLock::new(None)),
            decoder: Arc::new(RwLock::new(None)),
            current_codec: Arc::new(RwLock::new(None)),
            negotiator,
            adaptive_manager: Arc::new(RwLock::new(adaptive_manager)),
        }
    }

    /// Initialize encoder and decoder with negotiated codec
    pub async fn initialize(&self, negotiated: &NegotiatedCodec) -> Result<()> {
        let factory = self
            .factories
            .get(&negotiated.codec)
            .ok_or_else(|| anyhow!("Codec factory not found for {:?}", negotiated.codec))?;

        // Create encoder
        let encoder = factory.create_encoder(negotiated.params.clone())?;
        *self.encoder.write().await = Some(encoder);

        // Create decoder
        let decoder = factory.create_decoder(negotiated.params.clone())?;
        *self.decoder.write().await = Some(decoder);

        // Store current codec
        *self.current_codec.write().await = Some(negotiated.codec);

        // Update adaptive manager
        self.adaptive_manager
            .write()
            .await
            .set_current_codec(negotiated.clone());

        tracing::info!(
            "Initialized codec: {:?} at {} Hz, {} channels, {} bps",
            negotiated.codec,
            negotiated.params.sample_rate,
            negotiated.params.channels,
            negotiated.params.bitrate
        );

        Ok(())
    }

    /// Encode audio samples
    pub async fn encode(&self, samples: &[f32]) -> Result<(CodecType, Vec<u8>)> {
        let mut encoder_guard = self.encoder.write().await;
        let encoder = encoder_guard
            .as_mut()
            .ok_or_else(|| anyhow!("Encoder not initialized"))?;

        let codec = self
            .current_codec
            .read()
            .await
            .ok_or_else(|| anyhow!("No codec selected"))?;

        let encoded = encoder.encode(samples)?;
        Ok((codec, encoded))
    }

    /// Decode audio data
    pub async fn decode(&self, codec: CodecType, data: &[u8]) -> Result<Vec<f32>> {
        // Check if we need to switch codec
        let current = *self.current_codec.read().await;
        if current != Some(codec) {
            tracing::warn!(
                "Received data with different codec: {:?}, current: {:?}",
                codec,
                current
            );
            // For now, we'll try to decode with the current decoder
            // In a full implementation, we might want to create a new decoder
        }

        let mut decoder_guard = self.decoder.write().await;
        let decoder = decoder_guard
            .as_mut()
            .ok_or_else(|| anyhow!("Decoder not initialized"))?;

        decoder.decode(data)
    }

    /// Handle packet loss by generating concealment frame
    pub async fn conceal_packet_loss(&self) -> Result<Vec<f32>> {
        let mut decoder_guard = self.decoder.write().await;
        let decoder = decoder_guard
            .as_mut()
            .ok_or_else(|| anyhow!("Decoder not initialized"))?;

        decoder.conceal_packet_loss()
    }

    /// Update codec parameters based on network conditions
    pub async fn update_network_quality(&self, quality: NetworkQuality) -> Result<()> {
        let mut adaptive = self.adaptive_manager.write().await;
        adaptive.update_network_quality(quality);

        // Get adaptive parameters
        if let Some(params) = adaptive.get_adaptive_params() {
            // Update encoder parameters
            if let Some(ref mut encoder) = *self.encoder.write().await {
                encoder.set_bitrate(params.bitrate)?;
                encoder.set_packet_loss(params.expected_packet_loss)?;
            }
        }

        Ok(())
    }

    /// Create codec offer for negotiation
    pub fn create_offer(&self) -> NegotiatedCodec {
        self.negotiator.create_offer()
    }

    /// Create codec answer for negotiation
    pub fn create_answer(&self, remote_caps: &CodecCapabilities) -> Result<NegotiatedCodec> {
        self.negotiator.create_answer(remote_caps)
    }

    /// Process remote answer to finalize negotiation
    pub fn process_answer(
        &self,
        local_offer: &NegotiatedCodec,
        remote_answer: &NegotiatedCodec,
    ) -> Result<NegotiatedCodec> {
        self.negotiator.process_answer(local_offer, remote_answer)
    }

    /// Get encoder statistics
    pub async fn encoder_stats(&self) -> Option<CodecStats> {
        self.encoder
            .read()
            .await
            .as_ref()
            .map(|e| e.stats().clone())
    }

    /// Get decoder statistics
    pub async fn decoder_stats(&self) -> Option<CodecStats> {
        self.decoder
            .read()
            .await
            .as_ref()
            .map(|d| d.stats().clone())
    }

    /// Get current codec type
    pub async fn current_codec(&self) -> Option<CodecType> {
        self.current_codec.read().await.clone()
    }

    /// Get local capabilities
    pub fn capabilities(&self) -> &CodecCapabilities {
        self.negotiator.capabilities()
    }

    /// Reset codec state
    pub async fn reset(&self) -> Result<()> {
        if let Some(ref mut encoder) = *self.encoder.write().await {
            encoder.reset()?;
        }
        if let Some(ref mut decoder) = *self.decoder.write().await {
            decoder.reset()?;
        }
        Ok(())
    }
}

impl Default for CodecManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper to estimate audio quality (Mean Opinion Score)
pub fn estimate_mos(stats: &CodecStats, packet_loss: f32, rtt: f32) -> f32 {
    // Simple MOS estimation (1-5 scale)
    let base_score = 4.5;

    // Deduct for packet loss
    let loss_penalty = (packet_loss * 0.1).min(2.0);

    // Deduct for latency
    let latency_penalty = if rtt < 150.0 {
        0.0
    } else if rtt < 300.0 {
        0.5
    } else {
        1.0
    };

    // Deduct for decode errors
    let error_rate = if stats.frames_decoded > 0 {
        stats.decode_errors as f32 / stats.frames_decoded as f32
    } else {
        0.0
    };
    let error_penalty = (error_rate * 10.0).min(1.0);

    (base_score - loss_penalty - latency_penalty - error_penalty).max(1.0)
}

#[cfg(test)]
mod tests {
    use crate::audio::CodecParams;

    use super::*;

    #[tokio::test]
    async fn test_codec_manager_initialization() {
        let manager = CodecManager::new();

        // Create a negotiated codec
        let negotiated = NegotiatedCodec {
            codec: CodecType::PCMU,
            params: CodecParams::voice(),
            is_offerer: true,
        };

        // Initialize
        manager.initialize(&negotiated).await.unwrap();

        // Check current codec
        assert_eq!(manager.current_codec().await, Some(CodecType::PCMU));
    }

    #[tokio::test]
    async fn test_encode_decode() {
        let manager = CodecManager::new();

        // Initialize with PCMU (ADPCM)
        let negotiated = NegotiatedCodec {
            codec: CodecType::PCMU,
            params: CodecParams::voice(),
            is_offerer: true,
        };
        manager.initialize(&negotiated).await.unwrap();

        // Create test samples (20ms at 48kHz mono)
        let samples = vec![0.0f32; 960];

        // Encode
        let (codec_type, encoded) = manager.encode(&samples).await.unwrap();
        assert_eq!(codec_type, CodecType::PCMU);
        assert!(!encoded.is_empty());

        // Decode
        let decoded = manager.decode(codec_type, &encoded).await.unwrap();
        assert_eq!(decoded.len(), samples.len());
    }

    #[tokio::test]
    async fn test_raw_fallback() {
        let manager = CodecManager::new();

        // Initialize with Raw codec
        let negotiated = NegotiatedCodec {
            codec: CodecType::Raw,
            params: CodecParams::voice(),
            is_offerer: true,
        };
        manager.initialize(&negotiated).await.unwrap();

        // Create test samples
        let samples = vec![0.5, -0.5, 0.3, -0.3];

        // Encode
        let (codec_type, encoded) = manager.encode(&samples).await.unwrap();
        assert_eq!(codec_type, CodecType::Raw);
        assert_eq!(encoded.len(), samples.len() * 4); // 4 bytes per f32

        // Decode
        let decoded = manager.decode(codec_type, &encoded).await.unwrap();
        assert_eq!(decoded.len(), samples.len());

        // Check values match (lossless)
        for i in 0..samples.len() {
            assert!((samples[i] - decoded[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn test_mos_estimation() {
        let stats = CodecStats {
            frames_decoded: 1000,
            decode_errors: 10,
            ..Default::default()
        };

        // Good conditions
        let mos = estimate_mos(&stats, 0.0, 50.0);
        assert!(mos > 4.0);

        // Poor conditions
        let mos = estimate_mos(&stats, 20.0, 500.0);
        assert!(mos < 3.0);
    }
}
