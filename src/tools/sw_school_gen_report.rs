//! Built-in tool: generate school report materials via claw_query + LLM.
//!
//! Fetches school data through `claw_query`, then uses an LLM with
//! embedded writing guidelines to produce a structured report document.

use super::sw_lesson_common::{
    LlmProviderConfig, ensure_skill_scripts, err_result, fetch_sw_user_data, gen_session_id,
    opt_str, require_str, require_sw_token, run_claw_query_cached, truncate_str,
};
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct SwSchoolGenReportTool {
    workspace_dir: PathBuf,
    llm: LlmProviderConfig,
    cache_ttl_secs: u64,
    cache_delay_ms: u64,
}

impl SwSchoolGenReportTool {
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

const REPORT_SYSTEM_PROMPT: &str = "\
你是一位资深教育行业公文撰写专家。请根据提供的学校数据，撰写一篇专业的学校汇报材料。

## 整体结构（标准四段式框架）
1. **引言**：学校基本情况（教职工人数、班级、学生）、发展战略、总体成效概述
2. **主体内容**：
   - 建设/工作基本情况（硬件设施、资源建设、队伍建设）
   - 具体工作开展情况（分维度：管理、教学、培训、资源，每个维度包含措施+过程+结果）
   - 特色应用案例（典型案例介绍、应用场景、实际成效）
   - 存在的问题与不足（客观分析，避免空泛）
3. **未来发展思路与措施**：针对性改进方案、可操作措施、明确目标方向
4. **结语**：总结全文、表达展望和信心

## 段落组织技巧
- 采用「目标—措施—成果」三段式：先阐述目标，再展开措施，最后展示成果
- 或「现状—问题—解决方案」递进式：描述现状，分析问题，给出对策
- 或「功能—场景—成效」场景化结构：说明功能，描述应用场景，总结成效

## 数据支撑原则
- 使用具体数字而非模糊描述（例：「2024年上学期平板开课共计2730节」而非「教学成效显著」）
- 通过同比/环比展示发展趋势
- 从不同角度多维呈现数据

## 内容充实要求
- 每个观点须有具体数据或案例支撑
- 逻辑衔接自然流畅，使用因果/递进/转折/并列等衔接词
- 恰当使用教育术语、技术术语和政策术语

## 常见误区规避
- 避免空洞：不要出现「取得了显著成效」等无数据支撑的表述
- 避免逻辑混乱：每段聚焦一个核心内容
- 避免冗长：紧扣主题，精炼有力
- 避免枯燥：在保持专业性的同时，善用案例增强可读性

## 输出要求
- 直接输出汇报正文，不需要额外解释
- 使用 Markdown 格式
- 如果提供的数据中缺少某些信息，在对应位置标注 [待补充]";

#[async_trait]
impl Tool for SwSchoolGenReportTool {
    fn name(&self) -> &str {
        "sw_school_gen_report"
    }

    fn description(&self) -> &str {
        "学校汇报材料生成：根据主题自动查询学校数据并生成结构化、专业化的汇报文档。\
         适用于教育信息化建设成果汇报、工作总结报告、教育类正式公文材料等场景。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "topic": {
                    "type": "string",
                    "description": "汇报主题，例如：\"教育信息化建设成果\"、\"智慧课堂应用总结\""
                },
                "school_name": {
                    "type": "string",
                    "description": "单位名称（可选，不填则从用户信息自动获取）"
                },
                "extra_data": {
                    "type": "string",
                    "description": "用户补充的额外数据/素材（可选）"
                },
                "extra_requirements": {
                    "type": "string",
                    "description": "额外写作要求（可选）"
                },
                "length": {
                    "type": "string",
                    "enum": ["short", "medium", "long"],
                    "description": "篇幅要求：short(1500字左右)/medium(3000字左右)/long(5000字以上)，默认 medium",
                    "default": "medium"
                },
                "data_query": {
                    "type": "string",
                    "description": "自定义数据查询描述（可选，不填则根据主题自动生成查询）"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "数据查询超时秒数（默认 360）",
                    "default": 360
                },
                "sp_s_name": { "type": "string", "description": "前端显示的步骤名称，建议值：\"生成汇报材料\"" },
                "sp_s_icon": { "type": "string", "description": "前端显示的步骤图标，固定值：\"icon-gen-markdown\"" }
            },
            "required": ["topic", "sp_s_name", "sp_s_icon"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let topic = require_str(&args, "topic")?;
        let school_name = opt_str(&args, "school_name");
        let extra_data = opt_str(&args, "extra_data");
        let extra_req = opt_str(&args, "extra_requirements");
        let data_query = opt_str(&args, "data_query");
        let length = args
            .get("length")
            .and_then(|v| v.as_str())
            .unwrap_or("medium");
        let timeout = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(360);

        let session_id = gen_session_id();

        let token = match require_sw_token() {
            Ok(t) => t,
            Err(e) => return Ok(e),
        };

        // Resolve school name + ID from user info if not provided.
        let (resolved_school, school_scope) = if school_name.is_empty() {
            match fetch_sw_user_data(&token).await {
                Ok(data) => {
                    let name = data
                        .get("unitName")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or("本校");
                    let uid = data
                        .get("unitId")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or("");
                    let scope = if uid.is_empty() {
                        name.to_string()
                    } else {
                        format!("{name}(uid:{uid})")
                    };
                    (name.to_string(), scope)
                }
                Err(_) => ("本校".into(), "本校".into()),
            }
        } else {
            (school_name.clone(), school_name)
        };

        // Fetch school data via claw_query.
        let scripts_dir = match ensure_skill_scripts(&self.workspace_dir) {
            Ok(d) => d,
            Err(e) => return Ok(err_result(format!("释放脚本失败: {e}"))),
        };

        let query_text = {
            let raw = if data_query.is_empty() {
                format!(
                    "查询{resolved_school}的以下数据用于撰写「{topic}」汇报材料：\
                     学校基本情况、教职工人数、班级数量、学生人数、\
                     信息化设备数量及覆盖范围、网络建设情况、\
                     云课件和云教案数量、教研活动次数及培训数据、\
                     学生课堂参与度及点评数据、\
                     特色应用案例和成果、获奖情况"
                )
            } else {
                data_query
            };
            format!("以下查询仅限{school_scope}的数据：{raw}")
        };

        let work_dir = self.workspace_dir.join(".local/sw-report-tmp");
        std::fs::create_dir_all(&work_dir)?;

        let school_data = match run_claw_query_cached(
            &scripts_dir,
            &token,
            &query_text,
            &work_dir,
            timeout,
            &self.workspace_dir,
            "school_report",
            self.cache_ttl_secs,
            self.cache_delay_ms,
            Some(&self.llm),
        )
        .await
        {
            Ok(data) => data,
                Err(e) => return Ok(err_result(format!("学校数据查询失败: {e}"))),
            };

        // Build the LLM prompts.
        let length_hint = match length {
            "short" => "汇报正文约 1500 字",
            "long" => "汇报正文不少于 5000 字，内容需详实丰富",
            _ => "汇报正文约 3000 字",
        };

        let system_prompt = format!(
            "{REPORT_SYSTEM_PROMPT}\n\n## 篇幅要求\n{length_hint}\n\n## 汇报单位\n{resolved_school}"
        );

        let mut user_prompt = String::new();
        {
            use std::fmt::Write;
            let _ = write!(user_prompt, "## 汇报主题\n\n{topic}\n\n");
            let _ = write!(
                user_prompt,
                "## 学校数据\n\n{}\n\n",
                truncate_str(&school_data, 12000)
            );
            if !extra_data.is_empty() {
                let _ = write!(
                    user_prompt,
                    "## 补充数据/素材\n\n{}\n\n",
                    truncate_str(&extra_data, 4000)
                );
            }
            if !extra_req.is_empty() {
                let _ = write!(user_prompt, "## 额外要求\n\n{extra_req}\n\n");
            }
            let _ = write!(
                user_prompt,
                "请为{resolved_school}撰写一篇关于「{topic}」的汇报材料。直接输出汇报正文，不需要额外解释。"
            );
        }

        let provider = match self.llm.create_provider() {
            Ok(p) => p,
            Err(e) => return Ok(err_result(format!("创建 LLM provider 失败: {e}"))),
        };

        let report_text = match provider
            .chat_with_system(
                Some(&system_prompt),
                &user_prompt,
                &self.llm.model,
                self.llm.temperature,
            )
            .await
        {
            Ok(text) => text,
            Err(e) => return Ok(err_result(format!("LLM 汇报材料生成失败: {e}"))),
        };

        let out_dir = self
            .workspace_dir
            .join("artifacts/school-report")
            .join(&session_id);
        std::fs::create_dir_all(&out_dir)?;
        let report_path = out_dir.join("report.md");
        std::fs::write(&report_path, &report_text)?;

        let task_id = format!("task-school-report-{session_id}");
        let title = truncate_str(&topic, 60);
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let file_path = report_path.display().to_string();

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
                "学校汇报材料生成完成。session_id={session_id}\n文件: {file_path}\n\n\
                 请将以下task和notice标签组原样输出给用户：\n{xml}"
            ),
            error: None,
        })
    }
}
