use headless_chrome::{Browser, LaunchOptions, Tab};
use std::sync::Arc;
use std::time::Duration;
use std::error::Error;
use anyhow::{Result, anyhow};
use std::ffi::OsStr;


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
            args: vec![
                OsStr::new("--disable-dev-shm-usage"),
                OsStr::new("--no-sandbox"),
                OsStr::new("--disable-setuid-sandbox"),
                OsStr::new("--disable-gpu"),
                OsStr::new("--disable-software-rasterizer"),
                OsStr::new("--disable-background-timer-throttling"),
                OsStr::new("--disable-backgrounding-occluded-windows"),
                OsStr::new("--disable-renderer-backgrounding"),
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
        tab.close(true)?; // Force-close the ephemeral tab
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
        use std::time::Duration;
        use std::thread::sleep;
    
        let mut all_urls = Vec::new();
        let tab = self.browser.new_tab()?;
        tab.set_default_timeout(Duration::from_secs(60));
    
        tracing::info!("Navigating to downloads page...");
        tab.navigate_to(&format!("https://{}/my/download.html", self.config.domain))?;
        tab.wait_until_navigated()?;
        sleep(Duration::from_secs(2));
    
        tracing::info!("Selecting Custom Backing Track filter...");
        // Wait for the select element and set the filter.
        tab.wait_for_element("select[name='file_type']")?;
        let set_filter_js = r#"
          let select = document.querySelector('select[name="file_type"]');
          if(select) {
            select.value = '1';
            select.dispatchEvent(new Event('change'));
          }
          true;
        "#;
        tab.evaluate(set_filter_js, true)?;
        sleep(Duration::from_secs(2));
    
        let mut page_number = 1;
        loop {
            tracing::info!("Processing page {}...", page_number);
            // Wait for the table rows.
            if let Err(e) = tab.wait_for_element_with_custom_timeout("#tab_files tbody tr", Duration::from_secs(60)) {
                tracing::warn!("Rows did not appear on page {}: {}", page_number, e);
                break;
            }
            sleep(Duration::from_secs(2)); // Allow extra time for the rows to be populated.
    
            // Evaluate our extraction snippet.
            let extraction_js = r#"
                (function(){
                try {
                    let tbody = document.querySelector('#tab_files tbody');
                    if (!tbody) {
                    return JSON.stringify({error: "No tbody found"});
                    }
                    let rows = tbody.querySelectorAll('tr');
                    console.log("Number of rows found:", rows.length);
                    let links = Array.from(rows).map(function(row){
                    let anchor = row.querySelector('td.my-downloaded-files__song.min-w-120 a');
                    return anchor ? { href: anchor.getAttribute('href'), title: anchor.textContent.trim() } : null;
                    }).filter(x => x !== null);
                    return JSON.stringify(links);
                } catch(e) {
                    return JSON.stringify({error: e.toString()});
                }
                })();
            "#;
            let result = tab.evaluate(extraction_js, true)?;
            tracing::debug!("Extraction result raw: {:?}", result.value);
            
            // Expect result.value to be a JSON string
            if let Some(json_str) = result.value.and_then(|v| v.as_str().map(|s| s.to_owned())) {
                let parsed: serde_json::Value = serde_json::from_str(&json_str)?;

                if let serde_json::Value::Array(items) = parsed {
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
                } else if let Some(error) = parsed.get("error").and_then(|v| v.as_str()) {
                    tracing::warn!("Extraction error on page {}: {}", page_number, error);
                }
            } else {
                tracing::warn!("Extraction result on page {}: None", page_number);
            }

            // Pagination: Get next page link.
            let next_js = r#"
              (function(){
                let nextElem = document.querySelector('.pagination a.next');
                return nextElem ? nextElem.getAttribute('href') : null;
              })();
            "#;
            let next_result = tab.evaluate(next_js, true)?;
            // Convert the result to an owned String.
            let next_href_opt = next_result.value.and_then(|v| v.as_str().map(String::from));
            if let Some(next_href_value) = next_href_opt {
                tracing::info!("Found next page link: {}", next_href_value);
                let full_next_url = if next_href_value.starts_with("http") {
                    next_href_value
                } else {
                    format!("https://{}{}", self.config.domain, next_href_value)
                };
                tracing::info!("Navigating to next page: {}", full_next_url);
                tab.navigate_to(&full_next_url)?;
                tab.wait_until_navigated()?;
                sleep(Duration::from_secs(2));
                page_number += 1;
            } else {
                tracing::info!("No more pages (current page: {})", page_number);
                break;
            }
        }
    
        tracing::info!("Collection complete! Found {} total tracks", all_urls.len());
        tab.close(true)?; 
        Ok(all_urls)
    }
        
    pub fn type_fast(&self, tab: &Tab, text: &str) {
        for c in text.chars() {
            tab.send_character(&c.to_string())
                .expect("failed to send character");
        }
    }
}