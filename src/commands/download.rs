use std::{env, path::Path};
use crate::{
    audio::AudioProcessor,
    driver,
    keystore::{self, Credentials},
    tasks,
};
use anyhow::{anyhow, Result};
use clap::{arg, command, Args};

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

    #[arg(short = 'C', long, help = "Whether to count in an intro for all tracks")]
    count_in: bool,

}

pub struct Download;

impl Download {
    pub fn run(args: DownloadArgs) -> Result<()> {
        Download::start_download(args)
    }

    fn start_download(args: DownloadArgs) -> Result<()> {
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
            count_in: false, // We'll handle this per-track in the download process
            transpose: args.transpose.unwrap_or(0),
        };

        // Perform the actual download
        let _track_names = driver.download_song(&args.song_url, download_options)?;

        // Process the downloaded files
        let download_path = args.download_path
            .as_deref()
            .map(Path::new)
            .ok_or_else(|| anyhow!("Download directory must be specified with --download-path"))?;

        AudioProcessor::process_downloads(download_path)?;

        // Cleanup original MP3s (optional)
        for entry in std::fs::read_dir(download_path)? {
            let path = entry?.path();
            if path.extension().map(|e| e == "mp3").unwrap_or(false) {
                std::fs::remove_file(path)?;
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