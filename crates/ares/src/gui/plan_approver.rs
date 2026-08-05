//! 计划审批模式（2026-08-05 批次6：多命令计划列表审批）。
//!
//! 用户拍板：下达目标后，agent 把要执行的命令全部罗列出来，用户可以
//! **逐条批准/拒绝**（选中命令单独执行），或全部批准/全部拒绝。
//!
//! 实现：`PlanApprover` 把审批请求推进队列并阻塞等待；GUI 侧显示
//! 计划面板（非弹窗），用户对每条命令单独放行（respond channel 回传）。
//! 与 `GuiApprover`（单命令弹窗）互斥：plan 模式开启时用本 approver。
//!
//! 限制（第一批）：命令文本不可编辑（编辑需要改 handle_tool_call 的
//! prepare→ask→execute 数据流，第二批做）；注释仅展示。

use ares_agent::{ApprovalRequest, ApprovalResult, Approver};
use ares_core::{AresError, Result};
use async_trait::async_trait;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Mutex;

/// 计划中的一条审批请求。
pub struct PlanItem {
    pub req: ApprovalRequest,
    pub respond: Sender<ApprovalResult>,
}

/// 计划审批器：请求入队 + 阻塞等待 GUI 逐条放行。
pub struct PlanApprover {
    /// GUI 主线程轮询取走
    pub rx: Mutex<Receiver<PlanItem>>,
    tx: Sender<PlanItem>,
}

impl PlanApprover {
    /// 构造（approver + GUI 接收端）。
    pub fn new() -> (Self, Receiver<PlanItem>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (
            Self {
                rx: Mutex::new(std::sync::mpsc::channel().1),
                tx,
            },
            rx,
        )
    }
}

#[async_trait]
impl Approver for PlanApprover {
    async fn ask(&self, req: &ApprovalRequest) -> Result<ApprovalResult> {
        let (respond_tx, respond_rx) = std::sync::mpsc::channel();
        self.tx
            .send(PlanItem {
                req: req.clone(),
                respond: respond_tx,
            })
            .map_err(|_| AresError::ApprovalRejected)?;

        // 阻塞等待 GUI 对该条的放行（60s 超时自动拒绝）
        match respond_rx.recv_timeout(std::time::Duration::from_secs(300)) {
            Ok(result) => Ok(result),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(ApprovalResult::Timeout),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(AresError::ApprovalRejected)
            }
        }
    }
}

/// 批量放行辅助：向所有已排队条目发送结果（GUI 全部批准/拒绝用）。
pub fn settle_all(items: Vec<PlanItem>, result: ApprovalResult) {
    for it in items {
        let _ = it.respond.send(result.clone());
    }
}
