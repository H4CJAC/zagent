//! Built-in tool: student performance observation analysis.
//!
//! Queries student learning data via the agent-she SSE API.
//! Output can be consumed by lesson preparation, student feedback, or
//! learning diagnostics workflows.

use super::sw_lesson_common::{
    AgentSheConfig, analysis_artifacts_dir, err_result, fetch_user_meta, gen_session_id,
    require_str, require_sw_token, run_agent_she_query,
};
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct SwStudentAnalysisTool {
    workspace_dir: PathBuf,
    agent_she: AgentSheConfig,
}

impl SwStudentAnalysisTool {
    pub fn new(workspace_dir: PathBuf, agent_she: AgentSheConfig) -> Self {
        Self {
            workspace_dir,
            agent_she,
        }
    }
}

#[async_trait]
impl Tool for SwStudentAnalysisTool {
    fn name(&self) -> &str {
        "sw_student_analysis"
    }

    fn description(&self) -> &str {
        "学生表现观察分析：根据描述查询学生课堂参与度、作业完成情况和学习弱项等数据。\
         输出结构化 JSON，可用于备课参考、学生反馈、学情诊断等场景。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "要查询的内容描述（自然语言），例如：\"五年级数学最近学生参与度和作业错题分析\""
                },
                "session_id": { "type": "string", "description": "自定义会话 ID（可选，不填则自动生成 8 位 hex）" },
                "sp_s_name": { "type": "string", "description": "前端显示的步骤名称，建议值：\"学生表现观察分析\"" },
                "sp_s_icon": { "type": "string", "description": "前端显示的步骤图标，固定值：\"icon-course-stud-perf-analysis\"" },
                "timeout_secs": { "type": "integer", "description": "查询的超时秒数（默认 360）", "default": 360 }
            },
            "required": ["query", "sp_s_name", "sp_s_icon"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let query = require_str(&args, "query")?;

        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(gen_session_id);

        let token = match require_sw_token() {
            Ok(t) => t,
            Err(e) => return Ok(e),
        };

        let out_dir = analysis_artifacts_dir(&self.workspace_dir, &session_id);
        std::fs::create_dir_all(&out_dir)?;

        let meta = match fetch_user_meta(&token).await {
            Ok(m) => m,
            Err(e) => return Ok(err_result(format!("获取用户信息失败: {e}"))),
        };

        let timeout = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(360);

        let answer = match run_agent_she_query(
            &self.agent_she,
            &token,
            &query,
            &meta,
            "ktgc_question_answer_recommend",
            timeout,
        )
        .await
        {
            Ok(a) => a,
            Err(e) => return Ok(err_result(format!("数据查询失败: {e}"))),
        };

        let mut results = serde_json::Map::new();
        results.insert("session_id".into(), json!(session_id));
        results.insert("query".into(), json!(query));
        results.insert("analysis_data".into(), json!(answer));

        let data = Value::Object(results);
        let output_path = out_dir.join("student_analysis.json");
        std::fs::write(&output_path, serde_json::to_string_pretty(&data)?)?;

        Ok(ToolResult {
            success: true,
            output: format!(
                "学生表现观察分析完成。session_id={session_id}\n文件: {}",
                output_path.display()
            ),
            error: None,
        })
    }
}
