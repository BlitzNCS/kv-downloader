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
            // ... other flags ...
            ..Default::default()
        })
        .expect("Unable to create headless Chromium browser");

        // Set download path if needed
        if let Some(download_path) = &config.download_path {
            println!("Would set download path: {}", download_path);
            // set_download_path(...) logic goes here
        }

// 1. Create a raw Tab (which is a Tab, not an Arc<Tab>)
let raw_tab = browser
    .new_tab()
    .expect("Failed to create tab");

// 2. `set_default_timeout` returns `&Tab`, not a `Result`
raw_tab.set_default_timeout(Duration::from_secs(3600));

// 3. Wrap `raw_tab` in `Arc` exactly once
let main_tab = raw_tab;

// 4. Build the Driver with that single Arc
Self {
    config,
    browser,
    main_tab,
}
    }

    pub fn get_tab(&self) -> Result<Arc<Tab>> {
        // Check if tab is still responsive
        if self.main_tab.evaluate("true;", true).is_err() {
            return Err(anyhow!("Browser tab is no longer responsive"));
        }
        Ok(self.main_tab.clone())
    }




    fn set_download_path(browser: &Browser, download_path: &str) -> Result<(), Box<dyn Error>> {
        let tab = browser
            .new_tab()
            .expect("couldn't open a new tab to set download behavior");

        let download_behavior_method = headless_chrome::protocol::cdp::Browser::SetDownloadBehavior {
            browser_context_id: None,
            behavior: headless_chrome::protocol::cdp::Browser::SetDownloadBehaviorBehaviorOption::Allow,
            download_path: Some(download_path.to_string()),
            events_enabled: None
        };
        tracing::debug!("call_method (set download behavior)");
        tab.call_method(download_behavior_method)?;

        Ok(())
    }

    fn verify_table_content(&self, tab: &Tab) -> Result<bool> {
        // First check if we can find any rows
        let verify_js = r#"
            document.querySelectorAll('td.my-downloaded-files__song.min-w-120').length
        "#;
        
        let result = tab.evaluate(verify_js, true)?;
        if let Some(Value::Number(count)) = result.value {
            let count = count.as_u64().unwrap_or(0);
            tracing::debug!("Found {} table cells", count);
            Ok(count > 0)
        } else {
            tracing::debug!("No table cells found");
            Ok(false)
        }
    }

    pub fn collect_all_custom_track_urls(&self) -> Result<Vec<String>> {
        let mut all_urls = Vec::new();
        let tab = self.browser.new_tab()?;

        // Navigate to downloads page
        tracing::info!("Navigating to downloads page...");
        tab.navigate_to(&format!("https://{}/my/download.html", self.config.domain))?;
        tab.wait_until_navigated()?;
        sleep(Duration::from_secs(5));

        // Select "Custom Backing Track" from dropdown
        tracing::info!("Selecting Custom Backing Track filter...");
        tab.wait_for_element("select[name='file_type']")?
            .click()?;
        sleep(Duration::from_secs(2));

        // After setting dropdown value
        let js = r#"
            let select = document.querySelector('select[name="file_type"]');
            select.value = '1';
            select.dispatchEvent(new Event('change'));
        "#;
        tab.evaluate(js, true)?;
        sleep(Duration::from_secs(5)); 

        // Wait for table and content
        tab.wait_for_element("#tab_files")?;
        tab.wait_for_element("td.my-downloaded-files__song.min-w-120")?;
        sleep(Duration::from_secs(2)); // Additional wait to ensure content is fully loaded

        let mut page_number = 1;
        loop {
            tracing::info!("Processing page {}...", page_number);
            // At the start of each page processing:
            tracing::info!("Current page URL: {}", tab.get_url());
            tab.wait_for_element("#tab_files")?;
            sleep(Duration::from_secs(5));

            let js = r#"
            function getUrls() {
                const links = document.querySelectorAll('table#tab_files td.my-downloaded-files__song.min-w-120 a');
                const results = Array.from(links).map(a => {
                    return JSON.stringify({
                        href: a.getAttribute('href'),
                        title: a.textContent.trim()
                    });
                });
                return JSON.stringify(results);  // Double stringify to ensure serialization
            }
            getUrls();
        "#;
        
        let result = tab.evaluate(js, true)?;
        tracing::debug!("Raw JavaScript result: {:?}", result);
        
        // Parse the stringified JSON result
        if let Some(Value::String(json_str)) = result.value {
            if let Ok(parsed) = serde_json::from_str::<Vec<String>>(&json_str) {
                for item_str in parsed {
                    if let Ok(item) = serde_json::from_str::<serde_json::Value>(&item_str) {
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
            }
        } else {
            tracing::debug!("JavaScript result was not a string: {:?}", result.value);
        }
                
        
            let has_content = self.verify_table_content(&tab)?;
            if !has_content {
                tracing::warn!("No table content found on page {}", page_number);
                continue;
            }

            // Check if there are more pages by looking for the "next" link
            let has_next_page = tab.evaluate(
                r#"
                !!document.querySelector('.pagination a.next:not([style*="display: none"])')
                "#,
                true
            )?;

            if has_next_page.value.and_then(|v| v.as_bool()).unwrap_or(false) {
                tracing::info!("Moving to page {}...", page_number + 1);
                if let Ok(next_button) = tab.find_element(".pagination a.next") {
                    next_button.click()?;
                    sleep(Duration::from_secs(4));
                    page_number += 1;
                // After clicking next:
                let new_url = tab.get_url();
                tracing::info!("After clicking next, URL is: {}", new_url);

                } else {
                    tracing::warn!("Next button not found despite being detected");
                    break;
                }
            } else {
                tracing::info!("No more pages detected (current page: {})", page_number);
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