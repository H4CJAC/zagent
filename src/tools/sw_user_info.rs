//! Built-in tool: fetch current Seewo user profile.
//!
//! Calls the Seewo user-info API using the injected `sw_token` and returns
//! structured profile data (name, school, subject, grade, uid, etc.).

use super::sw_lesson_common::{err_result, fetch_sw_user_data, require_sw_token};
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};

pub struct SwUserInfoTool;

impl SwUserInfoTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for SwUserInfoTool {
    fn name(&self) -> &str {
        "sw_get_user_info"
    }

    fn description(&self) -> &str {
        "获取当前 Seewo 用户信息（姓名、学校、学科、学段、uid 等）。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "sp_s_name": { "type": "string", "description": "前端显示的步骤名称，建议值：\"获取用户信息\"" },
                "sp_s_icon": { "type": "string", "description": "前端显示的步骤图标，固定值：\"icon-user-info\"" }
            }
        })
    }

    async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
        let token = match require_sw_token() {
            Ok(t) => t,
            Err(e) => return Ok(e),
        };

        let data = match fetch_sw_user_data(&token).await {
            Ok(d) => d,
            Err(msg) => return Ok(err_result(msg)),
        };

        let pretty = serde_json::to_string_pretty(&data).unwrap_or_default();

        Ok(ToolResult {
            success: true,
            output: pretty,
            error: None,
        })
    }
}
