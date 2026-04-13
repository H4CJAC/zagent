//! Built-in tool: collect preparatory data for lesson planning and courseware.
//!
//! Focuses on curriculum and scheduling data that is NOT covered by
//! `sw_classroom_observation` or `sw_student_analysis`.

use super::sw_lesson_common::{
    artifacts_dir, ensure_skill_scripts, err_result, gen_session_id, opt_str, require_str,
    require_sw_token, run_claw_query,
};
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct SwLessonDataCollectTool {
    workspace_dir: PathBuf,
}

impl SwLessonDataCollectTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for SwLessonDataCollectTool {
    fn name(&self) -> &str {
        "sw_prepare_lesson_01_data_collect"
    }

    fn description(&self) -> &str {
        "备课使用此工具前，要先使用sw_classroom_observation和sw_student_analysis工具收集课堂观察和学生表现分析数据，\
        然后再使用此工具收集备课剩余的所需数据：课程大纲、课时安排、作业错题等。\
        本工具会输出结构化 JSON 到 artifacts 目录，供后续教案和课件生成使用。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "topic":   { "type": "string", "description": "课程主题" },
                "subject": { "type": "string", "description": "学科" },
                "grade":   { "type": "string", "description": "学段/年级" },
                "region":  { "type": "string", "description": "地区（可选）" },
                "progress": { "type": "string", "description": "已教进度描述（可选）" },
                "session_id": { "type": "string", "description": "自定义会话 ID（可选，不填则自动生成 8 位 hex）" },
                "sp_s_name": { "type": "string", "description": "前端显示的步骤名称，建议值：\"准备备课数据\"" },
                "sp_s_icon": { "type": "string", "description": "前端显示的步骤图标，固定值：\"icon-data-collect\"" },
                "timeout_secs": { "type": "integer", "description": "每次查询的超时秒数（默认 360", "default": 360 }
            },
            "required": ["topic", "subject", "grade"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let topic = require_str(&args, "topic")?;
        let subject = require_str(&args, "subject")?;
        let grade = require_str(&args, "grade")?;
        let region = opt_str(&args, "region");
        let progress = opt_str(&args, "progress");

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

        let out_dir = artifacts_dir(&self.workspace_dir, &session_id);
        std::fs::create_dir_all(&out_dir)?;

        let queries = [
            (
                "curriculum",
                format!("查询{subject}{grade}{topic}的课程大纲和课时安排"),
            ),
            (
                "homework_errors",
                format!("查询最近批改作业中{subject}的错题情况"),
            ),
        ];

        let mut results: serde_json::Map<String, Value> = serde_json::Map::new();
        results.insert("session_id".into(), json!(session_id));
        results.insert("topic".into(), json!(topic));
        results.insert("subject".into(), json!(subject));
        results.insert("grade".into(), json!(grade));
        results.insert("region".into(), json!(region));
        results.insert("progress".into(), json!(progress));

        let timeout = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(360);

        for (label, question) in &queries {
            let answer = run_claw_query(&scripts_dir, &token, question, &out_dir, timeout).await?;
            results.insert((*label).to_string(), json!(answer));
        }

        let data = Value::Object(results);
        let data_path = out_dir.join("data.json");
        std::fs::write(&data_path, serde_json::to_string_pretty(&data)?)?;

        Ok(ToolResult {
            success: true,
            output: format!(
                "数据收集完成。session_id={session_id}\n文件: {}\n摘要: topic={topic}, subject={subject}, grade={grade}",
                data_path.display()
            ),
            error: None,
        })
    }
}
