use std::{env, path::Path, thread::sleep, time::Duration};
use crate::{
    audio::AudioProcessor,
    driver,
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
    
    #[arg(short = 'H', long)]
    headless: bool,
    
    #[arg(short, long)]
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
        Download::start_download(args)
    }

    fn initialize_driver(args: &DownloadArgs, credentials: &Credentials) -> Result<driver::Driver> {
        let config = driver::Config {
            domain: args.song_url.as_deref()
                .and_then(extract_domain_from_url)
                .unwrap_or_else(|| "www.karaoke-version.com".to_string()),
            headless: args.headless,
            download_path: args.download_path.clone(),
        };

        let driver = driver::Driver::new(config);
        driver.sign_in(&credentials.user, &credentials.password)?;
        Ok(driver)
    }

    fn start_download(args: DownloadArgs) -> Result<()> {
        let download_path = args.download_path
            .as_deref()
            .map(Path::new)
            .ok_or_else(|| anyhow!("Download directory must be specified with --download-path"))?;

        if !args.skip_download {
            let credentials = credentials_from_env().unwrap_or_else(|| {
                keystore::Keystore::get_credentials().map_err(|e| {
                    anyhow!("Authentication required. Run `kv-downloader auth` first.\n{}", e)
                }).unwrap()
            });

            // Initialize single browser instance for all downloads
            let driver = Self::initialize_driver(&args, &credentials)?;

            // Create a persistent tab that will be reused
            let tab = driver.browser.new_tab()?;
            tab.set_default_timeout(Duration::from_secs(3600000)); // 1 hour timeout

            if args.all.is_some() {
                tracing::info!("Collecting all track URLs...");
                let urls = driver.collect_all_custom_track_urls()?;
                tracing::info!("Found {} tracks to download", urls.len());

                let skip_count = args.all.unwrap_or(0);
                if skip_count > 0 {
                    tracing::info!("Skipping first {} tracks", skip_count);
                }

                for (index, url) in urls.iter().enumerate().skip(skip_count) {
                    tracing::info!("Processing track {} of {}: {}", index + 1, urls.len(), url);
                    
                    // Check if folder already exists
                    if AudioProcessor::check_folder_exists(download_path, url)? {
                        tracing::info!("Skipping track {} - folder already exists", url);
                        continue;
                    }
                    
                    if index > skip_count {
                        sleep(Duration::from_secs(5));
                    }

                    match (|| -> Result<()> {
                        let download_options = tasks::download_song::DownloadOptions {
                            count_in: args.count_in,
                            transpose: args.transpose.unwrap_or(0),
                        };
                        
                        let _track_names = driver.download_song(url, download_options)?;
                        AudioProcessor::process_downloads(download_path, url, args.keep_mp3s)?;
                        Ok(())
                    })() {
                        Ok(_) => tracing::info!("Successfully processed track {}", url),
                        Err(e) => {
                            tracing::error!("Failed to process {}: {}", url, e);
                            // Keep the browser alive by sending a no-op command
                            if let Err(e) = tab.evaluate("true;", true) {
                                tracing::error!("Browser connection lost: {}", e);
                                return Err(anyhow!("Browser connection lost"));
                            }
                            continue;
                        }
                    }

                    // Keep connection alive between downloads
                    if let Err(e) = tab.evaluate("true;", true) {
                        tracing::error!("Browser connection lost: {}", e);
                        return Err(anyhow!("Browser connection lost"));
                    }
                }
            } else if let Some(ref url) = args.song_url {
                // Check if folder already exists for single download
                if AudioProcessor::check_folder_exists(download_path, url)? {
                    tracing::info!("Skipping download - folder already exists: {}", url);
                    return Ok(());
                }
                
                let download_options = tasks::download_song::DownloadOptions {
                    count_in: args.count_in,
                    transpose: args.transpose.unwrap_or(0),
                };
                
                let _track_names = driver.download_song(url, download_options)?;
                AudioProcessor::process_downloads(download_path, url, args.keep_mp3s)?;
            }
        } else {
            println!("Skipping download process...");
            if let Some(ref url) = args.song_url {
                // Even in skip_download mode, check if folder exists
                if AudioProcessor::check_folder_exists(download_path, url)? {
                    tracing::info!("Skipping processing - folder already exists: {}", url);
                    return Ok(());
                }
                AudioProcessor::process_downloads(download_path, url, args.keep_mp3s)?;
            }
        }

        Ok(())
    }
}

fn credentials_from_env() -> Option<Credentials> {
    env::var("KV_USERNAME").ok().and_then(|user| {
        env::var("KV_PASSWORD").ok().map(|password| Credentials { user, password })
    })
}

fn extract_domain_from_url(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(|h| h.to_string()))
}