// src/audio/processor.rs
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

pub struct AudioProcessor;

impl AudioProcessor {
    pub fn process_downloads(download_dir: &Path) -> Result<()> {
        let (click_path, _other_path) = Self::find_tracks(download_dir)?;
        let offset = Self::compute_offset(&click_path)?;
        Self::process_non_click_tracks(download_dir, offset)
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

    fn compute_offset(click_path: &Path) -> Result<u64> {
        let click_samples = Self::get_sample_count(click_path)?;
        let wav_path = Self::transcode_to_wav(click_path)?;
        let wav_duration = Self::get_wav_sample_count(&wav_path)?;
        Ok(wav_duration - click_samples)
    }

    fn get_sample_count(path: &Path) -> Result<u64> {
        let file = File::open(path)?;
        let source = ReadOnlySource::new(BufReader::new(file));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        
        let probe = get_probe();
        let format_opts = FormatOptions::default();
        let metadata_opts = MetadataOptions::default();

        let probed = probe.format(&Hint::new(), mss, &format_opts, &metadata_opts)?;
        let track = probed.format.default_track().ok_or_else(|| anyhow!("No default track"))?;
        Ok(track.codec_params.n_frames.ok_or_else(|| anyhow!("Sample count unknown"))?)
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

        // Get codec parameters before we start decoding
        let channels = track.codec_params.channels.map(|c| c.count() as u16).unwrap_or(2);
        let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);

        while let Ok(packet) = probed.format.next_packet() {
            match decoder.decode(&packet) {
                Ok(buffer) => match buffer {
                    AudioBufferRef::F32(buf) => {
                        let interleaved = buf.chan(0);
                        samples.extend(
                            interleaved.iter()
                                .map(|&s| (s * i16::MAX as f32) as i16)
                        );
                    },
                    AudioBufferRef::S16(buf) => {
                        let interleaved = buf.chan(0);
                        samples.extend(interleaved.iter().copied());
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

    fn get_wav_sample_count(path: &Path) -> Result<u64> {
        let reader = hound::WavReader::open(path)?;
        Ok(reader.duration() as u64 / reader.spec().channels as u64)
    }

    fn process_non_click_tracks(dir: &Path, padding_samples: u64) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                if !filename.to_lowercase().contains("click") 
                    && path.extension().map(|e| e == "mp3").unwrap_or(false) 
                {
                    let output_path = path.with_extension("wav");
                    Self::apply_padding(&path, &output_path, padding_samples)?;
                }
            }
        }
        Ok(())
    }

    fn apply_padding(input_path: &Path, output_path: &Path, padding_samples: u64) -> Result<()> {
        let (spec, samples) = Self::decode_mp3(input_path)?;
        
        let mut writer = WavWriter::create(output_path, spec)?;
        // Add silence
        for _ in 0..(padding_samples * spec.channels as u64) {
            writer.write_sample(0i16)?;
        }
        // Write original samples
        for sample in samples {
            writer.write_sample(sample)?;
        }
        Ok(())
    }
}