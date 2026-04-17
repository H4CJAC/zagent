//! Built-in tool: collect preparatory data for lesson planning and courseware.
//!
//! Accepts a free-form lesson preparation description and delegates to the
//! agent-she API, which infers the topic, subject, grade, region, progress,
//! curriculum outline, and recent homework errors.

use super::sw_lesson_common::{
    AgentSheConfig, LlmProviderConfig, artifacts_dir, err_result, extract_structured_fields,
    fetch_user_meta, gen_session_id, require_str, require_sw_token, run_agent_she_query_cached,
};
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct SwLessonDataCollectTool {
    workspace_dir: PathBuf,
    agent_she: AgentSheConfig,
    llm: Option<LlmProviderConfig>,
}

impl SwLessonDataCollectTool {
    pub fn new(
        workspace_dir: PathBuf,
        agent_she: AgentSheConfig,
        llm: Option<LlmProviderConfig>,
    ) -> Self {
        Self {
            workspace_dir,
            agent_she,
            llm,
        }
    }
}

#[async_trait]
impl Tool for SwLessonDataCollectTool {
    fn name(&self) -> &str {
        "sw_prepare_lesson_01_data_collect"
    }

    fn description(&self) -> &str {
        "（调用此工具前，必须先要用 sw_teaching_reflection 和 sw_student_feedback 工具生成教学反思和学生反馈。）\
        备课数据收集：根据备课需求描述，查询接下来要准备的课程主题、学科、学段/年级、地区、\
         已教进度、课程大纲课时安排和最近批改作业错题情况等信息。\
         输出结构化 JSON 到 artifacts 目录，供后续教案和课件生成使用。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "备课需求描述（自然语言），例如：\"我要准备五年级数学下册《分数的加减法》的备课资料\""
                },
                "session_id": { "type": "string", "description": "自定义会话 ID（可选，不填则自动生成 8 位 hex）" },
                "sp_s_name": { "type": "string", "description": "前端显示的步骤名称，建议值：\"准备备课数据\"" },
                "sp_s_icon": { "type": "string", "description": "前端显示的步骤图标，固定值：\"icon-data-collect\"" },
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

        let out_dir = artifacts_dir(&self.workspace_dir, &session_id);
        std::fs::create_dir_all(&out_dir)?;

        let meta = match fetch_user_meta(&token).await {
            Ok(m) => m,
            Err(e) => return Ok(err_result(format!("获取用户信息失败: {e}"))),
        };

        let timeout = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(360);

        let full_query = format!(
            "1. {query}\n\
             2. 并推理接下来要准备的课程主题、学科、学段/年级、地区、已教进度、课程大纲课时安排和最近批改作业错题情况\n\
             3. 请在回答末尾以如下格式输出关键信息：\n\
             【主题】xxx【学科】xxx【年级】xxx【课时时长】xxx分钟"
        );

        let answer = match run_agent_she_query_cached(
            &self.agent_she,
            &token,
            &full_query,
            &meta,
            "ktgc_question_answer_recommend",
            timeout,
            &self.workspace_dir,
            "data_collect",
            self.agent_she.cache_ttl_secs,
            self.agent_she.cache_delay_ms,
            self.llm.as_ref(),
        )
        .await
        {
            Ok(a) => a,
            Err(e) => return Ok(err_result(format!("数据查询失败: {e}"))),
        };

        let mut results = serde_json::Map::new();
        results.insert("session_id".into(), json!(session_id));
        results.insert("query".into(), json!(query));
        results.insert("collected_data".into(), json!(answer.clone()));

        if let Some(ref llm) = self.llm {
            if let Some(fields) = extract_structured_fields(llm, &query, &answer).await {
                for (k, v) in fields {
                    if let Some(s) = v.as_str() {
                        if !s.is_empty() {
                            results.insert(k, v);
                        }
                    }
                }
            }
        }

        let data = Value::Object(results);
        let data_path = out_dir.join("data.json");
        std::fs::write(&data_path, serde_json::to_string_pretty(&data)?)?;

        Ok(ToolResult {
            success: true,
            output: format!(
                "数据收集完成。session_id={session_id}\n文件: {}",
                data_path.display()
            ),
            error: None,
        })
    }
}
