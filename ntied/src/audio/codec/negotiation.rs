use anyhow::{Result, anyhow};

use super::traits::{CodecCapabilities, CodecParams, CodecType, NegotiatedCodec};

/// Negotiates codec selection between two peers
pub struct CodecNegotiator {
    local_capabilities: CodecCapabilities,
}

impl CodecNegotiator {
    /// Create a new codec negotiator with local capabilities
    pub fn new(local_capabilities: CodecCapabilities) -> Self {
        Self { local_capabilities }
    }

    /// Create negotiator with default capabilities
    pub fn default() -> Self {
        Self::new(CodecCapabilities::default())
    }

    /// Negotiate codec as offerer (initiator of the call)
    ///
    /// Returns the proposed codec configuration
    pub fn create_offer(&self) -> NegotiatedCodec {
        // Choose the best codec we support
        let codec = self
            .local_capabilities
            .codecs
            .first()
            .copied()
            .unwrap_or(CodecType::Raw);

        // Create parameters based on codec type
        let params = self.create_params_for_codec(codec);

        NegotiatedCodec {
            codec,
            params,
            is_offerer: true,
        }
    }

    /// Negotiate codec as answerer (receiver of the call)
    ///
    /// # Arguments
    /// * `remote_capabilities` - The capabilities of the remote peer
    ///
    /// Returns the negotiated codec configuration
    pub fn create_answer(
        &self,
        remote_capabilities: &CodecCapabilities,
    ) -> Result<NegotiatedCodec> {
        // Find the best codec that both sides support
        let codec = self
            .find_common_codec(remote_capabilities)
            .ok_or_else(|| anyhow!("No common codec found"))?;

        // Find common sample rate
        let sample_rate = self
            .find_common_sample_rate(remote_capabilities)
            .unwrap_or(48000);

        // Determine channel configuration
        let channels = self
            .local_capabilities
            .max_channels
            .min(remote_capabilities.max_channels)
            .min(2); // Limit to stereo

        // Determine bitrate
        let _bitrate = self
            .local_capabilities
            .max_bitrate
            .min(remote_capabilities.max_bitrate);

        // Determine feature support
        let fec = self.local_capabilities.supports_fec
            && remote_capabilities.supports_fec
            && codec.supports_fec();
        let dtx = self.local_capabilities.supports_dtx
            && remote_capabilities.supports_dtx
            && codec.supports_dtx();

        let params = CodecParams {
            sample_rate,
            channels,
            bitrate: codec.typical_bitrate() * 1000, // Convert to bps
            fec,
            dtx,
            expected_packet_loss: 5,
            complexity: 10,
        };

        Ok(NegotiatedCodec {
            codec,
            params,
            is_offerer: false,
        })
    }

    /// Process remote answer and finalize negotiation
    ///
    /// # Arguments
    /// * `local_offer` - The offer we sent
    /// * `remote_answer` - The answer from remote peer
    ///
    /// Returns the final negotiated codec configuration
    pub fn process_answer(
        &self,
        _local_offer: &NegotiatedCodec,
        remote_answer: &NegotiatedCodec,
    ) -> Result<NegotiatedCodec> {
        // Verify the codec is acceptable
        if !self
            .local_capabilities
            .codecs
            .contains(&remote_answer.codec)
        {
            return Err(anyhow!(
                "Remote selected unsupported codec: {:?}",
                remote_answer.codec
            ));
        }

        // Verify parameters are within our capabilities
        if !self
            .local_capabilities
            .sample_rates
            .contains(&remote_answer.params.sample_rate)
        {
            return Err(anyhow!(
                "Remote selected unsupported sample rate: {}",
                remote_answer.params.sample_rate
            ));
        }

        if remote_answer.params.channels > self.local_capabilities.max_channels {
            return Err(anyhow!(
                "Remote selected too many channels: {}",
                remote_answer.params.channels
            ));
        }

        // Accept the remote answer
        Ok(remote_answer.clone())
    }

    /// Find the best common codec between local and remote capabilities
    fn find_common_codec(&self, remote: &CodecCapabilities) -> Option<CodecType> {
        // Build a priority map for remote codecs
        let remote_priority: std::collections::HashMap<CodecType, usize> = remote
            .codecs
            .iter()
            .enumerate()
            .map(|(i, &c)| (c, i))
            .collect();

        // Find codec with best combined priority
        let mut candidates: Vec<(CodecType, usize)> = self
            .local_capabilities
            .codecs
            .iter()
            .enumerate()
            .filter_map(|(local_idx, &codec)| {
                remote_priority
                    .get(&codec)
                    .map(|&remote_idx| (codec, local_idx + remote_idx))
            })
            .collect();

        // Sort by combined priority (lower is better)
        candidates.sort_by_key(|&(codec, combined_priority)| {
            (combined_priority, std::cmp::Reverse(codec.priority()))
        });

        candidates.first().map(|&(codec, _)| codec)
    }

    /// Find a common sample rate between local and remote capabilities
    fn find_common_sample_rate(&self, remote: &CodecCapabilities) -> Option<u32> {
        // Prefer higher sample rates for better quality
        let preferred_rates = [48000, 44100, 32000, 24000, 16000, 8000];

        for rate in preferred_rates {
            if self.local_capabilities.sample_rates.contains(&rate)
                && remote.sample_rates.contains(&rate)
            {
                return Some(rate);
            }
        }

        // Find any common rate
        self.local_capabilities
            .sample_rates
            .iter()
            .find(|rate| remote.sample_rates.contains(rate))
            .copied()
    }

    /// Create optimized parameters for a specific codec
    fn create_params_for_codec(&self, codec: CodecType) -> CodecParams {
        match codec {
            CodecType::Raw => CodecParams {
                sample_rate: self
                    .local_capabilities
                    .sample_rates
                    .first()
                    .copied()
                    .unwrap_or(48000),
                channels: 1,
                bitrate: 0, // Not applicable
                fec: false,
                dtx: false,
                expected_packet_loss: 0,
                complexity: 0,
            },
            _ => CodecParams::default(),
        }
    }

    /// Update local capabilities
    pub fn set_capabilities(&mut self, capabilities: CodecCapabilities) {
        self.local_capabilities = capabilities;
    }

    /// Get local capabilities
    pub fn capabilities(&self) -> &CodecCapabilities {
        &self.local_capabilities
    }
}

/// Manages codec selection for adaptive quality
pub struct AdaptiveCodecManager {
    #[allow(unused)]
    negotiator: CodecNegotiator,
    current_codec: Option<NegotiatedCodec>,
    network_quality: NetworkQuality,
}

#[derive(Debug, Clone, Copy)]
pub struct NetworkQuality {
    /// Packet loss percentage (0-100)
    pub packet_loss: f32,
    /// Round-trip time in milliseconds
    pub rtt: f32,
    /// Available bandwidth in kbps
    pub bandwidth: u32,
    /// Jitter in milliseconds
    pub jitter: f32,
}

impl Default for NetworkQuality {
    fn default() -> Self {
        Self {
            packet_loss: 0.0,
            rtt: 50.0,
            bandwidth: 1000,
            jitter: 5.0,
        }
    }
}

impl AdaptiveCodecManager {
    pub fn new(negotiator: CodecNegotiator) -> Self {
        Self {
            negotiator,
            current_codec: None,
            network_quality: NetworkQuality::default(),
        }
    }

    /// Update network quality metrics
    pub fn update_network_quality(&mut self, quality: NetworkQuality) {
        self.network_quality = quality;
    }

    /// Get recommended codec parameters based on current network conditions
    pub fn get_adaptive_params(&self) -> Option<CodecParams> {
        self.current_codec.as_ref().map(|codec| {
            let mut params = codec.params.clone();

            // First, set base bitrate based on bandwidth
            if self.network_quality.bandwidth < 50 {
                params.bitrate = 16000; // Minimum voice quality
                params.complexity = 5; // Reduce CPU usage
            } else if self.network_quality.bandwidth < 100 {
                params.bitrate = 24000;
                params.complexity = 8;
            } else if self.network_quality.bandwidth < 200 {
                params.bitrate = 32000;
            } else {
                params.bitrate = 48000; // High quality
            }

            // Then adjust based on packet loss (after setting base bitrate)
            if self.network_quality.packet_loss > 10.0 {
                params.fec = true;
                params.expected_packet_loss = self.network_quality.packet_loss as u8;
                params.bitrate = params.bitrate.saturating_sub(8000); // Reduce bitrate
            } else if self.network_quality.packet_loss > 5.0 {
                params.fec = true;
                params.expected_packet_loss = self.network_quality.packet_loss as u8;
            }

            // Enable DTX on high latency or low bandwidth
            if self.network_quality.rtt > 200.0 || self.network_quality.bandwidth < 100 {
                params.dtx = true;
            }

            params
        })
    }

    /// Set the current negotiated codec
    pub fn set_current_codec(&mut self, codec: NegotiatedCodec) {
        self.current_codec = Some(codec);
    }

    /// Get the current codec
    pub fn current_codec(&self) -> Option<&NegotiatedCodec> {
        self.current_codec.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codec_negotiation() {
        // Setup local capabilities
        let local_caps = CodecCapabilities {
            codecs: vec![CodecType::SEA, CodecType::Raw],
            sample_rates: vec![48000, 16000],
            max_channels: 2,
            max_bitrate: 128000,
            supports_fec: true,
            supports_dtx: true,
        };

        // Setup remote capabilities
        let remote_caps = CodecCapabilities {
            codecs: vec![CodecType::Raw, CodecType::SEA], // Different preference
            sample_rates: vec![48000, 32000, 16000],
            max_channels: 1,
            max_bitrate: 64000,
            supports_fec: true,
            supports_dtx: false,
        };

        let negotiator = CodecNegotiator::new(local_caps);

        // Create answer based on remote capabilities
        let answer = negotiator.create_answer(&remote_caps).unwrap();

        // Should select SEA (best common codec)
        assert_eq!(answer.codec, CodecType::SEA);
        // Should select common sample rate
        assert_eq!(answer.params.sample_rate, 48000);
        // Should respect remote channel limit
        assert_eq!(answer.params.channels, 1);
        // DTX should be disabled (remote doesn't support)
        assert!(!answer.params.dtx);
        // FEC should be disabled (SEA codec doesn't support FEC even though capabilities say yes)
        assert!(!answer.params.fec);
    }

    #[test]
    fn test_adaptive_params() {
        let negotiator = CodecNegotiator::default();
        let mut adaptive = AdaptiveCodecManager::new(negotiator);

        // Set a codec
        let codec = NegotiatedCodec {
            codec: CodecType::SEA,
            params: CodecParams::voice(),
            is_offerer: true,
        };
        adaptive.set_current_codec(codec);

        // Test high packet loss scenario
        adaptive.update_network_quality(NetworkQuality {
            packet_loss: 15.0,
            rtt: 100.0,
            bandwidth: 200,
            jitter: 10.0,
        });

        let params = adaptive.get_adaptive_params().unwrap();
        assert!(params.fec);
        assert_eq!(params.expected_packet_loss, 15);
        assert_eq!(params.bitrate, 40000); // Base 48000 for bandwidth=200, minus 8000 for packet loss

        // Test low bandwidth scenario
        adaptive.update_network_quality(NetworkQuality {
            packet_loss: 2.0,
            rtt: 50.0,
            bandwidth: 40,
            jitter: 5.0,
        });

        let params = adaptive.get_adaptive_params().unwrap();
        assert_eq!(params.bitrate, 16000); // Minimum bitrate
        assert_eq!(params.complexity, 5); // Reduced complexity
        assert!(params.dtx); // DTX enabled for low bandwidth
    }
}
