// src/tasks/download_song.rs
use crate::driver::Driver;
use anyhow::{anyhow, Result};
use headless_chrome::Tab;
use std::{error::Error, thread::sleep, time::Duration};
use headless_chrome::Element;


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

impl std::fmt::Display for DownloadError {
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
    /// Download a song on an existing tab (instead of opening a new one).
    pub fn download_song_on_tab(
        &self,
        tab: &Tab,
        url: &str,
        options: DownloadOptions,
    ) -> Result<Vec<String>> {
        tracing::debug!("Navigating to URL: {}", url);
        tab.navigate_to(url)?.wait_until_navigated()?;
        std::thread::sleep(Duration::from_secs(2));
    
        // Validate that we are on a song page.
        if !self.is_a_song_page(tab) {
            return Err(anyhow!(DownloadError::NotASongPage));
        }
        // Check if the track is downloadable.
        if !self.is_downloadable(tab) {
            return Err(anyhow!(DownloadError::NotPurchased));
        }
    
        tracing::debug!("Adjusting pitch if needed");
        self.adjust_pitch(options.transpose, tab)?;
    
        tracing::debug!("Extracting track names");
        let track_names = self.extract_track_names(tab)?;
    
        tracing::debug!("Beginning download process for {} tracks", track_names.len());
        self.solo_and_download_tracks(tab, &track_names, options.count_in)?;
    
        // Verify that the tab is still responsive.
        match tab.evaluate("true;", true) {
            Ok(_) => {},
            Err(e) => tracing::warn!("Post-download evaluation failed: {}", e),
        }
    
        Ok(track_names)
    }

    

    fn click_reset_button(&self, tab: &Tab) -> Result<()> {
        let reset_button = tab.wait_for_element(".mixer__reset")
            .map_err(|_| anyhow!(DownloadError::ResetButtonNotFound))?;
        reset_button.scroll_into_view()?;
        sleep(Duration::from_secs(1));
        reset_button.click()?;
        sleep(Duration::from_secs(2));
        Ok(())
    }

    fn solo_and_download_tracks(&self, tab: &Tab, track_names: &[String], count_in: bool) -> Result<()> {
        let solo_button_sel = ".track__controls.track__solo";
        let solo_buttons = tab.find_elements(solo_button_sel)?;
        let download_button = tab.find_element("a.download")?;

        tab.enable_debugger()?;
        sleep(Duration::from_secs(2));

        // Click the reset button before processing tracks.
        self.click_reset_button(tab)?;

        // Check initial count-in state.
        let mut current_count_in_state = self.is_count_in_enabled(tab)?;
        tracing::info!("Initial count-in state: {}", if current_count_in_state { "Enabled" } else { "Disabled" });

        for (index, solo_btn) in solo_buttons.iter().enumerate() {
            let track_name = &track_names[index];
            tracing::info!("Processing track {} '{}'", index + 1, track_name);
            solo_btn.scroll_into_view()?;
            sleep(Duration::from_secs(2));
            solo_btn.click()?;
            sleep(Duration::from_secs(2));

            // Handle count-in toggle.
            if let Ok(count_in_toggle) = tab.wait_for_element_with_custom_timeout("input#precount", Duration::from_secs(60)) {
                if index == 0 {
                    if count_in && !current_count_in_state {
                        tracing::info!("Enabling count-in for the first track");
                        count_in_toggle.click()?;
                        current_count_in_state = true;
                    } else if !count_in && current_count_in_state {
                        tracing::info!("Disabling count-in for the first track");
                        count_in_toggle.click()?;
                        current_count_in_state = false;
                    }
                } else {
                    if current_count_in_state {
                        tracing::info!("Disabling count-in for track: {}", track_name);
                        count_in_toggle.click()?;
                        current_count_in_state = false;
                    }
                }
                sleep(Duration::from_secs(1));
            }

            tracing::info!("Count-in toggle state for track '{}': {}", track_name, if current_count_in_state { "Enabled" } else { "Disabled" });

            tracing::info!("- starting download...");
            download_button.scroll_into_view()?;
            sleep(Duration::from_secs(2));
            download_button.click()?;
            sleep(Duration::from_secs(2));

            tracing::info!("- waiting for download modal...");
            tab.wait_for_element_with_custom_timeout(".begin-download", Duration::from_secs(60))
                .expect("Timed out waiting for download modal.");

            tab.find_element("button.js-modal-close")?.click()?;
            sleep(Duration::from_secs(4));
            tracing::info!("- '{}' complete!", track_name);
        }

        tracing::info!(
            "Done! Check your download folder to make sure you have all of these tracks: {:?}",
            track_names
        );

        Ok(())
    }

    fn is_count_in_enabled(&self, tab: &Tab) -> Result<bool> {
        let count_in_toggle = tab.wait_for_element_with_custom_timeout("input#precount", Duration::from_secs(60))?;
        Ok(count_in_toggle.is_checked())
    }

    pub fn extract_track_names(&self, tab: &Tab) -> Result<Vec<String>> {
        let track_names = tab.find_elements(".mixer .track .track__caption")?;
        let mut names: Vec<String> = vec![];
        for el in track_names {
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
                .map(|v| v.to_string().replace("\\n", " ").replace('"', ""))
                .unwrap_or_default();
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
        let el = tab.find_element("a.download.addtocart").ok();
        el.is_none()
    }

    fn adjust_pitch(&self, desired_pitch: i8, tab: &Tab) -> Result<()> {
        let pitch_label = tab.find_element("span.pitch__value")
            .expect("can't find pitch value");
        let pitch_up_btn = tab.find_element("div.pitch button.btn--pitch[title='Key up' i]")
            .expect("can't find pitch up button");
        let pitch_down_btn = tab.find_element("div.pitch button.btn--pitch[title='Key down' i]")
            .expect("can't find pitch down button");

        pitch_up_btn.focus()?;
        let current_pitch: i8 = pitch_label.get_inner_text()?.parse()?;
        let diff = desired_pitch - current_pitch;
        if diff == 0 {
            return Ok(());
        }
        tracing::info!("Setting pitch to {} (currently: {})", desired_pitch, current_pitch);
        let button = if diff > 0 { pitch_up_btn } else { pitch_down_btn };
        let mut iterations_allowed = 10;
        loop {
            assert!(iterations_allowed > 0, "failed to set pitch, breaking to avoid infinite loop");
            iterations_allowed -= 1;
            tracing::debug!("Pitching tracks...");
            button.click().expect("couldn't click pitch button");
            sleep(Duration::from_millis(100));
            let new_pitch: i8 = pitch_label.get_inner_text()?.parse()?;
            tracing::debug!("Pitching is now {}, target: {}", new_pitch, desired_pitch);
            sleep(Duration::from_millis(100));
            if new_pitch == desired_pitch {
                break;
            }
        }
        tracing::info!("Reloading tracks after pitching...");
        tab.find_element("a#pitch-link")
            .expect("can't find pitch link")
            .click()?;
        sleep(Duration::from_secs(6));
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
