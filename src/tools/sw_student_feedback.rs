//! Built-in tool: generate student follow-up feedback via LLM.
//!
//! Reads student analysis data produced by `sw_student_analysis`, then calls
//! an LLM to select the top-3 students needing follow-up and produce
//! actionable communication strategies.

use super::sw_lesson_common::{
    LlmProviderConfig, analysis_artifacts_dir, cache_key, cached_query, err_result, opt_str,
    require_str, truncate_str,
};
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct SwStudentFeedbackTool {
    workspace_dir: PathBuf,
    llm: LlmProviderConfig,
    cache_ttl_secs: u64,
    cache_delay_ms: u64,
}

impl SwStudentFeedbackTool {
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

const SYSTEM_PROMPT: &str = "\
你是一位资深班主任和教育分析专家。请根据提供的学生课堂表现数据，\
挑选出最需要课后跟进的 3 名学生，分析其可能的问题并给出沟通策略。

## 核心原则

- 使用 studentId 作为学生主键
- 不依赖 ASR 姓名（可能包含课文人名、教师举例、语音识别错误）
- 如果能可靠映射真实姓名则附带，否则只报 studentId

## 学生风险分类（按优先级）

### A. 低参与型（最高优先）
特征: score=0, handsRaisedCount=0, questionAnswerCount=0, raiseHeadTotalSec 极低
解读: 未有效进入课堂互动，任务投入弱

### B. 低专注零参与型（高优先）
特征: score=0, 无举手, 无回答, raiseHeadTotalSec 明显低于班级中位数
解读: 注意力不稳定，课堂进入感弱

### C. 高专注但沉默型（高优先，易被忽视）
特征: raiseHeadTotalSec >= 班级中位数, handsRaisedCount=0, questionAnswerCount=0
解读: 在听但不表达，可能担心答错或性格谨慎

### D. 高意愿但低转化型（中高优先）
特征: 多次举手但有效回答少或为零
解读: 想参与但未能转化为有效输出
特别规则: 有 HAND_UP 无 ANSWER 时，描述为「有参与意愿但未转化为回答机会」，不推断回答质量

### E. 被动应答型（中优先）
特征: 有回答但无举手
解读: 被叫到能答，但缺乏主动性

## 选择规则

必须恰好选 3 名学生，默认组合:
1. 一名 A 或 B 类
2. 一名 C 类
3. 一名 D 类
仅当 A-D 证据不足时才选 E 类。不要全部来自同一分类。

## 沟通策略

不要使用道德评价（懒惰、态度差等），按此顺序:
1. 描述观察到的模式
2. 肯定积极信号（如有）
3. 给出一个小的下一步目标

## 输出格式

只输出 3 名学生，不要输出班级总览或额外排名列表。使用以下结构（中文）:

```
`studentId xxx`
问题: [可能的问题]
课堂表现: [定性描述，默认不暴露原始数字]
沟通建议: [1-3 句中文建议]
```

## 注意事项

- 不要将单节课过度解读为稳定性格标签
- 不要默认使用「缺席/设备异常/识别缺失」作为解释
- ASR 内容与结构化指标冲突时，信任结构化指标
- 表达证据时使用定性的教育场景语言，而非原始数据堆砌";

#[async_trait]
impl Tool for SwStudentFeedbackTool {
    fn name(&self) -> &str {
        "sw_student_feedback"
    }

    fn description(&self) -> &str {
        "基于学生表现观察分析数据，挑选最需要课后跟进的 3 名学生，\
         分析其可能的问题并给出班主任沟通策略。\
         需要先执行 sw_student_analysis 获取数据。"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "观察分析的会话 ID（来自 sw_student_analysis）"
                },
                "subject": {
                    "type": "string",
                    "description": "学科，可以先从 sw_get_user_info 工具获取"
                },
                "topic": {
                    "type": "string",
                    "description": "课程主题（可选）"
                },
                "extra_requirements": {
                    "type": "string",
                    "description": "额外要求（可选）"
                },
                "sp_s_name": { "type": "string", "description": "前端显示的步骤名称，建议值：\"学生跟进反馈\"" },
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
        let student_path = analysis_dir.join("student_analysis.json");

        if !student_path.exists() {
            return Ok(err_result(format!(
                "未找到学生分析数据。请先执行 sw_student_analysis (session_id={session_id})"
            )));
        }

        let student_data = std::fs::read_to_string(&student_path).unwrap_or_default();

        let mut user_prompt = String::from("## 学生表现分析数据\n\n");
        user_prompt.push_str(truncate_str(&student_data, 12000));
        user_prompt.push('\n');

        {
            use std::fmt::Write;
            if !topic.is_empty() {
                let _ = write!(user_prompt, "\n## 课程主题\n\n{topic}\n");
            }
            let _ = write!(user_prompt, "\n## 学科\n\n{subject}\n");
            if !extra_req.is_empty() {
                let _ = write!(user_prompt, "\n## 额外要求\n\n{extra_req}\n");
            }
            let _ = write!(
                user_prompt,
                "\n请从以上数据中挑选 3 名最需要课后跟进的学生，按要求输出分析和沟通建议。直接输出正文，不需要额外解释。"
            );
        }

        let key = cache_key(&[&session_id, &subject, &topic, &extra_req, &student_data]);
        let query_text = format!("{subject} {topic} 学生跟进反馈");
        let llm = self.llm.clone();

        let feedback_text = match cached_query(
            &self.workspace_dir,
            "llm_feedback/student_feedback",
            &key,
            &query_text,
            self.cache_ttl_secs,
            self.cache_delay_ms,
            Some(&self.llm),
            || async {
                let provider = llm.create_provider()
                    .map_err(|e| anyhow::anyhow!("创建 LLM provider 失败: {e}"))?;
                provider
                    .chat_with_system(Some(SYSTEM_PROMPT), &user_prompt, &llm.model, llm.temperature)
                    .await
            },
        )
        .await
        {
            Ok(text) => text,
            Err(e) => return Ok(err_result(format!("LLM 学生反馈生成失败: {e}"))),
        };

        std::fs::create_dir_all(&analysis_dir)?;
        let feedback_path = analysis_dir.join("student_feedback.md");
        std::fs::write(&feedback_path, &feedback_text)?;

        let title_src = if topic.is_empty() {
            format!("{subject}-学生跟进反馈")
        } else {
            format!("{topic}-学生跟进反馈")
        };
        let task_id = format!("task-stufeedback-{session_id}");
        let title = truncate_str(&title_src, 60);
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let file_path = feedback_path.display().to_string();

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
                "学生跟进反馈生成完成。session_id={session_id}\n文件: {file_path}\n\n\
                 请将以下task和notice标签组原样输出给用户：\n{xml}"
            ),
            error: None,
        })
    }
}
