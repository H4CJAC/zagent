//! Built-in tool: generate a lesson plan from collected data via search + LLM.

use super::sw_lesson_common::{
    LlmProviderConfig, artifacts_dir, cache_key, cached_query, ensure_skill_scripts, err_result,
    run_script, truncate_str,
};
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct SwLessonGenPlanTool {
    workspace_dir: PathBuf,
    llm: LlmProviderConfig,
    cache_ttl_secs: u64,
    cache_delay_ms: u64,
}

impl SwLessonGenPlanTool {
    pub fn new(
        workspace_dir: PathBuf,
        llm: LlmProviderConfig,
        cache_ttl_secs: u64,
        cache_delay_ms: u64,
    ) -> Self {
        Self {
            workspace_dir,
            llm,
            cache_ttl_secs,
            cache_delay_ms,
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
                },
                "sp_s_name": { "type": "string", "description": "前端显示的步骤名称，建议值：\"生成教案-{生成的教案名}\"" },
                "sp_s_icon": { "type": "string", "description": "前端显示的步骤图标，固定值：\"icon-gen-markdown\"" },
                "script_timeout_secs": { "type": "integer", "description": "外部脚本（web_search / kb_search）单次超时秒数（默认 180）", "default": 180 }
            },
            "required": ["session_id", "sp_s_name", "sp_s_icon"]
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
        let script_timeout = args
            .get("script_timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(180);

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

        let cache_ttl = self.cache_ttl_secs;
        let cache_delay = self.cache_delay_ms;
        let ws_dir = &self.workspace_dir;

        for theme in &search_themes {
            let key = cache_key(&[theme]);
            let web_script = web_script.clone();
            let topic_owned = topic.to_string();
            let theme_query = theme.clone();
            let theme_for_closure = theme.clone();
            let out_dir_ref = out_dir.clone();
            let llm_ref = &self.llm;
            let result = cached_query(ws_dir, "web_search/lesson_plan", &key, &theme_query, cache_ttl, cache_delay, Some(llm_ref), || async move {
                let (stdout, _stderr, success) = run_script(
                    "python3",
                    &[&web_script, "--query", &theme_for_closure, "--theme", &topic_owned],
                    &[],
                    &out_dir_ref,
                    script_timeout,
                )
                .await?;
                if success {
                    Ok(stdout)
                } else {
                    anyhow::bail!("web_search script failed for theme: {theme_for_closure}")
                }
            })
            .await;
            if let Ok(stdout) = result {
                web_results.push(
                    serde_json::from_str::<Value>(&stdout).unwrap_or_else(|_| json!(stdout.trim())),
                );
            }
        }

        {
            let key = cache_key(&[topic]);
            let kb_script = kb_script.clone();
            let topic_owned = topic.to_string();
            let out_dir_ref = out_dir.clone();
            let llm_ref = &self.llm;
            let result = cached_query(ws_dir, "web_search/lesson_plan_kb", &key, topic, cache_ttl, cache_delay, Some(llm_ref), || async move {
                let (stdout, _stderr, success) = run_script(
                    "python3",
                    &[&kb_script, "--keyword", &topic_owned],
                    &[],
                    &out_dir_ref,
                    script_timeout,
                )
                .await?;
                if success {
                    Ok(stdout)
                } else {
                    anyhow::bail!("kb_search script failed for topic: {topic_owned}")
                }
            })
            .await;
            if let Ok(stdout) = result {
                kb_results.push(
                    serde_json::from_str::<Value>(&stdout)
                        .unwrap_or_else(|_| json!(stdout.trim())),
                );
            }
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

        let provider = match self.llm.create_provider() {
            Ok(p) => p,
            Err(e) => return Ok(err_result(format!("创建 LLM provider 失败: {e}"))),
        };

        let plan_text = match provider
            .chat_with_system(
                Some(&system_prompt),
                &user_prompt,
                &self.llm.model,
                self.llm.temperature,
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
             <cardType>markdown</cardType>\n    <title>{title}</title>\n    \
             <createdAt>{now}</createdAt>\n  </payload>\n</task>\n\
             <notice>\n  <noticeType>task-complete</noticeType>\n  <taskId>{task_id}</taskId>\n  \
             <payload>\n    <filePath>{plan_path_str}</filePath>\n  </payload>\n</notice>"
        );

        Ok(ToolResult {
            success: true,
            output: format!(
                "教案生成完成。session_id={session_id}\n文件: {plan_path_str}\n\n\
                 请将以下task和notice标签组原样输出给用户：\n{xml}"
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
