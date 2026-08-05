//! Agent turn 循环。
//!
//! 一次 turn = 用户输入 → 模型响应 → 若有工具调用则执行并回灌 → 直到模型给出纯文本回复。
//! 每次工具调用严格走「判定 → 审批 → 执行 → 审计」四步。

use crate::approval::{ApprovalRequest, ApprovalResult, Approver};
use crate::prompt::PromptBuilder;
use ares_audit::{now_rfc3339, AuditRecord};
use ares_core::{AresError, Decision, HostId, Result};
use ares_llm::{CompletionRequest, Message, Provider, ToolCall, ToolDef, Usage};
use ares_tools::{TerminalExecuteTool, ToolContext, ToolRegistry};
use std::sync::Arc;

/// 单次工具执行的记录，用于向用户展示。
#[derive(Debug, Clone)]
pub struct ToolRun {
    pub tool: String,
    pub command: Option<String>,
    pub decision_label: String,
    pub display: String,
    pub success: bool,
}

#[derive(Debug, Clone)]
pub struct TurnResult {
    pub reply: String,
    pub tool_runs: Vec<ToolRun>,
    pub usage: Usage,
}

pub struct AgentLoop {
    provider: Arc<dyn Provider>,
    registry: ToolRegistry,
    ctx: ToolContext,
    approver: Arc<dyn Approver>,
    model: String,
    /// 会话历史，含 system 消息
    history: Vec<Message>,
    /// 单次 turn 内最多允许的工具调用轮数，防止无限循环
    max_tool_rounds: usize,
    /// 对话历史超过该条数时触发压缩（早期消息 LLM 摘要化）
    compress_threshold: usize,
    /// 本会话累计 token 用量（run_turn 更新；handle_tool_call 写审计用）
    usage: tokio::sync::Mutex<Usage>,
    /// 当前正在执行的工具（GUI 进度显示；独立 std Mutex 不经主体锁）
    pub progress: Arc<std::sync::Mutex<Option<String>>>,
    /// 取消请求标志（GUI 停止按钮置位；run_turn 各检查点提前终止）
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

impl AgentLoop {
    pub fn new(
        provider: Arc<dyn Provider>,
        registry: ToolRegistry,
        ctx: ToolContext,
        approver: Arc<dyn Approver>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let hosts: Vec<(HostId, String)> = ctx
            .scope()
            .iter()
            .map(|h| (h.clone(), ctx.policy().env_of(h).to_string()))
            .collect();

        // 记忆目录与技能目录就绪
        let _ = ares_core::memory::ensure_dirs();

        let system = PromptBuilder::load()?
            .with_tools(registry.specs())
            .with_hosts(hosts)
            .with_memory(ares_core::memory::memory_summary(80))
            .build();

        Ok(Self {
            provider,
            registry,
            ctx,
            approver,
            model: model.into(),
            history: vec![Message::system(system)],
            max_tool_rounds: 12,
            compress_threshold: 40,
            usage: tokio::sync::Mutex::new(Usage::default()),
            progress: Arc::new(std::sync::Mutex::new(None)),
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// 当前进度文本（无则 None）。
    pub fn current_progress(&self) -> Option<String> {
        self.progress.lock().unwrap().clone()
    }

    /// 请求中断当前 turn。
    pub fn request_cancel(&self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn tool_defs(&self) -> Vec<ToolDef> {
        self.registry
            .specs()
            .into_iter()
            .map(|s| ToolDef {
                name: s.name,
                description: s.description,
                parameters: s.parameters,
            })
            .collect()
    }

    /// 执行一次完整的对话轮次。
    pub async fn run_turn(&mut self, user_input: &str) -> Result<TurnResult> {
        // 上下文管理：历史超长先压缩（早期消息摘要化）
        if self.history.len() > self.compress_threshold {
            let _ = self.compress_history().await;
        }
        self.history.push(Message::user(user_input));

        let mut tool_runs = Vec::new();
        let mut usage = Usage::default();
        // 熔断计数：**整个 turn 累计**（不在 round 内声明 —— 若每轮重置，
        // 模型每轮只试 1 次变体、跨 12 轮永不触发「连续 3 次」承诺）
        let mut denials = 0usize;

        for round in 0..self.max_tool_rounds {
            // 中断检查点：用户可随时停止（GUI 停止按钮）
            if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let note = "已中断（用户停止）。报告当前进展。".to_string();
                self.history.push(Message::user(note.clone()));
                return Ok(TurnResult {
                    reply: note,
                    tool_runs,
                    usage,
                });
            }

            let req = CompletionRequest::new(&self.model, self.history.clone())
                .with_tools(self.tool_defs());

            let resp = self.provider.complete(req).await?;
            usage.input += resp.usage.input;
            usage.output += resp.usage.output;
            *self.usage.lock().await = usage;

            if resp.tool_calls.is_empty() {
                self.history
                    .push(Message::assistant(resp.content.clone(), vec![]));
                return Ok(TurnResult {
                    reply: resp.content,
                    tool_runs,
                    usage,
                });
            }

            self.history.push(Message::assistant(
                resp.content.clone(),
                resp.tool_calls.clone(),
            ));

            for call in &resp.tool_calls {
                // 进度：显示正在执行的工具与命令（GUI 实时读取）
                let hint = call.arguments["command"]
                    .as_str()
                    .map(|c| {
                        let short: String = c.chars().take(48).collect();
                        if c.chars().count() > 48 {
                            format!("{short}…")
                        } else {
                            short
                        }
                    })
                    .unwrap_or_default();
                *self.progress.lock().unwrap() =
                    Some(format!("{} {hint}", call.name).trim().to_string());

                let (run, tool_message) = self.handle_tool_call(call).await;
                *self.progress.lock().unwrap() = None;
                if matches!(run.decision_label.as_str(), "deny" | "rejected" | "timeout") {
                    denials += 1;
                }
                tool_runs.push(run);
                self.history.push(tool_message);
            }

            // 连续被拒 / 被禁 3 次即熔断：模型可能在被拒绝后换写法绕过，
            // 无限尝试变体既浪费 token 也是安全风险（绕过尝试本身就是攻击面）
            if denials >= 3 {
                let note = format!(
                    "本轮已有 {denials} 次操作被拒绝或禁止。停止执行，\
                     向用户报告已完成的部分与需要人工处理的事项。"
                );
                self.history.push(Message::user(note.clone()));
                return Ok(TurnResult {
                    reply: note,
                    tool_runs,
                    usage,
                });
            }

            // 最后一轮仍在调用工具，说明陷入了循环
            if round == self.max_tool_rounds - 1 {
                let note = format!(
                    "已达到单轮工具调用上限（{} 次）。停止执行并把当前进展报告给用户。",
                    self.max_tool_rounds
                );
                self.history.push(Message::user(note.clone()));
                return Ok(TurnResult {
                    reply: format!("（{note}）"),
                    tool_runs,
                    usage,
                });
            }
        }

        unreachable!("循环体内必定返回")
    }

    /// 处理单次工具调用，返回展示记录与回灌给模型的消息。
    ///
    /// 本方法不返回 Err —— 任何失败都转换成给模型的文本反馈，
    /// 让 Agent 有机会自行纠正，而不是整个 turn 崩掉。
    ///
    /// **所有工具都必须走「判定 → 审批 → 执行 → 审计」**：
    /// 只对 terminal_execute 做策略判定会让其余工具成为绕过路径
    ///（例如 M2 的 sftp_write_file 若裸调用，等于完全没有护栏）。
    /// 只读工具（category == Read）判定为 observer 自动执行，但同样写审计。
    async fn handle_tool_call(&self, call: &ToolCall) -> (ToolRun, Message) {
        let make = |decision_label: &str, display: String, success: bool, content: String| {
            (
                ToolRun {
                    tool: call.name.clone(),
                    command: call.arguments["command"].as_str().map(String::from),
                    decision_label: decision_label.to_string(),
                    display,
                    success,
                },
                // **脱敏收口点**：一切进入 LLM 上下文的工具内容都必须先脱敏。
                // 只靠 terminal_execute 的输出侧脱敏（budget.apply）不够 ——
                // 未来任何新工具（dossier_read / memory_search / audit_query）
                // 若自带内容直接 push，凭据会裸奔进上下文与审计。
                // 收口在回灌处（单一强制点），见 §7.3「所有进入 LLM 上下文
                // 的内容经 redaction」。
                Message::tool(call.id.clone(), ares_core::redact::redact(&content)),
            )
        };

        // terminal_execute 走专用路径（命令级策略）
        if call.name == "terminal_execute" {
            return self.handle_terminal_execute(call).await;
        }

        let tool = match self.registry.get(&call.name) {
            Ok(t) => t,
            Err(e) => {
                let msg = e.to_string();
                return make("error", msg.clone(), false, format!("错误：{msg}"));
            }
        };

        // ── 1. 判定 ──
        // scope 首个主机作为判定目标（工具内部自己会按实际主机 check_scope）。
        // **架构留痕（M2+ 必修）**：非 terminal 工具（dossier_write / sftp_write_file）
        // 的真实目标主机 ≠ scope.first() —— M2 引入 Write 工具时，PolicyQuery 必须
        // 增加 target_host / target_host_count 字段，按**工具调用参数**解析真实目标
        //（否则审批按 scope.first() 的 env 判定，prod 目标会被当成 dev 放行）。
        let host = self
            .ctx
            .scope()
            .first()
            .cloned()
            .unwrap_or_else(HostId::localhost);
        let category = tool.category();
        let decision = self.ctx.policy().evaluate(&ares_policy::PolicyQuery {
            host: host.clone(),
            tool: call.name.clone(),
            category,
            command: None,
            host_count: 1,
        });

        // ── 2. 审批（仅需要交互的级别）──
        let approval = if decision.needs_interaction() {
            self.approver
                .ask(&ApprovalRequest {
                    host: host.clone(),
                    env: self.ctx.policy().env_of(&host),
                    command: format!("<tool:{}({})>", call.name, call.arguments),
                    decision: decision.clone(),
                    host_count: 1,
                    // 工具调用没有命令字符串，is_critical 无意义，恒为 false
                    require_typed_host: false,
                })
                .await
        } else {
            Ok(ApprovalResult::Approved)
        };

        let (approved, label, note) = match approval {
            Ok(ApprovalResult::Approved) => (true, decision.label().to_string(), None),
            // 非 terminal 工具没有命令文本可编辑；编辑内容忽略，视为批准
            Ok(ApprovalResult::ApprovedWithEdit(_)) => (true, decision.label().to_string(), None),
            Ok(ApprovalResult::Rejected) => (
                false,
                "rejected".to_string(),
                Some("用户拒绝了此操作。不要重试。".to_string()),
            ),
            Ok(ApprovalResult::Timeout) => (
                false,
                "timeout".to_string(),
                Some("审批超时，操作未执行。".to_string()),
            ),
            Err(AresError::Denied(reason)) => (
                false,
                "deny".to_string(),
                Some(format!("此操作被安全策略禁止：{reason}")),
            ),
            Err(e) => (false, "error".to_string(), Some(format!("审批失败：{e}"))),
        };

        // ── 3. 执行 ──
        let (display, content, success) = if approved {
            match tool.call(&self.ctx, call.arguments.clone()).await {
                Ok(out) => (out.display.clone(), out.content, true),
                Err(e) => {
                    let msg = e.to_string();
                    (msg.clone(), format!("执行失败：{msg}"), false)
                }
            }
        } else {
            let n = note.unwrap_or_default();
            (n.clone(), n, false)
        };

        // ── 4. 审计：无论批准、拒绝、禁止都要留痕 ──
        let mut rec = ares_audit::AuditRecord::new(
            ares_audit::now_rfc3339(),
            host.as_str(),
            &call.name,
            &format!("<tool-call arguments={}>", call.arguments),
            None,
            &content,
            &label,
            self.ctx.caller(),
            self.ctx.session_id(),
        )
        .with_policy_hit(match &decision {
            Decision::Deny { reason } => reason.clone(),
            Decision::Confirm { rule, .. } => rule.clone(),
            Decision::Auto { rule } => rule.clone(),
            other => other.label().to_string(),
        });
        // model/tokens 入审计（spec §14.3 字段清单；模型与 token 用量
        // 是成本审计的核心字段，M1 就要写入而非留 null）
        {
            let usage = self.usage.lock().await;
            rec = rec.with_model(&self.model, usage.input, usage.output);
        }
        if let Err(e) = self.ctx.audit().lock().await.append(rec) {
            eprintln!("\x1b[38;5;196m审计写入失败：{e}\x1b[0m");
        }

        make(&label, display, success, content)
    }

    async fn handle_terminal_execute(&self, call: &ToolCall) -> (ToolRun, Message) {
        let exec_tool = TerminalExecuteTool::new();

        // ── 1+2. 判定 + 审批（2026-08-05 plan 编辑：用户编辑后的命令
        //         重新走策略判定 —— 编辑不能绕过审批）──
        let mut arguments = call.arguments.clone();
        let (prepared, approved, label, note) = loop {
            let prepared = match exec_tool.prepare(&self.ctx, &arguments) {
                Ok(p) => p,
                Err(e) => {
                    let msg = e.to_string();
                    return (
                        ToolRun {
                            tool: call.name.clone(),
                            command: arguments["command"].as_str().map(String::from),
                            decision_label: "error".into(),
                            display: msg.clone(),
                            success: false,
                        },
                        Message::tool(call.id.clone(), format!("错误：{msg}")),
                    );
                }
            };

            let host = prepared.request.host.clone();
            let command = prepared.request.command.clone();
            let decision = prepared.decision.clone();

            let approval = self
                .approver
                .ask(&ApprovalRequest {
                    host: host.clone(),
                    env: self.ctx.policy().env_of(&host),
                    command: command.clone(),
                    decision: decision.clone(),
                    host_count: 1,
                    // 极高危命令（spec §14.2）：确认之外还要手打主机名
                    require_typed_host: self.ctx.policy().is_critical(&command),
                })
                .await;

            match approval {
                Ok(ApprovalResult::Approved) => {
                    break (prepared, true, decision.label().to_string(), None);
                }
                Ok(ApprovalResult::ApprovedWithEdit(edited)) => {
                    // plan 模式：用户修改了命令 → 替换并重新判定（安全链不跳过）
                    if let Some(obj) = arguments.as_object_mut() {
                        obj.insert(
                            "command".to_string(),
                            serde_json::Value::String(edited.trim().to_string()),
                        );
                    }
                    continue;
                }
                Ok(ApprovalResult::Rejected) => {
                    break (
                        prepared,
                        false,
                        "rejected".to_string(),
                        Some(
                            "用户拒绝了此操作。不要重试，也不要换一种写法达到同样效果。"
                                .to_string(),
                        ),
                    );
                }
                Ok(ApprovalResult::Timeout) => {
                    break (
                        prepared,
                        false,
                        "timeout".to_string(),
                        Some("审批超时，操作未执行。".to_string()),
                    );
                }
                Err(AresError::Denied(reason)) => {
                    break (
                        prepared,
                        false,
                        "deny".to_string(),
                        Some(format!(
                            "此操作被安全策略禁止：{reason}\n\
                             不要尝试变形、拆分或用其他工具达到同样效果。\
                             直接告诉用户这件事需要人工执行。"
                        )),
                    );
                }
                Err(e) => {
                    break (
                        prepared,
                        false,
                        "error".to_string(),
                        Some(format!("审批失败：{e}")),
                    );
                }
            }
        };

        // ── 3. 执行 ──
        // 执行前快照（审计用：execute 会移动 request）
        let audited_command = prepared.request.command.clone();
        let audited_host = prepared.request.host.clone();
        let (display, content, success) = if approved {
            match exec_tool.execute(&self.ctx, prepared.request).await {
                Ok(out) => (out.display.clone(), out.content, true),
                Err(e) => {
                    let m = e.to_string();
                    (m.clone(), format!("执行失败：{m}"), false)
                }
            }
        } else {
            let n = note.unwrap_or_default();
            (n.clone(), n, false)
        };

        // ── 4. 审计：无论批准、拒绝、禁止都要留痕 ──
        // 只记录被执行的操作，审计日志就只能证明做过什么，
        // 不能证明拦下过什么 —— 后者在事后复盘时同样重要
        let rec = AuditRecord::new(
            now_rfc3339(),
            audited_host.as_str(),
            "terminal_execute",
            &audited_command,
            None,
            &content,
            &label,
            self.ctx.caller(),
            self.ctx.session_id(),
        )
        .with_policy_hit(match &prepared.decision {
            Decision::Deny { reason } => reason.clone(),
            Decision::Confirm { rule, .. } => rule.clone(),
            Decision::Auto { rule } => rule.clone(),
            other => other.label().to_string(),
        });

        if let Err(e) = self.ctx.audit().lock().await.append(rec) {
            // 审计写入失败必须可见 —— 静默失败等于没有审计
            eprintln!("\x1b[38;5;196m审计写入失败：{e}\x1b[0m");
        }

        (
            ToolRun {
                tool: "terminal_execute".into(),
                command: Some(audited_command),
                decision_label: label,
                display,
                success,
            },
            Message::tool(call.id.clone(), content),
        )
    }

    /// 当前会话历史长度，用于 M5 的压缩触发。
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// 上下文压缩：早期对话消息用 LLM 摘要替换（保留最近 keep_recent 条）。
    /// 摘要以 system 消息注入，不改变角色语义。
    pub async fn compress_history(&mut self) -> Result<()> {
        let total = self.history.len();
        if total <= self.compress_threshold {
            return Ok(());
        }
        let keep_recent = 15usize;
        let compress_end = total - keep_recent;
        if compress_end <= 1 {
            return Ok(());
        }
        let early: Vec<String> = self.history[1..compress_end]
            .iter()
            .map(|m| format!("[{:?}] {}", m.role, m.content))
            .collect();
        let transcript = early.join("\n");

        let req = CompletionRequest::new(
            &self.model,
            vec![
                Message::system(
                    "把以下对话历史压缩成一段简洁摘要（≤200 字）。必须保留：\n\
                     - 已完成的关键操作与结果\n\
                     - 未完成事项 / 下一步\n\
                     - 用户表达的任何偏好\n\
                     - 与安全相关的任何信息（被拒操作、危险命令、注意事项）\n\
                     - 重要环境事实\n\
                     只输出摘要正文，不要任何前缀。",
                ),
                Message::user(transcript),
            ],
        );
        let resp = self.provider.complete(req).await?;
        let summary = resp.content.trim().to_string();
        if summary.is_empty() {
            return Ok(());
        }

        let system = self.history[0].clone();
        let mut new_history = vec![system];
        new_history.push(Message::system(format!(
            "## 会话摘要（早期对话已压缩）\n\n{summary}"
        )));
        new_history.extend_from_slice(&self.history[compress_end..]);
        self.history = new_history;
        Ok(())
    }

    /// 记忆压缩：lessons.md 超过阈值时用 LLM 合并去重（保留结构）。
    pub async fn compress_memory(&self) -> Result<()> {
        const MAX_LINES: usize = 200;
        let Some(lessons) = ares_core::memory::read_memory("lessons.md") else {
            return Ok(());
        };
        if lessons.lines().count() <= MAX_LINES {
            return Ok(());
        }
        let req = CompletionRequest::new(
            &self.model,
            vec![
                Message::system(
                    "合并以下运维经验条目：去除重复与过时项，同类合并，保留每条的原始含义。\
                     输出 markdown 列表（每条 `- ...`），不超过 60 条。只输出列表正文。",
                ),
                Message::user(lessons),
            ],
        );
        let resp = self.provider.complete(req).await?;
        let merged = resp.content.trim().to_string();
        if !merged.is_empty() {
            let _ = ares_core::memory::write_memory_reset("lessons.md", &merged);
        }
        Ok(())
    }

    /// 自进化反思：从最近对话提炼记忆（facts/lessons/skill 草稿）。
    /// 返回原始提炼文本（调用方解析后写入记忆库）。
    pub async fn reflect(&self, recent: &[(String, String)]) -> Result<String> {
        let transcript = recent
            .iter()
            .map(|(r, t)| format!("[{r}] {t}"))
            .collect::<Vec<_>>()
            .join("\n");
        let req = CompletionRequest::new(
            &self.model,
            vec![
                Message::system(
                    "你是 ARES 运维 Agent 的反思模块。阅读最近对话，提炼值得长期记住的内容：\n\
                     1. 稳定的环境事实 / 用户偏好 → FACTS\n\
                     2. 踩坑 / 教训 / 成功模式 → LESSONS\n\
                     3. 若发现可复用的重复任务流程 → SKILL_IF_ANY（给出 SKILL.md 草稿）\n\
                     输出严格格式：\nFACTS:\n- ...\nLESSONS:\n- ...\nSKILL_IF_ANY:\n（无则省略）\n\
                     只写有价值、稳定、不重复的内容；泛泛而谈不要写。",
                ),
                Message::user(transcript),
            ],
        );
        let resp = self.provider.complete(req).await?;
        Ok(resp.content)
    }

    /// 恢复历史消息（对话持久化：从存档载入的 user/assistant 消息）。
    pub fn restore_history(&mut self, msgs: &[(String, String)]) {
        for (role, text) in msgs {
            match role.as_str() {
                "user" => self.history.push(Message::user(text.clone())),
                _ => self.history.push(Message::assistant(text.clone(), vec![])),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::AutoApprover;
    use ares_llm::CompletionResponse;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Mutex as StdMutex;

    /// 按预设脚本依次返回响应的假 provider。
    struct ScriptedProvider {
        responses: StdMutex<Vec<CompletionResponse>>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<CompletionResponse>) -> Arc<Self> {
            Arc::new(Self {
                responses: StdMutex::new(responses),
            })
        }
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse> {
            let mut r = self.responses.lock().unwrap();
            if r.is_empty() {
                return Ok(CompletionResponse {
                    content: "（脚本已用尽）".into(),
                    tool_calls: vec![],
                    usage: Usage::default(),
                });
            }
            Ok(r.remove(0))
        }
        fn name(&self) -> &str {
            "scripted"
        }
    }

    fn text_response(s: &str) -> CompletionResponse {
        CompletionResponse {
            content: s.into(),
            tool_calls: vec![],
            usage: Usage {
                input: 10,
                output: 5,
            },
        }
    }

    fn call_response(command: &str) -> CompletionResponse {
        CompletionResponse {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "terminal_execute".into(),
                arguments: json!({"command": command}),
            }],
            usage: Usage {
                input: 20,
                output: 10,
            },
        }
    }

    fn make_loop(responses: Vec<CompletionResponse>, approver: Arc<dyn Approver>) -> AgentLoop {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("ARES_CONFIG_DIR", tmp.path().join("cfg"));
        std::env::set_var("ARES_DATA_DIR", tmp.path().join("data"));
        std::mem::forget(tmp);

        let ctx = ares_tools::test_support_ctx(vec![HostId::localhost()]);
        AgentLoop::new(
            ScriptedProvider::new(responses),
            ares_tools::default_registry(),
            ctx,
            approver,
            "test-model",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn plain_text_response_ends_turn() {
        let mut l = make_loop(
            vec![text_response("磁盘使用率 44%")],
            Arc::new(AutoApprover::approve_all()),
        );
        let r = l.run_turn("磁盘怎么样").await.unwrap();

        assert_eq!(r.reply, "磁盘使用率 44%");
        assert!(r.tool_runs.is_empty());
        assert_eq!(r.usage.input, 10);
    }

    #[tokio::test]
    async fn tool_call_is_executed_and_fed_back() {
        let mut l = make_loop(
            vec![call_response("uptime"), text_response("系统已运行多日")],
            Arc::new(AutoApprover::approve_all()),
        );
        let r = l.run_turn("看下 uptime").await.unwrap();

        assert_eq!(r.tool_runs.len(), 1);
        assert!(r.tool_runs[0].success);
        assert_eq!(r.tool_runs[0].decision_label, "observer");
        assert_eq!(r.reply, "系统已运行多日");
        // usage 应累加两次调用
        assert_eq!(r.usage.input, 30);
    }

    #[tokio::test]
    async fn denied_command_is_not_executed_and_agent_is_told_why() {
        let mut l = make_loop(
            vec![
                call_response("rm -rf /"),
                text_response("这个操作需要你人工执行"),
            ],
            Arc::new(AutoApprover::approve_all()),
        );
        let r = l.run_turn("清空磁盘").await.unwrap();

        assert_eq!(r.tool_runs.len(), 1);
        assert!(!r.tool_runs[0].success);
        assert_eq!(r.tool_runs[0].decision_label, "deny");
        assert!(r.tool_runs[0].display.contains("需要人工执行"));
    }

    #[tokio::test]
    async fn rejected_command_tells_agent_not_to_retry() {
        let mut l = make_loop(
            vec![
                call_response("touch /tmp/ares-test-file"),
                text_response("好的"),
            ],
            Arc::new(AutoApprover::reject_all()),
        );
        let r = l.run_turn("建个文件").await.unwrap();

        assert_eq!(r.tool_runs[0].decision_label, "rejected");
        assert!(r.tool_runs[0].display.contains("不要重试"));
        assert!(!std::path::Path::new("/tmp/ares-test-file").exists());
    }

    #[tokio::test]
    async fn every_call_is_audited_including_denials() {
        let mut l = make_loop(
            vec![call_response("rm -rf /"), text_response("done")],
            Arc::new(AutoApprover::approve_all()),
        );
        l.run_turn("x").await.unwrap();

        let path = l.ctx.audit().lock().await.path().to_path_buf();
        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("\"decision\":\"deny\""));
    }

    #[tokio::test]
    async fn unknown_tool_becomes_feedback_not_crash() {
        let resp = CompletionResponse {
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "nonexistent_tool".into(),
                arguments: json!({}),
            }],
            usage: Usage::default(),
        };
        let mut l = make_loop(
            vec![resp, text_response("抱歉，我用错了工具")],
            Arc::new(AutoApprover::approve_all()),
        );
        let r = l.run_turn("x").await.unwrap();

        assert!(!r.tool_runs[0].success);
        assert!(r.tool_runs[0].display.contains("不存在"));
        assert_eq!(r.reply, "抱歉，我用错了工具");
    }

    #[tokio::test]
    async fn runaway_tool_loop_is_capped() {
        // 模型一直调用工具从不给结论时，必须能停下来
        let responses: Vec<CompletionResponse> = (0..30).map(|_| call_response("uptime")).collect();
        let mut l = make_loop(responses, Arc::new(AutoApprover::approve_all()));
        let r = l.run_turn("死循环").await.unwrap();

        assert!(r.reply.contains("上限"));
        assert_eq!(r.tool_runs.len(), 12);
    }

    /// 可编程审批器：按预设脚本依次返回结果；Deny 判定恒返回错误（与 AutoApprover 一致）。
    struct ScriptedApprover {
        answers: StdMutex<Vec<ApprovalResult>>,
    }

    impl ScriptedApprover {
        fn new(answers: Vec<ApprovalResult>) -> Arc<Self> {
            Arc::new(Self {
                answers: StdMutex::new(answers),
            })
        }
    }

    #[async_trait]
    impl Approver for ScriptedApprover {
        async fn ask(&self, req: &ApprovalRequest) -> Result<ApprovalResult> {
            if let Decision::Deny { reason } = &req.decision {
                return Err(AresError::Denied(reason.clone()));
            }
            let mut a = self.answers.lock().unwrap();
            Ok(a.remove(0))
        }
    }

    #[tokio::test]
    async fn plan_edit_executes_edited_command() {
        // plan 模式：审批返回编辑后的命令 → agent 重新判定并执行编辑后的命令
        let mut l = make_loop(
            vec![call_response("uptime"), text_response("已按编辑后命令执行")],
            ScriptedApprover::new(vec![
                ApprovalResult::ApprovedWithEdit("df -P".into()),
                ApprovalResult::Approved,
            ]),
        );
        let r = l.run_turn("看下 uptime").await.unwrap();

        assert_eq!(r.tool_runs.len(), 1);
        assert!(r.tool_runs[0].success);
        // 执行的是编辑后的命令（重新判定：df -P = observer）
        assert_eq!(r.tool_runs[0].command.as_deref(), Some("df -P"));
        assert_eq!(r.tool_runs[0].decision_label, "observer");
    }

    #[tokio::test]
    async fn plan_edit_cannot_bypass_deny() {
        // 编辑成高危命令 → 重新判定命中 deny → 不执行（编辑不能绕过审批）
        let mut l = make_loop(
            vec![
                call_response("uptime"),
                text_response("编辑后的命令被策略禁止"),
            ],
            ScriptedApprover::new(vec![ApprovalResult::ApprovedWithEdit("rm -rf /".into())]),
        );
        let r = l.run_turn("看下 uptime").await.unwrap();

        assert_eq!(r.tool_runs.len(), 1);
        assert!(!r.tool_runs[0].success);
        assert_eq!(r.tool_runs[0].decision_label, "deny");
        // 记录的命令是编辑后的（审计可见最终目标）
        assert_eq!(r.tool_runs[0].command.as_deref(), Some("rm -rf /"));
    }
}
