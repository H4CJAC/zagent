//! Built-in tool: generate courseware using the embedded create-courseware scripts.

use super::sw_lesson_common::{artifacts_dir, ensure_skill_scripts, run_script};
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct SwLessonGenCwTool {
    workspace_dir: PathBuf,
}

impl SwLessonGenCwTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for SwLessonGenCwTool {
    fn name(&self) -> &str {
        "sw_prepare_lesson_03_gen_cw"
    }

    fn description(&self) -> &str {
        "根据备课数据和教案生成课件。通过浏览器自动化调用希沃课件生成服务，\
         分两阶段（草稿 + 最终结果）完成，输出 XML 交付信息和课件文件。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "会话 ID（来自工具 1）"
                },
                "topic": {
                    "type": "string",
                    "description": "课件主题"
                },
                "context_file": {
                    "type": "string",
                    "description": "额外上下文文件路径（可选）"
                },
                "sp_s_name": { "type": "string", "description": "前端显示的步骤名称，建议值：\"生成课件-{生成的课件名}\"" },
                "sp_s_icon": { "type": "string", "description": "前端显示的步骤图标，固定值：\"icon-gen-cw\"" }
            },
            "required": ["session_id", "topic"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let session_id = match args.get("session_id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            _ => return Ok(err_result("缺少必填参数: session_id".into())),
        };
        let topic = match args.get("topic").and_then(|v| v.as_str()) {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => return Ok(err_result("缺少必填参数: topic".into())),
        };
        let extra_context = args
            .get("context_file")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let out_dir = artifacts_dir(&self.workspace_dir, session_id);
        std::fs::create_dir_all(&out_dir)?;

        let scripts_dir = match ensure_skill_scripts(&self.workspace_dir) {
            Ok(d) => d,
            Err(e) => return Ok(err_result(format!("释放脚本失败: {e}"))),
        };

        let token = crate::gateway::sw_state::get_sw_token().unwrap_or_default();
        if token.is_empty() {
            return Ok(err_result(
                "Seewo token 未注入，请先通过 /api/user/sw_token 设置".into(),
            ));
        }

        // Determine context file: prefer plan.md if available
        let plan_path = out_dir.join("plan.md");
        let context_path = if !extra_context.is_empty() {
            extra_context.to_string()
        } else if plan_path.exists() {
            plan_path.to_string_lossy().to_string()
        } else {
            String::new()
        };

        let cw_output = out_dir.join("courseware.json");
        let cw_scripts = scripts_dir.join("create-courseware/scripts");
        let script1 = cw_scripts.join("run-generate-courseware.js");
        let script2 = cw_scripts.join("wait-courseware-result.js");

        // ── Phase 1: start + draft ──
        let mut envs: Vec<(&str, String)> = vec![
            ("SEEWO_CLAW_TOPIC", topic.clone()),
            ("SEEWO_CLAW_X_TOKEN", token),
            ("SEEWO_CLAW_OUTPUT", cw_output.to_string_lossy().to_string()),
            ("SEEWO_CLAW_SESSION", format!("claw-{session_id}")),
        ];
        if !context_path.is_empty() {
            envs.push(("SEEWO_CLAW_CONTEXT_FILE", context_path));
        }

        let env_refs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let script1_str = script1.to_string_lossy().to_string();

        let (stdout1, stderr1, ok1) =
            run_script("node", &[&script1_str], &env_refs, &cw_scripts, 300).await?;

        if !ok1 {
            let detail = if stderr1.is_empty() {
                &stdout1
            } else {
                &stderr1
            };
            return Ok(err_result(format!(
                "课件生成 Phase 1 失败: {}",
                detail.trim()
            )));
        }

        let cw_session = extract_field(&stderr1, "SESSION_NAME=")
            .unwrap_or_else(|| format!("claw-{session_id}"));
        let task_id =
            extract_field(&stderr1, "TASK_ID=").unwrap_or_else(|| format!("task-{session_id}"));
        let task_xml = extract_xml_block(&stdout1, "task");
        let piece_id = extract_xml_field(&stdout1, "pieceId");

        // ── Phase 2: wait for final result ──
        let script2_str = script2.to_string_lossy().to_string();
        let phase2_envs: Vec<(&str, String)> = vec![
            ("SEEWO_CLAW_SESSION", cw_session.clone()),
            ("SEEWO_CLAW_TASK_ID", task_id.clone()),
            ("SEEWO_CLAW_TOPIC", topic.clone()),
            ("SEEWO_CLAW_OUTPUT", cw_output.to_string_lossy().to_string()),
        ];
        let phase2_refs: Vec<(&str, &str)> =
            phase2_envs.iter().map(|(k, v)| (*k, v.as_str())).collect();

        let (stdout2, stderr2, ok2) =
            run_script("node", &[&script2_str], &phase2_refs, &cw_scripts, 960).await?;

        if !ok2 {
            let detail = if stderr2.is_empty() {
                &stdout2
            } else {
                &stderr2
            };
            return Ok(ToolResult {
                success: false,
                output: format!(
                    "Phase 1 成功但 Phase 2 失败。\ntask_id={task_id}\n{task_xml}\n\n错误: {}",
                    detail.trim()
                ),
                error: Some("课件生成 Phase 2 失败".into()),
            });
        }

        let notice_xml = extract_xml_block(&stdout2, "notice");

        use std::fmt::Write;
        let mut xml_block = String::new();
        if !task_xml.is_empty() {
            xml_block.push_str(&task_xml);
            xml_block.push('\n');
        }
        if !notice_xml.is_empty() {
            xml_block.push_str(&notice_xml);
        }

        let mut output = String::new();
        let _ = write!(
            output,
            "课件生成完成。session_id={session_id}\n\
             task_id={task_id}\npiece_id={piece_id}\n文件: {}\n\n\
             请将以下task和notice标签组原样输出给用户：\n{xml_block}",
            cw_output.display()
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

fn extract_field(text: &str, prefix: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("[seewo-claw] ") {
            if let Some(val) = rest.strip_prefix(prefix) {
                return Some(val.trim().to_string());
            }
        }
        if let Some(val) = trimmed.strip_prefix(prefix) {
            return Some(val.trim().to_string());
        }
    }
    None
}

fn extract_xml_block(text: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    if let Some(start) = text.find(&open) {
        if let Some(end) = text[start..].find(&close) {
            return text[start..start + end + close.len()].to_string();
        }
    }
    String::new()
}

fn extract_xml_field(text: &str, field: &str) -> String {
    let open = format!("<{field}>");
    let close = format!("</{field}>");
    if let Some(start) = text.find(&open) {
        let after = start + open.len();
        if let Some(end) = text[after..].find(&close) {
            return text[after..after + end].trim().to_string();
        }
    }
    String::new()
}

fn err_result(msg: String) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(msg),
    }
}
