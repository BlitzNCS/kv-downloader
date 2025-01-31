use anyhow::{anyhow, Result};
use symphonia::core::{
    audio::AudioBufferRef,
    codecs::DecoderOptions,
    formats::FormatOptions,
    io::{ReadOnlySource, MediaSourceStream},
    meta::MetadataOptions,
    probe::Hint,
};
use symphonia::core::audio::Signal;
use symphonia::default::{get_codecs, get_probe};
use hound::{WavWriter, WavSpec};
use std::path::{Path, PathBuf};
use std::fs::File;
use std::io::BufReader;
use std::time::Duration;

pub struct AudioProcessor;

impl AudioProcessor {
    pub fn process_downloads(download_dir: &Path, keep_mp3s: bool) -> Result<()> {
        let (click_path, _other_path) = Self::find_tracks(download_dir)?;
        let click_duration = Self::get_mp3_duration(&click_path)?;
        Self::process_click_track(&click_path)?;
        Self::process_non_click_tracks(download_dir, click_duration)?;

        if !keep_mp3s {
            Self::cleanup_mp3s(download_dir)?;
        }

        Ok(())
    }

    fn find_tracks(dir: &Path) -> Result<(PathBuf, PathBuf)> {
        let mut click = None;
        let mut other = None;

        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                if filename.to_lowercase().contains("click") {
                    click = Some(path);
                } else if other.is_none() {
                    other = Some(path);
                }
            }
        }

        Ok((
            click.ok_or_else(|| anyhow!("Click track not found"))?,
            other.ok_or_else(|| anyhow!("No other tracks found"))?,
        ))
    }

    fn get_mp3_duration(path: &Path) -> Result<Duration> {
        let (spec, samples) = Self::decode_mp3(path)?;
        let duration_seconds = samples.len() as f64 / (spec.channels as f64 * spec.sample_rate as f64);
        Ok(Duration::from_secs_f64(duration_seconds))
    }

    fn transcode_to_wav(src: &Path) -> Result<PathBuf> {
        let (spec, samples) = Self::decode_mp3(src)?;
        let dest = src.with_extension("wav");
        
        let mut writer = WavWriter::create(&dest, spec)?;
        for sample in samples {
            writer.write_sample(sample)?;
        }
        
        Ok(dest)
    }

    fn decode_mp3(path: &Path) -> Result<(WavSpec, Vec<i16>)> {
        let file = File::open(path)?;
        let source = ReadOnlySource::new(BufReader::new(file));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());

        let probe = get_probe();
        let format_opts = FormatOptions::default();
        let metadata_opts = MetadataOptions::default();
        let decoder_opts = DecoderOptions::default();

        let mut probed = probe.format(&Hint::new(), mss, &format_opts, &metadata_opts)?;
        let track = probed.format.default_track().ok_or(anyhow!("No default track"))?;
        let mut decoder = get_codecs().make(&track.codec_params, &decoder_opts)?;
        let mut samples = Vec::new();

        let channels = 2; // Force stereo
        let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);

        while let Ok(packet) = probed.format.next_packet() {
            match decoder.decode(&packet) {
                Ok(buffer) => match buffer {
                    AudioBufferRef::F32(buf) => {
                        for frame in 0..buf.frames() {
                            let left = (buf.chan(0)[frame] * i16::MAX as f32) as i16;
                            let right = if buf.spec().channels.count() > 1 {
                                (buf.chan(1)[frame] * i16::MAX as f32) as i16
                            } else {
                                left
                            };
                            samples.push(left);
                            samples.push(right);
                        }
                    },
                    AudioBufferRef::S16(buf) => {
                        for frame in 0..buf.frames() {
                            let left = buf.chan(0)[frame];
                            let right = if buf.spec().channels.count() > 1 {
                                buf.chan(1)[frame]
                            } else {
                                left
                            };
                            samples.push(left);
                            samples.push(right);
                        }
                    },
                    _ => return Err(anyhow!("Unsupported audio format")),
                },
                Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                Err(e) => return Err(e.into()),
            }
        }

        let spec = WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };

        Ok((spec, samples))
    }

    fn process_click_track(click_path: &Path) -> Result<()> {
        let _output_path = Self::transcode_to_wav(click_path)?;
        Ok(())
    }

    fn process_non_click_tracks(dir: &Path, click_duration: Duration) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                if !filename.to_lowercase().contains("click")
                    && path.extension().map(|e| e == "mp3").unwrap_or(false)
                {
                    let output_path = path.with_extension("wav");
                    let track_duration = Self::get_mp3_duration(&path)?;
                    let padding_duration = click_duration.saturating_sub(track_duration);
                    Self::apply_padding(&path, &output_path, padding_duration)?;
                }
            }
        }
        Ok(())
    }

    fn apply_padding(input_path: &Path, output_path: &Path, padding_duration: Duration) -> Result<()> {
        let (spec, samples) = Self::decode_mp3(input_path)?;
        
        let mut writer = WavWriter::create(output_path, spec)?;
        
        // Calculate the number of padding samples
        let padding_samples = (padding_duration.as_secs_f64() * spec.sample_rate as f64) as u32 * spec.channels as u32;
        
        // Add silence at the beginning
        for _ in 0..padding_samples {
            writer.write_sample(0i16)?;
        }
        
        // Write original samples
        for sample in samples {
            writer.write_sample(sample)?;
        }
        Ok(())
    }

    fn cleanup_mp3s(dir: &Path) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().map(|e| e == "mp3").unwrap_or(false) {
                std::fs::remove_file(path)?;
            }
        }
        Ok(())
    }
}