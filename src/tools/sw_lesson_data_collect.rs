//! Built-in tool: collect preparatory data for lesson planning and courseware.

use super::sw_lesson_common::{artifacts_dir, ensure_skill_scripts, gen_session_id, run_script};
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
        "收集备课所需数据：课程大纲、学生弱项、课堂报告、作业错题等。\
         输出结构化 JSON 到 artifacts 目录，供后续教案和课件生成使用。"
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
                "sp_s_icon": { "type": "string", "description": "前端显示的步骤图标，固定值：\"icon-data-collect\"" }
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

        let scripts_dir = match ensure_skill_scripts(&self.workspace_dir) {
            Ok(d) => d,
            Err(e) => return Ok(err_result(format!("释放脚本失败: {e}"))),
        };

        let out_dir = artifacts_dir(&self.workspace_dir, &session_id);
        std::fs::create_dir_all(&out_dir)?;

        let token = crate::gateway::sw_state::get_sw_token().unwrap_or_default();
        if token.is_empty() {
            return Ok(err_result(
                "Seewo token 未注入，请先通过 /api/user/sw_token 设置".into(),
            ));
        }

        let query_script = scripts_dir
            .join("claw-data-querying/scripts/claw_query.py")
            .to_string_lossy()
            .to_string();

        let queries = [
            format!("查询{subject}{grade}{topic}的课程大纲和课时安排"),
            format!("查询{grade}学生在{subject}的弱项和常见错误"),
            "查询最近的课堂报告中学生表现情况".to_string(),
            format!("查询最近批改作业中{subject}的错题情况"),
        ];

        let labels = [
            "curriculum",
            "student_weaknesses",
            "classroom_report",
            "homework_errors",
        ];

        let mut results: serde_json::Map<String, Value> = serde_json::Map::new();
        results.insert("session_id".into(), json!(session_id));
        results.insert("topic".into(), json!(topic));
        results.insert("subject".into(), json!(subject));
        results.insert("grade".into(), json!(grade));
        results.insert("region".into(), json!(region));
        results.insert("progress".into(), json!(progress));

        for (query, label) in queries.iter().zip(labels.iter()) {
            let (stdout, stderr, success) = run_script(
                "python3",
                &[
                    &query_script,
                    "--token",
                    &token,
                    "--question",
                    query,
                    "--no-stream",
                ],
                &[],
                &out_dir,
                120,
            )
            .await?;

            if success {
                let parsed: Value =
                    serde_json::from_str(&stdout).unwrap_or_else(|_| json!(stdout.trim()));
                let answer = parsed
                    .get("answer")
                    .and_then(|v| v.as_str())
                    .unwrap_or(stdout.trim());
                results.insert((*label).to_string(), json!(answer));
            } else {
                let msg = if stderr.is_empty() {
                    stdout.clone()
                } else {
                    stderr.clone()
                };
                results.insert(
                    (*label).to_string(),
                    json!(format!("[查询失败] {}", msg.trim())),
                );
            }
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

fn require_str(args: &Value, key: &str) -> anyhow::Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("缺少必填参数: {key}"))
}

fn opt_str(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn err_result(msg: String) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(msg),
    }
}
