//! Built-in tool that exposes the runtime-injected Seewo token to the agent.

use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

pub struct SwTokenTool;

impl SwTokenTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for SwTokenTool {
    fn name(&self) -> &str {
        "get_sw_token"
    }

    fn description(&self) -> &str {
        "获取当前注入的 Seewo 认证 Token，用于调用希沃云端 API"
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": [],
        })
    }

    async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
        match crate::gateway::sw_state::get_sw_token() {
            Some(token) => Ok(ToolResult {
                success: true,
                output: token,
                error: None,
            }),
            None => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("No Seewo token has been injected yet".into()),
            }),
        }
    }
}
