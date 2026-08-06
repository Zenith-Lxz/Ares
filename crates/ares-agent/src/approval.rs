//! 审批呈现。
//!
//! 判定（policy）与呈现（approval）分离：判定决定「需不需要确认、
//! 需要哪种确认」，呈现决定「怎么问用户」。M5 的 MCP server 会换掉
//! 呈现方式而复用同一套判定。

use ares_core::{AresError, Decision, Env, HostId, Result};
use async_trait::async_trait;
use regex::Regex;
use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalResult {
    Approved,
    /// 批准并携带编辑后的命令文本（plan 模式：用户改过命令再批准；
    /// 执行前会**重新走策略判定**，编辑后的命令不能绕过审批）
    ApprovedWithEdit(String),
    Rejected,
    Timeout,
}

impl ApprovalResult {
    /// 用于审计记录的稳定标签。
    pub fn audit_label(self, decision: &Decision) -> String {
        match self {
            ApprovalResult::Approved => decision.label().to_string(),
            ApprovalResult::ApprovedWithEdit(_) => {
                format!("{}-edited", decision.label())
            }
            ApprovalResult::Rejected => "rejected".to_string(),
            ApprovalResult::Timeout => "timeout".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub host: HostId,
    pub env: Env,
    pub command: String,
    pub decision: Decision,
    /// 本次操作影响的主机数
    pub host_count: usize,
    /// 是否需要在确认之外手动输入主机名（极高危，spec §14.2）
    pub require_typed_host: bool,
}

#[async_trait]
pub trait Approver: Send + Sync {
    async fn ask(&self, req: &ApprovalRequest) -> Result<ApprovalResult>;
}

/// 终端审批器。全部走终端读取（确认/拒绝）；极高危（critical）追加输入主机名。
pub struct CliApprover;

impl CliApprover {
    pub fn new() -> Self {
        Self
    }

    /// 渲染审批横幅。只用前景色，不用背景色块 ——
    /// 用户的终端背景图必须完整保留。
    ///
    /// `command` 由 LLM 生成、可被注入控制，**必须**过 `sanitize_for_display`：
    /// 否则命令串里塞入 ANSI 光标控制序列就能重绘屏幕，伪造用户看到的内容
    ///（例如用 `\x1b[2K` + 空行把真实命令顶出屏幕，用户以为在放行 `uptime`）。
    fn banner(req: &ApprovalRequest) -> String {
        // ANSI 前景色：crimson=196, terracotta=173, bronze=179, stone=245
        let (mark, color) = match &req.decision {
            Decision::Confirm { .. } => ("⌘", 179),
            _ => ("·", 245),
        };
        let prod_note = if req.env == Env::Prod {
            format!(
                "  \x1b[38;5;196m⚑ {}\x1b[0m",
                sanitize_for_display(&req.env.to_string())
            )
        } else {
            format!(
                "  \x1b[38;5;245m{}\x1b[0m",
                sanitize_for_display(&req.env.to_string())
            )
        };
        let blast = if req.host_count > 1 {
            format!("  \x1b[38;5;173m{} 台主机\x1b[0m", req.host_count)
        } else {
            String::new()
        };
        let rule = match &req.decision {
            Decision::Confirm { rule, critical } => {
                let tag = if *critical {
                    "  ☢ 极高危（需输入主机名）"
                } else {
                    ""
                };
                // 同一行内追加（不换行）—— banner 行数必须固定为 3，
                // 由 banner_strips_injected_control_characters 守护。
                format!(" · {}{}", sanitize_for_display(rule), tag)
            }
            _ => String::new(),
        };

        let cmd = sanitize_for_display(&req.command);
        let host = sanitize_for_display(&req.host.to_string());
        format!(
            "\n  \x1b[38;5;{color}m{mark}\x1b[0m  \x1b[38;5;{color}m{}\x1b[0m\n  \x1b[38;5;245m{}\x1b[0m{}{}{}",
            cmd, host, prod_note, blast, rule
        )
    }
}

/// 完整 CSI 序列：ESC [ 参数字节 最终字节（含 SGR 颜色码，38;5;196m 等）。
/// **必须放在模块级** —— Rust 不允许 impl 块内的 associated static。
static CSI_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;:?]*[ -/]*[@-~]").expect("static"));

/// 清洗任意进入终端 / 通知的用户可见字符串。
///
/// **模块级自由函数**（Task 7 display.rs 同风格）：banner / ask 裸调用。
/// - 先剔除**完整 CSI 序列**（`\x1b[2K` 清行、`\x1b[38;5;196m` 改色等），
///   只删 ESC 本身会留下 `[2K` 这类残留文本
/// - 剔除全部 C0/C1 控制字符与 ESC（`\x00-\x1f`、`\x7f`、`\u{80}-\u{9f}`）
/// - 剔除不可见 / 双向控制 Unicode（U+200B–U+200F、U+202A–U+202E、U+2066–U+2069）
/// - 折叠换行为空格，单行硬截断到 120 字符
//
// clippy::nonminimal_bool：下方布尔表达式按「每行一条剔除规则」组织（与
// ares-core/display.rs 同款），等价简化会破坏注释对应关系；与业务无关，豁免。
#[allow(clippy::nonminimal_bool)]
fn sanitize_for_display(s: &str) -> String {
    // 1. 先剔除完整 CSI 序列（ESC [ 参数 最终字节）。只删 ESC 本身会留下
    //    `[2K` 这类残留文本，攻击者可用 `\x1b[2K` 擦行后残留肉眼可见的
    //    垃圾字符；必须整段删除。
    let no_csi = CSI_RE.replace_all(s, "");
    // 2. 再剔除全部 C0/C1 控制字符与不可见 Unicode
    let cleaned: String = no_csi
        .chars()
        .filter(|c| {
            let cp = *c as u32;
            !(cp <= 0x1f || cp == 0x7f || (0x80..=0x9f).contains(&cp))
                && !(0x200b..=0x200f).contains(&cp)
                && !(0x202a..=0x202e).contains(&cp)
                && !(0x2066..=0x2069).contains(&cp)
        })
        .collect();
    // 换行/回车折叠为空格（banner 是固定行数布局，命令不允许换行）
    let one_line = cleaned
        .replace(['\n', '\r'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if one_line.chars().count() > 120 {
        let truncated: String = one_line.chars().take(120).collect();
        format!("{truncated}…（已截断，完整命令见审计）")
    } else {
        one_line
    }
}

impl Default for CliApprover {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Approver for CliApprover {
    async fn ask(&self, req: &ApprovalRequest) -> Result<ApprovalResult> {
        match &req.decision {
            Decision::Deny { reason } => Err(AresError::Denied(reason.clone())),

            Decision::Observer | Decision::Auto { .. } => Ok(ApprovalResult::Approved),

            Decision::Confirm { .. } => {
                // 非 TTY（stdin 被重定向 / MCP 场景）下从 stdin 读确认，
                // 等于让「被审批方」替人按 y。fail-closed：直接拒绝。
                // M5 的 MCP 模式走 daemon 的系统通知/弹窗审批，不走这里。
                if !io::stdin().is_terminal() {
                    return Err(AresError::ApprovalRejected);
                }
                println!("{}", Self::banner(req));
                print!("  确认执行？ [y/N] ");
                io::stdout().flush()?;

                let mut line = String::new();
                io::stdin().lock().read_line(&mut line)?;
                let ans = line.trim().to_lowercase();
                if ans != "y" && ans != "yes" {
                    return Ok(ApprovalResult::Rejected);
                }

                // 极高危（spec §14.2）：确认之外还要求输入完整主机名 ——
                // 类似 GitHub 删仓库的确认方式，防止确认被肌肉记忆化。
                if req.require_typed_host {
                    print!(
                        "  极高危操作：请输入完整主机名 {} 确认：",
                        sanitize_for_display(&req.host.to_string())
                    );
                    io::stdout().flush()?;
                    let mut host_line = String::new();
                    io::stdin().lock().read_line(&mut host_line)?;
                    if host_line.trim() != req.host.as_str() {
                        return Ok(ApprovalResult::Rejected);
                    }
                }
                Ok(ApprovalResult::Approved)
            }
        }
    }
}

/// 测试用：总是返回预设答案，不做任何交互。
pub struct AutoApprover {
    pub answer: ApprovalResult,
}

impl AutoApprover {
    pub fn approve_all() -> Self {
        Self {
            answer: ApprovalResult::Approved,
        }
    }
    pub fn reject_all() -> Self {
        Self {
            answer: ApprovalResult::Rejected,
        }
    }
}

#[async_trait]
impl Approver for AutoApprover {
    async fn ask(&self, req: &ApprovalRequest) -> Result<ApprovalResult> {
        // 即使是自动审批器，deny 也必须被拒绝 ——
        // 否则测试里会掩盖掉 deny 的行为
        if let Decision::Deny { reason } = &req.decision {
            return Err(AresError::Denied(reason.clone()));
        }
        Ok(self.answer.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(decision: Decision) -> ApprovalRequest {
        ApprovalRequest {
            host: HostId::localhost(),
            env: Env::Local,
            command: "uptime".into(),
            decision,
            host_count: 1,
            require_typed_host: false,
        }
    }

    fn confirm_req(rule: &str, critical: bool) -> ApprovalRequest {
        req(Decision::Confirm {
            rule: rule.into(),
            critical,
        })
    }

    #[tokio::test]
    async fn observer_and_auto_need_no_interaction() {
        let a = CliApprover::new();
        assert_eq!(
            a.ask(&req(Decision::Observer)).await.unwrap(),
            ApprovalResult::Approved
        );
        assert_eq!(
            a.ask(&req(Decision::Auto { rule: "r".into() }))
                .await
                .unwrap(),
            ApprovalResult::Approved
        );
    }

    #[tokio::test]
    async fn deny_returns_error_not_rejection() {
        // deny 与「用户拒绝」是不同性质的事件，不能混为一谈
        let a = CliApprover::new();
        let err = a
            .ask(&req(Decision::Deny {
                reason: "内置禁止".into(),
            }))
            .await
            .unwrap_err();
        assert!(matches!(err, AresError::Denied(_)));
    }

    #[tokio::test]
    async fn auto_approver_still_honours_deny() {
        let a = AutoApprover::approve_all();
        assert!(a
            .ask(&req(Decision::Deny { reason: "x".into() }))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn auto_approver_returns_preset_answer() {
        assert_eq!(
            AutoApprover::approve_all()
                .ask(&confirm_req("r", false))
                .await
                .unwrap(),
            ApprovalResult::Approved
        );
        assert_eq!(
            AutoApprover::reject_all()
                .ask(&confirm_req("r", false))
                .await
                .unwrap(),
            ApprovalResult::Rejected
        );
    }

    #[test]
    fn banner_uses_only_foreground_colors() {
        // 背景色会遮挡用户的终端背景图 —— 这是硬约束
        let b = CliApprover::banner(&confirm_req("无放行规则", false));
        assert!(b.contains("\x1b[38;5;"), "应使用 38;5 前景色");
        assert!(!b.contains("\x1b[48;"), "不得使用 48 开头的背景色");
        assert!(!b.contains("\x1b[7m"), "不得使用反显填充整行");
    }

    #[test]
    fn banner_strips_injected_control_characters() {
        // 对抗性测试：LLM 可被注入控制，命令串里可能塞 ANSI 光标控制、
        // 换行伪造横幅、双向控制字符。banner 输出必须不含任何 ESC 字节
        //（模板自身的 38;5 颜色码除外）且不产生新行。
        let mut r = confirm_req("无放行规则", false);
        r.command =
            "uptime\x1b[2K\r  \x1b[38;5;245m·  uptime\x1b[0m\n\n\x1b[1A伪造横幅\u{202e}evil".into();
        let b = CliApprover::banner(&r);

        // 除模板自身的 38;5 颜色码与 0m 复位码外，不应出现其他 ESC 序列。
        // 注意：`str::matches` 返回的是**被匹配的子串本身**（不是匹配后的
        // 剩余文本），对它做 chars().skip() 毫无意义 —— 必须用 find 循环
        // 或正则提取完整序列后判断。
        let mut pos = 0;
        while let Some(idx) = b[pos..].find("\x1b[") {
            let start = pos + idx;
            let tail: String = b[start..].chars().take(10).collect();
            let seg: String = tail.chars().take_while(|&c| c != '\n').collect();
            assert!(
                // 前缀判断即可：模板序列只有 38;5; 颜色码与 0m 复位码两种
                seg.starts_with("\x1b[38;5;") || seg.starts_with("\x1b[0m"),
                "banner 中出现了非模板的 ESC 序列：{seg:?}"
            );
            pos = start + 1;
        }
        // 换行被折叠：banner 结构固定为有限行数
        assert_eq!(b.lines().count(), 3, "banner 行数必须固定（\n 折叠后）");
        // 双向控制字符被剔除
        assert!(!b.contains('\u{202e}'));
    }

    #[test]
    fn sanitize_for_display_strips_all_control_chars() {
        let s = sanitize_for_display("a\x1b[2Kb\x00c\x7fd\u{200b}e\u{202a}f");
        assert_eq!(s, "abcdef");
        assert!(!s.contains('\u{1b}'));
    }

    #[test]
    fn banner_highlights_prod_and_blast_radius() {
        let mut r = confirm_req("prod 环境的变更操作", true);
        r.env = Env::Prod;
        r.host_count = 12;
        let b = CliApprover::banner(&r);

        assert!(b.contains("⚑"));
        assert!(b.contains("prod"));
        assert!(b.contains("12 台主机"));
        assert!(b.contains("prod 环境的变更操作"));
        assert!(b.contains("极高危"));
    }

    #[test]
    fn audit_label_reflects_outcome() {
        let d = Decision::Confirm {
            rule: "r".into(),
            critical: false,
        };
        assert_eq!(ApprovalResult::Approved.audit_label(&d), "confirm");
        assert_eq!(ApprovalResult::Rejected.audit_label(&d), "rejected");
        assert_eq!(ApprovalResult::Timeout.audit_label(&d), "timeout");
    }
}
