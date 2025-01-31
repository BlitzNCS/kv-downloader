use std::{env, path::Path};
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
    song_url: String,
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

            let config = driver::Config {
                domain: extract_domain_from_url(&args.song_url)
                    .unwrap_or_else(|| "www.karaoke-version.com".to_string()),
                headless: args.headless,
                download_path: args.download_path.clone(),
            };

            let driver = driver::Driver::new(config);
            driver.sign_in(&credentials.user, &credentials.password)?;

            let download_options = tasks::download_song::DownloadOptions {
                count_in: args.count_in,
                transpose: args.transpose.unwrap_or(0),
            };

            // Perform the actual download
            let _track_names = driver.download_song(&args.song_url, download_options)?;

            // The browser will be closed automatically when it goes out of scope
        } else {
            println!("Skipping download process...");
        }

        // Process the downloaded files
        AudioProcessor::process_downloads(download_path, &args.song_url, args.keep_mp3s)?;

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