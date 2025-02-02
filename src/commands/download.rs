// src/commands/download.rs
use std::{
    env,
    fs,
    path::Path,
    thread::sleep,
    time::Duration,
};

use crate::{
    audio::AudioProcessor,
    driver,
    keystore::{self, Credentials},
    tasks,
};
use anyhow::{anyhow, Result};
use clap::{Args};
use headless_chrome::Tab;

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

    /// Initialize the driver and use its main tab for all operations.
    fn initialize_driver(
    args: &DownloadArgs,
    credentials: &Credentials,
) -> Result<(driver::Driver, std::sync::Arc<Tab>)> {
        let config = driver::Config {
            domain: args
                .song_url
                .as_deref()
                .and_then(extract_domain_from_url)
                .unwrap_or_else(|| "www.karaoke-version.com".to_string()),
            headless: args.headless,
            download_path: args.download_path.clone(),
        };

        let driver = driver::Driver::new(config);
        let tab = driver.get_tab()?;
        tab.set_default_timeout(Duration::from_secs(3600));
        
        // Use the main tab for sign in
        driver.sign_in_on_tab(&*tab, &credentials.user, &credentials.password)?;
        
        Ok((driver, tab))
    }

    fn start_download(args: DownloadArgs) -> Result<()> {
        let download_path = args
            .download_path
            .as_deref()
            .map(Path::new)
            .ok_or_else(|| anyhow!("Download directory must be specified with --download-path"))?;

        if !args.skip_download {
            let credentials = credentials_from_env().unwrap_or_else(|| {
                keystore::Keystore::get_credentials().map_err(|e| {
                    anyhow!(
                        "Authentication required. Run `kv-downloader auth` first.\n{}",
                        e
                    )
                })
                .unwrap()
            });

            let (driver, tab) = Self::initialize_driver(&args, &credentials)?;

            if let Some(skip_count) = args.all {
                // Use a cached track list if available.
                let track_list_path = download_path.join("track_list.json");
                let urls: Vec<String> = if args.reuse && track_list_path.exists() {
                    tracing::info!("Reusing saved track list from {:?}", track_list_path);
                    let data = fs::read_to_string(&track_list_path)
                        .map_err(|e| anyhow!("Failed to read track list file: {}", e))?;
                    serde_json::from_str(&data)
                        .map_err(|e| anyhow!("Failed to parse track list: {}", e))?
                } else {
                    tracing::info!("Collecting all track URLs...");
                    tab.navigate_to(&format!("https://{}/my/download.html", driver.config.domain))?;
                    tab.wait_until_navigated()?;
                    let set_filter_js = r#"
                        let select = document.querySelector('select[name="file_type"]');
                        if (select) {
                            select.value = '1';
                            select.dispatchEvent(new Event('change'));
                        }
                        true;
                    "#;
                    tab.evaluate(set_filter_js, true)?;
                    sleep(Duration::from_secs(2));
                    let extraction_js = r#"
                        (function(){
                            let tbody = document.querySelector('#tab_files tbody');
                            if (!tbody) return JSON.stringify([]);
                            let rows = tbody.querySelectorAll('tr');
                            let links = Array.from(rows).map(function(row){
                                let anchor = row.querySelector('td.my-downloaded-files__song.min-w-120 a');
                                return anchor ? "https://www.karaoke-version.com" + anchor.getAttribute('href') : null;
                            }).filter(x => x !== null);
                            return JSON.stringify(links);
                        })();
                    "#;
                    let result = tab.evaluate(extraction_js, true)?;
                    let urls_str = result
                        .value
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| "[]".to_string());
                    let urls: Vec<String> = serde_json::from_str(&urls_str)
                        .map_err(|e| anyhow!("Failed to parse track URLs: {}", e))?;
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
                    if AudioProcessor::check_folder_exists(download_path, url)? {
                        tracing::info!("Skipping track {} - folder already exists", url);
                        continue;
                    }

                    if index > skip_count {
                        sleep(Duration::from_secs(5));
                    }

                    tab.navigate_to(url)?;
                    tab.wait_until_navigated()?;

                    // Download the song using our helper function on the existing tab.
                    driver.download_song_on_tab(
                        &*tab,
                        url,
                        tasks::download_song::DownloadOptions {
                            count_in: args.count_in,
                            transpose: args.transpose.unwrap_or(0),
                        },
                    )?;
                    

                    AudioProcessor::process_downloads(
                        &*tab,             // Pass a reference to the main Tab
                        download_path,
                        url,
                        args.keep_mp3s
                    )?;
                                }
            } else if let Some(ref url) = args.song_url {
                if AudioProcessor::check_folder_exists(download_path, url)? {
                    tracing::info!("Skipping download - folder already exists: {}", url);
                    return Ok(());
                }
                tab.navigate_to(url)?;
                tab.wait_until_navigated()?;
                driver.download_song_on_tab(
                    &tab,
                    url,
                    tasks::download_song::DownloadOptions {
                        count_in: args.count_in,
                        transpose: args.transpose.unwrap_or(0),
                    },
                )?;
                AudioProcessor::process_downloads(
                    &*tab,             // Pass a reference to the main Tab
                    download_path,
                    url,
                    args.keep_mp3s
                )?;            }
            } else {
                println!("Skipping download process...");
                // Create a new driver and tab (no login needed for processing)
                let driver = driver::Driver::new(driver::Config {
                    domain: args
                        .song_url
                        .as_deref()
                        .and_then(extract_domain_from_url)
                        .unwrap_or_else(|| "www.karaoke-version.com".to_string()),
                    headless: args.headless,
                    download_path: args.download_path.clone(),
                });
                let tab = driver.get_tab()?;
                
                if let Some(ref url) = args.song_url {
                    if AudioProcessor::check_folder_exists(download_path, url)? {
                        tracing::info!("Skipping processing - folder already exists: {}", url);
                        return Ok(());
                    }
                    AudioProcessor::process_downloads(
                        &*tab,  // Now `tab` is defined
                        download_path,
                        url,
                        args.keep_mp3s
                    )?;
                }
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
