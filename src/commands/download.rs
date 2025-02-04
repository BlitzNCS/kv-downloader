use std::sync::{Arc, Mutex};
use headless_chrome::Tab;
use std::time::Duration;
use std::thread::sleep;
use crate::driver::Driver;
use crate::driver;
use std::{
    env,
    fs,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{
    audio::AudioProcessor,
    keystore::{self, Credentials},
    tasks,
};
use anyhow::{anyhow, Result};
use clap::{arg, Args};

#[derive(Debug, Args)]
pub struct DownloadArgs {
    #[arg(required_unless_present = "all")]
    song_url: Option<String>,

    #[arg(
        short = 'A',
        long,
        help = "Download all custom backing tracks. Optionally specify a number to skip that many tracks",
        value_name = "SKIP"
    )]
    all: Option<usize>,

    #[arg(short = 'R', long, help = "Reuse saved track list (only valid in -A mode)")]
    reuse: bool,

    #[arg(short = 'H', long, help = "Run headless")]
    headless: bool,

    #[arg(short, long, help = "Path to download directory")]
    download_path: Option<String>,

    #[arg(
        short = 'T',
        long,
        value_parser = clap::value_parser!(i8).range(-4..=4),
        default_value = "0",
        allow_hyphen_values = true,
    )]
    transpose: Option<i8>,

    #[arg(short = 'C', long, help = "Whether to count in an intro for the click track")]
    count_in: bool,

    #[arg(short = 'S', long, help = "Skip download and only process existing files")]
    skip_download: bool,

    #[arg(short = 'K', long, help = "Keep original MP3 files after processing")]
    keep_mp3s: bool,
}

pub struct Download;

impl Download {
    pub fn run(args: DownloadArgs) -> Result<()> {
        Self::start_download(args)
    }

    fn initialize_driver(
        args: &DownloadArgs,
        credentials: &Credentials,
    ) -> Result<(Driver, Arc<Mutex<Arc<Tab>>>)> {
        let config = driver::Config {
            domain: args
                .song_url
                .as_deref()
                .and_then(extract_domain_from_url)
                .unwrap_or_else(|| "www.karaoke-version.com".to_string()),
            headless: args.headless,
            download_path: args.download_path.clone(),
        };

        let driver = Driver::new(config);
        let tab = driver.browser.new_tab()?;
        tab.set_default_timeout(Duration::from_secs(3600));
        driver.sign_in(&credentials.user, &credentials.password)?;

        Ok((driver, Arc::new(Mutex::new(tab))))
    }

    fn start_download(args: DownloadArgs) -> Result<()> {
        let download_path = args
            .download_path
            .as_deref()
            .map(Path::new)
            .ok_or_else(|| anyhow!("Download directory must be specified with --download-path"))?;

        // Get credentials and initialize driver before any operations
        let credentials = credentials_from_env().unwrap_or_else(|| {
            keystore::Keystore::get_credentials().map_err(|e| {
                anyhow!(
                    "Authentication required. Run `kv-downloader auth` first.\n{}",
                    e
                )
            })
            .unwrap()
        });

        // Initialize the driver and create our shared persistent tab
        let driver_and_tab: Option<(Driver, Arc<Mutex<Arc<Tab>>>)> = if !args.skip_download {
            let (driver, tab) = Self::initialize_driver(&args, &credentials)?;
            Some((driver, tab))
        } else {
            None
        };

        // Set up keep-alive if we have a driver
        let keep_alive_flag = Arc::new(AtomicBool::new(false));
        let keep_alive_handle = if let Some((_, ref persistent_tab)) = driver_and_tab {
            let keep_alive_tab: Arc<Mutex<Arc<Tab>>> = Arc::clone(persistent_tab);
            let keep_alive_flag_clone = Arc::clone(&keep_alive_flag);
            let handle = std::thread::spawn(move || {
                while !keep_alive_flag_clone.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_secs(30));
                    let tab = keep_alive_tab.lock().unwrap();
                    if let Err(e) = tab.evaluate("true;", true) {
                        tracing::warn!("Keep-alive ping failed: {}", e);
                    } else {
                        tracing::debug!("Keep-alive ping succeeded");
                    }
                }
            });
            Some(handle)
        } else {
            None
        };

        // Process URLs
        if let Some(skip_count) = args.all {
            if let Some((ref driver, ref persistent_tab)) = driver_and_tab {
                // In all mode, reuse the saved track list if the --reuse flag is set.
                let track_list_path = download_path.join("track_list.json");
                let urls: Vec<String> = if args.reuse && track_list_path.exists() {
                    tracing::info!("Reusing saved track list from {:?}", track_list_path);
                    let data = fs::read_to_string(&track_list_path)
                        .map_err(|e| anyhow!("Failed to read track list file: {}", e))?;
                    serde_json::from_str(&data)
                        .map_err(|e| anyhow!("Failed to parse track list: {}", e))?
                } else {
                    tracing::info!("Collecting all track URLs...");
                    let urls: Vec<String> = driver.collect_all_custom_track_urls()?;
                    tracing::info!("Found {} tracks to download", urls.len());
                    fs::write(&track_list_path, serde_json::to_string_pretty(&urls)?)
                        .map_err(|e| anyhow!("Failed to write track list file: {}", e))?;
                    urls
                };

                if skip_count > 0 {
                    tracing::info!("Skipping first {} tracks", skip_count);
                }

                for (index, url) in urls.iter().enumerate().skip(skip_count) {
                    tracing::info!(
                        "Processing track {} of {}: {}",
                        index + 1,
                        urls.len(),
                        url
                    );

                    // Check if the track folder already exists.
                    if AudioProcessor::check_folder_exists(download_path, url)? {
                        tracing::info!("Skipping track {} - folder already exists", url);
                        continue;
                    }

                    if index > skip_count {
                        sleep(Duration::from_secs(5));
                    }

                    // Before processing each track, check if our persistent tab is still valid
                    {
                        // Put this in its own scope so the lock is dropped before download
                        let tab_valid = {
                            let tab_lock: std::sync::MutexGuard<Arc<Tab>> = persistent_tab.lock().unwrap();
                            tab_lock.evaluate("true;", true).is_ok()
                        };

                        if !tab_valid {
                            // Only acquire the lock again if we need to reinitialize
                            let mut tab_lock = persistent_tab.lock().unwrap();
                            tracing::warn!("Persistent tab lost connection, reinitializing it");
                            *tab_lock = driver.browser.new_tab()?;
                            tab_lock.set_default_timeout(Duration::from_secs(3600));
                            driver.sign_in(&credentials.user, &credentials.password)?;
                            // Lock is dropped here
                        }
                    }

                    // Now the persistent tab lock is dropped, we can safely do the download
                    match (|| -> Result<()> {
                        let download_options = tasks::download_song::DownloadOptions {
                            count_in: args.count_in,
                            transpose: args.transpose.unwrap_or(0),
                        };

                        let _: Vec<String> = driver.download_song(url, download_options)?;
                        AudioProcessor::process_downloads(download_path, url, args.keep_mp3s)?;
                        Ok(())
                    })() {
                        Ok(_) => tracing::info!("Successfully processed track {}", url),
                        Err(e) => {
                            tracing::error!("Failed to process {}: {}", url, e);
                            continue;
                        }
                    }
                }
            }
        } else if let Some(ref url) = args.song_url {
            // For a single track download.
            if AudioProcessor::check_folder_exists(download_path, url)? {
                tracing::info!("Skipping download - folder already exists: {}", url);
                return Ok(());
            }

            if !args.skip_download {
                if let Some((ref driver, _)) = driver_and_tab {
                    let download_options = tasks::download_song::DownloadOptions {
                        count_in: args.count_in,
                        transpose: args.transpose.unwrap_or(0),
                    };

                    let _: Vec<String> = driver.download_song(url, download_options)?;
                    AudioProcessor::process_downloads(download_path, url, args.keep_mp3s)?;
                }
            } else {
                println!("Skipping download process...");
                AudioProcessor::process_downloads(download_path, url, args.keep_mp3s)?;
            }
        }

        // Clean up keep-alive thread if it exists
        if let Some(handle) = keep_alive_handle {
            keep_alive_flag.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }

        Ok(())
    }
}

fn credentials_from_env() -> Option<Credentials> {
    env::var("KV_USERNAME").ok().and_then(|user| {
        env::var("KV_PASSWORD")
            .ok()
            .map(|password| Credentials { user, password })
    })
}

fn extract_domain_from_url(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(|h| h.to_string()))
}