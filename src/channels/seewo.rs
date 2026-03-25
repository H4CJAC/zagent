use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;

/// Seewo Enterprise IM channel.
///
/// Skeleton implementation — fill in the API integration when the Seewo IM
/// SDK/webhook documentation is available.
pub struct SeewoChannel {
    api_base: String,
    app_key: String,
    app_secret: String,
    allowed_users: Vec<String>,
}

impl SeewoChannel {
    pub fn new(
        api_base: String,
        app_key: String,
        app_secret: String,
        allowed_users: Vec<String>,
    ) -> Self {
        Self {
            api_base,
            app_key,
            app_secret,
            allowed_users,
        }
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.seewo")
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }
}

#[async_trait]
impl Channel for SeewoChannel {
    fn name(&self) -> &str {
        "seewo"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // TODO: implement outbound message via Seewo IM API
        let _ = (&self.api_base, &self.app_key, &self.app_secret);
        let _ = self.http_client();
        tracing::warn!(
            "Seewo: send not yet implemented (recipient={}, len={})",
            message.recipient,
            message.content.len()
        );
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // TODO: implement inbound message polling / webhook / WebSocket
        tracing::info!("Seewo: channel ready (not yet implemented, awaiting shutdown)");
        tx.closed().await;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        // TODO: ping Seewo API to verify connectivity
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> SeewoChannel {
        SeewoChannel::new(
            "https://im-api.seewo.com".into(),
            "test-key".into(),
            "test-secret".into(),
            vec![],
        )
    }

    #[test]
    fn name_returns_seewo() {
        assert_eq!(make_channel().name(), "seewo");
    }

    #[test]
    fn user_allowed_wildcard() {
        let ch = SeewoChannel::new(
            "https://im-api.seewo.com".into(),
            "k".into(),
            "s".into(),
            vec!["*".into()],
        );
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn user_allowed_specific() {
        let ch = SeewoChannel::new(
            "https://im-api.seewo.com".into(),
            "k".into(),
            "s".into(),
            vec!["user123".into()],
        );
        assert!(ch.is_user_allowed("user123"));
        assert!(!ch.is_user_allowed("other"));
    }

    #[test]
    fn user_denied_empty() {
        assert!(!make_channel().is_user_allowed("anyone"));
    }

    #[test]
    fn config_serde() {
        let toml_str = r#"
api_base = "https://im-api.seewo.com"
app_key = "my-key"
app_secret = "my-secret"
allowed_users = ["user1", "*"]
"#;
        let config: crate::config::schema::SeewoConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.app_key, "my-key");
        assert_eq!(config.allowed_users, vec!["user1", "*"]);
    }

    #[test]
    fn config_serde_defaults() {
        let toml_str = r#"
app_key = "k"
app_secret = "s"
"#;
        let config: crate::config::schema::SeewoConfig = toml::from_str(toml_str).unwrap();
        assert!(config.allowed_users.is_empty());
        assert_eq!(config.api_base, "https://im-api.seewo.com");
    }
}
