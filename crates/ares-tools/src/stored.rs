//! 按需取回落盘的大输出。

use crate::budget;
use crate::registry::{Tool, ToolContext, ToolOutput};
use ares_core::{AresError, Result, ToolCategory};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct Args {
    #[serde(rename = "ref")]
    ref_id: String,
    #[serde(default)]
    start_line: usize,
    #[serde(default = "default_count")]
    line_count: usize,
}

fn default_count() -> usize {
    100
}

pub struct ReadStoredOutputTool;

#[async_trait]
impl Tool for ReadStoredOutputTool {
    fn name(&self) -> &'static str {
        "read_stored_output"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn description(&self) -> &'static str {
        "取回此前因过大而被截断的完整输出的一段。\
         当某次命令返回中出现 ref=xxxx 时，可用本工具按行区间读取。\
         一次不要读太多行 —— 先读一小段定位，再按需精确取。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "ref": {"type": "string", "description": "输出引用 ID"},
                "start_line": {"type": "integer", "description": "起始行号，0 起，默认 0"},
                "line_count": {"type": "integer", "description": "读取行数，默认 100"}
            },
            "required": ["ref"]
        })
    }

    async fn call(&self, ctx: &ToolContext, args: serde_json::Value) -> Result<ToolOutput> {
        let a: Args = serde_json::from_value(args)
            .map_err(|e| AresError::InvalidArgs(format!("read_stored_output 参数无效：{e}")))?;

        // 单次读取上限，防止 Agent 一次把落盘内容全捞回上下文 ——
        // 那样落盘就白做了
        let count = a.line_count.min(500);
        let text = budget::load_stored(&a.ref_id, a.start_line, count)?;
        let budgeted = ctx.budget().apply(&text)?;

        Ok(ToolOutput {
            content: budgeted.text.clone(),
            display: format!(
                "ref={} 第 {}..{} 行",
                a.ref_id,
                a.start_line,
                a.start_line + count
            ),
            stored_ref: budgeted.stored_ref,
        })
    }
}
