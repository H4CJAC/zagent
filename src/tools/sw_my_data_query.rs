//! Built-in tool: query teacher + school data via claw_query.
//!
//! Accepts an arbitrary natural-language question, scoped to the authenticated
//! teacher's identity **and** their school to prevent cross-org data access.

use super::sw_lesson_common::{
    ensure_skill_scripts, err_result, fetch_sw_user_data, require_str, require_sw_token,
    run_claw_query_cached,
};
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct SwMyDataQueryTool {
    workspace_dir: PathBuf,
    cache_ttl_secs: u64,
    cache_delay_ms: u64,
}

impl SwMyDataQueryTool {
    pub fn new(workspace_dir: PathBuf, cache_ttl_secs: u64, cache_delay_ms: u64) -> Self {
        Self {
            workspace_dir,
            cache_ttl_secs,
            cache_delay_ms,
        }
    }
}

#[async_trait]
impl Tool for SwMyDataQueryTool {
    fn name(&self) -> &str {
        "sw_my_data_query"
    }

    fn description(&self) -> &str {
        "查询当前教师及所在学校的教学相关数据（课堂报告、学生表现、作业批改、课程安排、学校教研等）。\
         只能查到与当前教师或其所在学校关联的数据，不允许越权查询无关数据。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "要查询的问题（自然语言），例如：\"我最近三节语文课的课堂报告\""
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "查询超时秒数（默认 360",
                    "default": 360
                },
                "sp_s_name": { "type": "string", "description": "前端显示的步骤名称，建议值：\"查询我的数据\"" },
                "sp_s_icon": { "type": "string", "description": "前端显示的步骤图标，固定值：\"icon-data-collect\"" }
            },
            "required": ["question", "sp_s_name", "sp_s_icon"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let raw_question = require_str(&args, "question")?;
        let timeout = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(360);

        let token = match require_sw_token() {
            Ok(t) => t,
            Err(e) => return Ok(e),
        };

        let scripts_dir = match ensure_skill_scripts(&self.workspace_dir) {
            Ok(d) => d,
            Err(e) => return Ok(err_result(format!("释放脚本失败: {e}"))),
        };

        let (teacher, school) = match fetch_sw_user_data(&token).await {
            Ok(data) => {
                let name = data
                    .get("realName")
                    .or_else(|| data.get("nickName"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let uid = data
                    .get("thirdUid")
                    .or_else(|| data.get("uid"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let unit = data.get("unitName").and_then(|v| v.as_str()).unwrap_or("");
                let unit_id = data.get("unitId").and_then(|v| v.as_str()).unwrap_or("");

                let t = if !name.is_empty() && !uid.is_empty() {
                    format!("教师{name}(uid:{uid})")
                } else if !name.is_empty() {
                    format!("教师{name}")
                } else {
                    "当前教师".into()
                };
                let s = if !unit.is_empty() && !unit_id.is_empty() {
                    format!("{unit}(uid:{unit_id})")
                } else if !unit.is_empty() {
                    unit.to_string()
                } else {
                    String::new()
                };
                (t, s)
            }
            Err(_) => ("当前教师".into(), String::new()),
        };

        let scope = if school.is_empty() {
            format!("{teacher}本人")
        } else {
            format!("{teacher}本人及其所在学校{school}")
        };
        let scoped_question = format!("以下查询仅限{scope}的数据：{raw_question}");

        let work_dir = self.workspace_dir.join(".local/sw-query-tmp");
        std::fs::create_dir_all(&work_dir)?;

        let answer = run_claw_query_cached(
            &scripts_dir,
            &token,
            &scoped_question,
            &work_dir,
            timeout,
            &self.workspace_dir,
            "my_data",
            self.cache_ttl_secs,
            self.cache_delay_ms,
        )
        .await?;

        Ok(ToolResult {
            success: true,
            output: answer,
            error: None,
        })
    }
}
