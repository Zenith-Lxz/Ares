//! GUI 审批呈现：确认弹窗桥接。
//!
//! agent 的 `ask()` 在 tokio 线程中运行 —— 把请求发给 GUI 主线程（egui
//! 每帧轮询），用户点击「批准/拒绝」后经 channel 回传。GUI 关闭视为拒绝
//! （fail-closed）。

use ares_agent::{ApprovalRequest, ApprovalResult, Approver};
use ares_core::{AresError, Result};
use async_trait::async_trait;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

/// 审批弹窗最长等待：超时自动按拒绝处理（fail-closed，2026-08-05）。
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);

/// 待用户决策的审批请求（GUI 侧持有）。
pub struct PendingApproval {
    pub req: ApprovalRequest,
    pub respond: Sender<ApprovalResult>,
}

pub struct GuiApprover {
    tx: Sender<PendingApproval>,
}

impl GuiApprover {
    /// 构建完整的 (approver, GUI 接收端) 对：approver 交给 agent，
    /// 接收端由 GUI 主线程每帧轮询。
    pub fn pair() -> (Self, Receiver<PendingApproval>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (Self { tx }, rx)
    }
}

#[async_trait]
impl Approver for GuiApprover {
    async fn ask(&self, req: &ApprovalRequest) -> Result<ApprovalResult> {
        let (respond_tx, respond_rx) = std::sync::mpsc::channel();
        self.tx
            .send(PendingApproval {
                req: req.clone(),
                respond: respond_tx,
            })
            .map_err(|_| AresError::ApprovalRejected)?;

        // 阻塞等待用户决策（60s 超时自动拒绝，fail-closed）；GUI 关闭 → 拒绝
        match respond_rx.recv_timeout(APPROVAL_TIMEOUT) {
            Ok(result) => Ok(result),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(ApprovalResult::Timeout),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(AresError::ApprovalRejected)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_creates_working_bridge() {
        let (approver, rx) = GuiApprover::pair();
        // 模拟 GUI：收到请求后批准
        std::thread::spawn(move || {
            let p = rx.recv().unwrap();
            p.respond.send(ApprovalResult::Approved).unwrap();
        });

        let rt = tokio::runtime::Runtime::new().unwrap();
        let req = ApprovalRequest {
            host: "test-host".into(),
            command: "rm -rf /tmp/x".into(),
            decision: ares_core::Decision::Confirm {
                rule: "变更操作".into(),
                critical: false,
            },
            env: ares_core::Env::Dev,
            host_count: 1,
            require_typed_host: false,
        };
        let result = rt.block_on(approver.ask(&req)).unwrap();
        assert_eq!(result, ApprovalResult::Approved);
    }
}
