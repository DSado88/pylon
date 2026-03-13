use std::path::PathBuf;

use crate::error::{CockpitError, Result};

#[derive(Debug, Clone)]
pub struct AccountConfig {
    pub name: String,
    pub org_id: String,
    pub auth_method: AuthMethod,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    OAuth,
    SessionKey,
}

#[derive(Debug, Clone)]
pub struct TrackerConfig {
    pub poll_interval_secs: u64,
    pub accounts: Vec<AccountConfig>,
    pub active_account: usize,
}

impl TrackerConfig {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;
        let content = std::fs::read_to_string(&config_path)
            .map_err(|e| CockpitError::Config(format!("read claude-tracker config: {e}")))?;

        let table: toml::Table = content
            .parse()
            .map_err(|e| CockpitError::Config(format!("invalid claude-tracker TOML: {e}")))?;

        let settings = table.get("settings");

        let poll_interval_secs = settings
            .and_then(|s| s.get("poll_interval_secs"))
            .and_then(|v| v.as_integer())
            .map(|v| v.max(30) as u64)
            .unwrap_or(180);

        let active_account = settings
            .and_then(|s| s.get("active_account"))
            .and_then(|v| v.as_integer())
            .map(|v| v as usize)
            .unwrap_or(0);

        let accounts = table
            .get("accounts")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|entry| {
                        let name = entry.get("name")?.as_str()?.to_string();
                        let org_id = entry.get("org_id")?.as_str()?.to_string();
                        let auth_method =
                            match entry.get("auth_method").and_then(|v| v.as_str()) {
                                Some("oauth") => AuthMethod::OAuth,
                                _ => AuthMethod::SessionKey,
                            };
                        Some(AccountConfig {
                            name,
                            org_id,
                            auth_method,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(TrackerConfig {
            poll_interval_secs,
            accounts,
            active_account,
        })
    }

    fn config_path() -> Result<PathBuf> {
        let home = std::env::var("HOME")
            .map_err(|_| CockpitError::Config("HOME not set".into()))?;
        Ok(PathBuf::from(home).join(".config/claude-tracker/config.toml"))
    }
}
