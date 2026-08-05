//! 简易 iTerm2：eframe GUI 应用。
//!
//! 布局：顶部 Tab 栏（多 ssh 会话）· 中央终端渲染区 · 右侧可折叠
//! Agent 面板（Ctrl-a a）· 底部状态栏。主机选择弹窗读 `~/.ssh/config`。

use crate::gui::approver::{GuiApprover, PendingApproval};
use crate::gui::exec::TerminalSessionExecutor;
use crate::gui::session::{ConnTarget, Session};
use crate::gui::term;
use ares_agent::{AgentLoop, ApprovalResult, TurnResult};
use ares_core::config::{HostEntry, HostsConfig};
use ares_core::ssh_config::{self, SshHost};
use ares_core::{Decision, Env};
use egui::{Color32, FontFamily, FontId, RichText};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

const MONO: FontId = FontId::monospace(14.0);

/// 「添加主机」弹窗的表单字段。
#[derive(Default)]
struct AddHostFields {
    name: String,
    hostname: String,
    user: String,
    port: String,
    env: String,
    tags: String,
}

pub struct GuiApp {
    /// ARES 主机簿（hosts.toml 独立维护，2026-08-05 起不再直接读 ssh_config）。
    hosts: std::collections::BTreeMap<String, HostEntry>,
    /// 导入源：ssh_config 主机（一次性导入进主机簿）。
    ssh_imports: Vec<SshHost>,
    tabs: Vec<Session>,
    active: usize,
    /// 当前 tab 的终端尺寸（用于 resize 检测）。
    last_size: Option<(u16, u16)>,
    picking: bool,
    filter: String,
    filter_selected: Option<usize>,
    add_modal: bool,
    add_fields: AddHostFields,
    import_modal: bool,
    import_selected: std::collections::BTreeSet<String>,
    agent_open: bool,
    agent: Option<AgentBridge>,
    rt: Arc<tokio::runtime::Runtime>,
    /// GUI 审批通道：agent 线程 → GUI 主线程
    approve_rx: std::sync::mpsc::Receiver<PendingApproval>,
    pending_approval: Option<PendingApproval>,
}

struct AgentBridge {
    agent: Arc<tokio::sync::Mutex<AgentLoop>>,
    messages: Vec<(String, String)>,
    input: String,
    busy: bool,
    tx: Sender<AgentEvent>,
    rx: Receiver<AgentEvent>,
}

enum AgentEvent {
    Turn(Result<TurnResult, String>),
}

impl GuiApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: Arc<tokio::runtime::Runtime>) -> Self {
        install_fonts(&cc.egui_ctx);
        // 主机簿：hosts.toml 独立维护；ssh_config 仅作导入源
        let hosts = HostsConfig::load().map(|c| c.hosts).unwrap_or_default();
        let ssh_imports = ssh_config::load();
        Self {
            hosts,
            ssh_imports,
            tabs: Vec::new(),
            active: 0,
            last_size: None,
            picking: true, // 启动即打开主机选择
            filter: String::new(),
            filter_selected: None,
            add_modal: false,
            add_fields: AddHostFields::default(),
            import_modal: false,
            import_selected: std::collections::BTreeSet::new(),
            agent_open: false,
            agent: None,
            rt,
            approve_rx: std::sync::mpsc::channel().1,
            pending_approval: None,
        }
    }

    /// 打开一个会话 tab（主机簿条目 → 连接参数）。
    fn open_tab(&mut self, key: &str) {
        let Some(entry) = self.hosts.get(key).cloned() else {
            eprintln!("主机簿中没有 {key}");
            return;
        };
        let target = ConnTarget {
            hostname: entry.hostname.clone(),
            user: if entry.user.is_empty() {
                None
            } else {
                Some(entry.user.clone())
            },
            port: entry.port,
        };
        match Session::open(key, &target, 24, 80) {
            Ok(s) => {
                self.tabs.push(s);
                self.active = self.tabs.len() - 1;
                self.picking = false;
            }
            Err(e) => eprintln!("无法打开 {key}：{e}"),
        }
    }

    /// 保存主机簿到 hosts.toml（失败仅告警，不打断 GUI）。
    fn save_hosts(&self) {
        let cfg = HostsConfig {
            hosts: self.hosts.clone(),
        };
        if let Err(e) = cfg.save() {
            eprintln!("保存主机簿失败：{e}");
        }
    }

    /// 新增/更新主机并落盘。
    fn add_host(&mut self, key: String, entry: HostEntry) {
        self.hosts.insert(key, entry);
        self.save_hosts();
    }

    /// 关闭当前 tab。
    fn close_active(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.tabs.remove(self.active);
        if self.active >= self.tabs.len() && !self.tabs.is_empty() {
            self.active = self.tabs.len() - 1;
        }
        if self.tabs.is_empty() {
            self.agent = None;
            self.picking = true;
        }
    }

    /// 转发输入事件到当前会话（拦截全局快捷键）。
    fn handle_input(&mut self, ctx: &egui::Context) {
        let events = ctx.input(|i| i.events.clone());
        for ev in events {
            match ev {
                egui::Event::Text(t) => {
                    if let Some(s) = self.tabs.get(self.active) {
                        s.write(t.as_bytes());
                    }
                }
                egui::Event::Key { key, modifiers, .. } => {
                    let ctrl = modifiers.ctrl;
                    // 全局快捷键
                    if ctrl {
                        match key {
                            egui::Key::T => {
                                self.picking = true;
                                continue;
                            }
                            egui::Key::W => {
                                self.close_active();
                                continue;
                            }
                            egui::Key::A => {
                                // Ctrl-a a 切 Agent 面板（egui 事件流里 Ctrl+a
                                // 与后续 a 分离，这里直接以 Ctrl-a 唤起）
                                self.toggle_agent_simple();
                                continue;
                            }
                            _ => {}
                        }
                    }
                    // 转发给当前会话（可打印字符已走 Event::Text）
                    if let Some(s) = self.tabs.get(self.active) {
                        if let Some(bytes) = key_bytes(key, ctrl) {
                            s.write(&bytes);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn toggle_agent_simple(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.agent_open = !self.agent_open;
        if self.agent_open && self.agent.is_none() {
            let host = self.tabs[self.active].alias.clone();
            // 终端注入执行器：agent 的命令直接写进当前终端会话
            // （Session 内部状态全 Arc 共享，clone 后注入的是同一个会话）
            let executor = Arc::new(TerminalSessionExecutor::new(self.tabs[self.active].clone()));
            // GUI 审批通道
            let (approver, rx) = GuiApprover::pair();
            self.approve_rx = rx;
            match self.rt.block_on(crate::build_agent(
                vec![ares_core::HostId::new(host.clone())],
                executor,
                Arc::new(approver),
            )) {
                Ok(agent) => {
                    self.agent = Some(AgentBridge::new(agent));
                }
                Err(e) => {
                    eprintln!("Agent 面板启动失败：{e}");
                    self.agent_open = false;
                }
            }
        }
    }
}

impl AgentBridge {
    fn new(agent: AgentLoop) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            agent: Arc::new(tokio::sync::Mutex::new(agent)),
            messages: vec![(
                "system".into(),
                "Agent 已就绪。输入运维目标，将针对当前主机执行（变更操作需确认）。".into(),
            )],
            input: String::new(),
            busy: false,
            tx,
            rx,
        }
    }

    fn send(&mut self, rt: &Arc<tokio::runtime::Runtime>) {
        if self.busy || self.input.trim().is_empty() {
            return;
        }
        let text = self.input.trim().to_string();
        self.input.clear();
        self.messages.push(("user".into(), text.clone()));
        self.busy = true;

        let agent = Arc::clone(&self.agent);
        let tx = self.tx.clone();
        rt.spawn(async move {
            let result = {
                let mut a = agent.lock().await;
                a.run_turn(&text).await
            };
            tx.send(AgentEvent::Turn(result.map_err(|e| e.to_string())))
                .ok();
        });
    }

    /// 每帧收取完成的任务。
    fn poll(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                AgentEvent::Turn(Ok(r)) => {
                    let mut body = r.reply.clone();
                    for run in &r.tool_runs {
                        body = format!(
                            "{}\n[{}] {} {}\n{}",
                            body,
                            run.decision_label,
                            run.tool,
                            run.command.clone().unwrap_or_default(),
                            run.display
                        );
                    }
                    self.messages.push(("assistant".into(), body));
                }
                AgentEvent::Turn(Err(e)) => {
                    self.messages
                        .push(("assistant".into(), format!("错误：{e}")));
                }
            }
            self.busy = false;
        }
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 处理输入（转发 / 快捷键）
        self.handle_input(ctx);

        // Agent 面板任务轮询
        if let Some(a) = &mut self.agent {
            a.poll();
        }

        // 审批轮询：agent 线程发来的确认请求 → 弹窗
        while let Ok(p) = self.approve_rx.try_recv() {
            self.pending_approval = Some(p);
        }
        if let Some(p) = &self.pending_approval {
            let mut approve = false;
            let mut reject = false;
            egui::Window::new("操作确认")
                .collapsible(false)
                .resizable(false)
                .default_size([480.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new(format!("主机：{}", p.req.host))
                            .color(Color32::from_rgb(179, 146, 74)),
                    );
                    if let Decision::Confirm { rule, critical } = &p.req.decision {
                        let tag = if *critical {
                            "  ☢ 极高危（需输入主机名）"
                        } else {
                            ""
                        };
                        ui.label(format!("规则：{rule}{tag}"));
                    } else {
                        ui.label(format!("判定：{}", decision_label(&p.req.decision)));
                    }
                    ui.add_space(4.0);
                    ui.monospace(ares_core::display::sanitize(&p.req.command));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        approve = ui.button(RichText::new("批准执行").strong()).clicked();
                        reject = ui.button("拒绝").clicked();
                    });
                });
            if approve {
                let _ = p.respond.send(ApprovalResult::Approved);
                self.pending_approval = None;
            } else if reject {
                let _ = p.respond.send(ApprovalResult::Rejected);
                self.pending_approval = None;
            }
        }

        // ── 顶部：Tab 栏 ──
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let mut to_close: Option<usize> = None;
                let mut to_activate: Option<usize> = None;
                for (i, s) in self.tabs.iter().enumerate() {
                    let selected = i == self.active;
                    let label = if s.is_exited() {
                        format!("✗ {}", s.alias)
                    } else {
                        s.alias.clone()
                    };
                    let btn = ui.selectable_label(selected, label);
                    if btn.clicked() {
                        to_activate = Some(i);
                    }
                    // 关闭按钮（选中 tab 显示 ×）
                    if selected && ui.small_button("×").clicked() {
                        to_close = Some(i);
                    }
                }
                if ui.button("+").clicked() {
                    self.picking = true;
                }
                if let Some(i) = to_activate {
                    self.active = i;
                }
                if let Some(i) = to_close {
                    self.active = i;
                    self.close_active();
                }
                // Agent 面板开关
                if ui
                    .selectable_label(self.agent_open, RichText::new("Agent").strong())
                    .clicked()
                {
                    self.toggle_agent_simple();
                }
            });
        });

        // ── 底部：状态栏 ──
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(s) = self.tabs.get(self.active) {
                    ui.label(
                        RichText::new(format!(
                            "{} · {}",
                            s.alias,
                            if s.is_exited() {
                                "已退出"
                            } else {
                                "已连接"
                            }
                        ))
                        .color(Color32::from_rgb(179, 146, 74)),
                    );
                } else {
                    ui.label(RichText::new("无会话").color(Color32::GRAY));
                }
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "{} 个会话 · Ctrl-T 新会话 · Ctrl-a a Agent",
                        self.tabs.len()
                    ))
                    .color(Color32::GRAY),
                );
            });
        });

        // ── 右侧：Agent 面板 ──
        if self.agent_open {
            egui::SidePanel::right("agent_panel")
                .resizable(true)
                .default_width(380.0)
                .show(ctx, |ui| {
                    ui.heading("Agent");
                    ui.label(
                        RichText::new(format!(
                            "当前主机：{}",
                            self.tabs
                                .get(self.active)
                                .map(|s| s.alias.clone())
                                .unwrap_or_default()
                        ))
                        .color(Color32::GRAY),
                    );
                    ui.separator();

                    if let Some(a) = &mut self.agent {
                        // 消息区
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                for (role, text) in &a.messages {
                                    let color = match role.as_str() {
                                        "user" => Color32::from_rgb(107, 179, 74),
                                        "system" => Color32::GRAY,
                                        _ => Color32::from_rgb(220, 220, 220),
                                    };
                                    ui.label(RichText::new(format!("[{role}]")).color(color));
                                    for line in text.lines() {
                                        ui.label(line);
                                    }
                                    ui.add_space(4.0);
                                }
                            });
                        // 输入区
                        ui.separator();
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut a.input)
                                .hint_text("运维目标…（Enter 发送）")
                                .desired_width(f32::INFINITY),
                        );
                        let enter =
                            resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if enter && !a.busy {
                            a.send(&self.rt);
                        }
                        let mut send_clicked = false;
                        if a.busy {
                            ui.label(RichText::new("…").color(Color32::GRAY));
                        } else {
                            send_clicked = ui.button("发送").clicked();
                        }
                        if send_clicked {
                            a.send(&self.rt);
                        }
                        if enter {
                            resp.request_focus();
                        }
                    } else {
                        ui.label("Agent 未就绪（需配置 provider）。");
                    }
                });
        }

        // ── 中央：终端渲染 ──
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(s) = self.tabs.get(self.active) {
                // 尺寸变化 → resize 会话
                let (rows, cols) = term::size_for(ui, &MONO);
                if self.last_size != Some((rows, cols)) {
                    s.resize(rows, cols);
                    self.last_size = Some((rows, cols));
                }
                let screen = s.screen();
                term::draw_terminal(ui, &screen, MONO);
                ui.allocate_space(ui.available_size());
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("按 + 或 Ctrl-T 选择主机连接").color(Color32::GRAY));
                });
            }
        });

        // ── 主机选择弹窗 ──
        if self.picking {
            let mut close = false;
            let mut open_add = false;
            let mut open_import = false;
            egui::Window::new("选择主机")
                .default_size([460.0, 440.0])
                .collapsible(false)
                .resizable(true)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("{} 台主机（hosts.toml）", self.hosts.len()))
                                .color(Color32::GRAY),
                        );
                        if ui.button("+ 添加").clicked() {
                            open_add = true;
                        }
                        if ui.button("从 ssh_config 导入").clicked() {
                            open_import = true;
                        }
                    });
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("过滤（名称 / 地址 / 用户）")
                            .desired_width(f32::INFINITY),
                    );
                    ui.separator();
                    let f = self.filter.to_lowercase();
                    let vis: Vec<String> = self
                        .hosts
                        .iter()
                        .filter(|(k, e)| {
                            f.is_empty()
                                || k.to_lowercase().contains(&f)
                                || e.hostname.to_lowercase().contains(&f)
                                || e.user.to_lowercase().contains(&f)
                        })
                        .map(|(k, _)| k.clone())
                        .collect();

                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (i, key) in vis.iter().enumerate() {
                                let e = &self.hosts[key];
                                let target = e.connect_target(key);
                                let label = format!("{key}  ·  {target}");
                                let row =
                                    ui.selectable_label(self.filter_selected == Some(i), label);
                                if row.double_clicked() {
                                    self.open_tab(key);
                                    close = true;
                                    break;
                                }
                                if row.clicked() {
                                    self.filter_selected = Some(i);
                                }
                            }
                            if vis.is_empty() {
                                ui.label(
                                    RichText::new(
                                        "主机簿为空 —— 点「添加」或「从 ssh_config 导入」",
                                    )
                                    .color(Color32::GRAY),
                                );
                            }
                        });
                    ui.separator();
                    ui.horizontal(|ui| {
                        let connect = ui.button("连接").clicked();
                        if connect
                            && self.filter_selected.is_some()
                            && self.filter_selected.unwrap() < vis.len()
                        {
                            let key = vis[self.filter_selected.unwrap()].clone();
                            self.open_tab(&key);
                            close = true;
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                });
            if open_add {
                self.add_modal = true;
            }
            if open_import {
                self.import_modal = true;
                self.import_selected.clear();
            }
            if close {
                self.picking = false;
            }
        }

        // ── 添加主机弹窗 ──
        if self.add_modal {
            let mut close = false;
            egui::Window::new("添加主机")
                .collapsible(false)
                .resizable(false)
                .default_size([380.0, 0.0])
                .show(ctx, |ui| {
                    let mut f = std::mem::take(&mut self.add_fields);
                    egui::Grid::new("add_host_grid")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("名称*");
                            ui.text_edit_singleline(&mut f.name);
                            ui.end_row();
                            ui.label("地址 / IP");
                            ui.text_edit_singleline(&mut f.hostname);
                            ui.end_row();
                            ui.label("用户");
                            ui.text_edit_singleline(&mut f.user);
                            ui.end_row();
                            ui.label("端口");
                            ui.text_edit_singleline(&mut f.port);
                            ui.end_row();
                            ui.label("环境");
                            ui.text_edit_singleline(&mut f.env);
                            ui.end_row();
                            ui.label("标签(逗号分隔)");
                            ui.text_edit_singleline(&mut f.tags);
                            ui.end_row();
                        });
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("名称必填；地址留空则用名称连接（ssh 别名语义）")
                            .color(Color32::GRAY),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("保存").clicked() {
                            let name = f.name.trim().to_string();
                            if name.is_empty() {
                                eprintln!("主机名称不能为空");
                                f = AddHostFields::default();
                            } else {
                                let mut entry = HostEntry {
                                    hostname: f.hostname.trim().to_string(),
                                    user: f.user.trim().to_string(),
                                    port: f.port.trim().parse::<u16>().ok(),
                                    ..Default::default()
                                };
                                match f.env.trim().to_lowercase().as_str() {
                                    "prod" => entry.env = Env::Prod,
                                    "staging" => entry.env = Env::Staging,
                                    "dev" => entry.env = Env::Dev,
                                    "local" => entry.env = Env::Local,
                                    _ => entry.env = Env::Unknown,
                                }
                                entry.tags = f
                                    .tags
                                    .split(',')
                                    .map(|t| t.trim().to_string())
                                    .filter(|t| !t.is_empty())
                                    .collect();
                                let key = name.clone();
                                self.add_host(key, entry);
                                close = true;
                            }
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                    self.add_fields = f;
                });
            if close {
                self.add_modal = false;
            }
        }

        // ── 从 ssh_config 导入弹窗 ──
        if self.import_modal {
            let mut close = false;
            egui::Window::new("从 ssh_config 导入")
                .collapsible(false)
                .default_size([440.0, 400.0])
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} 台可导入（勾选后写入主机簿，之后与 ssh_config 无关）",
                            self.ssh_imports.len()
                        ))
                        .color(Color32::GRAY),
                    );
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for h in &self.ssh_imports {
                                let target = match &h.user {
                                    Some(u) => format!("{u}@{}", h.hostname),
                                    None => h.hostname.clone(),
                                };
                                let mut label = format!("{}  ·  {target}", h.alias);
                                if let Some(p) = h.port {
                                    if p != 22 {
                                        label = format!("{label} :{p}");
                                    }
                                }
                                if self.hosts.contains_key(&h.alias) {
                                    label = format!("{label}  ✓已导入");
                                }
                                let mut checked = self.import_selected.contains(&h.alias);
                                if ui.checkbox(&mut checked, label).changed() {
                                    if checked {
                                        self.import_selected.insert(h.alias.clone());
                                    } else {
                                        self.import_selected.remove(&h.alias);
                                    }
                                }
                            }
                        });
                    ui.separator();
                    ui.horizontal(|ui| {
                        let import = ui
                            .button(format!("导入 {} 台", self.import_selected.len()))
                            .clicked();
                        if import {
                            let keys: Vec<String> = self.import_selected.iter().cloned().collect();
                            for k in keys {
                                if let Some(h) = self.ssh_imports.iter().find(|h| h.alias == k) {
                                    // 已存在的主机不覆盖（用户手动维护的优先）
                                    self.hosts.entry(k).or_insert_with(|| HostEntry {
                                        hostname: h.hostname.clone(),
                                        user: h.user.clone().unwrap_or_default(),
                                        port: h.port,
                                        ..Default::default()
                                    });
                                }
                            }
                            self.save_hosts();
                            self.import_selected.clear();
                            close = true;
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                });
            if close {
                self.import_modal = false;
            }
        }

        // 无 tab 且未在选择 → 打开选择
        if self.tabs.is_empty() && !self.picking {
            self.picking = true;
        }
    }
}

/// 键盘事件 → 终端字节（egui Key → 转义序列 / 控制字符）。
/// 可打印字符由 `Event::Text` 承载转发，这里只处理控制/特殊键与 Ctrl 组合。
fn key_bytes(key: egui::Key, ctrl: bool) -> Option<Vec<u8>> {
    use egui::Key;
    // Ctrl+字母/数字 → 控制字符（Ctrl+C=0x03、Ctrl+Z=0x1a 等）
    if ctrl {
        if let Some(c) = key_char(key) {
            if c.is_ascii_lowercase() {
                return Some(vec![(c as u8) - b'a' + 1]);
            }
            if c.is_ascii_uppercase() {
                return Some(vec![(c as u8) - b'A' + 1]);
            }
        }
        if key == Key::Space {
            return Some(vec![0]);
        }
    }
    // 特殊键
    match key {
        Key::Enter => Some(b"\r".to_vec()),
        Key::Backspace => Some(b"\x7f".to_vec()),
        Key::Tab => Some(b"\t".to_vec()),
        Key::ArrowUp => Some(b"\x1b[A".to_vec()),
        Key::ArrowDown => Some(b"\x1b[B".to_vec()),
        Key::ArrowRight => Some(b"\x1b[C".to_vec()),
        Key::ArrowLeft => Some(b"\x1b[D".to_vec()),
        Key::Home => Some(b"\x1b[H".to_vec()),
        Key::End => Some(b"\x1b[F".to_vec()),
        Key::Delete => Some(b"\x1b[3~".to_vec()),
        Key::PageUp => Some(b"\x1b[5~".to_vec()),
        Key::PageDown => Some(b"\x1b[6~".to_vec()),
        Key::Escape => Some(b"\x1b".to_vec()),
        _ => None,
    }
}

/// egui::Key → 字符（字母/数字键；标点走 Event::Text 不在此列）。
fn key_char(key: egui::Key) -> Option<char> {
    use egui::Key;
    Some(match key {
        Key::A => 'a',
        Key::B => 'b',
        Key::C => 'c',
        Key::D => 'd',
        Key::E => 'e',
        Key::F => 'f',
        Key::G => 'g',
        Key::H => 'h',
        Key::I => 'i',
        Key::J => 'j',
        Key::K => 'k',
        Key::L => 'l',
        Key::M => 'm',
        Key::N => 'n',
        Key::O => 'o',
        Key::P => 'p',
        Key::Q => 'q',
        Key::R => 'r',
        Key::S => 's',
        Key::T => 't',
        Key::U => 'u',
        Key::V => 'v',
        Key::W => 'w',
        Key::X => 'x',
        Key::Y => 'y',
        Key::Z => 'z',
        Key::Num0 => '0',
        Key::Num1 => '1',
        Key::Num2 => '2',
        Key::Num3 => '3',
        Key::Num4 => '4',
        Key::Num5 => '5',
        Key::Num6 => '6',
        Key::Num7 => '7',
        Key::Num8 => '8',
        Key::Num9 => '9',
        _ => return None,
    })
}

/// 字体：等宽 Monaco + 中文 PingFang fallback。
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    // 等宽放最前（终端渲染）
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/Monaco.ttf") {
        fonts.font_data.insert(
            "mono".into(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        fonts
            .families
            .get_mut(&FontFamily::Monospace)
            .unwrap()
            .insert(0, "mono".into());
    }
    // 中文 fallback（ttc 取首 face；若失败则中文输出为占位符，可接受）
    for path in [
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Helvetica.ttc",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(
                "cjk".into(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts.families.get_mut(&family).unwrap().push("cjk".into());
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
}

/// Decision 的简短标签（确认弹窗展示用）。
fn decision_label(d: &Decision) -> &'static str {
    match d {
        Decision::Deny { .. } => "deny（已禁止）",
        Decision::Confirm { critical: true, .. } => "confirm（极高危）",
        Decision::Confirm { .. } => "confirm",
        Decision::Observer => "observer（只读）",
        Decision::Auto { .. } => "auto（自动）",
    }
}
