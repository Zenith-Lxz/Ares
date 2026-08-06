//! 简易 iTerm2：eframe GUI 应用。
//!
//! 布局：顶部 Tab 栏（多 ssh 会话）· 中央终端渲染区 · 右侧可折叠
//! Agent 面板（Ctrl-a a）· 底部状态栏。主机选择弹窗读 `~/.ssh/config`。

use crate::gui::approver::{GuiApprover, PendingApproval};
use crate::gui::exec::{RoutedExecutor, TerminalSessionExecutor};
use crate::gui::plan_approver::{settle_all, PlanApprover, PlanItem};
use crate::gui::session::{ConnTarget, Session};
use crate::gui::sftp::SftpPanel;
use crate::gui::term;
use crate::mcp::McpManager;
use ares_agent::{AgentLoop, ApprovalResult, Approver, TurnResult};
use ares_core::config::{HostEntry, HostsConfig};
use ares_core::ssh_config::{self, SshHost};
use ares_core::{Decision, Env, HostId};
use ares_llm::config::{ProviderEntry, ProviderKind, ProvidersConfig};
use ares_tools::Tool;
use egui::{Color32, FontFamily, FontId, RichText};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

fn mono_font(size: f32) -> FontId {
    FontId::monospace(size)
}

/// 分割方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Split {
    /// 上下分（两个 pane 水平堆叠）
    Horizontal,
    /// 左右分（两个 pane 垂直堆叠）
    Vertical,
}

/// 一个终端 tab 的工作区：1~2 个 pane（分屏）。
struct TermWorkspace {
    sessions: Vec<Session>,
    active: usize,
    split: Option<Split>,
    /// 每个 pane 上次渲染尺寸（resize 检测）。
    last_sizes: Vec<Option<(u16, u16)>>,
    /// 主机指定的主题名（主题随主机切换；None = 跟随全局）
    theme: Option<String>,
}

impl TermWorkspace {
    fn new(s: Session) -> Self {
        Self::with_theme(s, None)
    }

    fn with_theme(s: Session, theme: Option<String>) -> Self {
        Self {
            sessions: vec![s],
            active: 0,
            split: None,
            last_sizes: vec![None],
            theme,
        }
    }

    fn title(&self) -> &str {
        &self.sessions[self.active].alias
    }

    fn is_exited(&self) -> bool {
        self.sessions.iter().any(|s| s.is_exited())
    }

    /// 全部 pane 是否已收到首笔数据（连接中反馈）。
    fn is_connected(&self) -> bool {
        !self.sessions.is_empty() && self.sessions.iter().all(|s| s.is_connected())
    }

    /// 分屏：把新会话加为第二个 pane。已有 split 时替换第二个。
    fn split_with(&mut self, s: Session, dir: Split) {
        if self.sessions.len() == 2 {
            self.sessions[1] = s;
        } else {
            self.sessions.push(s);
        }
        self.split = Some(dir);
        self.active = self.sessions.len() - 1;
        self.last_sizes = vec![None; self.sessions.len()];
    }

    /// 关闭当前 pane；剩一个时取消 split。
    fn close_active_pane(&mut self) {
        if self.sessions.len() == 1 {
            return;
        }
        self.sessions.remove(self.active);
        if self.active >= self.sessions.len() {
            self.active = 0;
        }
        self.split = None;
        self.last_sizes = vec![None; self.sessions.len()];
    }
}

/// 一个 tab：终端工作区、SFTP 面板或主机列表页。
enum Tab {
    Term(TermWorkspace),
    Sftp(SftpPanel),
    /// 主机列表页（极简 UI：不弹窗，作为一个可关闭的 tab）
    Picker,
}

impl Tab {
    fn title(&self) -> &str {
        match self {
            Tab::Term(w) => w.title(),
            Tab::Sftp(p) => &p.title,
            Tab::Picker => "主机",
        }
    }
}

/// 「添加主机」弹窗的表单字段。
#[derive(Default)]
struct AddHostFields {
    name: String,
    hostname: String,
    user: String,
    port: String,
    env: String,
    tags: String,
    /// key | password（密码主机：密码存 keychain ssh-pw:<name>）
    auth: String,
    password: String,
    /// 主机主题（可选：主题随主机切换；空 = 跟随全局）
    theme: String,
}

/// 「模型设置」弹窗的表单字段。
struct SettingsFields {
    name: String,
    base_url: String,
    model: String,
    api_key: String,
}

impl Default for SettingsFields {
    fn default() -> Self {
        Self {
            name: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-chat".into(),
            api_key: String::new(),
        }
    }
}

pub struct GuiApp {
    /// ARES 主机簿（hosts.toml 独立维护，2026-08-05 起不再直接读 ssh_config）。
    hosts: std::collections::BTreeMap<String, HostEntry>,
    /// 导入源：ssh_config 主机（一次性导入进主机簿）。
    ssh_imports: Vec<SshHost>,
    tabs: Vec<Tab>,
    active: usize,
    /// 当前 tab 的终端尺寸（用于 resize 检测）。
    last_size: Option<(u16, u16)>,
    /// resize 节流：150ms 内只允许一次（防 SIGWINCH 风暴）。
    last_resize: std::time::Instant,
    picking: bool,
    filter: String,
    filter_selected: Option<usize>,
    add_modal: bool,
    add_fields: AddHostFields,
    import_modal: bool,
    import_selected: std::collections::BTreeSet<String>,
    settings_modal: bool,
    settings_fields: SettingsFields,
    error_toast: Option<String>,
    agent_open: bool,
    /// 分屏待定方向：用户按分屏快捷键后进入主机选择，选中即分屏。
    split_pending: Option<Split>,
    /// Agent 目标主机集合（多主机编排；chips 多选）。
    agent_targets: std::collections::BTreeSet<String>,
    /// Agent 后台构建中（避免 block_on 卡 GUI 帧 + keychain 授权弹窗）。
    agent_starting: bool,
    /// Agent 构建/事件通道（GuiApp 持有，与 AgentBridge 共用）。
    agent_tx: Sender<AgentEvent>,
    /// Agent 事件统一接收端（永远归 GuiApp 持有，Agent 重建不丢失事件）。
    agent_rx: Receiver<AgentEvent>,
    /// Agent 构建失败信息（面板显示 + 配置入口）。
    agent_error: Option<String>,
    /// 对话历史弹窗 + 恢复（a4 对话持久化）。
    history_modal: bool,
    history_preview: Option<Vec<(String, String)>>,
    restore_msgs: Option<Vec<(String, String)>>,
    /// MCP 工具（批次4：外部 server 工具注入 agent 注册表）。
    mcp_tools: Vec<Arc<dyn Tool>>,
    mcp_errors: Vec<String>,
    agent: Option<AgentBridge>,
    rt: Arc<tokio::runtime::Runtime>,
    /// 供读线程触发的重画句柄（Session 构造时使用）。
    egui_ctx: Option<egui::Context>,
    /// GUI 审批通道：agent 线程 → GUI 主线程
    approve_rx: std::sync::mpsc::Receiver<PendingApproval>,
    pending_approval: Option<PendingApproval>,
    /// 计划审批模式（批次6）：多命令计划列表逐条/批量批准
    plan_mode: bool,
    plan_rx: std::sync::mpsc::Receiver<PlanItem>,
    plan_items: Vec<PlanItem>,
    /// Agent 回复 markdown 渲染缓存（批次7）
    md_cache: egui_commonmark::CommonMarkCache,
    /// 终端主题（批次8）
    theme: crate::gui::themes::Theme,
    theme_name: String,
    /// 终端字号（批次8，默认 14）
    font_size: f32,
    /// 主题导入路径输入（.itermcolors）
    theme_import: String,
    theme_msg: Option<String>,
    /// 持久化设置（批次8b）
    settings: crate::gui::settings::GuiSettings,
    /// 背景图纹理（已加载的路径）
    bg_texture: Option<egui::TextureHandle>,
    bg_loaded: Option<String>,
}

struct AgentBridge {
    agent: Arc<tokio::sync::Mutex<AgentLoop>>,
    messages: Vec<(String, String)>,
    input: String,
    busy: bool,
    tx: Sender<AgentEvent>,
    /// 实时进度（AgentLoop 共享状态，不经主体锁）
    progress: Arc<std::sync::Mutex<Option<String>>>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    progress_text: Option<String>,
    /// 已写入存档的消息条数（JSONL 增量追加）。
    saved_count: usize,
    /// 已发送轮次（每 5 轮触发一次自进化反思）。
    turns: usize,
}

enum AgentEvent {
    Turn(Result<TurnResult, String>),
    /// 自进化反思结果（已写入记忆库的摘要）
    Reflection(String),
    /// 后台构建 Agent 完成（成功 → bridge 数据；失败 → 错误文本）
    AgentBuilt(
        Result<Box<AgentLoop>, String>,
        Option<Vec<(String, String)>>,
    ),
}

impl GuiApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: Arc<tokio::runtime::Runtime>) -> Self {
        install_fonts(&cc.egui_ctx);
        // 主机簿：hosts.toml 独立维护；ssh_config 仅作导入源
        let hosts = HostsConfig::load().map(|c| c.hosts).unwrap_or_default();
        let ssh_imports = ssh_config::load();
        // MCP：连接配置的 server 并注册其工具（失败记录，不阻塞启动）
        let mcp = McpManager::load_and_connect(&rt);
        let mcp_errors = mcp.errors.clone();
        let settings = crate::gui::settings::GuiSettings::load();
        let theme = crate::gui::themes::load_theme(&settings.theme_name);
        let theme_name = settings.theme_name.clone();
        let font_size = settings.font_size;
        let (agent_tx, agent_rx) = std::sync::mpsc::channel();
        Self {
            theme,
            theme_name,
            font_size,
            hosts,
            ssh_imports,
            tabs: Vec::new(),
            active: 0,
            last_size: None,
            last_resize: std::time::Instant::now(),
            picking: true, // 启动即打开主机选择
            filter: String::new(),
            filter_selected: None,
            agent_starting: false,
            agent_error: None,
            agent_tx,
            agent_rx,
            add_modal: false,
            add_fields: AddHostFields::default(),
            import_modal: false,
            import_selected: std::collections::BTreeSet::new(),
            settings_modal: false,
            settings_fields: SettingsFields::default(),
            error_toast: None,
            agent_open: false,
            split_pending: None,
            agent_targets: std::collections::BTreeSet::new(),
            history_modal: false,
            history_preview: None,
            restore_msgs: None,
            mcp_tools: mcp.tools,
            mcp_errors,
            agent: None,
            rt,
            egui_ctx: None,
            approve_rx: std::sync::mpsc::channel().1,
            pending_approval: None,
            plan_mode: false,
            plan_rx: std::sync::mpsc::channel().1,
            plan_items: Vec::new(),
            md_cache: egui_commonmark::CommonMarkCache::default(),
            settings: crate::gui::settings::GuiSettings::load(),
            theme_import: String::new(),
            theme_msg: None,
            bg_texture: None,
            bg_loaded: None,
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
        let repaint: Arc<dyn Fn() + Send + Sync> = match &self.egui_ctx {
            Some(ctx) => Arc::new({
                let ctx = ctx.clone();
                move || ctx.request_repaint()
            }),
            None => Arc::new(|| {}),
        };
        let dir = self.split_pending.take();
        let auth = if entry.auth.is_empty() {
            "key"
        } else {
            entry.auth.as_str()
        };
        match Session::open_with_auth(key, &target, 24, 80, repaint, auth) {
            Ok(s) => {
                if let Some(dir) = dir {
                    // 分屏：加为当前 tab 的第二个 pane
                    if let Some(Tab::Term(w)) = self.tabs.get_mut(self.active) {
                        w.split_with(s, dir);
                        self.picking = false;
                        return;
                    }
                }
                let theme = if entry.theme.is_empty() {
                    None
                } else {
                    Some(entry.theme.clone())
                };
                self.tabs
                    .push(Tab::Term(TermWorkspace::with_theme(s, theme)));
                self.active = self.tabs.len() - 1;
                self.picking = false;
            }
            Err(e) => eprintln!("无法打开 {key}：{e}"),
        }
    }

    /// 面板背景色：主题 bg 向黑方向压暗（沉浸式跟随主题）。
    fn theme_bg(&self) -> Color32 {
        let b = self.current_theme().bg;
        Color32::from_rgb(b.r() / 2 + 18, b.g() / 2 + 18, b.b() / 2 + 18)
    }

    /// undecorated（隐藏红绿灯）模式：顶部空白区域拖动窗口。
    fn window_drag_area(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if !self.settings.undecorated {
            return;
        }
        let rect = ui.available_rect_before_wrap();
        let resp = ui.interact(rect, ui.id().with("ares_window_drag"), egui::Sense::drag());
        if resp.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
    }

    /// 当前生效主题：优先当前 tab 主机的主题（主题随主机切换），否则全局。
    fn current_theme(&self) -> crate::gui::themes::Theme {
        if let Some(Tab::Term(ws)) = self.tabs.get(self.active) {
            if let Some(name) = &ws.theme {
                return crate::gui::themes::load_theme(name);
            }
        }
        self.theme.clone()
    }

    /// 打开主机列表页（tab 形式）：已存在则切换，否则新建。
    fn open_picker(&mut self) {
        for (i, t) in self.tabs.iter().enumerate() {
            if matches!(t, Tab::Picker) {
                self.active = i;
                return;
            }
        }
        self.tabs.push(Tab::Picker);
        self.active = self.tabs.len() - 1;
    }

    /// 重连当前 tab：关闭后按同主机名重新打开（需在主机簿中）。
    fn reconnect_active(&mut self) {
        let title = self
            .tabs
            .get(self.active)
            .map(|t| t.title().to_string())
            .unwrap_or_default();
        if title.is_empty() || !self.hosts.contains_key(&title) {
            return;
        }
        self.close_active();
        self.open_tab(&title);
    }

    /// 打开 SFTP 浏览 tab（Netcatty 的 SFTP 功能）。
    fn open_sftp_tab(&mut self, key: &str) {
        let Some(entry) = self.hosts.get(key).cloned() else {
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
        let user = if entry.user.is_empty() {
            std::env::var("USER").unwrap_or_else(|_| "root".into())
        } else {
            entry.user.clone()
        };
        let auth = if entry.auth.is_empty() {
            "key"
        } else {
            entry.auth.as_str()
        };
        match SftpPanel::connect(key, &target, &user, auth, &self.rt) {
            Ok(p) => {
                self.tabs.push(Tab::Sftp(p));
                self.active = self.tabs.len() - 1;
                self.picking = false;
            }
            Err(e) => {
                self.error_toast = Some(format!("SFTP 连接失败：{e}"));
                eprintln!("SFTP 连接失败：{e}");
            }
        }
    }

    /// 打开本地终端 tab（无需主机簿条目）。
    fn open_local_tab(&mut self) {
        let repaint: Arc<dyn Fn() + Send + Sync> = match &self.egui_ctx {
            Some(ctx) => Arc::new({
                let ctx = ctx.clone();
                move || ctx.request_repaint()
            }),
            None => Arc::new(|| {}),
        };
        match Session::open_local(24, 80, repaint) {
            Ok(s) => {
                self.tabs.push(Tab::Term(TermWorkspace::new(s)));
                self.active = self.tabs.len() - 1;
                self.picking = false;
            }
            Err(e) => eprintln!("无法打开本地终端：{e}"),
        }
    }

    /// 分屏快捷键：设置方向 + 打开主机选择。
    fn start_split(&mut self, dir: Split) {
        if self.tabs.is_empty() {
            return;
        }
        self.split_pending = Some(dir);
        self.open_picker();
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

    /// 关闭当前 tab（分屏时先关 pane）。
    fn close_active(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        // 分屏 tab：先关 pane；只剩一个 pane 时正常关 tab
        if let Some(Tab::Term(ws)) = self.tabs.get_mut(self.active) {
            if ws.sessions.len() > 1 {
                ws.close_active_pane();
                return;
            }
        }
        self.tabs.remove(self.active);
        if self.active >= self.tabs.len() && !self.tabs.is_empty() {
            self.active = self.tabs.len() - 1;
        }
        if self.tabs.is_empty() {
            self.agent = None;
            self.open_picker();
        }
    }

    /// 转发输入事件到当前会话（拦截全局快捷键）。
    ///
    /// 焦点检测：当 Agent 输入框 / 主机过滤框 / 设置表单有焦点时，
    /// 用户打字只进输入框，不再同步到终端（2026-08-05 用户反馈）。
    fn handle_input(&mut self, ctx: &egui::Context) {
        let any_focus = ctx.memory(|m| m.focused().is_some());
        let events = ctx.input(|i| i.events.clone());
        // Enter 去重：egui/winit 对回车同时产生 Key(Enter) 与 Text("\r")
        // 两个事件，若都转发 shell 会收到两次回车 → 结果后多一个空行。
        // （2026-08-05 用户反馈「命令返回结果多一个回车」）
        let mut enter_forwarded = false;
        for ev in events {
            match ev {
                egui::Event::MouseWheel { delta, .. } => {
                    // 终端区域滚轮 → 滚动回退（scrollback，M1）
                    if !any_focus {
                        if let Some(Tab::Term(ws)) = self.tabs.get(self.active) {
                            let s = &ws.sessions[ws.active];
                            let d = (delta.y * 3.0).round() as i32;
                            if d != 0 {
                                s.scroll_lines(d);
                            }
                        }
                    }
                }
                egui::Event::Text(t) => {
                    if !any_focus {
                        if let Some(Tab::Term(w)) = self.tabs.get(self.active) {
                            if t == "\r" || t == "\n" {
                                if enter_forwarded {
                                    continue; // Key(Enter) 已转发
                                }
                                enter_forwarded = true;
                            }
                            w.sessions[w.active].write(t.as_bytes());
                        }
                    }
                }
                egui::Event::Key {
                    key,
                    pressed,
                    modifiers,
                    ..
                } => {
                    // 只处理按下事件；release（pressed=false）会导致
                    // Enter/方向键/退格被转发两次（2026-08-05 双提示符根因）
                    if !pressed {
                        continue;
                    }
                    let ctrl = modifiers.ctrl;
                    // 全局快捷键
                    if ctrl {
                        match key {
                            egui::Key::T => {
                                self.open_picker();
                                continue;
                            }
                            egui::Key::W => {
                                self.close_active();
                                continue;
                            }
                            // Ctrl+1..9：切换到第 N 个 tab（极简模式 tab 栏隐藏时）
                            k @ (egui::Key::Num1
                            | egui::Key::Num2
                            | egui::Key::Num3
                            | egui::Key::Num4
                            | egui::Key::Num5
                            | egui::Key::Num6
                            | egui::Key::Num7
                            | egui::Key::Num8
                            | egui::Key::Num9) => {
                                let idx = (k as u8 - egui::Key::Num1 as u8) as usize;
                                if idx < self.tabs.len() {
                                    self.active = idx;
                                }
                                continue;
                            }
                            egui::Key::D if modifiers.shift => {
                                // Ctrl+Shift+D：垂直分屏（左右）
                                self.start_split(Split::Vertical);
                                continue;
                            }
                            egui::Key::E if modifiers.shift => {
                                // Ctrl+Shift+E：水平分屏（上下）
                                self.start_split(Split::Horizontal);
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
                    // 转发给当前终端会话（可打印字符已走 Event::Text；SFTP 面板不转发；
                    // 输入框聚焦时不转发；Enter 与 Text 事件去重）
                    if !any_focus {
                        if let Some(Tab::Term(w)) = self.tabs.get(self.active) {
                            // End：滚动回到底部；Home：滚到顶部（scrollback，M1）
                            if key == egui::Key::End {
                                w.sessions[w.active].scroll_reset();
                                continue;
                            }
                            if key == egui::Key::Home {
                                w.sessions[w.active].scroll_lines(100_000);
                                continue;
                            }
                            if key == egui::Key::Enter {
                                if enter_forwarded {
                                    continue; // Text("\r") 已转发
                                }
                                enter_forwarded = true;
                            }
                            if let Some(bytes) = key_bytes(key, ctrl) {
                                w.sessions[w.active].write(&bytes);
                            }
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
            // 面板立即打开；Agent 后台构建（keychain 授权弹窗期间 GUI 不卡帧）
            let Tab::Term(ws) = &self.tabs[self.active] else {
                self.agent_error =
                    Some("当前页不是终端会话，Agent 需要连接终端后才能操作。".into());
                return;
            };
            let session = &ws.sessions[ws.active];
            let host = session.alias.clone();
            // 多主机编排：默认只操作当前 pane 主机；用户在面板 chips 可
            // 勾选更多主机（RoutedExecutor：当前 pane 注入，其他走 ssh）
            if self.agent_targets.is_empty() {
                self.agent_targets.insert(host.clone());
            }
            let scope: Vec<HostId> = self
                .agent_targets
                .iter()
                .map(|h| ares_core::HostId::new(h.clone()))
                .collect();
            // 终端注入执行器：agent 的命令直接写进当前终端会话
            // （Session 内部状态全 Arc 共享，clone 后注入的是同一个会话）
            let current = TerminalSessionExecutor::new(session.clone());
            let hosts_cfg = Arc::new(HostsConfig::load().unwrap_or_default());
            let executor = Arc::new(RoutedExecutor::new(
                current,
                HostId::new(host.clone()),
                hosts_cfg,
            ));
            // GUI 审批通道：plan 模式用计划队列，否则单命令弹窗
            let approver: Arc<dyn Approver> = if self.plan_mode {
                let (pa, rx) = PlanApprover::new();
                self.plan_rx = rx;
                self.plan_items.clear();
                Arc::new(pa)
            } else {
                let (ga, rx) = GuiApprover::pair();
                self.approve_rx = rx;
                Arc::new(ga)
            };
            let mcp_tools = self.mcp_tools.clone();
            let restore_msgs = self.restore_msgs.take();
            let tx = self.agent_tx.clone();
            let rt = self.rt.clone();
            self.agent_starting = true;
            self.agent_error = None;
            rt.spawn(async move {
                let result = crate::build_agent(scope, executor, approver, mcp_tools).await;
                tx.send(AgentEvent::AgentBuilt(
                    result.map(Box::new).map_err(|e| e.to_string()),
                    restore_msgs,
                ))
                .ok();
            });
        }
    }
}

impl AgentBridge {
    fn new(agent: AgentLoop, tx: Sender<AgentEvent>) -> Self {
        let agent = Arc::new(tokio::sync::Mutex::new(agent));
        // 刚构造无人持锁：直接提取共享状态
        let (progress, cancel) = {
            let a = agent.try_lock().expect("新构建的 AgentLoop 无人持锁");
            (a.progress.clone(), a.cancel.clone())
        };
        Self {
            agent,
            messages: vec![(
                "system".into(),
                "Agent 已就绪。输入运维目标，将针对当前主机执行（变更操作需确认）。".into(),
            )],
            input: String::new(),
            busy: false,
            tx,
            progress,
            cancel,
            progress_text: None,
            saved_count: 0,
            turns: 0,
        }
    }

    /// 追加保存对话到 `{data_dir}/sessions/<host>.jsonl`（增量 JSONL）。
    fn save_session(&mut self) {
        let host = self
            .messages
            .first()
            .map(|(_, _)| "agent")
            .unwrap_or("agent");
        // 用 messages 里第一条 user 前的 system 不含主机信息 —— 用文件名 agent.jsonl 即可
        let _ = host;
        let dir = ares_core::paths::data_dir().join("sessions");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let path = dir.join("agent.jsonl");
        let mut out = String::new();
        for (role, text) in &self.messages[self.saved_count..] {
            if role == "system" {
                continue;
            }
            if let Ok(line) = serde_json::to_string(&serde_json::json!({
                "role": role,
                "text": text,
            })) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        if !out.is_empty() {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = f.write_all(out.as_bytes());
                self.saved_count = self.messages.len();
            }
        }
    }

    /// 读取全部存档消息（历史恢复用）。
    fn load_session() -> Vec<(String, String)> {
        let path = ares_core::paths::data_dir()
            .join("sessions")
            .join("agent.jsonl");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).ok()?;
                let role = v["role"].as_str()?.to_string();
                let text = v["text"].as_str()?.to_string();
                Some((role, text))
            })
            .collect()
    }

    /// 停止当前任务。
    fn stop(&self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn send(&mut self, rt: &Arc<tokio::runtime::Runtime>) {
        if self.busy || self.input.trim().is_empty() {
            return;
        }
        let text = self.input.trim().to_string();
        self.input.clear();
        self.messages.push(("user".into(), text.clone()));
        self.save_session();
        self.busy = true;
        self.cancel
            .store(false, std::sync::atomic::Ordering::Relaxed);

        self.turns += 1;
        let do_reflect = self.turns % 5 == 0;

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

        // 自进化：每 5 轮后台反思最近对话 → 提炼写入记忆库
        if do_reflect {
            let agent = Arc::clone(&self.agent);
            let tx = self.tx.clone();
            let recent = self
                .messages
                .iter()
                .rev()
                .take(20)
                .cloned()
                .collect::<Vec<_>>();
            rt.spawn(async move {
                let result = {
                    let a = agent.lock().await;
                    a.reflect(&recent).await
                };
                match result {
                    Ok(raw) => {
                        let summary = persist_reflection(&raw);
                        // 顺带：记忆库超长时压缩合并（防无限增长）
                        let a = agent.lock().await;
                        let _ = a.compress_memory().await;
                        let _ = tx.send(AgentEvent::Reflection(summary));
                    }
                    Err(_) => {
                        // 反思失败静默（不影响主对话）
                    }
                }
            });
        }
    }

    /// 处理单个 Agent 事件（由 GuiApp 从统一 rx 分发；AgentBuilt 返回 outcome）。
    fn on_event(&mut self, ev: AgentEvent) -> Option<PollOutcome> {
        self.progress_text = self.progress.try_lock().ok().and_then(|p| p.clone());
        match ev {
            AgentEvent::Turn(Ok(r)) => {
                let mut body = r.reply.clone();
                // 工具执行记录只保留一行精简摘要（完整输出在终端可见，
                // 不在面板里重复 —— 2026-08-05 用户反馈「回答太乱」）
                for run in &r.tool_runs {
                    let cmd = run.command.clone().unwrap_or_default();
                    let short: String = cmd.chars().take(60).collect();
                    let cmd_txt = if cmd.chars().count() > 60 {
                        format!("{short}…")
                    } else {
                        cmd
                    };
                    body = format!(
                        "\n{body}\n· [{}] {}{}",
                        run.decision_label,
                        run.tool,
                        if cmd_txt.is_empty() {
                            String::new()
                        } else {
                            format!(" {cmd_txt}")
                        }
                    );
                }
                self.messages.push(("assistant".into(), body));
            }
            AgentEvent::Turn(Err(e)) => {
                self.messages
                    .push(("assistant".into(), format!("错误：{e}")));
            }
            AgentEvent::Reflection(summary) => {
                self.messages
                    .push(("system".into(), format!("🧠 {summary}")));
            }
            AgentEvent::AgentBuilt(result, restore_msgs) => {
                return Some(PollOutcome::Built(result, restore_msgs));
            }
        }
        self.save_session();
        self.busy = false;
        None
    }
}

/// AgentBridge::poll 的返回：AgentBuilt 事件（GUI 层处理）。
enum PollOutcome {
    Built(
        Result<Box<AgentLoop>, String>,
        Option<Vec<(String, String)>>,
    ),
}

/// SFTP 双栏浏览 UI（本地 | 远程）。
fn sftp_ui(rt: &tokio::runtime::Runtime, ui: &mut egui::Ui, p: &mut SftpPanel) {
    if p.busy {
        ui.label(RichText::new("… 工作中").color(Color32::GRAY));
    }
    if let Some(err) = &p.error {
        ui.colored_label(Color32::from_rgb(220, 90, 90), err);
    }
    ui.columns(2, |cols| {
        // ── 本地栏 ──
        let local = &mut cols[0];
        local.horizontal(|ui| {
            if ui.button("↑ 上级").clicked() {
                p.go_up_local(rt);
            }
            if ui.button("⟳ 刷新").clicked() {
                p.list_local(rt);
            }
        });
        local.label(RichText::new(&p.local_path).color(Color32::from_rgb(120, 120, 130)));
        local.separator();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(local, |ui| {
                for (name, is_dir, size) in p.local_entries.clone() {
                    let icon = if is_dir { "📁" } else { "📄" };
                    let size_txt = if is_dir {
                        String::new()
                    } else {
                        format!("  {}", human_size(size))
                    };
                    let label = format!("{icon} {name}{size_txt}");
                    if ui.selectable_label(false, label).double_clicked() {
                        if is_dir {
                            p.enter_local(rt, &name);
                        } else {
                            p.upload(rt, &name);
                        }
                    }
                }
            });

        // ── 远程栏 ──
        let remote = &mut cols[1];
        remote.horizontal(|ui| {
            if ui.button("↑ 上级").clicked() {
                p.go_up(rt);
            }
            if ui.button("⟳ 刷新").clicked() {
                let path = p.remote_path.clone();
                p.list_remote(rt, &path);
            }
            if let Some(name) = p.selected.clone() {
                if ui.button("⬇ 下载").clicked() {
                    p.download(rt, &name);
                }
            }
        });
        remote.label(RichText::new(&p.remote_path).color(Color32::from_rgb(120, 120, 130)));
        remote.separator();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(remote, |ui| {
                let mut to_enter: Option<String> = None;
                let mut to_select: Option<String> = None;
                for (name, is_dir, size) in p.entries.clone() {
                    let icon = if is_dir { "📁" } else { "📄" };
                    let size_txt = if is_dir {
                        String::new()
                    } else {
                        format!("  {}", human_size(size))
                    };
                    let label = format!("{icon} {name}{size_txt}");
                    let sel = p.selected.as_deref() == Some(name.as_str());
                    let row = ui.selectable_label(sel, label);
                    if row.double_clicked() {
                        if is_dir {
                            to_enter = Some(name.clone());
                        } else {
                            to_select = Some(name.clone());
                        }
                    } else if row.clicked() {
                        to_select = Some(name.clone());
                    }
                }
                if let Some(n) = to_enter {
                    p.enter_remote(rt, &n);
                }
                if let Some(n) = to_select {
                    p.selected = Some(n);
                }
            });
    });
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 数据驱动重画：读线程有新数据时调用 ctx.request_repaint()，
        // 静止时 egui 不重画（画面保留、CPU 为零）
        self.egui_ctx = Some(ctx.clone());

        // 处理输入（转发 / 快捷键）
        self.handle_input(ctx);

        // Agent 事件统一轮询（rx 归 GuiApp：agent 未构建时也能收到
        // AgentBuilt —— 否则"正在启动"永远不结束）
        let mut outcome = None;
        while let Ok(ev) = self.agent_rx.try_recv() {
            match ev {
                AgentEvent::AgentBuilt(result, msgs) => {
                    outcome = Some(PollOutcome::Built(result, msgs));
                }
                other => {
                    if let Some(a) = &mut self.agent {
                        outcome = a.on_event(other);
                    }
                }
            }
        }
        if let Some(PollOutcome::Built(result, restore_msgs)) = outcome {
            self.agent_starting = false;
            match result {
                Ok(agent) => {
                    let tx = self.agent_tx.clone();
                    let mut bridge = AgentBridge::new(*agent, tx);
                    if let Some(msgs) = restore_msgs {
                        // 恢复历史：注入 LLM 上下文 + 面板显示
                        {
                            let mut a = bridge.agent.try_lock().expect("刚构建无人持锁");
                            a.restore_history(&msgs);
                        }
                        bridge.messages = msgs;
                        bridge.saved_count = bridge.messages.len();
                    }
                    self.agent = Some(bridge);
                }
                Err(e) => {
                    self.agent_error = Some(e);
                }
            }
        }

        // 审批轮询：agent 线程发来的确认请求 → 弹窗
        while let Ok(p) = self.approve_rx.try_recv() {
            self.pending_approval = Some(p);
        }
        // 计划模式轮询：agent 线程排队的计划命令
        while let Ok(item) = self.plan_rx.try_recv() {
            self.plan_items.push(item);
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
                        ui.colored_label(
                            Color32::from_rgb(230, 190, 90),
                            format!("规则：{rule}{tag}"),
                        );
                    } else {
                        // 判定等级：deny=红 / observer=绿 / auto=蓝
                        let (txt, color) = match &p.req.decision {
                            Decision::Deny { .. } => {
                                ("判定：禁止执行".to_string(), Color32::from_rgb(220, 90, 90))
                            }
                            Decision::Observer => (
                                "判定：观察执行".to_string(),
                                Color32::from_rgb(110, 190, 110),
                            ),
                            Decision::Auto { .. } => (
                                "判定：自动执行".to_string(),
                                Color32::from_rgb(110, 150, 220),
                            ),
                            _ => (
                                format!("判定：{}", decision_label(&p.req.decision)),
                                Color32::from_rgb(179, 146, 74),
                            ),
                        };
                        ui.colored_label(color, txt);
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

        // ── 顶部：Tab 栏（可隐藏：iTerm2 极简模式；Ctrl-T/W 仍可用）──
        if self.settings.hide_tabs {
            // 隐藏时只保留一个极小的「+」入口，避免无法新增 tab
            let panel_bg = self.theme_bg();
            egui::TopBottomPanel::top("tabs_hidden")
                .frame(egui::Frame::none().fill(panel_bg))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("+").clicked() {
                            self.open_picker();
                        }
                        if ui
                            .selectable_label(self.agent_open, RichText::new("Agent").strong())
                            .clicked()
                        {
                            self.toggle_agent_simple();
                        }
                    });
                });
        } else {
            let panel_bg = self.theme_bg();
            egui::TopBottomPanel::top("tabs")
                .frame(egui::Frame::none().fill(panel_bg))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        let mut to_close: Option<usize> = None;
                        let mut to_activate: Option<usize> = None;
                        for (i, t) in self.tabs.iter().enumerate() {
                            let selected = i == self.active;
                            let exited = matches!(t, Tab::Term(w) if w.is_exited());
                            let label = if exited {
                                format!("✗ {}", t.title())
                            } else {
                                t.title().to_string()
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
                            self.open_picker();
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
                    self.window_drag_area(ui, ctx);
                });
        }

        // ── 底部：状态栏 ──
        let panel_bg = self.theme_bg();
        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::none().fill(panel_bg))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let mut reconnect = false;
                    if let Some(t) = self.tabs.get(self.active) {
                        let (label, color) = match t {
                            Tab::Term(w) => {
                                // 连接状态：连接中 → 已连接 → 已断开
                                let st = if w.is_exited() {
                                    "已断开"
                                } else if !w.is_connected() {
                                    "连接中…"
                                } else {
                                    "已连接"
                                };
                                let panes = if w.sessions.len() > 1 {
                                    format!("{} ({}pane)", w.title(), w.sessions.len())
                                } else {
                                    w.title().to_string()
                                };
                                let color = match st {
                                    "已断开" => Color32::from_rgb(220, 90, 90),
                                    "连接中…" => Color32::from_rgb(230, 190, 90),
                                    _ => Color32::from_rgb(179, 146, 74),
                                };
                                (format!("{panes} · {st}"), color)
                            }
                            Tab::Sftp(p) => (
                                format!("{} · SFTP", p.title),
                                Color32::from_rgb(90, 160, 200),
                            ),
                            Tab::Picker => {
                                ("主机列表".to_string(), Color32::from_rgb(120, 140, 170))
                            }
                        };
                        ui.label(RichText::new(label).color(color));
                        // 断开时提供重连入口（UI-C）
                        if let Tab::Term(w) = t {
                            if w.is_exited() && ui.small_button("↻ 重连").clicked() {
                                reconnect = true;
                            }
                        }
                    } else {
                        ui.label(RichText::new("无会话").color(Color32::GRAY));
                    }
                    // 滚动位置（scrollback，M1）：非底部时显示已滚行数
                    if let Some(Tab::Term(ws)) = self.tabs.get(self.active) {
                        let off = ws.sessions[ws.active].scroll_offset();
                        if off > 0 {
                            ui.label(
                                RichText::new(format!("↑{off} 行"))
                                    .color(Color32::from_rgb(120, 140, 170)),
                            );
                        }
                    }
                    ui.separator();
                    ui.label(
                        RichText::new(
                            "Ctrl-T 新会话 · Ctrl-a a Agent · Ctrl+1-9 切换 · Ctrl+Shift+D/E 分屏",
                        )
                        .color(Color32::GRAY),
                    );
                    if ui
                        .button(RichText::new("⚙ 设置").small())
                        .on_hover_text("模型 / 主题 / 外观 / 导入 .itermcolors")
                        .clicked()
                    {
                        self.settings_modal = true;
                    }
                    if reconnect {
                        self.reconnect_active();
                    }
                });
            });

        // ── 右侧：Agent 面板（侧边栏，Ctrl-a a 唤起）──
        if self.agent_open {
            let panel_bg = self.theme_bg();
            egui::SidePanel::right("agent_panel")
                .resizable(true)
                .default_width(360.0)
                .min_width(260.0)
                .frame(egui::Frame::none().fill(panel_bg))
                .show(ctx, |ui| {
                    ui.heading("Agent");
                    ui.label(
                        RichText::new(format!(
                            "当前主机：{}",
                            self.tabs
                                .get(self.active)
                                .map(|t| t.title().to_string())
                                .unwrap_or_default()
                        ))
                        .color(Color32::GRAY),
                    );
                    // 目标主机 chips（多主机编排：勾选后 agent 可操作多台；
                    // 当前 pane 注入，其他主机走 ssh 通道）
                    ui.horizontal_wrapped(|ui| {
                        let mut changed = false;
                        let mut candidates: Vec<String> = self.hosts.keys().cloned().collect();
                        if let Some(t) = self.tabs.get(self.active) {
                            let title = t.title().to_string();
                            if !candidates.contains(&title) {
                                candidates.push(title);
                            }
                        }
                        for host in candidates {
                            let sel = self.agent_targets.contains(&host);
                            if ui.selectable_label(sel, &host).clicked() {
                                changed = true;
                                if sel {
                                    self.agent_targets.remove(&host);
                                } else {
                                    self.agent_targets.insert(host);
                                }
                            }
                        }
                        // 目标变更 → 重建 agent（scope 已变）
                        if changed {
                            self.agent = None;
                            self.agent_open = false;
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.small_button("历史").clicked() {
                            self.history_preview = Some(AgentBridge::load_session());
                            self.history_modal = true;
                        }
                        if ui.small_button("清空").clicked() {
                            self.agent = None;
                            self.agent_open = false;
                            let _ = std::fs::remove_file(
                                ares_core::paths::data_dir()
                                    .join("sessions")
                                    .join("agent.jsonl"),
                            );
                        }
                        // 计划审批模式开关（批次6）
                        if ui
                            .selectable_label(self.plan_mode, "📋 计划模式")
                            .on_hover_text("agent 要执行的命令先进计划列表，逐条/批量批准后才执行")
                            .clicked()
                        {
                            self.plan_mode = !self.plan_mode;
                            self.agent = None;
                            self.agent_open = false;
                            self.plan_items.clear();
                        }
                    });
                    // 计划列表（plan 模式）：每条命令可单独批准/拒绝
                    if self.plan_mode && !self.plan_items.is_empty() {
                        ui.separator();
                        ui.label(
                            RichText::new(format!("📋 计划（{} 条待审批）", self.plan_items.len()))
                                .strong(),
                        );
                        egui::ScrollArea::vertical()
                            .id_salt("plan_list")
                            .max_height(140.0)
                            .show(ui, |ui| {
                                let mut approve_one: Option<usize> = None;
                                let mut reject_one: Option<usize> = None;
                                let mut all_approve = false;
                                let mut all_reject = false;
                                let mut toggle_edit: Option<usize> = None;
                                for (i, item) in self.plan_items.iter_mut().enumerate() {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(item.req.host.to_string())
                                                .small()
                                                .color(Color32::GRAY),
                                        );
                                        let shown = item
                                            .pending_edit
                                            .clone()
                                            .unwrap_or_else(|| item.req.command.clone());
                                        ui.label(RichText::new(&shown).monospace().small());
                                        if ui.small_button("批准").clicked() {
                                            approve_one = Some(i);
                                        }
                                        if ui.small_button("拒绝").clicked() {
                                            reject_one = Some(i);
                                        }
                                        if ui.small_button("✏️ 编辑").clicked() {
                                            toggle_edit = Some(i);
                                        }
                                    });
                                    // 编辑态：命令输入框（首次点击编辑时预填原命令）
                                    if let Some(editing) = item.pending_edit.as_mut() {
                                        ui.text_edit_singleline(editing);
                                    }
                                    // 注释/备注（仅展示，不影响执行）
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new("备注：").small().color(Color32::GRAY),
                                        );
                                        ui.add(
                                            egui::TextEdit::singleline(&mut item.note)
                                                .desired_width(120.0),
                                        );
                                    });
                                    if !item.note.is_empty() {
                                        ui.label(
                                            RichText::new(format!("💬 {}", item.note))
                                                .small()
                                                .color(Color32::from_rgb(120, 170, 220)),
                                        );
                                    }
                                }
                                if let Some(i) = toggle_edit {
                                    if let Some(item) = self.plan_items.get_mut(i) {
                                        if item.pending_edit.is_none() {
                                            item.pending_edit = Some(item.req.command.clone());
                                        }
                                    }
                                }
                                ui.horizontal(|ui| {
                                    if ui.button("✅ 全部批准").clicked() {
                                        all_approve = true;
                                    }
                                    if ui.button("⛔ 全部拒绝").clicked() {
                                        all_reject = true;
                                    }
                                });
                                if let Some(i) = approve_one {
                                    let item = self.plan_items.remove(i);
                                    // 编辑过 → ApprovedWithEdit（agent 侧重新判定后执行）
                                    if let Some(edited) = item.pending_edit {
                                        let edited = edited.trim().to_string();
                                        if !edited.is_empty() && edited != item.req.command {
                                            let _ = item
                                                .respond
                                                .send(ApprovalResult::ApprovedWithEdit(edited));
                                        } else {
                                            let _ = item.respond.send(ApprovalResult::Approved);
                                        }
                                    } else {
                                        let _ = item.respond.send(ApprovalResult::Approved);
                                    }
                                }
                                if let Some(i) = reject_one {
                                    let item = self.plan_items.remove(i);
                                    let _ = item.respond.send(ApprovalResult::Rejected);
                                }
                                if all_approve {
                                    settle_all(
                                        std::mem::take(&mut self.plan_items),
                                        ApprovalResult::Approved,
                                    );
                                }
                                if all_reject {
                                    settle_all(
                                        std::mem::take(&mut self.plan_items),
                                        ApprovalResult::Rejected,
                                    );
                                }
                            });
                        ui.separator();
                    }
                    ui.separator();

                    let md_cache = &mut self.md_cache;
                    if let Some(a) = &mut self.agent {
                        // 消息区：限制高度，给底部输入区留空间（否则被挤出面板）
                        let input_h = 60.0;
                        let avail_h = ui.available_height().max(input_h + 20.0);
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .stick_to_bottom(true)
                            .max_height(avail_h - input_h)
                            .show(ui, |ui| {
                                for (role, text) in &a.messages {
                                    let color = match role.as_str() {
                                        "user" => Color32::from_rgb(107, 179, 74),
                                        "system" => Color32::GRAY,
                                        _ => Color32::from_rgb(220, 220, 220),
                                    };
                                    ui.label(RichText::new(format!("[{role}]")).color(color));
                                    // assistant 消息用 markdown 渲染（代码块/列表/表格更清晰），
                                    // 其他角色保持纯文本
                                    if role == "assistant" {
                                        egui_commonmark::CommonMarkViewer::new()
                                            .show(ui, md_cache, text);
                                    } else {
                                        for line in text.lines() {
                                            ui.label(line);
                                        }
                                    }
                                    ui.add_space(4.0);
                                }
                            });
                        // 快捷指令（运维常见动作一键填入）
                        ui.horizontal_wrapped(|ui| {
                            for (label, prompt) in [
                                ("磁盘", "查看磁盘占用情况"),
                                ("内存", "查看内存使用情况"),
                                ("日志", "查看系统日志"),
                                ("服务", "查看服务状态"),
                            ] {
                                if ui.small_button(label).clicked() && !a.busy {
                                    a.input = prompt.to_string();
                                }
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
                            // 实时进度：直接读共享状态（不依赖事件驱动的缓存）
                            let cur_progress = a.progress.try_lock().ok().and_then(|p| p.clone());
                            if let Some(p) = &cur_progress {
                                ui.label(
                                    RichText::new(format!("⏳ {p}"))
                                        .color(Color32::from_rgb(179, 146, 74)),
                                );
                            } else {
                                ui.label(RichText::new("…").color(Color32::GRAY));
                            }
                            if ui.button("■ 停止").clicked() {
                                a.stop();
                            }
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
                        // Agent 未就绪：启动中 / 失败信息 / 配置入口（都可见）
                        if self.agent_starting {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new("正在启动 Agent…")
                                    .color(Color32::from_rgb(230, 190, 90)),
                            );
                            ui.label(
                                RichText::new(
                                    "首次启动会请求 macOS 钥匙串授权（输入登录密码后仅需一次）。",
                                )
                                .small()
                                .color(Color32::GRAY),
                            );
                        }
                        if let Some(err) = &self.agent_error {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(format!("Agent 启动失败：{err}"))
                                    .color(Color32::from_rgb(220, 90, 90)),
                            );
                        }
                        ui.add_space(8.0);
                        ui.label("Agent 未就绪（需配置模型）。");
                        if ui.button("⚙ 配置模型").clicked() {
                            self.settings_modal = true;
                        }
                    }
                });
        }

        // ── 中央：终端渲染 ──
        egui::CentralPanel::default().show(ctx, |ui| {
            // 背景图（iTerm2 化）：加载 + 铺底（终端默认背景透明处透出）
            let bg_path = self.settings.background_image.clone();
            if !bg_path.is_empty() && self.bg_loaded.as_deref() != Some(bg_path.as_str()) {
                match std::fs::read(&bg_path) {
                    Ok(bytes) => {
                        let img = image::load_from_memory(&bytes)
                            .map_err(|e| format!("图片解析失败：{e}"))
                            .map(|img| {
                                let rgba = img.to_rgba8();
                                let (w, h) = rgba.dimensions();
                                egui::ColorImage::from_rgba_unmultiplied(
                                    [w as usize, h as usize],
                                    rgba.as_raw(),
                                )
                            });
                        match img {
                            Ok(color_image) => {
                                let handle = ctx.load_texture(
                                    "ares_bg",
                                    color_image,
                                    egui::TextureOptions::LINEAR,
                                );
                                self.bg_texture = Some(handle);
                                self.bg_loaded = Some(bg_path.clone());
                            }
                            Err(e) => {
                                self.theme_msg = Some(format!("背景图加载失败：{e}"));
                                self.bg_loaded = Some(bg_path.clone());
                            }
                        }
                    }
                    Err(e) => {
                        self.theme_msg = Some(format!("背景图读取失败：{e}"));
                        self.bg_loaded = Some(bg_path.clone());
                    }
                }
            }
            // 绘制背景图 + 半透明遮罩（终端默认 bg 的 cell 会透出）
            let bg_rect = ui.available_rect_before_wrap();
            if let Some(tex) = &self.bg_texture {
                ui.painter().image(
                    tex.id(),
                    bg_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                // 遮罩：保证文字可读性
                ui.painter().rect_filled(
                    bg_rect,
                    0.0,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 140),
                );
            }
            let cur_theme = self.current_theme();
            match self.tabs.get_mut(self.active) {
                Some(Tab::Term(ws)) => {
                    let n = ws.sessions.len();
                    if n == 1 {
                        // 单 pane：尺寸变化 → resize 会话
                        let (rows, cols) = term::size_for(ui, &mono_font(self.font_size));
                        if self.last_size != Some((rows, cols))
                            && self.last_resize.elapsed() > std::time::Duration::from_millis(150)
                        {
                            ws.sessions[0].resize(rows, cols);
                            self.last_size = Some((rows, cols));
                            self.last_resize = std::time::Instant::now();
                        }
                        let screen = ws.sessions[0].screen();
                        term::draw_terminal(ui, &screen, mono_font(self.font_size), &cur_theme);
                        // 点击终端空白处：清除输入框焦点，恢复直接输入
                        let tr = ui.available_rect_before_wrap();
                        if ui
                            .interact(tr, ui.id().with("term_focus"), egui::Sense::click())
                            .clicked()
                        {
                            ctx.memory_mut(|m| m.stop_text_input());
                        }
                    } else {
                        // 分屏：按方向均分区域，每个 pane 独立渲染
                        let split = ws.split.unwrap_or(Split::Vertical);
                        let rect = ui.available_rect_before_wrap();
                        let is_v = split == Split::Vertical;
                        let half = if is_v {
                            rect.width() * 0.5
                        } else {
                            rect.height() * 0.5
                        };
                        for (i, s) in ws.sessions.iter().enumerate() {
                            let r = if is_v {
                                egui::Rect::from_min_size(
                                    rect.min + egui::vec2(i as f32 * half, 0.0),
                                    egui::vec2(half, rect.height()),
                                )
                            } else {
                                egui::Rect::from_min_size(
                                    rect.min + egui::vec2(0.0, i as f32 * half),
                                    egui::vec2(rect.width(), half),
                                )
                            };
                            // 点击切换 active pane + 清除输入框焦点
                            let id = ui.id().with(("pane", i));
                            if ui.interact(r, id, egui::Sense::click()).clicked() {
                                ws.active = i;
                                ctx.memory_mut(|m| m.stop_text_input());
                            }
                            let mut child =
                                ui.new_child(egui::UiBuilder::new().max_rect(r.shrink(if is_v {
                                    1.0
                                } else {
                                    0.0
                                })));
                            let (rows, cols) = term::size_for(&child, &mono_font(self.font_size));
                            if ws.last_sizes[i] != Some((rows, cols))
                                && self.last_resize.elapsed()
                                    > std::time::Duration::from_millis(150)
                            {
                                s.resize(rows, cols);
                                ws.last_sizes[i] = Some((rows, cols));
                                self.last_resize = std::time::Instant::now();
                            }
                            let screen = s.screen();
                            term::draw_terminal(
                                &mut child,
                                &screen,
                                mono_font(self.font_size),
                                &cur_theme,
                            );
                        }
                        // 分割线
                        let mid = if is_v {
                            egui::Pos2::new(rect.min.x + half, rect.min.y)
                        } else {
                            egui::Pos2::new(rect.min.x, rect.min.y + half)
                        };
                        ui.painter().line_segment(
                            [
                                mid,
                                if is_v {
                                    egui::Pos2::new(mid.x, rect.max.y)
                                } else {
                                    egui::Pos2::new(rect.max.x, mid.y)
                                },
                            ],
                            egui::Stroke::new(1.0_f32, Color32::from_rgb(70, 70, 80)),
                        );
                    }
                }
                Some(Tab::Sftp(p)) => {
                    p.poll();
                    let rt = self.rt.clone();
                    sftp_ui(&rt, ui, p);
                }
                Some(Tab::Picker) => {
                    // 主机列表页（tab 形式，极简；连接后自动切到终端 tab）
                    let mut open_add = false;
                    let mut open_import = false;
                    let mut open_local = false;
                    let mut connect_key: Option<String> = None;
                    let mut sftp_key: Option<String> = None;
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
                        if ui.button("🖥 本地终端").clicked() {
                            open_local = true;
                        }
                    });
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("过滤（名称 / 地址 / 用户）")
                            .desired_width(f32::INFINITY),
                    );
                    ui.separator();
                    let f = self.filter.to_lowercase();
                    // 按环境分组（Netcatty Vault 风格），组内保持字典序
                    let mut vis: Vec<(Env, String)> = self
                        .hosts
                        .iter()
                        .filter(|(k, e)| {
                            f.is_empty()
                                || k.to_lowercase().contains(&f)
                                || e.hostname.to_lowercase().contains(&f)
                                || e.user.to_lowercase().contains(&f)
                        })
                        .map(|(k, e)| (e.env, k.clone()))
                        .collect();
                    vis.sort_by(|a, b| {
                        env_order(&a.0)
                            .cmp(&env_order(&b.0))
                            .then_with(|| a.1.cmp(&b.1))
                    });

                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let mut last_env: Option<Env> = None;
                            for (idx, (env, key)) in vis.iter().enumerate() {
                                if last_env != Some(*env) {
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new(env_label(*env))
                                            .color(Color32::from_rgb(120, 120, 130))
                                            .small(),
                                    );
                                    last_env = Some(*env);
                                }
                                let e = &self.hosts[key];
                                let target = e.connect_target(key);
                                let label = format!("{key}  ·  {target}");
                                let row =
                                    ui.selectable_label(self.filter_selected == Some(idx), label);
                                if row.double_clicked() {
                                    connect_key = Some(key.clone());
                                    break;
                                }
                                if row.clicked() {
                                    self.filter_selected = Some(idx);
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
                        if let Some(sel) = self.filter_selected {
                            if connect && sel < vis.len() {
                                connect_key = Some(vis[sel].1.clone());
                            }
                        }
                        let sftp = ui.button("SFTP 浏览").clicked();
                        if let Some(sel) = self.filter_selected {
                            if sftp && sel < vis.len() {
                                sftp_key = Some(vis[sel].1.clone());
                            }
                        }
                    });
                    if open_add {
                        self.add_modal = true;
                    }
                    if open_import {
                        self.import_modal = true;
                        self.import_selected.clear();
                    }
                    if open_local {
                        self.open_local_tab();
                    }
                    if let Some(k) = connect_key {
                        self.open_tab(&k);
                    }
                    if let Some(k) = sftp_key {
                        self.open_sftp_tab(&k);
                    }
                }
                None => {
                    // 空状态引导（首次使用：新会话 / 添加主机 / 导入 ssh_config）
                    ui.centered_and_justified(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new("欢迎使用 ARES")
                                    .size(22.0)
                                    .strong()
                                    .color(self.current_theme().fg),
                            );
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new("极简 iTerm2 式 AI 运维终端 · 纯 Rust")
                                    .color(Color32::GRAY),
                            );
                            ui.add_space(16.0);
                            let mut act: Option<&str> = None;
                            ui.horizontal(|ui| {
                                if ui.button("🖥 新会话").clicked() {
                                    act = Some("new");
                                }
                                if ui.button("➕ 添加主机").clicked() {
                                    act = Some("add");
                                }
                                if ui.button("📥 从 ssh_config 导入").clicked() {
                                    act = Some("import");
                                }
                            });
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(
                                    "快捷键：Ctrl-T 新会话 · Ctrl-a a Agent · Ctrl+Shift+D/E 分屏",
                                )
                                .small()
                                .color(Color32::GRAY),
                            );
                            match act {
                                Some("new") => {
                                    self.open_picker();
                                }
                                Some("add") => {
                                    self.add_modal = true;
                                }
                                Some("import") => {
                                    self.import_modal = true;
                                }
                                _ => {}
                            }
                        });
                    });
                }
            }
        });

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
                            ui.label("认证方式");
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut f.auth, "key".to_string(), "🔑 密钥");
                                ui.selectable_value(
                                    &mut f.auth,
                                    "password".to_string(),
                                    "🔒 密码（存钥匙串）",
                                );
                            });
                            ui.end_row();
                            if f.auth == "password" {
                                ui.label("密码");
                                ui.add(
                                    egui::TextEdit::singleline(&mut f.password)
                                        .password(true)
                                        .hint_text("写入 macOS 钥匙串 ssh-pw:<名称>"),
                                );
                                ui.end_row();
                            }
                            ui.label("主题(可选)");
                            ui.add(
                                egui::TextEdit::singleline(&mut f.theme)
                                    .hint_text("空=跟随全局；如 Snazzy/Dracula"),
                            );
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
                                // 认证方式：password → 密码存钥匙串（vault），
                                // hosts.toml 只存 auth 标记不进密码
                                entry.theme = f.theme.trim().to_string();
                                if f.auth == "password" {
                                    entry.auth = "password".into();
                                    if !f.password.trim().is_empty() {
                                        let _ = ares_darwin::keychain::set_secret(
                                            &format!("ssh-pw:{name}"),
                                            f.password.trim(),
                                        );
                                    }
                                }
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

        // ── 模型设置弹窗 ──
        if self.settings_modal {
            let mut close = false;
            let mut saved = false;
            let mut save_err: Option<String> = None;
            egui::Window::new("模型设置")
                .collapsible(false)
                .resizable(false)
                .default_size([420.0, 0.0])
                .show(ctx, |ui| {
                    let f = &mut self.settings_fields;
                    // 预设快捷填充
                    ui.horizontal(|ui| {
                        ui.label("预设：");
                        if ui.button("DeepSeek").clicked() {
                            f.name = "deepseek".into();
                            f.base_url = "https://api.deepseek.com/v1".into();
                            f.model = "deepseek-chat".into();
                        }
                        if ui.button("Anthropic").clicked() {
                            f.name = "anthropic".into();
                            f.base_url = "https://api.anthropic.com/v1".into();
                            f.model = "claude-sonnet-4-5".into();
                        }
                        if ui.button("OpenRouter").clicked() {
                            f.name = "openrouter".into();
                            f.base_url = "https://openrouter.ai/api/v1".into();
                            f.model = "deepseek/deepseek-chat".into();
                        }
                    });
                    ui.separator();
                    egui::Grid::new("settings_grid")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("名称");
                            ui.text_edit_singleline(&mut f.name);
                            ui.end_row();
                            ui.label("Base URL");
                            ui.text_edit_singleline(&mut f.base_url);
                            ui.end_row();
                            ui.label("模型");
                            ui.text_edit_singleline(&mut f.model);
                            ui.end_row();
                            ui.label("API Key");
                            ui.add(egui::TextEdit::singleline(&mut f.api_key).password(true));
                            ui.end_row();
                        });
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "Key 写入 macOS 钥匙串（llm:<名称>），providers.toml 只存账户名。",
                        )
                        .color(Color32::GRAY),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("保存").strong()).clicked() {
                            let name = f.name.trim().to_string();
                            if name.is_empty() || f.api_key.trim().is_empty() {
                                save_err = Some("名称与 API Key 不能为空".into());
                            } else {
                                // 写 providers.toml（保留已有其他 provider）
                                let mut cfg = ProvidersConfig::load().unwrap_or_default();
                                cfg.providers.insert(
                                    name.clone(),
                                    ProviderEntry {
                                        kind: ProviderKind::Openai,
                                        base_url: f.base_url.trim().to_string(),
                                        model: f.model.trim().to_string(),
                                        keychain_account: format!("llm:{name}"),
                                    },
                                );
                                cfg.active = name.clone();
                                match cfg.save() {
                                    Ok(()) => {
                                        // 写 Keychain
                                        match ares_darwin::keychain::set_secret(
                                            &format!("llm:{name}"),
                                            f.api_key.trim(),
                                        ) {
                                            Ok(()) => {
                                                saved = true;
                                                close = true;
                                            }
                                            Err(e) => {
                                                save_err = Some(format!("写入钥匙串失败：{e}"));
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        save_err = Some(format!("保存配置失败：{e}"));
                                    }
                                }
                            }
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                    if let Some(err) = &save_err {
                        ui.colored_label(Color32::from_rgb(220, 90, 90), err);
                    }
                    // ── 外观（批次8：iTerm2 化）──
                    ui.add_space(8.0);
                    ui.separator();
                    ui.label(RichText::new("外观").strong());
                    egui::Grid::new("appearance_grid")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("主题");
                            let names = crate::gui::themes::available_themes();
                            let cur = self.theme_name.clone();
                            let mut sel = cur.clone();
                            egui::ComboBox::from_id_salt("theme_combo")
                                .selected_text(&cur)
                                .show_ui(ui, |ui| {
                                    for n in &names {
                                        ui.selectable_value(&mut sel, n.clone(), n);
                                    }
                                });
                            if sel != cur {
                                self.theme_name = sel.clone();
                                self.theme = crate::gui::themes::load_theme(&sel);
                                self.settings.theme_name = sel;
                                self.settings.save();
                            }
                            ui.end_row();
                            ui.label("字号");
                            if ui
                                .add(
                                    egui::Slider::new(&mut self.font_size, 10.0..=24.0)
                                        .text("pt")
                                        .fixed_decimals(1),
                                )
                                .changed()
                            {
                                self.settings.font_size = self.font_size;
                                self.settings.save();
                            }
                            ui.end_row();
                            ui.label("隐藏 Tab 栏");
                            if ui
                                .checkbox(&mut self.settings.hide_tabs, "极简模式")
                                .changed()
                            {
                                self.settings.save();
                            }
                            ui.end_row();
                            ui.label("隐藏红绿灯");
                            if ui
                                .checkbox(&mut self.settings.undecorated, "无边框（重启生效）")
                                .changed()
                            {
                                self.settings.save();
                            }
                            ui.end_row();
                            ui.label("背景图");
                            ui.horizontal(|ui| {
                                ui.text_edit_singleline(&mut self.settings.background_image);
                                if ui.small_button("应用").clicked() {
                                    self.bg_loaded = None; // 强制重载
                                    self.settings.save();
                                }
                            });
                            ui.end_row();
                            ui.label("导入主题");
                            ui.horizontal(|ui| {
                                ui.text_edit_singleline(&mut self.theme_import);
                                if ui.small_button("导入 .itermcolors").clicked() {
                                    let p = self.theme_import.trim().to_string();
                                    if p.is_empty() {
                                        self.theme_msg =
                                            Some("请输入 .itermcolors 文件路径".into());
                                    } else {
                                        match crate::gui::themes::import_itermcolors(
                                            std::path::Path::new(&p),
                                        ) {
                                            Ok(name) => {
                                                self.theme_name = name.clone();
                                                self.theme = crate::gui::themes::load_theme(&name);
                                                self.theme_msg =
                                                    Some(format!("✓ 已导入主题 {name}"));
                                            }
                                            Err(e) => {
                                                self.theme_msg = Some(format!("导入失败：{e}"))
                                            }
                                        }
                                    }
                                }
                            });
                            ui.end_row();
                        });
                    if let Some(msg) = &self.theme_msg {
                        ui.label(RichText::new(msg).color(Color32::from_rgb(120, 190, 120)));
                    }
                });
            if saved {
                self.settings_modal = false;
                // 已配置模型：若 Agent 面板此前因缺 provider 失败，重试打开
                self.agent_open = false;
                self.agent = None;
            }
            if close {
                self.settings_modal = false;
            }
        }

        // ── 对话历史弹窗 ──
        if self.history_modal {
            let mut close = false;
            let mut restore = false;
            let mut clear_preview = false;
            egui::Window::new("对话历史")
                .collapsible(false)
                .default_size([440.0, 420.0])
                .show(ctx, |ui| {
                    if let Some(msgs) = &self.history_preview {
                        if msgs.is_empty() {
                            ui.label(RichText::new("暂无历史记录").color(Color32::GRAY));
                        }
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for (role, text) in msgs {
                                    let color = match role.as_str() {
                                        "user" => Color32::from_rgb(107, 179, 74),
                                        _ => Color32::from_rgb(220, 220, 220),
                                    };
                                    ui.label(
                                        RichText::new(format!("[{role}] {text}")).color(color),
                                    );
                                }
                            });
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("恢复此会话").clicked() {
                                restore = true;
                            }
                            if ui.button("关闭").clicked() {
                                close = true;
                            }
                        });
                    } else {
                        ui.label(RichText::new("加载中…").color(Color32::GRAY));
                    }
                });
            if restore {
                if let Some(msgs) = self.history_preview.clone() {
                    self.restore_msgs = Some(msgs);
                }
                // 重建 agent（应用恢复）
                self.agent = None;
                self.agent_open = false;
                self.toggle_agent_simple();
                clear_preview = true;
                close = true;
            }
            if clear_preview {
                self.history_preview = None;
            }
            if close {
                self.history_modal = false;
            }
        }

        // ── MCP 连接错误提示（一次性）──
        if !self.mcp_errors.is_empty() {
            let errs = self.mcp_errors.clone();
            self.mcp_errors.clear();
            self.error_toast = Some(format!("MCP 连接失败：\n{}", errs.join("\n")));
        }

        // ── 错误 toast（一次性）──
        if let Some(msg) = self.error_toast.clone() {
            egui::Window::new("提示")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -60.0])
                .show(ctx, |ui| {
                    ui.label(msg);
                    if ui.button("知道了").clicked() {
                        self.error_toast = None;
                    }
                });
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

/// 环境分组顺序（prod 最前，unknown 最后）。
fn env_order(e: &Env) -> u8 {
    match e {
        Env::Prod => 0,
        Env::Staging => 1,
        Env::Dev => 2,
        Env::Local => 3,
        Env::Unknown => 4,
    }
}

/// 环境分组标题文本。
fn env_label(e: Env) -> &'static str {
    match e {
        Env::Prod => "▍生产 prod",
        Env::Staging => "▍预发 staging",
        Env::Dev => "▍开发 dev",
        Env::Local => "▍本机 local",
        Env::Unknown => "▍未标注",
    }
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

/// 文件大小人性化显示。
fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// 解析反思输出并写入记忆库（FACTS/LESSONS/SKILL_IF_ANY 分节）。
/// 返回给用户看的摘要。
fn persist_reflection(raw: &str) -> String {
    let mut facts = Vec::new();
    let mut lessons = Vec::new();
    let mut skill: Option<(String, String, String)> = None;
    let mut section = "";

    for line in raw.lines() {
        let t = line.trim();
        if t.starts_with("FACTS:") {
            section = "facts";
            continue;
        }
        if t.starts_with("LESSONS:") {
            section = "lessons";
            continue;
        }
        if t.starts_with("SKILL_IF_ANY:") {
            section = "skill";
            continue;
        }
        if t.starts_with('-') {
            let item = t.trim_start_matches('-').trim().to_string();
            if item.is_empty() {
                continue;
            }
            match section {
                "facts" => facts.push(item),
                "lessons" => lessons.push(item),
                "skill" => {
                    // SKILL 段：逐行收集 frontmatter 与正文
                    if skill.is_none() {
                        skill =
                            Some(("skill".into(), "自进化生成的技能草稿".into(), String::new()));
                    }
                    if let Some((_, _, body)) = &mut skill {
                        body.push_str(line);
                        body.push('\n');
                    }
                }
                _ => {}
            }
        }
    }

    let mut n = 0usize;
    if !facts.is_empty() {
        let content = facts
            .iter()
            .map(|f| format!("- {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        if ares_core::memory::append_memory("facts.md", &content).is_ok() {
            n += facts.len();
        }
    }
    if !lessons.is_empty() {
        let content = lessons
            .iter()
            .map(|l| format!("- {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        if ares_core::memory::append_memory("lessons.md", &content).is_ok() {
            n += lessons.len();
        }
    }
    if let Some((name, desc, body)) = skill {
        if !body.trim().is_empty() {
            let full = format!("---\nname: {name}\ndescription: {desc}\n---\n\n{body}");
            if ares_core::memory::write_skill(&name, &full).is_ok() {
                n += 1;
            }
        }
    }

    if n > 0 {
        format!("自进化：已更新 {n} 条记忆（facts/lessons）")
    } else {
        "反思完成：无可新增的稳定记忆".to_string()
    }
}
