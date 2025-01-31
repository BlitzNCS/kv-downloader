use headless_chrome::{Browser, LaunchOptions, Tab};
use std::sync::Arc;
use std::time::Duration;
use std::thread::sleep;
use std::error::Error;
use serde_json::Value;
use anyhow::{Result, anyhow};

pub struct Config {
    pub domain: String,
    pub headless: bool,
    pub download_path: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            domain: "www.karaoke-version.com".to_owned(),
            headless: false,
            download_path: None,
        }
    }
}

pub struct Driver {
    pub config: Config,
    pub browser: Browser,
    main_tab: Arc<Tab>,
}

impl Driver {
    pub fn new(config: Config) -> Self {
        let browser = Browser::new(LaunchOptions {
            headless: config.headless,
            window_size: Some((1440, 1200)),
            enable_logging: true,
            ignore_certificate_errors: true,
            sandbox: false,
            additional_args: vec![
                "--disable-dev-shm-usage",
                "--no-sandbox",
                "--disable-setuid-sandbox",
                "--disable-gpu",
                "--disable-software-rasterizer",
            ],
            ..Default::default()
        })
        .expect("Unable to create headless Chromium browser");

        if let Some(download_path) = &config.download_path {
            Self::set_download_path(&browser, download_path)
                .expect("Failed to set download path");
        }

        let raw_tab = browser.new_tab().expect("Failed to create tab");
        raw_tab.set_default_timeout(Duration::from_secs(3600));
        
        Self {
            config,
            browser,
            main_tab: raw_tab,
        }
    }

    pub fn get_tab(&self) -> Result<Arc<Tab>> {
        if self.main_tab.evaluate("true;", true).is_err() {
            return Err(anyhow!("Browser tab is no longer responsive"));
        }
        Ok(self.main_tab.clone())
    }

    fn set_download_path(browser: &Browser, download_path: &str) -> Result<(), Box<dyn Error>> {
        let tab = browser.new_tab()?;
        
        let download_behavior_method = headless_chrome::protocol::cdp::Browser::SetDownloadBehavior {
            browser_context_id: None,
            behavior: headless_chrome::protocol::cdp::Browser::SetDownloadBehaviorBehaviorOption::Allow,
            download_path: Some(download_path.to_string()),
            events_enabled: None
        };
        
        tab.call_method(download_behavior_method)?;
        Ok(())
    }

    fn verify_table_content(&self, tab: &Tab) -> Result<bool> {
        let verify_js = r#"
            document.querySelectorAll('td.my-downloaded-files__song.min-w-120').length > 0
        "#;
        
        let result = tab.evaluate(verify_js, true)?;
        Ok(result.value.and_then(|v| v.as_bool()).unwrap_or(false))
    }

    pub fn collect_all_custom_track_urls(&self) -> Result<Vec<String>> {
        let mut all_urls = Vec::new();
        let tab = self.browser.new_tab()?;
        tab.set_default_timeout(Duration::from_secs(60));

        // Navigate to downloads page
        tracing::info!("Navigating to downloads page...");
        tab.navigate_to(&format!("https://{}/my/download.html", self.config.domain))?;
        tab.wait_until_navigated()?;
        sleep(Duration::from_secs(5));

        // Select "Custom Backing Track" filter
        tracing::info!("Selecting Custom Backing Track filter...");
        tab.wait_for_element("select[name='file_type']")?;
        
        let js = r#"
            let select = document.querySelector('select[name="file_type"]');
            select.value = '1';
            select.dispatchEvent(new Event('change'));
        "#;
        tab.evaluate(js, true)?;
        sleep(Duration::from_secs(5));

        let mut page_number = 1;
        loop {
            tracing::info!("Processing page {}...", page_number);
            
            // Wait for table and verify content
            tab.wait_for_element("#tab_files")?;
            sleep(Duration::from_secs(5));

            if !self.verify_table_content(&tab)? {
                tracing::warn!("No table content found on page {}", page_number);
                break;
            }

            // Extract URLs from current page
            let js = r#"
                Array.from(document.querySelectorAll('td.my-downloaded-files__song.min-w-120 a')).map(a => ({
                    href: a.getAttribute('href'),
                    title: a.textContent.trim()
                }))
            "#;
            
            let result = tab.evaluate(js, true)?;
            if let Some(Value::Array(items)) = result.value {
                for item in items {
                    if let (Some(href), Some(title)) = (
                        item.get("href").and_then(|v| v.as_str()),
                        item.get("title").and_then(|v| v.as_str())
                    ) {
                        let full_url = format!("https://{}{}", self.config.domain, href);
                        tracing::info!("Found track: {} at {}", title, full_url);
                        all_urls.push(full_url);
                    }
                }
            }

            // Check for next page
            let has_next = tab.evaluate(
                "!!document.querySelector('.pagination a.next:not([style*=\"display: none\"])')",
                true
            )?;

            if has_next.value.and_then(|v| v.as_bool()).unwrap_or(false) {
                tracing::info!("Moving to page {}...", page_number + 1);
                tab.find_element(".pagination a.next")?.click()?;
                sleep(Duration::from_secs(5));
                page_number += 1;
            } else {
                tracing::info!("No more pages (current page: {})", page_number);
                break;
            }
        }

        tracing::info!("Collection complete! Found {} total tracks", all_urls.len());
        Ok(all_urls)
    }

    pub fn type_fast(&self, tab: &Tab, text: &str) {
        for c in text.chars() {
            tab.send_character(&c.to_string())
                .expect("failed to send character");
        }
    }
}