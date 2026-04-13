//! Built-in tool: fetch current Seewo user profile.
//!
//! Calls the Seewo user-info API using the injected `sw_token` and returns
//! structured profile data (name, school, subject, grade, uid, etc.).

use super::sw_lesson_common::{err_result, require_sw_token};
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};

const USER_INFO_URL: &str = "https://edu.seewo.com/api/v2/user/both/info";
const AUTH_APP: &str = "EasiNote5";

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

        let client = reqwest::Client::new();
        let resp = client
            .get(USER_INFO_URL)
            .header("accept", "*/*")
            .header(
                "Cookie",
                format!("x-auth-token={token}; x-auth-app={AUTH_APP};"),
            )
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => return Ok(err_result(format!("请求用户信息失败: {e}"))),
        };

        if !resp.status().is_success() {
            return Ok(err_result(format!("用户信息接口返回 {}", resp.status())));
        }

        let body: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => return Ok(err_result(format!("解析响应 JSON 失败: {e}"))),
        };

        let error_code = body
            .get("error_code")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        if error_code != 0 {
            let msg = body
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Ok(err_result(format!("用户信息接口错误: {msg}")));
        }

        let data = body.get("data").cloned().unwrap_or(json!({}));
        let pretty = serde_json::to_string_pretty(&data).unwrap_or_default();

        Ok(ToolResult {
            success: true,
            output: pretty,
            error: None,
        })
    }
}
