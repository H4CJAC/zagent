//! Async cloud session reporting with retry for Seewo backend.

use std::time::Duration;

const MAX_RETRIES: u32 = 3;

/// Fire-and-forget session report to Seewo cloud.
///
/// Retries up to 3 times with exponential back-off (1s / 2s / 4s).
/// All errors are logged and swallowed — this must never block or panic.
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

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    for attempt in 0..MAX_RETRIES {
        match client
            .post(&url)
            .header("Cookie", &cookie)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!(uid = %uid, attempt, "cloud session report succeeded");
                return;
            }
            Ok(resp) => {
                tracing::warn!(
                    uid = %uid,
                    attempt,
                    status = %resp.status(),
                    "cloud session report non-success status"
                );
            }
            Err(e) => {
                tracing::warn!(uid = %uid, attempt, error = %e, "cloud session report failed");
            }
        }

        if attempt + 1 < MAX_RETRIES {
            tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
        }
    }

    tracing::error!(uid = %uid, "cloud session report exhausted all retries");
}
