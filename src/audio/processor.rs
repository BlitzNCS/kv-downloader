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
use std::fs::{File, OpenOptions, create_dir_all};
use std::io::{BufReader, Write, Seek, SeekFrom};

use std::time::Duration;
use reqwest;

pub struct AudioProcessor;

impl AudioProcessor {
    fn normalize_track_name(filename: &str) -> String {
        // Find everything after the first parenthesis but before "_Custom_Backing_Track)"
        if let Some(start_idx) = filename.find('(') {
            if let Some(end_idx) = filename[start_idx..].find("_Custom_Backing_Track)") {
                let track_name = filename[start_idx + 1..start_idx + end_idx].to_string();
                // Replace underscores with spaces in the track name
                return track_name.replace('_', " ");
            }
        }
        filename.replace('_', " ")
    }
    
    fn format_song_title(song_title: &str) -> Result<String> {
        // Clean up the song title
        let clean_title = song_title
            .replace("_", " ")
            //.replace("-", " ")
            .trim()
            .to_string();
    
        // Capitalize words
        let formatted = clean_title
            .split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => {
                        let mut result = first.to_uppercase().to_string();
                        result.extend(chars.map(|c| c.to_lowercase().next().unwrap_or(c)));
                        result
                    }
                }
            })
            .collect::<Vec<String>>()
            .join(" ");
    
        if formatted.is_empty() {
            Err(anyhow!("Unable to format empty song title"))
        } else {
            Ok(formatted)
        }
    }

    pub fn check_folder_exists(download_dir: &Path, song_url: &str) -> Result<bool> {
        let song_title = Self::extract_song_title(song_url)?;
        let song_dir = download_dir.join(&song_title);
        Ok(song_dir.exists())
    }

    pub fn process_downloads(download_dir: &Path, song_url: &str, keep_mp3s: bool) -> Result<()> {
        let song_title = Self::extract_song_title(song_url)?;
        let song_dir = download_dir.join(&song_title);
        let stems_dir = song_dir.join("STEMS");

        // Create all necessary directories upfront
        let mp3_dir = stems_dir.join("MP3");
        let wav_st_dir = stems_dir.join("WAV ST");
        let wav_mono_dir = stems_dir.join("WAV MONO");
        let mt_project_dir = song_dir.join("MT PROJECT");

        create_dir_all(&song_dir)?;
        create_dir_all(&stems_dir)?;
        create_dir_all(&mp3_dir)?;
        create_dir_all(&wav_st_dir)?;
        create_dir_all(&wav_mono_dir)?;
        create_dir_all(&mt_project_dir)?;

        let (click_path, _other_tracks) = Self::find_tracks(download_dir)?;
        let click_duration = Self::get_mp3_duration(&click_path)?;
        let click_wav_path = Self::process_click_track(&click_path, &wav_st_dir)?;
        
        // Process all non-click tracks found in the directory
        let other_wav_paths = Self::process_non_click_tracks(download_dir, &wav_st_dir, click_duration)?;
        
        // Convert to mono and adjust gain
        let mono_paths = Self::convert_to_mono(&click_wav_path, &other_wav_paths, &wav_mono_dir)?;
        
        // Move all WAV files to their respective directories
        let all_wav_files: Vec<PathBuf> = std::fs::read_dir(&wav_st_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().map_or(false, |ext| ext == "wav"))
            .collect();
            
        // Move all processed WAV files to their respective folders
        Self::move_wav_files(&wav_st_dir, &all_wav_files)?;
        
        // Generate Reaper project file
        Self::generate_reaper_project(&mt_project_dir, &mono_paths, &stems_dir)?;

        // Generate AAF file
        Self::generate_aaf(&mt_project_dir, &mono_paths, &stems_dir)?;

        if keep_mp3s {
            Self::move_mp3s(download_dir, &mp3_dir)?;
        } else {
            Self::cleanup_mp3s(download_dir)?;
        }

        Ok(())
    }

    fn move_wav_files(dest_dir: &Path, files: &[PathBuf]) -> Result<()> {
        for path in files {
            let original_name = path.file_name().unwrap().to_str().unwrap();
            let normalized_name = Self::normalize_track_name(original_name);
            
            // Handle mono vs stereo naming
            let new_filename = if original_name.contains("_mono") {
                format!("{}_mono.wav", normalized_name)
            } else {
                format!("{}.wav", normalized_name)
            };
            
            let new_path = dest_dir.join(new_filename);
            std::fs::rename(path, new_path)?;
        }
        Ok(())
    }

    fn move_mp3s(src_dir: &Path, dest_dir: &Path) -> Result<()> {
        for entry in std::fs::read_dir(src_dir)? {
            let path = entry?.path();
            if path.extension().map(|e| e == "mp3").unwrap_or(false) {
                let original_name = path.file_name().unwrap().to_str().unwrap();
                let normalized_name = Self::normalize_track_name(original_name);
                let new_filename = format!("{}.mp3", normalized_name);
                let new_path = dest_dir.join(new_filename);
                std::fs::rename(&path, new_path)?;
            }
        }
        Ok(())
    }

    fn extract_song_title(url: &str) -> Result<String> {
        if url.starts_with("http") {
            let response = reqwest::blocking::get(url)?;
            let body = response.text()?;
            
            // Try to extract from HTML first
            if let Some(title_start) = body.find(r#"<h1 class="song-details__title""#) {
                if let Some(title_end) = body[title_start..].find("</h1>") {
                    let title_html = &body[title_start..title_start + title_end];
                    if let Some(content_start) = title_html.find('>') {
                        let mut title = title_html[content_start + 1..].trim().to_string();
                        
                        // Remove " - Custom Backing Track MP3" from the end
                        if let Some(index) = title.rfind(" - Custom Backing Track MP3") {
                            title.truncate(index);
                        }
                        return Ok(title);
                    }
                }
            }
        }
        
        // Fallback to URL parsing if HTML extraction fails
        Self::format_song_title(url)
    }

    fn find_tracks(dir: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
        let mut click = None;
        let mut others = Vec::new();
    
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                if filename.to_lowercase().contains("click") {
                    click = Some(path.clone());
                    tracing::info!("Found click track: {:?}", path);
                } else if path.extension().map(|e| e == "mp3").unwrap_or(false) {
                    others.push(path.clone());
                    tracing::info!("Found other track: {:?}", path);
                }
            }
        }
    
        if click.is_none() {
            tracing::error!("No click track found in directory: {:?}", dir);
            tracing::info!("Files found in directory:");
            for entry in std::fs::read_dir(dir)? {
                let path = entry?.path();
                tracing::info!("  {:?}", path);
            }
        }
    
        Ok((
            click.ok_or_else(|| anyhow!("Click track not found in {:?}", dir))?,
            others,
        ))
    }

    fn get_mp3_duration(path: &Path) -> Result<Duration> {
        let (spec, samples) = Self::decode_mp3(path)?;
        let duration_seconds = samples.len() as f64 / (spec.channels as f64 * spec.sample_rate as f64);
        Ok(Duration::from_secs_f64(duration_seconds))
    }

    fn transcode_to_wav(src: &Path, dest_dir: &Path) -> Result<PathBuf> {
        let (spec, samples) = Self::decode_mp3(src)?;
        let dest = dest_dir.join(src.file_name().unwrap()).with_extension("wav");
        
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

    fn process_click_track(click_path: &Path, wav_st_dir: &Path) -> Result<PathBuf> {
        Self::transcode_to_wav(click_path, wav_st_dir)
    }

    fn process_non_click_tracks(dir: &Path, wav_st_dir: &Path, click_duration: Duration) -> Result<Vec<PathBuf>> {
        let mut processed_paths = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                if !filename.to_lowercase().contains("click")
                    && path.extension().map(|e| e == "mp3").unwrap_or(false)
                {
                    let output_path = wav_st_dir.join(path.file_name().unwrap()).with_extension("wav");
                    let track_duration = Self::get_mp3_duration(&path)?;
                    let padding_duration = click_duration.saturating_sub(track_duration);
                    Self::apply_padding(&path, &output_path, padding_duration)?;
                    processed_paths.push(output_path);
                }
            }
        }
        Ok(processed_paths)
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

    fn convert_to_mono(click_path: &Path, other_paths: &[PathBuf], wav_mono_dir: &Path) -> Result<Vec<PathBuf>> {
        let mut mono_paths = Vec::new();

        // Process click track
        let mono_click_path = Self::stereo_to_mono(click_path, wav_mono_dir)?;
        mono_paths.push(mono_click_path);

        // Process other tracks
        for path in other_paths {
            let mono_path = Self::stereo_to_mono(path, wav_mono_dir)?;
            mono_paths.push(mono_path);
        }

        Ok(mono_paths)
    }

    fn stereo_to_mono(input_path: &Path, wav_mono_dir: &Path) -> Result<PathBuf> {
        let mut reader = hound::WavReader::open(input_path)?;
        let spec = reader.spec();
        
        if spec.channels != 2 {
            return Err(anyhow!("Input file is not stereo"));
        }
        
        // Get the original filename and normalize it
        let original_name = input_path.file_stem().unwrap().to_str().unwrap();
        let normalized_name = Self::normalize_track_name(original_name);
        let output_path = wav_mono_dir.join(format!("{}_mono.wav", normalized_name));
        let mut writer = hound::WavWriter::create(
            &output_path,
            WavSpec {
                channels: 1,
                sample_rate: spec.sample_rate,
                bits_per_sample: spec.bits_per_sample,
                sample_format: spec.sample_format,
            },
        )?;

        let mut samples: Vec<i16> = reader.samples().map(|s| s.unwrap()).collect();
        for chunk in samples.chunks_mut(2) {
            let mono_sample = ((chunk[0] as i32 + chunk[1] as i32) / 2) as i16;
            writer.write_sample(mono_sample)?;
        }

        writer.finalize()?;
        Ok(output_path)
    }


    fn generate_reaper_project(mt_project_dir: &Path, mono_paths: &[PathBuf], stems_dir: &Path) -> Result<()> {
        let song_title = Self::extract_song_title(&stems_dir.parent().unwrap().file_name().unwrap().to_str().unwrap())?;
        let formatted_title = Self::format_song_title(&song_title)?;
        let project_path = mt_project_dir.join(format!("{}.rpp", formatted_title));
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(project_path)?;

        writeln!(file, "<REAPER_PROJECT 0.1 \"6.13/linux64\" 1681658689")?;
        writeln!(file, "  TEMPO 120 4 4")?;
        writeln!(file, "  MASTER_VOLUME 1 0 -1 -1 1")?;
        writeln!(file, "  <METRONOME 6 2")?;
        writeln!(file, "    VOL 0.25 0.125")?;
        writeln!(file, "    FREQ 800 1600 1")?;
        writeln!(file, "    BEATLEN 4")?;
        writeln!(file, "    SAMPLES \"\" \"\"")?;
        writeln!(file, "    PATTERN 2863311530 2863311529")?;
        writeln!(file, "  >")?;

        let mut max_duration: f64 = 0.0;

        for (i, path) in mono_paths.iter().enumerate() {
            let is_click = path.file_stem().unwrap().to_str().unwrap().to_lowercase().contains("click");
            let pan = if is_click { -1.0 } else { 1.0 };

            let wav_reader = hound::WavReader::open(path)?;
            let duration_seconds = wav_reader.duration() as f64 / wav_reader.spec().sample_rate as f64;
            max_duration = max_duration.max(duration_seconds);

            // Use the absolute path for the audio file
            let absolute_path = path.canonicalize()?;
            let file_path = absolute_path.to_str().unwrap().replace("\\", "/");

            writeln!(file, "  <TRACK {}", i + 1)?;
            writeln!(file, "    NAME \"{}\"", path.file_stem().unwrap().to_str().unwrap())?;
            writeln!(file, "    PEAKCOL 16576")?;
            writeln!(file, "    BEAT -1")?;
            writeln!(file, "    AUTOMODE 0")?;
            writeln!(file, "    VOLPAN 1 {} -1 -1 1", pan)?;
            writeln!(file, "    MUTESOLO 0 0 0")?;
            writeln!(file, "    IPHASE 0")?;
            writeln!(file, "    ISBUS 0 0")?;
            writeln!(file, "    BUSCOMP 0 0 0 0 0")?;
            writeln!(file, "    SHOWINMIX 1 0.6667 0.5 1 0.5 0 -1 0")?;
            writeln!(file, "    FREEMODE 0")?;
            writeln!(file, "    SEL 0")?;
            writeln!(file, "    REC 0 0 1 0 0 0 0")?;
            writeln!(file, "    VU 2")?;
            writeln!(file, "    TRACKHEIGHT 0 0 0 0 0 0")?;
            writeln!(file, "    INQ 0 0 0 0.5 100 0 0 100")?;
            writeln!(file, "    NCHAN 2")?;
            writeln!(file, "    FX 1")?;
            writeln!(file, "    TRACKID {{7FE0D07C-DFA2-4D85-8A77-6AB24173DC8{}}}", i)?;
            writeln!(file, "    PERF 0")?;
            writeln!(file, "    MIDIOUT -1")?;
            writeln!(file, "    MAINSEND 1 0")?;
            writeln!(file, "    <ITEM")?;
            writeln!(file, "      POSITION 0")?;
            writeln!(file, "      SNAPOFFS 0")?;
            writeln!(file, "      LENGTH {}", duration_seconds)?;
            writeln!(file, "      LOOP 1")?;
            writeln!(file, "      ALLTAKES 0")?;
            writeln!(file, "      FADEIN 1 0.01 0 1 0 0 0")?;
            writeln!(file, "      FADEOUT 1 0.01 0 1 0 0 0")?;
            writeln!(file, "      MUTE 0 0")?;
            writeln!(file, "      SEL 0")?;
            writeln!(file, "      IGUID {{EAE098FB-B9B0-4F57-9D7C-2656D9861A0{}}}", i)?;
            writeln!(file, "      IID 1")?;
            writeln!(file, "      NAME \"{}\"", path.file_name().unwrap().to_str().unwrap())?;
            writeln!(file, "      VOLPAN 1 0 1 -1")?;
            writeln!(file, "      SOFFS 0")?;
            writeln!(file, "      PLAYRATE 1 1 0 -1 0 0.0025")?;
            writeln!(file, "      CHANMODE 0")?;
            writeln!(file, "      GUID {{5E5B68F0-4717-4D85-8A77-6AB24173DC8{}}}", i)?;
            writeln!(file, "      <SOURCE WAVE")?;
            writeln!(file, "        FILE \"{}\"", file_path)?;
            writeln!(file, "      >")?;
            writeln!(file, "    >")?;
            writeln!(file, "  >")?;
        }

        // Add MIDI track
        writeln!(file, "  <TRACK {}", mono_paths.len() + 1)?;
        writeln!(file, "    NAME \"MIDI\"")?;
        writeln!(file, "    PEAKCOL 16576")?;
        writeln!(file, "    BEAT -1")?;
        writeln!(file, "    AUTOMODE 0")?;
        writeln!(file, "    VOLPAN 1 0 -1 -1 1")?;
        writeln!(file, "    MUTESOLO 0 0 0")?;
        writeln!(file, "    IPHASE 0")?;
        writeln!(file, "    ISBUS 0 0")?;
        writeln!(file, "    BUSCOMP 0 0 0 0 0")?;
        writeln!(file, "    SHOWINMIX 1 0.6667 0.5 1 0.5 0 -1 0")?;
        writeln!(file, "    FREEMODE 0")?;
        writeln!(file, "    SEL 0")?;
        writeln!(file, "    REC 1 5088 1 0 0 0 0")?;
        writeln!(file, "    VU 2")?;
        writeln!(file, "    TRACKHEIGHT 0 0 0 0 0 0")?;
        writeln!(file, "    INQ 0 0 0 0.5 100 0 0 100")?;
        writeln!(file, "    NCHAN 2")?;
        writeln!(file, "    FX 1")?;
        writeln!(file, "    TRACKID {{7FE0D07C-DFA2-4D85-8A77-6AB24173DC9{}}}", mono_paths.len())?;
        writeln!(file, "    PERF 0")?;
        writeln!(file, "    MIDIOUT -1")?;
        writeln!(file, "    MAINSEND 1 0")?;
        writeln!(file, "    <ITEM MIDI")?;
        writeln!(file, "      POSITION 0")?;
        writeln!(file, "      SNAPOFFS 0")?;
        writeln!(file, "      LENGTH {}", max_duration)?;
        writeln!(file, "      ALLTAKES 0")?;
        writeln!(file, "      FADEIN 1 0.01 0 1 0 0 0")?;
        writeln!(file, "      FADEOUT 1 0.01 0 1 0 0 0")?;
        writeln!(file, "      MUTE 0 0")?;
        writeln!(file, "      SEL 0")?;
        writeln!(file, "      IGUID {{EAE098FB-B9B0-4F57-9D7C-2656D9861A1{}}}", mono_paths.len())?;
        writeln!(file, "      IID 2")?;
        writeln!(file, "      NAME \"MIDI\"")?;
        writeln!(file, "      VOLPAN 1 0 1 -1")?;
        writeln!(file, "      SOFFS 0")?;
        writeln!(file, "      PLAYRATE 1 1 0 -1 0 0.0025")?;
        writeln!(file, "      CHANMODE 0")?;
        writeln!(file, "      GUID {{5E5B68F0-4717-4D85-8A77-6AB24173DC9{}}}", mono_paths.len())?;
        writeln!(file, "      <SOURCE MIDI")?;
        writeln!(file, "        HASDATA 1 960 QN")?;
        writeln!(file, "        E 0 b0 7b 00")?;
        // Calculate MIDI ticks: duration * (120 BPM * 960 PPQN / 60 seconds)
        writeln!(file, "        E {} b0 7b 00", (max_duration * 120.0 * 960.0 / 60.0) as u32)?;        writeln!(file, "      >")?;
        writeln!(file, "    >")?;
        writeln!(file, "  >")?;

        writeln!(file, ">")?;
        Ok(())
    }

    fn generate_aaf(mt_project_dir: &Path, mono_paths: &[PathBuf], stems_dir: &Path) -> Result<()> {
        let omf_path = mt_project_dir.join("project.omf");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(omf_path)?;
    
        // Write OMF header
        file.write_all(b"FORM")?;
        // 4-byte length placeholder
        file.write_all(&[0, 0, 0, 0])?;
        file.write_all(b"OMFI")?;
        file.write_all(b"HEAD")?;
        
        // Write HEAD chunk length (4 bytes)
        file.write_all(&[0, 0, 0, 24])?;
        
        // Write version (2.0)
        file.write_all(&[0x02, 0x00])?;
        
        // Write byte order (big-endian)
        file.write_all(&[0x00, 0x00])?;
    
        // Write time stamp (current time as 32-bit unix timestamp)
        let timestamp = chrono::Utc::now().timestamp() as u32;
        file.write_all(&timestamp.to_be_bytes())?;
    
        // Write MOBJ chunk header
        file.write_all(b"MOBJ")?;
        // Write MOBJ chunk length placeholder
        let mobj_pos = file.stream_position()?;
        file.write_all(&[0, 0, 0, 0])?;
    
        for path in mono_paths {
            let wav_reader = hound::WavReader::open(path)?;
            let relative_path = path.strip_prefix(stems_dir)?;
            let file_path = format!("STEMS/{}", relative_path.to_str().unwrap().replace("\\", "/"));
            
            // Write CLIP chunk
            file.write_all(b"CLIP")?;
            let clip_len_pos = file.stream_position()?;
            file.write_all(&[0, 0, 0, 0])?;
    
            // Write file path
            let path_bytes = file_path.as_bytes();
            file.write_all(&(path_bytes.len() as u32).to_be_bytes())?;
            file.write_all(path_bytes)?;
    
            // Write audio properties
            file.write_all(&wav_reader.spec().sample_rate.to_be_bytes())?;
            file.write_all(&wav_reader.spec().channels.to_be_bytes())?;
            file.write_all(&wav_reader.duration().to_be_bytes())?;
    
            // Update CLIP chunk length
            let current_pos = file.stream_position()?;
            file.seek(SeekFrom::Start(clip_len_pos))?;
            file.write_all(&((current_pos - clip_len_pos - 4) as u32).to_be_bytes())?;
            file.seek(SeekFrom::Start(current_pos))?;
        }
    
        // Update MOBJ chunk length
        let end_pos = file.stream_position()?;
        file.seek(SeekFrom::Start(mobj_pos))?;
        file.write_all(&((end_pos - mobj_pos - 4) as u32).to_be_bytes())?;
    
        // Update total file length
        file.seek(SeekFrom::Start(4))?;
        file.write_all(&((end_pos - 8) as u32).to_be_bytes())?;
    
        Ok(())
    }
}