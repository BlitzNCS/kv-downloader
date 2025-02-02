// src/tasks/sign_in.rs

use crate::keystore::Keystore;
use std::{thread::sleep, time::Duration};
use crate::driver::Driver;
use anyhow::{Result, anyhow};

const LOGIN_TIMEOUT: std::time::Duration = Duration::from_secs(30);

impl Driver {
    fn validate_session(&self, tab: &headless_chrome::Tab) -> bool {
        tracing::debug!("Validating session state...");

        // Check if we're redirected to login page
        if tab.get_url().contains("/my/login.html") {
            tracing::debug!("On login page - session invalid");
            return false;
        }

        // If we can access the account page, we're definitely logged in
        if tab.get_url().contains("/my/account") || tab.get_url().contains("/my/download") {
            tracing::debug!("On account/download page - session valid");
            return true;
        }
        
        // Check for various logged-in indicators
        let logged_in_indicators = [
            ".account-menu",
            ".user-menu",
            ".my-account",
            "#logout",
            "[href*='logout']",
        ];

        for indicator in logged_in_indicators.iter() {
            if tab.find_element(indicator).is_ok() {
                tracing::debug!("Found logged-in indicator: {} - session valid", indicator);
                return true;
            }
        }

        // Only check for login form if no logged-in indicators were found
        if tab.find_element("#frm_login").is_ok() {
            tracing::debug!("Login form found - session invalid");
            return false;
        }

        tracing::debug!("No session indicators found - assuming invalid");
        false
    }

    /// Sign in using the provided tab. This function does not create a new tab.
    pub fn sign_in_on_tab(&self, tab: &headless_chrome::Tab, user: &str, pass: &str) -> Result<()> {
        tab.set_default_timeout(LOGIN_TIMEOUT);
        tracing::info!("Starting sign-in process for user: {}", user);

        // Navigate to homepage.
        tracing::info!("Navigating to homepage...");
        tab.navigate_to(&format!("https://{}", self.config.domain))?;
        tab.wait_until_navigated()?;
        sleep(Duration::from_secs(3));

        // Try to restore a previous session from a stored cookie.
        if let Some(cookie) = Keystore::get_auth_cookie().ok() {
            tracing::info!("Found previous session cookie, attempting to restore...");
            tab.set_cookies(vec![cookie])?;
            tab.reload(true, None)?;
            sleep(Duration::from_secs(3));
            if self.validate_session(tab) {
                tracing::info!("Successfully restored previous session");
                return Ok(());
            }
            tracing::info!("Previous session expired or invalid");
        }

        // Perform a fresh login.
        tracing::info!("Performing fresh login");
        let login_url = format!("https://{}/my/login.html", self.config.domain);
        tracing::info!("Navigating to login page: {}", login_url);
        tab.navigate_to(&login_url)?;
        tab.wait_until_navigated()?;
        sleep(Duration::from_secs(3));

        if self.validate_session(tab) {
            tracing::info!("Already logged in!");
            return Ok(());
        }

        tracing::info!("Filling login form...");
        let username_field = tab.wait_for_element("#frm_login")
            .map_err(|_| anyhow!("Could not find username field"))?;
        username_field.focus()?;
        sleep(Duration::from_millis(500));
        self.type_fast(tab, user);
        sleep(Duration::from_secs(1));

        let password_field = tab.wait_for_element("#frm_password")
            .map_err(|_| anyhow!("Could not find password field"))?;
        password_field.focus()?;
        sleep(Duration::from_millis(500));
        self.type_fast(tab, pass);
        sleep(Duration::from_secs(1));

        tracing::info!("Submitting login form...");
        let submit_button = tab.wait_for_element("#sbm")
            .map_err(|_| anyhow!("Could not find submit button"))?;
        submit_button.click()?;
        tab.wait_until_navigated()?;
        sleep(Duration::from_secs(5));

        if !self.validate_session(tab) {
            return Err(anyhow!("Login failed - unable to validate session"));
        }

        // Save the session cookie for later use.
        if let Ok(cookies) = tab.get_cookies() {
            if let Some(session_cookie) = cookies.iter().find(|c| c.name == "karaoke-version") {
                tracing::info!("Saving new session cookie");
                Keystore::set_auth_cookie(session_cookie)?;
            }
        }

        tracing::info!("Login successful!");
        Ok(())
    }


}
