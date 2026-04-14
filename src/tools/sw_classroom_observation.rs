//! Built-in tool: classroom teaching observation analysis.
//!
//! Queries classroom teaching data via the agent-she SSE API.
//! Output can be consumed by lesson preparation, teaching reflection, or
//! instructional research workflows.

use super::sw_lesson_common::{
    AgentSheConfig, analysis_artifacts_dir, err_result, fetch_user_meta, gen_session_id, opt_str,
    require_str, require_sw_token, run_agent_she_query,
};
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct SwClassroomObservationTool {
    workspace_dir: PathBuf,
    agent_she: AgentSheConfig,
}

impl SwClassroomObservationTool {
    pub fn new(workspace_dir: PathBuf, agent_she: AgentSheConfig) -> Self {
        Self {
            workspace_dir,
            agent_she,
        }
    }
}

#[async_trait]
impl Tool for SwClassroomObservationTool {
    fn name(&self) -> &str {
        "sw_classroom_observation"
    }

    fn description(&self) -> &str {
        "课堂教学观察分析：查询最近的课堂教学报告、师生互动数据和课堂观察评估。\
         输出结构化 JSON，可用于备课参考、教学反思、教研分析等场景。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "subject": { "type": "string", "description": "学科" },
                "grade":   { "type": "string", "description": "学段/年级" },
                "topic":   { "type": "string", "description": "具体课程主题（可选，用于更精准查询）" },
                "session_id": { "type": "string", "description": "自定义会话 ID（可选，不填则自动生成 8 位 hex）" },
                "sp_s_name": { "type": "string", "description": "前端显示的步骤名称，建议值：\"课堂教学观察分析\"" },
                "sp_s_icon": { "type": "string", "description": "前端显示的步骤图标，固定值：\"icon-course-observ-analysis\"" },
                "timeout_secs": { "type": "integer", "description": "查询的超时秒数（默认 360）", "default": 360 }
            },
            "required": ["subject", "grade", "sp_s_name", "sp_s_icon"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let subject = require_str(&args, "subject")?;
        let grade = require_str(&args, "grade")?;
        let topic = opt_str(&args, "topic");

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

        let mut meta = match fetch_user_meta(&token).await {
            Ok(m) => m,
            Err(e) => return Ok(err_result(format!("获取用户信息失败: {e}"))),
        };
        meta.subject_name = subject.clone();

        let topic_clause = if topic.is_empty() {
            String::new()
        } else {
            format!("（{topic}）")
        };
        let question = format!(
            "查询最近{subject}{grade}{topic_clause}课堂教学报告中教师授课情况、师生互动数据和课堂观察评估"
        );

        let timeout = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(360);

        let answer = match run_agent_she_query(
            &self.agent_she,
            &token,
            &question,
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
        results.insert("subject".into(), json!(subject));
        results.insert("grade".into(), json!(grade));
        if !topic.is_empty() {
            results.insert("topic".into(), json!(topic));
        }
        results.insert("observation_data".into(), json!(answer));

        let data = Value::Object(results);
        let output_path = out_dir.join("classroom_observation.json");
        std::fs::write(&output_path, serde_json::to_string_pretty(&data)?)?;

        Ok(ToolResult {
            success: true,
            output: format!(
                "课堂教学观察分析完成。session_id={session_id}\n文件: {}\n学科={subject}, 年级={grade}",
                output_path.display()
            ),
            error: None,
        })
    }
}
