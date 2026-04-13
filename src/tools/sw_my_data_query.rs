//! Built-in tool: query the current teacher's own data via claw_query.
//!
//! Accepts an arbitrary natural-language question, but enforces that the query
//! is scoped to the authenticated teacher's identity (name + uid) to prevent
//! cross-user data access.

use super::sw_lesson_common::{
    ensure_skill_scripts, err_result, fetch_teacher_identity, require_str, require_sw_token,
    run_claw_query,
};
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct SwMyDataQueryTool {
    workspace_dir: PathBuf,
}

impl SwMyDataQueryTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for SwMyDataQueryTool {
    fn name(&self) -> &str {
        "sw_my_data_query"
    }

    fn description(&self) -> &str {
        "查询当前教师自身的教学相关数据（课堂报告、学生表现、作业批改、课程安排等）。\
         只能查到与当前教师关联的数据，不允许越权查询其他教师或无关数据。"
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
            "required": ["question"]
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

        let teacher = fetch_teacher_identity(&token).await;
        let scoped_question = format!("以下查询仅限{teacher}本人的数据：{raw_question}");

        let work_dir = self.workspace_dir.join(".local/sw-query-tmp");
        std::fs::create_dir_all(&work_dir)?;

        let answer =
            run_claw_query(&scripts_dir, &token, &scoped_question, &work_dir, timeout).await?;

        Ok(ToolResult {
            success: true,
            output: answer,
            error: None,
        })
    }
}
