use std::time::Duration;

use tokio::sync::mpsc;

use super::api;
use super::config::{AuthMethod, TrackerConfig};

#[derive(Debug, Clone)]
pub enum UsageUpdate {
    Data {
        account_name: String,
        data: api::UsageData,
    },
    Error(String),
}

pub fn spawn(
    rt: &tokio::runtime::Handle,
    poll_interval: Duration,
) -> mpsc::UnboundedReceiver<UsageUpdate> {
    let (tx, rx) = mpsc::unbounded_channel();

    rt.spawn(async move {
        let config = match TrackerConfig::load() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(UsageUpdate::Error(format!("config: {e}")));
                return;
            }
        };

        // Use the configured interval, minimum 30s
        let interval = Duration::from_secs(config.poll_interval_secs.max(30));
        let effective = if poll_interval > interval {
            poll_interval
        } else {
            interval
        };

        // Initial fetch immediately
        fetch_and_send(&config, &tx).await;

        let mut ticker = tokio::time::interval(effective);
        ticker.tick().await; // consume immediate tick

        loop {
            ticker.tick().await;
            fetch_and_send(&config, &tx).await;
        }
    });

    rx
}

async fn fetch_and_send(config: &TrackerConfig, tx: &mpsc::UnboundedSender<UsageUpdate>) {
    for (i, account) in config.accounts.iter().enumerate() {
        if account.auth_method != AuthMethod::OAuth {
            continue;
        }

        // Stagger accounts by 100ms to avoid thundering herd
        if i > 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let name = account.name.clone();
        let token_info = match tokio::task::spawn_blocking(move || api::load_token(&name)).await {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                let _ = tx.send(UsageUpdate::Error(format!("{}: {e}", account.name)));
                continue;
            }
            Err(e) => {
                let _ = tx.send(UsageUpdate::Error(format!("join: {e}")));
                continue;
            }
        };

        match api::fetch_usage(&token_info.access_token).await {
            Ok(data) => {
                let _ = tx.send(UsageUpdate::Data {
                    account_name: account.name.clone(),
                    data,
                });
            }
            Err(e) => {
                let err_msg = format!("{e}");
                // Try token refresh on auth errors
                if err_msg.contains("401") || err_msg.contains("403") {
                    if let Some(ref refresh_tok) = token_info.refresh_token {
                        match try_refresh_and_retry(
                            refresh_tok,
                            &token_info.raw_credential,
                            &account.name,
                        )
                        .await
                        {
                            Ok(data) => {
                                let _ = tx.send(UsageUpdate::Data {
                                    account_name: account.name.clone(),
                                    data,
                                });
                                continue;
                            }
                            Err(e2) => {
                                let _ = tx.send(UsageUpdate::Error(format!(
                                    "{}: refresh failed: {e2}",
                                    account.name
                                )));
                                continue;
                            }
                        }
                    }
                }
                let _ = tx.send(UsageUpdate::Error(format!("{}: {e}", account.name)));
            }
        }
    }
}

async fn try_refresh_and_retry(
    refresh_token: &str,
    raw_credential: &str,
    account_name: &str,
) -> crate::error::Result<api::UsageData> {
    let refreshed = api::refresh_access_token(refresh_token).await?;

    let new_cred = api::update_credential_json(
        raw_credential,
        &refreshed.access_token,
        refreshed.refresh_token.as_deref(),
        refreshed.expires_at,
    );

    // Persist refreshed token to keychain
    let name = account_name.to_string();
    let cred = new_cred;
    let _ = tokio::task::spawn_blocking(move || api::save_refreshed_token(&name, &cred)).await;

    // Retry with new token
    api::fetch_usage(&refreshed.access_token).await
}
