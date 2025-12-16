use crate::driver::Driver;
use anyhow::{anyhow, Result};
use headless_chrome::{Element, Tab};
use std::fmt::Display;
use std::{error::Error, thread::sleep, time::{Duration, Instant}};
use std::path::Path;
use std::fs;

#[derive(Default, Clone)]
pub struct DownloadOptions {
    pub count_in: bool,
    pub transpose: i8,
}

#[derive(Debug)]
pub enum DownloadError {
    NotPurchased,
    NotASongPage,
    ResetButtonNotFound,
    DownloadTimeout,
    BrowserError(String),
}

impl Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPurchased => f.write_str("This track has not been purchased"),
            Self::NotASongPage => f.write_str("This doesn't look like a song page. Check the url."),
            Self::ResetButtonNotFound => f.write_str("Reset button not found on the page"),
            Self::DownloadTimeout => f.write_str("Download operation timed out"),
            Self::BrowserError(msg) => write!(f, "Browser error: {}", msg),
        }
    }
}
impl Error for DownloadError {}

impl Driver {
    pub fn download_song(&self, url: &str, options: DownloadOptions) -> anyhow::Result<Vec<String>> {
        // Create a fresh tab for this download.
        let tab = self.browser.new_tab()?;
        tab.set_default_timeout(std::time::Duration::from_secs(3600));

        tracing::debug!("Navigating to URL: {}", url);
        tab.navigate_to(url)?.wait_until_navigated()?;

        // Wait for mixer to be present instead of arbitrary sleep
        if let Err(_) = tab.wait_for_element_with_custom_timeout(".mixer", Duration::from_secs(10)) {
             tracing::warn!("Mixer element not found immediately, page might be slow.");
        }

        // Validate that we are on a song page.
        if !self.is_a_song_page(&tab) {
            return Err(anyhow::anyhow!(DownloadError::NotASongPage));
        }

        // Check if the track is downloadable (i.e. it has been purchased).
        if !self.is_downloadable(&tab) {
            return Err(anyhow::anyhow!(DownloadError::NotPurchased));
        }

        tracing::debug!("Adjusting pitch if needed");
        self.adjust_pitch(options.transpose, &tab)?;

        tracing::debug!("Extracting track names");
        let track_names = Self::extract_track_names(&tab)?;

        tracing::debug!("Beginning download process for {} tracks", track_names.len());
        self.solo_and_download_tracks(&tab, &track_names, options.count_in)?;

        // Instead of immediately erroring out if the tab is unresponsive,
        // log a warning and continue.
        match tab.evaluate("true;", true) {
            Ok(_) => {},
            Err(e) => tracing::warn!("Post-download evaluation failed: {}", e),
        }

        // Close the temporary tab to free resources.
        tab.close(true)?;

        Ok(track_names)
    }


    fn click_reset_button(&self, tab: &Tab) -> Result<()> {
        let reset_button = tab.wait_for_element(".mixer__reset")
            .map_err(|_| anyhow!(DownloadError::ResetButtonNotFound))?;

        reset_button.scroll_into_view()?;
        reset_button.click()?;

        // Wait for the reset to actually happen.
        // We can check if all solo buttons are inactive or similar,
        // but for now, a small sleep is safer than checking 20 buttons.
        // Or we can check if the "is-active" class is removed from a known active element.
        // But since we don't know state, let's wait a moment but less than before.
        sleep(Duration::from_millis(500));

        Ok(())
    }


    fn solo_and_download_tracks(&self, tab: &Tab, track_names: &[String], count_in: bool) -> Result<()> {
        let solo_button_sel = ".track__controls.track__solo";
        // Ensure buttons are loaded
        tab.wait_for_element(solo_button_sel)?;

        let solo_buttons = tab.find_elements(solo_button_sel)?;
        let download_button = tab.find_element("a.download")?;

        // Click the reset button before processing tracks to ensure clean state
        self.click_reset_button(tab)?;

        // Check initial count-in state
        let mut current_count_in_state = self.is_count_in_enabled(tab)?;
        tracing::info!("Initial count-in state: {}", if current_count_in_state { "Enabled" } else { "Disabled" });

        // Get download path from config
        let download_path = self.config.download_path.clone()
            .unwrap_or_else(|| ".".to_string());

        for (index, solo_btn) in solo_buttons.iter().enumerate() {
            let track_name = &track_names[index];

            tracing::info!("Processing track {} '{}'", index + 1, track_name);
            solo_btn.scroll_into_view()?;

            // Click and wait for active state
            solo_btn.click()?;
            self.wait_for_solo_active(tab, index)?;

            // Handle count-in toggle
            // We use a shorter timeout for the element check since it should be there
            if let Ok(count_in_toggle) = tab.wait_for_element_with_custom_timeout("input#precount", Duration::from_secs(5)) {
                if index == 0 {
                    // For the first track (click track)
                    if count_in && !current_count_in_state {
                        tracing::info!("Enabling count-in for the first track");
                        count_in_toggle.click()?;
                        self.wait_for_count_in_state(tab, true)?;
                        current_count_in_state = true;
                    } else if !count_in && current_count_in_state {
                        tracing::info!("Disabling count-in for the first track");
                        count_in_toggle.click()?;
                        self.wait_for_count_in_state(tab, false)?;
                        current_count_in_state = false;
                    }
                } else {
                    // For subsequent tracks
                    if current_count_in_state {
                        tracing::info!("Disabling count-in for track: {}", track_name);
                        count_in_toggle.click()?;
                        self.wait_for_count_in_state(tab, false)?;
                        current_count_in_state = false;
                    }
                }
            }

            // Download the track
            tracing::info!("- starting download...");
            download_button.scroll_into_view()?;
            download_button.click()?;

            // Wait for download to complete by watching file system
            match self.wait_for_download(&download_path, Duration::from_secs(30)) {
                Ok(filename) => tracing::info!("- '{}' downloaded successfully as {}", track_name, filename),
                Err(e) => {
                    tracing::error!("- download failed for '{}': {}", track_name, e);
                    // Try to recover by closing modal if it exists
                     if let Ok(close_btn) = tab.find_element("button.js-modal-close") {
                        let _ = close_btn.click();
                    }
                    return Err(e);
                }
            }

            // Handle the "Begin Download" modal if it appears and stays (sometimes it auto-closes, sometimes not?)
            // If the download started, the modal might still be there.
            // The original code waited for .begin-download and then closed it.
            // If we already have the file, we can just ensure the modal is closed.
             if let Ok(close_btn) = tab.find_element("button.js-modal-close") {
                tracing::debug!("Closing download modal");
                let _ = close_btn.click();
                sleep(Duration::from_millis(500)); // Short wait for animation
            }
        }

        tracing::info!(
            "Done! Check your download folder to make sure you have all of these tracks: {:?}\n - ",
            track_names.join("\n - ")
        );

        Ok(())
    }

    fn wait_for_solo_active(&self, tab: &Tab, index: usize) -> Result<()> {
        let start = Instant::now();
        let timeout = Duration::from_secs(10);

        let js = format!(
            r#"
            (function() {{
                let btns = document.querySelectorAll('.track__controls.track__solo');
                if (btns.length <= {}) return false;
                return btns[{}].classList.contains('is-active');
            }})()
            "#,
            index, index
        );

        while start.elapsed() < timeout {
            let result = tab.evaluate(&js, true)?;
            if let Some(true) = result.value.and_then(|v| v.as_bool()) {
                return Ok(());
            }
            sleep(Duration::from_millis(100));
        }

        Err(anyhow!("Timed out waiting for solo button {} to become active", index))
    }

    fn wait_for_count_in_state(&self, tab: &Tab, expected_checked: bool) -> Result<()> {
        let start = Instant::now();
        let timeout = Duration::from_secs(5);

        while start.elapsed() < timeout {
             let is_checked = self.is_count_in_enabled(tab)?;
             if is_checked == expected_checked {
                 return Ok(());
             }
             sleep(Duration::from_millis(100));
        }

        Err(anyhow!("Timed out waiting for count-in state to become {}", expected_checked))
    }

    fn wait_for_download(&self, download_path: &str, timeout: Duration) -> Result<String> {
        let start = Instant::now();
        let path = Path::new(download_path);

        // Take a snapshot of existing files to identify the new one
        let initial_files: Vec<String> = fs::read_dir(path)?
            .filter_map(|e| e.ok())
            .map(|e| e.path().to_string_lossy().into_owned())
            .collect();

        tracing::debug!("Waiting for new file in {:?}", path);

        loop {
            if start.elapsed() > timeout {
                return Err(anyhow!(DownloadError::DownloadTimeout));
            }

            // Check for new files
            if let Ok(entries) = fs::read_dir(path) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let p = entry.path();
                    let s = p.to_string_lossy().into_owned();

                    if !initial_files.contains(&s) {
                        // Found a new file!
                        // Check if it's a temporary download file
                        let extension = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                        if extension == "crdownload" || extension == "part" {
                            tracing::debug!("Found temp file: {:?}", p);
                            sleep(Duration::from_millis(500));
                            continue;
                        }

                        // It seems to be a final file.
                        // Let's verify size is stable (download finished)
                        if self.is_file_stable(&p)? {
                            tracing::info!("Download detected: {:?}", p);
                            return Ok(p.file_name().unwrap().to_string_lossy().into_owned());
                        }
                    }
                }
            }

            sleep(Duration::from_millis(500));
        }
    }

    fn is_file_stable(&self, path: &Path) -> Result<bool> {
        // Check if file size remains constant for a short period
        let meta1 = fs::metadata(path)?;
        let size1 = meta1.len();

        sleep(Duration::from_millis(500));

        let meta2 = fs::metadata(path)?;
        let size2 = meta2.len();

        if size1 == size2 && size1 > 0 {
             Ok(true)
        } else {
             Ok(false)
        }
    }

    fn is_count_in_enabled(&self, tab: &Tab) -> Result<bool> {
        let count_in_toggle = tab.wait_for_element_with_custom_timeout("input#precount", Duration::from_secs(60))?;
        Ok(count_in_toggle.is_checked())
    }


    pub fn extract_track_names(tab: &Tab) -> Result<Vec<String>> {
        let track_names = tab.find_elements(".mixer .track .track__caption")?;
        let mut names: Vec<String> = vec![];
        for el in track_names {
            // the name may contain other child nodes, so we'll execute a js function
            // to just grab the last child, which is the text.
            let name: String = el
                .call_js_fn(
                    r#"
                    function get_name() {
                        return this.lastChild.nodeValue.trim();
                    }
                    "#,
                    vec![],
                    true,
                )?
                .value
                // remove quotes & new lines from the extracted text
                .map(|v| v.to_string().replace("\\n", " ").replace('"', ""))
                .unwrap_or(String::new());
            names.push(name);
        }

        Ok(names)
    }

    fn is_a_song_page(&self, tab: &Tab) -> bool {
        let has_mixer = tab.find_element("div.mixer").is_ok();
        let has_download_button = tab.find_element("a.download").is_ok();
        has_mixer && has_download_button
    }

    fn is_downloadable(&self, tab: &Tab) -> bool {
        // if the download button also has the addtocart class, then this hasn't been purchased
        let el = tab.find_element("a.download.addtocart").ok();
        el.is_none()
    }

    fn adjust_pitch(&self, desired_pitch: i8, tab: &Tab) -> Result<()> {
        // pitch is remembered per-son on your account, so this logic cannot be deterministic. Instead
        // we''l try to infer the direction we need to go based on what the pitch is currently set to.
        let pitch_label = tab
            .find_element("span.pitch__value")
            .expect("can't find pitch value");
        let pitch_up_btn = tab
            .find_element("div.pitch button.btn--pitch[title='Key up' i]")
            .expect("can't find pitch up button");
        let pitch_down_btn = tab
            .find_element("div.pitch button.btn--pitch[title='Key down' i]")
            .expect("can't find pitch down button");

        pitch_up_btn.focus()?;

        let current_pitch: i8 = pitch_label.get_inner_text()?.parse()?;
        let diff = desired_pitch - current_pitch;
        if diff == 0 {
            return Ok(());
        }
        tracing::info!(
            "Setting pitch to {} (currently: {})",
            desired_pitch,
            current_pitch
        );

        let button = if diff > 0 {
            pitch_up_btn
        } else {
            pitch_down_btn
        };

        let mut iterations_allowed = 10;
        loop {
            assert!(
                iterations_allowed > 0,
                "failed to set pitch, breaking to avoid infinite loop"
            );
            iterations_allowed -= 1;

            tracing::debug!("Pitching tracks...");
            button.click().expect("couldn't click pitch button");

            // Wait for pitch value to change
            // We'll just wait a bit because pitch logic is tricky to verify quickly
            // without knowing the exact step latency
            sleep(Duration::from_millis(250));

            let new_pitch: i8 = pitch_label.get_inner_text()?.parse()?;
            tracing::debug!("Pitching is now {}, target: {}", new_pitch, desired_pitch);

            if new_pitch == desired_pitch {
                break;
            }
        }

        // need to reload the song after pitching
        tracing::info!("Reloading tracks after pitching...");
        tab.find_element("a#pitch-link")
            .expect("can't find pitch link")
            .click()?;

        // Wait for mixer to reload instead of fixed sleep
        // The mixer element might stay there, but we can wait for the URL to change or just wait a bit longer than usual
        // Reloading usually takes a moment.
        sleep(Duration::from_secs(4)); // Still keeping this sleep as page reload is hard to track perfectly without navigation events, and we are on same page

        Ok(())
    }
}

trait Checkable {
    fn is_checked(&self) -> bool;
}

impl<'a> Checkable for Element<'a> {
    fn is_checked(&self) -> bool {
        match self.attributes.as_ref() {
            Some(attrs) => attrs.contains(&String::from("checked")),
            None => false,
        }
    }
}
