//! Built-in tool: student performance observation analysis.
//!
//! A general-purpose tool that queries student learning data via claw_query.
//! Output can be consumed by lesson preparation, student feedback, or
//! learning diagnostics workflows.

use super::sw_lesson_common::{
    analysis_artifacts_dir, ensure_skill_scripts, err_result, fetch_teacher_identity,
    gen_session_id, opt_str, require_str, require_sw_token, run_claw_query,
};
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct SwStudentAnalysisTool {
    workspace_dir: PathBuf,
}

impl SwStudentAnalysisTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for SwStudentAnalysisTool {
    fn name(&self) -> &str {
        "sw_student_analysis"
    }

    fn description(&self) -> &str {
        "学生表现观察分析：查询最近的学生课堂参与度、作业完成情况和学习弱项等数据。\
         输出结构化 JSON，可用于备课参考、学生反馈、学情诊断等场景。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "subject": { "type": "string", "description": "学科" },
                "grade":   { "type": "string", "description": "学段/年级" },
                "topic":   { "type": "string", "description": "具体课程主题（可选，用于更精准查询）" },
                "session_id": { "type": "string", "description": "自定义会话 ID（可选，不填则自动生成 8 位 hex）" },
                "sp_s_name": { "type": "string", "description": "前端显示的步骤名称，建议值：\"学生表现观察分析\"" },
                "sp_s_icon": { "type": "string", "description": "前端显示的步骤图标，固定值：\"icon-course-stud-perf-analysis\"" },
                "timeout_secs": { "type": "integer", "description": "每次查询的超时秒数（默认 360）", "default": 360 }
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

        let scripts_dir = match ensure_skill_scripts(&self.workspace_dir) {
            Ok(d) => d,
            Err(e) => return Ok(err_result(format!("释放脚本失败: {e}"))),
        };

        let out_dir = analysis_artifacts_dir(&self.workspace_dir, &session_id);
        std::fs::create_dir_all(&out_dir)?;

        let teacher = fetch_teacher_identity(&token).await;

        let topic_clause = if topic.is_empty() {
            String::new()
        } else {
            format!("（{topic}相关）")
        };

        let queries = [
            (
                "participation",
                format!(
                    "查询{teacher}最近{subject}{grade}课堂中学生参与度和表现数据{topic_clause}"
                ),
            ),
            (
                "homework_analysis",
                format!(
                    "查询{teacher}所教{grade}学生{subject}作业完成情况和错题分析{topic_clause}"
                ),
            ),
            (
                "learning_weaknesses",
                format!(
                    "查询{teacher}所教{grade}学生在{subject}的学习弱项和常见错误{topic_clause}"
                ),
            ),
        ];

        let mut results: serde_json::Map<String, Value> = serde_json::Map::new();
        results.insert("session_id".into(), json!(session_id));
        results.insert("subject".into(), json!(subject));
        results.insert("grade".into(), json!(grade));
        if !topic.is_empty() {
            results.insert("topic".into(), json!(topic));
        }

        let timeout = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(360);

        for (label, question) in &queries {
            let answer = run_claw_query(&scripts_dir, &token, question, &out_dir, timeout).await?;
            results.insert((*label).to_string(), json!(answer));
        }

        let data = Value::Object(results);
        let output_path = out_dir.join("student_analysis.json");
        std::fs::write(&output_path, serde_json::to_string_pretty(&data)?)?;

        Ok(ToolResult {
            success: true,
            output: format!(
                "学生表现观察分析完成。session_id={session_id}\n文件: {}\n学科={subject}, 年级={grade}",
                output_path.display()
            ),
            error: None,
        })
    }
}
