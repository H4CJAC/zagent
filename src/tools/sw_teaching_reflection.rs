//! Built-in tool: generate a data-driven teaching reflection via LLM.
//!
//! Reads classroom observation and student analysis data produced by
//! `sw_classroom_observation` / `sw_student_analysis`, then calls an LLM
//! to generate a structured teaching reflection document.

use super::sw_lesson_common::{
    LlmProviderConfig, analysis_artifacts_dir, cache_key, cached_query, err_result, opt_str,
    require_str, truncate_str,
};
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct SwTeachingReflectionTool {
    workspace_dir: PathBuf,
    llm: LlmProviderConfig,
    cache_ttl_secs: u64,
    cache_delay_ms: u64,
}

impl SwTeachingReflectionTool {
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
impl Tool for SwTeachingReflectionTool {
    fn name(&self) -> &str {
        "sw_teaching_reflection"
    }

    fn description(&self) -> &str {
        "基于课堂教学观察和学生表现分析数据，生成专业的、数据驱动的教学反思文档。\
         需要先执行 sw_classroom_observation 和 sw_student_analysis 获取数据。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "观察分析的会话 ID（来自 sw_classroom_observation / sw_student_analysis）"
                },
                "subject": {
                    "type": "string",
                    "description": "学科（用于对齐学科核心素养维度），可以先从 sw_get_user_info 工具获取"
                },
                "topic": {
                    "type": "string",
                    "description": "课程主题（可选）"
                },
                "extra_requirements": {
                    "type": "string",
                    "description": "额外反思要求（可选）"
                },
                "sp_s_name": { "type": "string", "description": "前端显示的步骤名称，建议值：\"生成教学反思\"" },
                "sp_s_icon": { "type": "string", "description": "前端显示的步骤图标，固定值：\"icon-gen-markdown\"" }
            },
            "required": ["session_id", "subject", "sp_s_name", "sp_s_icon"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let session_id = require_str(&args, "session_id")?;
        let subject = require_str(&args, "subject")?;
        let topic = opt_str(&args, "topic");
        let extra_req = opt_str(&args, "extra_requirements");

        let analysis_dir = analysis_artifacts_dir(&self.workspace_dir, &session_id);

        let classroom_path = analysis_dir.join("classroom_observation.json");
        let student_path = analysis_dir.join("student_analysis.json");

        if !classroom_path.exists() && !student_path.exists() {
            return Ok(err_result(format!(
                "未找到分析数据文件。请先执行 sw_classroom_observation 和 sw_student_analysis \
                 (session_id={session_id})"
            )));
        }

        let classroom_data = if classroom_path.exists() {
            std::fs::read_to_string(&classroom_path).unwrap_or_default()
        } else {
            String::new()
        };
        let student_data = if student_path.exists() {
            std::fs::read_to_string(&student_path).unwrap_or_default()
        } else {
            String::new()
        };

        let system_prompt = format!(
            "你是一位资深教育教学反思专家。请根据以下课堂观察和学生表现数据，\
             生成一篇专业的、数据驱动的教学反思文档。\n\n\
             要求：\n\
             - 学科：{subject}，需要对齐该学科的新课标核心素养维度\n\
             - 采用「回顾—诊断—重构」框架\n\
             - 每个观点都必须用具体数据支撑\n\
             - 语气专业、客观、深入\n\
             - 不要简单罗列数据，要解读数据对教学和学生学习的意义\n\
             - 反思正文不少于 1500 字\n\n\
             输出结构：\n\
             1. **课时基本信息概览** — 课程信息、综合评价、核心素养目标\n\
             2. **核心素养的达成情况与数据诊断** — 对照学科核心素养逐项分析\n\
             3. **教学设计与实施过程反思** — 亮点（得意之作）与高光时刻\n\
             4. **存在的问题与深度剖析** — 数据揭示的痛点\n\
             5. **素养导向的重构策略与行动规划** — 具体可执行的改进方案\n\
             6. **核心提炼** — 一句话总结"
        );

        let mut user_prompt = String::new();
        if !classroom_data.is_empty() {
            user_prompt.push_str("## 课堂教学观察数据\n\n");
            user_prompt.push_str(truncate_str(&classroom_data, 8000));
            user_prompt.push_str("\n\n");
        }
        if !student_data.is_empty() {
            user_prompt.push_str("## 学生表现分析数据\n\n");
            user_prompt.push_str(truncate_str(&student_data, 8000));
            user_prompt.push_str("\n\n");
        }
        if !topic.is_empty() {
            use std::fmt::Write;
            let _ = write!(user_prompt, "## 课程主题\n\n{topic}\n\n");
        }
        {
            use std::fmt::Write;
            if !extra_req.is_empty() {
                let _ = write!(user_prompt, "## 额外要求\n\n{extra_req}\n\n");
            }
            let _ = write!(
                user_prompt,
                "请为{subject}学科生成一篇完整的教学反思。直接输出反思正文，不需要额外解释。"
            );
        }

        let key = cache_key(&[&session_id, &subject, &topic, &extra_req, &classroom_data, &student_data]);
        let query_text = format!("{subject} {topic} 教学反思");
        let llm = self.llm.clone();
        let sys_prompt = system_prompt.clone();

        let reflection_text = match cached_query(
            &self.workspace_dir,
            "llm_feedback/teaching_reflection",
            &key,
            &query_text,
            self.cache_ttl_secs,
            self.cache_delay_ms,
            Some(&self.llm),
            || async {
                let provider = llm.create_provider()
                    .map_err(|e| anyhow::anyhow!("创建 LLM provider 失败: {e}"))?;
                provider
                    .chat_with_system(Some(&sys_prompt), &user_prompt, &llm.model, llm.temperature)
                    .await
            },
        )
        .await
        {
            Ok(text) => text,
            Err(e) => return Ok(err_result(format!("LLM 教学反思生成失败: {e}"))),
        };

        let reflection_path = analysis_dir.join("reflection.md");
        std::fs::create_dir_all(&analysis_dir)?;
        std::fs::write(&reflection_path, &reflection_text)?;

        let title_src = if topic.is_empty() { &subject } else { &topic };
        let task_id = format!("task-reflection-{session_id}");
        let title = truncate_str(title_src, 60);
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let file_path = reflection_path.display().to_string();

        let xml = format!(
            "<task>\n  <taskType>card</taskType>\n  <taskId>{task_id}</taskId>\n  <payload>\n    \
             <cardType>markdown</cardType>\n    <title>{title}</title>\n    \
             <createdAt>{now}</createdAt>\n  </payload>\n</task>\n\
             <notice>\n  <noticeType>task-complete</noticeType>\n  <taskId>{task_id}</taskId>\n  \
             <payload>\n    <filePath>{file_path}</filePath>\n  </payload>\n</notice>"
        );

        Ok(ToolResult {
            success: true,
            output: format!(
                "教学反思生成完成。session_id={session_id}\n文件: {file_path}\n\n\
                 请将以下task和notice标签组原样输出给用户：\n{xml}"
            ),
            error: None,
        })
    }
}
