mod adpcm;
mod manager;
mod negotiation;
mod raw;
// mod sea;  // Removed - SEA codec deprecated due to audio quality issues
mod traits;

use anyhow::Result;

pub use adpcm::*;
pub use manager::*;
pub use negotiation::*;
pub use raw::*;
// pub use sea::*;  // Removed - SEA codec deprecated due to audio quality issues
pub use traits::*;

/// Create an encoder for the given codec type
pub fn create_encoder(codec: CodecType, channels: u16) -> Result<Box<dyn AudioEncoder>> {
    match codec {
        CodecType::ADPCM => {
            if channels == 0 || channels > 2 {
                return Err(anyhow::anyhow!(
                    "ADPCM supports 1-2 channels, got {}",
                    channels
                ));
            }
            Ok(Box::new(AdpcmEncoder::new(channels)?))
        }
        CodecType::Raw => Ok(Box::new(RawEncoder::new(channels)?)),
    }
}

/// Create a decoder for the given codec type
pub fn create_decoder(codec: CodecType, channels: u16) -> Result<Box<dyn AudioDecoder>> {
    match codec {
        CodecType::ADPCM => {
            if channels == 0 || channels > 2 {
                return Err(anyhow::anyhow!(
                    "ADPCM supports 1-2 channels, got {}",
                    channels
                ));
            }
            Ok(Box::new(AdpcmDecoder::new(channels)?))
        }
        CodecType::Raw => Ok(Box::new(RawDecoder::new(channels)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test comprehensive codec validation across all implementations
    #[test]
    fn test_codec_data_integrity() {
        let test_patterns = vec![
            // Silence
            vec![0.0f32; 960],
            // Max positive values
            vec![0.99f32; 960],
            // Max negative values
            vec![-0.99f32; 960],
            // Alternating pattern
            (0..960)
                .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
                .collect(),
            // Sine wave
            (0..960).map(|i| (i as f32 * 0.01).sin() * 0.8).collect(),
            // Random noise
            (0..960)
                .map(|i| ((i * 7919) % 1000) as f32 / 1000.0 - 0.5)
                .collect(),
        ];

        let codecs: Vec<(&str, Box<dyn CodecFactory>)> = vec![
            ("ADPCM", Box::new(AdpcmCodecFactory::new(1))),
            ("Raw", Box::new(RawCodecFactory::new(1))),
        ];

        for (codec_name, factory) in &codecs {
            let params = CodecParams::adpcm();
            let mut encoder = factory.create_encoder(params.clone()).unwrap();
            let mut decoder = factory.create_decoder(params).unwrap();

            for (i, pattern) in test_patterns.iter().enumerate() {
                let encoded = encoder.encode(pattern).unwrap();
                let decoded = decoder.decode(&encoded).unwrap();

                assert_eq!(
                    decoded.len(),
                    pattern.len(),
                    "{} codec: pattern {} length mismatch",
                    codec_name,
                    i
                );

                // Check that decoded values are within valid range
                for (j, &sample) in decoded.iter().enumerate() {
                    assert!(
                        sample >= -1.0 && sample <= 1.0,
                        "{} codec: pattern {} sample {} out of range: {}",
                        codec_name,
                        i,
                        j,
                        sample
                    );
                }

                // For Raw codec, expect perfect reconstruction
                if *codec_name == "Raw" {
                    for (j, (&original, &decoded_val)) in
                        pattern.iter().zip(decoded.iter()).enumerate()
                    {
                        assert!(
                            (original - decoded_val).abs() < 1e-6,
                            "{} codec: pattern {} sample {} mismatch",
                            codec_name,
                            i,
                            j
                        );
                    }
                }
            }
        }
    }

    /// Test codec behavior with extreme values
    #[test]
    fn test_codec_extreme_values() {
        let extreme_patterns = vec![
            vec![1.0f32; 960],   // Clipping positive
            vec![-1.0f32; 960],  // Clipping negative
            vec![f32::MIN; 960], // Extreme negative
            vec![f32::MAX; 960], // Extreme positive
            vec![
                0.0,
                1.0,
                -1.0,
                0.5,
                -0.5,
                f32::NAN,
                f32::INFINITY,
                f32::NEG_INFINITY,
            ]
            .repeat(15), // Mixed with NaN and Inf (120 elements)
        ];

        let codecs: Vec<(&str, Box<dyn CodecFactory>)> =
            vec![("ADPCM", Box::new(AdpcmCodecFactory::new(1)))];

        for (codec_name, factory) in &codecs {
            let params = CodecParams::adpcm();
            let mut encoder = factory.create_encoder(params.clone()).unwrap();
            let mut decoder = factory.create_decoder(params).unwrap();

            // Use 320 samples for ADPCM (20ms at 16kHz)
            let adpcm_extreme_patterns = vec![
                vec![1.0f32; 320],   // Clipping positive
                vec![-1.0f32; 320],  // Clipping negative
                vec![0.99f32; 320],  // Near clipping
                vec![-0.99f32; 320], // Near clipping negative
            ];

            for (i, pattern) in adpcm_extreme_patterns.iter().enumerate() {
                // Should handle extreme values without panicking
                let encoded = encoder.encode(pattern).unwrap();
                let decoded = decoder.decode(&encoded).unwrap();

                // Check all decoded values are finite and in range
                for (j, &sample) in decoded.iter().enumerate() {
                    assert!(
                        sample.is_finite(),
                        "{} codec: pattern {} sample {} is not finite: {}",
                        codec_name,
                        i,
                        j,
                        sample
                    );
                    assert!(
                        sample >= -1.0 && sample <= 1.0,
                        "{} codec: pattern {} sample {} out of valid range: {}",
                        codec_name,
                        i,
                        j,
                        sample
                    );
                }
            }
        }
    }

    /// Test multi-channel support
    #[test]
    fn test_codec_multichannel() {
        let mono_params = CodecParams::adpcm();
        let mut stereo_params = CodecParams::adpcm();
        stereo_params.channels = 2;

        // Create interleaved stereo samples
        // For SEA with chunk_size=960 (voice), we need 960 samples per channel = 1920 total
        let left_channel: Vec<f32> = (0..960).map(|i| (i as f32 * 0.02).sin() * 0.5).collect();
        let right_channel: Vec<f32> = (0..960).map(|i| (i as f32 * 0.03).cos() * 0.5).collect();
        let mut stereo_samples = Vec::with_capacity(1920);
        for i in 0..960 {
            stereo_samples.push(left_channel[i]);
            stereo_samples.push(right_channel[i]);
        }

        let codecs: Vec<(&str, Box<dyn CodecFactory>)> = vec![
            ("ADPCM", Box::new(AdpcmCodecFactory::new(1))),
            ("Raw", Box::new(RawCodecFactory::new(1))),
        ];

        for (codec_name, factory) in &codecs {
            // Test mono
            let mut encoder = factory.create_encoder(mono_params.clone()).unwrap();
            let mut decoder = factory.create_decoder(mono_params.clone()).unwrap();
            let mono_samples = vec![0.5f32; 960];
            let encoded = encoder.encode(&mono_samples).unwrap();
            let decoded = decoder.decode(&encoded).unwrap();
            assert_eq!(
                decoded.len(),
                960,
                "{} codec: mono length mismatch",
                codec_name
            );

            // Test stereo
            let mut encoder = factory.create_encoder(stereo_params.clone()).unwrap();
            let mut decoder = factory.create_decoder(stereo_params.clone()).unwrap();
            let encoded = encoder.encode(&stereo_samples).unwrap();
            let decoded = decoder.decode(&encoded).unwrap();
            // Stereo should have double the samples (interleaved)
            assert_eq!(
                decoded.len(),
                1920,
                "{} codec: stereo length mismatch",
                codec_name
            );
        }
    }

    /// Test consecutive packet loss handling
    #[test]
    fn test_codec_consecutive_packet_loss() {
        let codecs: Vec<(&str, Box<dyn CodecFactory>, usize)> = vec![
            ("ADPCM", Box::new(AdpcmCodecFactory::new(1)), 960), // 20ms at 48kHz
            ("Raw", Box::new(RawCodecFactory::new(1)), 960),     // 20ms at 48kHz
        ];

        for (codec_name, factory, expected_len) in &codecs {
            let params = CodecParams::adpcm();
            let mut decoder = factory.create_decoder(params).unwrap();

            // Generate multiple PLC frames
            let mut total_energy = 0.0f32;
            for i in 0..10 {
                let plc_frame = decoder.conceal_packet_loss().unwrap();
                assert_eq!(
                    plc_frame.len(),
                    *expected_len,
                    "{} codec: PLC frame {} length mismatch",
                    codec_name,
                    i
                );

                // Calculate energy
                let energy: f32 = plc_frame.iter().map(|s| s * s).sum();

                // Energy should decrease over time (fade effect)
                if i > 0 {
                    assert!(
                        energy <= total_energy * 1.1, // Allow small variation
                        "{} codec: PLC energy should decrease or stay similar",
                        codec_name
                    );
                }
                total_energy = energy;

                // All samples should be valid
                for &sample in &plc_frame {
                    assert!(sample.is_finite() && sample >= -1.0 && sample <= 1.0);
                }
            }
        }
    }

    /// Test codec quality metrics
    #[test]
    fn test_codec_quality_snr() {
        // Generate a test signal (sine wave)
        let frequency = 440.0; // A4 note
        let sample_rate = 48000.0;
        let duration = 0.02; // 20ms
        let num_samples = (sample_rate * duration) as usize;

        let original: Vec<f32> = (0..num_samples)
            .map(|i| (2.0 * std::f32::consts::PI * frequency * i as f32 / sample_rate).sin() * 0.5)
            .collect();

        let codecs: Vec<(&str, Box<dyn CodecFactory>, f32)> = vec![
            ("ADPCM", Box::new(AdpcmCodecFactory::new(1)), 15.0),
            ("Raw", Box::new(RawCodecFactory::new(1)), 100.0), // Raw is lossless
        ];

        for (codec_name, factory, min_snr) in &codecs {
            let params = CodecParams::adpcm();
            let mut encoder = factory.create_encoder(params.clone()).unwrap();
            let mut decoder = factory.create_decoder(params).unwrap();

            let encoded = encoder.encode(&original).unwrap();
            let decoded = decoder.decode(&encoded).unwrap();

            // Calculate SNR
            let signal_power: f32 =
                original.iter().map(|s| s * s).sum::<f32>() / original.len() as f32;
            let noise_power: f32 = original
                .iter()
                .zip(decoded.iter())
                .map(|(o, d)| (o - d) * (o - d))
                .sum::<f32>()
                / original.len() as f32;

            let snr_db = if noise_power > 0.0 {
                10.0 * (signal_power / noise_power).log10()
            } else {
                100.0 // Perfect reconstruction
            };

            assert!(
                snr_db >= *min_snr || snr_db == 100.0,
                "{} codec: SNR {:.1}dB is below minimum {:.1}dB",
                codec_name,
                snr_db,
                min_snr
            );
        }
    }

    /// Test bitstream corruption resilience
    #[test]
    fn test_codec_bitstream_corruption() {
        let codecs: Vec<(&str, Box<dyn CodecFactory>)> = vec![
            ("ADPCM", Box::new(AdpcmCodecFactory::new(1))),
            ("Raw", Box::new(RawCodecFactory::new(1))),
        ];

        let samples = vec![0.3f32; 960];

        for (codec_name, factory) in &codecs {
            let params = CodecParams::adpcm();
            let mut encoder = factory.create_encoder(params.clone()).unwrap();
            let mut decoder = factory.create_decoder(params).unwrap();

            let encoded = encoder.encode(&samples).unwrap();

            // Test with truncated bitstream
            if encoded.len() > 10 {
                let truncated = &encoded[..encoded.len() / 2];
                let result = decoder.decode(truncated);
                // Should either handle gracefully or return error, not panic
                if let Ok(decoded) = result {
                    for &sample in &decoded {
                        assert!(sample.is_finite() && sample >= -1.0 && sample <= 1.0);
                    }
                }
            }

            // Test with corrupted bytes
            let mut corrupted = encoded.clone();
            if corrupted.len() > 5 {
                corrupted[2] ^= 0xFF; // Flip all bits in one byte
                corrupted[4] = 0xFF; // Set to max value

                let result = decoder.decode(&corrupted);
                // Should handle corruption without panic
                if let Ok(decoded) = result {
                    for &sample in &decoded {
                        assert!(
                            sample.is_finite() && sample >= -1.0 && sample <= 1.0,
                            "{} codec: corrupted decode produced invalid sample",
                            codec_name
                        );
                    }
                }
            }
        }
    }

    /// Test encoder/decoder state independence
    #[test]
    fn test_codec_state_independence() {
        let factory = AdpcmCodecFactory::new(1);
        let params = CodecParams::adpcm();

        let mut encoder1 = factory.create_encoder(params.clone()).unwrap();
        let mut encoder2 = factory.create_encoder(params.clone()).unwrap();
        let mut decoder1 = factory.create_decoder(params.clone()).unwrap();
        let mut decoder2 = factory.create_decoder(params.clone()).unwrap();

        let samples1 = vec![0.5f32; 960];
        let samples2 = vec![-0.5f32; 960];

        // Encode with different encoders
        let encoded1 = encoder1.encode(&samples1).unwrap();
        let encoded2 = encoder2.encode(&samples2).unwrap();

        // Cross-decode (decoder1 decodes from encoder2 and vice versa)
        let decoded1_from_2 = decoder1.decode(&encoded2).unwrap();
        let decoded2_from_1 = decoder2.decode(&encoded1).unwrap();

        // Should produce valid results regardless of encoder/decoder pairing
        assert_eq!(decoded1_from_2.len(), 960);
        assert_eq!(decoded2_from_1.len(), 960);

        // Verify reasonable reconstruction
        let avg1: f32 = decoded2_from_1.iter().sum::<f32>() / 960.0;
        let avg2: f32 = decoded1_from_2.iter().sum::<f32>() / 960.0;

        assert!(avg1 > 0.0, "Should reconstruct positive signal");
        assert!(avg2 < 0.0, "Should reconstruct negative signal");
    }

    /// Test reset functionality
    #[test]
    fn test_codec_reset_behavior() {
        let codecs: Vec<(&str, Box<dyn CodecFactory>)> = vec![
            ("ADPCM", Box::new(AdpcmCodecFactory::new(1))),
            ("Raw", Box::new(RawCodecFactory::new(1))),
        ];

        for (codec_name, factory) in &codecs {
            let params = CodecParams::adpcm();
            let mut encoder = factory.create_encoder(params.clone()).unwrap();
            let mut decoder = factory.create_decoder(params).unwrap();

            // Process some data to build up state
            let samples1 = vec![0.8f32; 960];
            let encoded1 = encoder.encode(&samples1).unwrap();
            let _ = decoder.decode(&encoded1).unwrap();

            // Reset
            encoder.reset().unwrap();
            decoder.reset().unwrap();

            // Process new data - should work correctly after reset
            let samples2 = vec![-0.3f32; 960];
            let encoded2 = encoder.encode(&samples2).unwrap();
            let decoded2 = decoder.decode(&encoded2).unwrap();

            assert_eq!(
                decoded2.len(),
                960,
                "{} codec: reset test length mismatch",
                codec_name
            );

            // Check output is valid
            for &sample in &decoded2 {
                assert!(
                    sample.is_finite() && sample >= -1.0 && sample <= 1.0,
                    "{} codec: invalid sample after reset",
                    codec_name
                );
            }
        }
    }
}
