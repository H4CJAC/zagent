//! Built-in tool: generate a lesson plan from collected data via search + LLM.

use super::sw_lesson_common::{artifacts_dir, ensure_skill_scripts, run_script};
use super::traits::{Tool, ToolResult};
use crate::providers::{self, Provider, ProviderRuntimeOptions};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct SwLessonGenPlanTool {
    workspace_dir: PathBuf,
    provider_name: String,
    model: String,
    temperature: f64,
    api_key: Option<String>,
    runtime_options: ProviderRuntimeOptions,
}

impl SwLessonGenPlanTool {
    pub fn new(
        workspace_dir: PathBuf,
        provider_name: String,
        model: String,
        temperature: f64,
        api_key: Option<String>,
        runtime_options: ProviderRuntimeOptions,
    ) -> Self {
        Self {
            workspace_dir,
            provider_name,
            model,
            temperature,
            api_key,
            runtime_options,
        }
    }
}

#[async_trait]
impl Tool for SwLessonGenPlanTool {
    fn name(&self) -> &str {
        "sw_prepare_lesson_02_gen_plan"
    }

    fn description(&self) -> &str {
        "根据收集的备课数据生成完整教案。内部先进行资料搜索，再调用 LLM 生成教案正文，\
         写入 artifacts 目录。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "工具 1 返回的会话 ID"
                },
                "extra_requirements": {
                    "type": "string",
                    "description": "额外教学要求（可选）"
                }
            },
            "required": ["session_id"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let session_id = match args.get("session_id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            _ => return Ok(err_result("缺少必填参数: session_id".into())),
        };
        let extra_req = args
            .get("extra_requirements")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let out_dir = artifacts_dir(&self.workspace_dir, session_id);
        let data_path = out_dir.join("data.json");
        if !data_path.exists() {
            return Ok(err_result(format!(
                "data.json 不存在，请先运行 sw_prepare_lesson_01_data_collect (session_id={session_id})"
            )));
        }

        let data: Value = serde_json::from_str(&std::fs::read_to_string(&data_path)?)?;
        let topic = data.get("topic").and_then(|v| v.as_str()).unwrap_or("");
        let subject = data.get("subject").and_then(|v| v.as_str()).unwrap_or("");
        let grade = data.get("grade").and_then(|v| v.as_str()).unwrap_or("");

        let scripts_dir = match ensure_skill_scripts(&self.workspace_dir) {
            Ok(d) => d,
            Err(e) => return Ok(err_result(format!("释放脚本失败: {e}"))),
        };

        // ── Phase A: research via web_search + knowledge_base_search ──
        let web_script = scripts_dir
            .join("lesson-plan-generator/scripts/web_search.py")
            .to_string_lossy()
            .to_string();
        let kb_script = scripts_dir
            .join("lesson-plan-generator/scripts/knowledge_base_search.py")
            .to_string_lossy()
            .to_string();

        let search_themes = vec![
            format!("{topic}教学方法"),
            format!("{topic}课堂活动设计"),
            format!("{topic}课程标准"),
            format!("{topic}教学重点难点"),
            format!("{topic}教材解析"),
        ];

        let mut web_results = Vec::new();
        let mut kb_results = Vec::new();

        for theme in &search_themes {
            let (stdout, _stderr, success) = run_script(
                "python3",
                &[&web_script, "--query", theme, "--theme", topic],
                &[],
                &out_dir,
                60,
            )
            .await?;
            if success {
                web_results.push(
                    serde_json::from_str::<Value>(&stdout).unwrap_or_else(|_| json!(stdout.trim())),
                );
            }
        }

        let (kb_stdout, _kb_stderr, kb_ok) = run_script(
            "python3",
            &[&kb_script, "--keyword", topic],
            &[],
            &out_dir,
            60,
        )
        .await?;
        if kb_ok {
            kb_results.push(
                serde_json::from_str::<Value>(&kb_stdout)
                    .unwrap_or_else(|_| json!(kb_stdout.trim())),
            );
        }

        let research = json!({
            "web_search": web_results,
            "knowledge_base": kb_results,
        });
        let research_path = out_dir.join("research.json");
        std::fs::write(&research_path, serde_json::to_string_pretty(&research)?)?;

        // ── Phase B: LLM lesson plan generation ──
        let template = include_str!(
            "../../assets/sw-skill-scripts/lesson-plan-generator/templates/lesson_plan_template.md"
        );

        let system_prompt = format!(
            "你是一位资深教学设计专家。请根据以下资料生成一份完整、高质量的教案。\n\
             要求：\n\
             - 严格按照教案模板结构输出\n\
             - 教案正文不少于 2000 字\n\
             - 将搜索资料中的高质量内容融入教案\n\
             - 所有模板占位符都必须填充实际内容\n\n\
             教案模板：\n```\n{template}\n```"
        );

        let data_summary = serde_json::to_string_pretty(&data)?;
        let research_summary = summarize_research(&research);

        let user_prompt = format!(
            "## 备课数据\n\n{data_summary}\n\n\
             ## 搜索资料摘要\n\n{research_summary}\n\n\
             {extra}\n\n\
             请生成主题为「{topic}」、学科「{subject}」、年级「{grade}」的完整教案。\
             直接输出教案正文，不需要额外解释。",
            extra = if extra_req.is_empty() {
                String::new()
            } else {
                format!("## 额外要求\n\n{extra_req}\n")
            }
        );

        let provider: Box<dyn Provider> = match providers::create_provider_with_options(
            &self.provider_name,
            self.api_key.as_deref(),
            &self.runtime_options,
        ) {
            Ok(p) => p,
            Err(e) => return Ok(err_result(format!("创建 LLM provider 失败: {e}"))),
        };

        let plan_text = match provider
            .chat_with_system(
                Some(&system_prompt),
                &user_prompt,
                &self.model,
                self.temperature,
            )
            .await
        {
            Ok(text) => text,
            Err(e) => return Ok(err_result(format!("LLM 教案生成失败: {e}"))),
        };

        let plan_path = out_dir.join("plan.md");
        std::fs::write(&plan_path, &plan_text)?;

        let task_id = format!("task-plan-{session_id}");
        let title = truncate_str(topic, 60);
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let plan_path_str = plan_path.display().to_string();

        let xml = format!(
            "<task>\n  <taskType>card</taskType>\n  <taskId>{task_id}</taskId>\n  <payload>\n    \
             <cardType>teaching-plan</cardType>\n    <title>{title}</title>\n    \
             <createdAt>{now}</createdAt>\n  </payload>\n</task>\n\
             <notice>\n  <noticeType>task-complete</noticeType>\n  <taskId>{task_id}</taskId>\n  \
             <payload>\n    <filePath>{plan_path_str}</filePath>\n  </payload>\n</notice>"
        );

        Ok(ToolResult {
            success: true,
            output: format!(
                "教案生成完成。session_id={session_id}\n文件: {plan_path_str}\n\n\
                 请将以下标签组原样输出给用户：\n{xml}"
            ),
            error: None,
        })
    }
}

fn summarize_research(research: &Value) -> String {
    use std::fmt::Write;
    let mut summary = String::new();

    if let Some(web_arr) = research.get("web_search").and_then(|v| v.as_array()) {
        for (i, item) in web_arr.iter().enumerate() {
            if let Some(results) = item.get("results").and_then(|v| v.as_array()) {
                for r in results.iter().take(3) {
                    let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let content = r.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    let _ = write!(summary, "### Web 搜索 #{} — {title}\n{content}\n\n", i + 1);
                }
            }
        }
    }

    if let Some(kb_arr) = research.get("knowledge_base").and_then(|v| v.as_array()) {
        for item in kb_arr {
            if let Some(result) = item.get("result") {
                let chapter = result
                    .get("chapterName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let content = result.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let preview = truncate_str(content, 1000);
                let _ = write!(summary, "### 知识库 — {chapter}\n{preview}\n\n");
            }
        }
    }

    if summary.is_empty() {
        summary.push_str("（无搜索结果）");
    }
    summary
}

/// Truncate a string to at most `max_bytes` while respecting UTF-8 char boundaries.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn err_result(msg: String) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(msg),
    }
}
