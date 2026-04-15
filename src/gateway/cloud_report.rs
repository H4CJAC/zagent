//! Async cloud session reporting with retry for Seewo backend.

use std::time::Duration;

const MAX_RETRIES: u32 = 3;

/// POST JSON with cookie auth, retrying up to [`MAX_RETRIES`] times (exponential back-off).
///
/// All errors are logged and swallowed — callers fire-and-forget.
async fn post_with_retry(
    url: &str,
    cookie: &str,
    body: &serde_json::Value,
    label: &str,
    uid: &str,
) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    for attempt in 0..MAX_RETRIES {
        match client
            .post(url)
            .header("Cookie", cookie)
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!(uid, attempt, label, "cloud {label} succeeded");
                return;
            }
            Ok(resp) => {
                tracing::warn!(uid, attempt, status = %resp.status(), label, "cloud {label} non-success status");
            }
            Err(e) => {
                tracing::warn!(uid, attempt, error = %e, label, "cloud {label} failed");
            }
        }

        if attempt + 1 < MAX_RETRIES {
            tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
        }
    }

    tracing::error!(uid, label, "cloud {label} exhausted all retries");
}

/// Fire-and-forget session record to Seewo cloud.
pub async fn report_session(
    url: String,
    token: String,
    app_code: String,
    uid: String,
    name: String,
    description: String,
) {
    let cookie = format!("x-auth-token={token}; x-auth-app={app_code}");
    let body = serde_json::json!({
        "uid": uid,
        "name": name,
        "description": description,
    });
    post_with_retry(&url, &cookie, &body, "session_report", &uid).await;
}

/// Fire-and-forget session deletion from Seewo cloud.
pub async fn delete_session(url: String, token: String, app_code: String, uid: String) {
    let cookie = format!("x-auth-token={token}; x-auth-app={app_code}");
    let body = serde_json::json!({ "uid": uid });
    post_with_retry(&url, &cookie, &body, "session_delete", &uid).await;
}
