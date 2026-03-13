use std::sync::OnceLock;
use std::time::Duration;

use crate::error::{CockpitError, Result};

const USAGE_ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const REFRESH_ENDPOINT: &str = "https://api.anthropic.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const BETA_HEADER: &str = "oauth-2025-04-20";
const USER_AGENT: &str = "claude-code/2.0.32";
const KEYRING_SERVICE: &str = "claude-tracker";

pub(crate) fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

#[derive(Debug, Clone)]
pub struct UsageData {
    pub utilization: u32,
    pub resets_at: Option<chrono::DateTime<chrono::Utc>>,
    pub weekly_utilization: Option<u32>,
    pub weekly_resets_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub struct TokenInfo {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub raw_credential: String,
}

pub fn load_token(account_name: &str) -> Result<TokenInfo> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, account_name)
        .map_err(|e| CockpitError::Sidebar(format!("keyring entry: {e}")))?;
    let raw = entry
        .get_password()
        .map_err(|e| CockpitError::Sidebar(format!("keyring get '{account_name}': {e}")))?;

    let access_token = normalize_stored_token(&raw);
    let refresh_token = extract_refresh_token(&raw);

    Ok(TokenInfo {
        access_token,
        refresh_token,
        raw_credential: raw,
    })
}

pub async fn fetch_usage(access_token: &str) -> Result<UsageData> {
    let resp = http_client()
        .get(USAGE_ENDPOINT)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("anthropic-beta", BETA_HEADER)
        .header("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| CockpitError::Sidebar(format!("usage fetch: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(CockpitError::Sidebar(format!(
            "usage API returned {status}"
        )));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| CockpitError::Sidebar(format!("usage parse: {e}")))?;

    let five_hour = body
        .get("five_hour")
        .ok_or_else(|| CockpitError::Sidebar("missing five_hour in usage response".into()))?;

    let utilization = parse_utilization(five_hour);
    let resets_at = parse_resets_at(five_hour);

    let (weekly_utilization, weekly_resets_at) = body
        .get("seven_day")
        .filter(|v| !v.is_null())
        .map(|seven_day| {
            (
                Some(parse_utilization(seven_day)),
                parse_resets_at(seven_day),
            )
        })
        .unwrap_or((None, None));

    Ok(UsageData {
        utilization,
        resets_at,
        weekly_utilization,
        weekly_resets_at,
    })
}

#[derive(Debug)]
pub struct RefreshedToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: i64,
}

pub async fn refresh_access_token(refresh_token: &str) -> Result<RefreshedToken> {
    let resp = http_client()
        .post(REFRESH_ENDPOINT)
        .header("User-Agent", USER_AGENT)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
        ])
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| CockpitError::Sidebar(format!("token refresh: {e}")))?;

    if !resp.status().is_success() {
        return Err(CockpitError::Sidebar(format!(
            "token refresh returned {}",
            resp.status()
        )));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| CockpitError::Sidebar(format!("refresh parse: {e}")))?;

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CockpitError::Sidebar("no access_token in refresh response".into()))?
        .to_string();

    let refresh_token = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let expires_in = body
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .unwrap_or(28800);

    let expires_at = chrono::Utc::now().timestamp_millis() + (expires_in * 1000);

    Ok(RefreshedToken {
        access_token,
        refresh_token,
        expires_at,
    })
}

pub fn save_refreshed_token(account_name: &str, raw_credential: &str) -> Result<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, account_name)
        .map_err(|e| CockpitError::Sidebar(format!("keyring entry: {e}")))?;
    entry
        .set_password(raw_credential)
        .map_err(|e| CockpitError::Sidebar(format!("keyring set: {e}")))?;
    Ok(())
}

fn normalize_stored_token(raw: &str) -> String {
    if raw.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
            let creds = value.get("claudeAiOauth").unwrap_or(&value);
            if let Some(token) = creds
                .get("accessToken")
                .or_else(|| creds.get("access_token"))
                .and_then(|v| v.as_str())
            {
                return token.to_string();
            }
        }
    }
    raw.to_string()
}

fn extract_refresh_token(raw: &str) -> Option<String> {
    if !raw.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let creds = value.get("claudeAiOauth").unwrap_or(&value);
    creds
        .get("refreshToken")
        .or_else(|| creds.get("refresh_token"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub fn update_credential_json(
    raw: &str,
    new_access: &str,
    new_refresh: Option<&str>,
    expires_at: i64,
) -> String {
    if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(raw) {
        let creds = if value.get("claudeAiOauth").is_some() {
            match value.get_mut("claudeAiOauth") {
                Some(c) => c,
                None => &mut value,
            }
        } else {
            &mut value
        };

        if let Some(obj) = creds.as_object_mut() {
            if obj.contains_key("accessToken") {
                obj.insert("accessToken".into(), new_access.into());
            } else {
                obj.insert("access_token".into(), new_access.into());
            }
            if let Some(rt) = new_refresh {
                if obj.contains_key("refreshToken") {
                    obj.insert("refreshToken".into(), rt.into());
                } else {
                    obj.insert("refresh_token".into(), rt.into());
                }
            }
            if obj.contains_key("expiresAt") {
                obj.insert("expiresAt".into(), expires_at.into());
            } else {
                obj.insert("expires_at".into(), expires_at.into());
            }
        }
        serde_json::to_string(&value).unwrap_or_else(|_| raw.to_string())
    } else {
        new_access.to_string()
    }
}

fn parse_utilization(bucket: &serde_json::Value) -> u32 {
    bucket
        .get("utilization")
        .and_then(|v| v.as_u64().map(|n| n as f64).or_else(|| v.as_f64()))
        .map(|v| v.round() as u32)
        .unwrap_or(0)
}

fn parse_resets_at(bucket: &serde_json::Value) -> Option<chrono::DateTime<chrono::Utc>> {
    bucket
        .get("resets_at")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
}
