mod cli;
mod gui;
mod mcp;
mod repl;
mod vault;

use anyhow::{Context, Result};
use ares_agent::{AgentLoop, Approver, CliApprover};
use ares_audit::AuditWriter;
use ares_core::config::HostsConfig;
use ares_core::{paths, AresError, HostId};
use ares_exec::{Executor, LocalExecutor};
use ares_llm::{AnthropicProvider, OpenAiProvider, Provider, ProviderKind, ProvidersConfig};
use ares_policy::{PolicyConfig, PolicyEngine};
use ares_tools::{default_registry, Tool, ToolContext};
use clap::Parser;
use cli::{AuditAction, Cli, Command, ProviderAction};
use std::sync::Arc;
use tokio::sync::Mutex;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("ARES_LOG")
                .unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Some(Command::Init) => cmd_init(),
        Some(Command::Chat) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(cmd_chat())
        }
        Some(Command::Audit { action }) => cmd_audit(action),
        Some(Command::Provider { action }) => cmd_provider(action),
        Some(Command::VaultGet { alias }) => {
            // SSH_ASKPASS 用途：stdout 直接输出 secret（无换行）
            if let Some(s) = crate::vault::get(&alias) {
                print!("{s}");
            }
            Ok(())
        }
        Some(Command::VaultMigrate) => {
            // 从 hosts.toml + providers.toml 收集 alias，一次性迁移 keychain → vault
            use ares_core::config::HostsConfig;
            use ares_llm::ProvidersConfig;
            let mut aliases: Vec<String> = Vec::new();
            if let Ok(h) = HostsConfig::load() {
                for (k, e) in &h.hosts {
                    if e.auth == "password" {
                        aliases.push(format!("ssh-pw:{k}"));
                    }
                }
            }
            if let Ok(p) = ProvidersConfig::load() {
                for e in p.providers.values() {
                    if !e.keychain_account.is_empty() {
                        aliases.push(e.keychain_account.clone());
                    }
                }
            }
            let n = crate::vault::migrate_from_keychain(&aliases).map_err(anyhow::Error::msg)?;
            println!("✓ 已迁移 {n} 条凭据到本地 vault（之后不再弹系统授权）");
            Ok(())
        }
        // M1.5 形态调整（2026-08-05 用户拍板）：默认入口 = 简易 iTerm2 GUI
        None => {
            use std::io::IsTerminal;
            if !std::io::stdin().is_terminal() {
                eprintln!(
                    "\x1b[38;5;245mARES GUI 需要从终端启动（当前非 tty 环境）。\
                     \n请直接运行 `ares`，或用 `ares chat` 开始本机对话。\x1b[0m"
                );
                Ok(())
            } else {
                run_gui()
            }
        }
    }
}

/// GUI 入口：eframe 窗口（多 tab 终端 + Agent 面板）。
fn run_gui() -> Result<()> {
    let rt = Arc::new(tokio::runtime::Runtime::new()?);
    let settings = crate::gui::settings::GuiSettings::load();
    let options = eframe::NativeOptions {
        // α 架构：wgpu 渲染后端（自研终端渲染器用原生纹理合成）
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1080.0, 700.0])
            .with_min_inner_size([640.0, 400.0])
            .with_title("ARES — Autonomous Remote Engineering System")
            // iTerm2 化：隐藏红绿灯（设置里可关，重启生效）
            .with_decorations(!settings.undecorated),
        ..Default::default()
    };
    eframe::run_native(
        "ARES",
        options,
        Box::new(move |cc| Ok(Box::new(gui::GuiApp::new(cc, rt.clone())))),
    )
    .map_err(|e| anyhow::anyhow!("GUI 启动失败：{e}"))?;
    Ok(())
}

fn cmd_init() -> Result<()> {
    paths::ensure_dirs()?;
    ares_agent::prompt::install_defaults()?;
    write_providers_template()?;
    println!("✓ 配置目录已就绪：{}", paths::config_dir().display());
    println!("  已写入 SOUL.md、USER.md 与 providers.toml 模板（若已存在则保留原文件）");
    println!();
    println!("下一步：");
    println!(
        "  1. 编辑 {}/providers.toml 填入你的模型供应商",
        paths::config_dir().display()
    );
    println!("  2. 运行 ares provider add <name> 写入 API key");
    println!("  3. 运行 ares 选择主机开始");
    Ok(())
}

/// 写入 providers.toml 模板。
///
/// 没有模板是鸡生蛋问题：`ares` 启动时要求 providers.toml 存在，
/// 而用户没有任何可参考的格式。模板给出注释齐全的示例，
/// 用户复制改 name / base_url / model 即可。
fn write_providers_template() -> Result<()> {
    let path = paths::config_dir().join("providers.toml");
    if path.exists() {
        return Ok(());
    }
    let template = r#"# LLM 供应商配置。API key 不写在这里 —— 用 `ares provider add <name>` 存入 Keychain。
# 修改后无需重启：下次对话生效。

active = "deepseek"

[providers.deepseek]
kind = "openai"                      # openai | anthropic
base_url = "https://api.deepseek.com/v1"
model = "deepseek-chat"
keychain_account = "llm:deepseek"

# [providers.anthropic]
# kind = "anthropic"
# base_url = "https://api.anthropic.com/v1"
# model = "claude-sonnet-4-5"
# keychain_account = "llm:anthropic"
"#;
    std::fs::write(&path, template)
        .map_err(|e| AresError::Config(format!("无法写入 {}: {e}", path.display())))?;
    Ok(())
}

fn cmd_audit(action: AuditAction) -> Result<()> {
    match action {
        AuditAction::Verify => {
            let report = ares_audit::verify_all()?;
            match report.broken_at {
                None => {
                    println!(
                        "✓ 审计链完整，共 {} 条记录，{} 个文件，未发现篡改",
                        report.total, report.files
                    );
                    Ok(())
                }
                Some(b) => {
                    eprintln!(
                        "✗ 审计链在 {} 第 {} 条记录处断裂\n  {}",
                        b.file.display(),
                        b.index,
                        b.reason
                    );
                    std::process::exit(1);
                }
            }
        }
    }
}

fn cmd_provider(action: ProviderAction) -> Result<()> {
    match action {
        ProviderAction::Add { name } => {
            let cfg = ProvidersConfig::load()?;
            let entry = cfg
                .providers
                .get(&name)
                .with_context(|| format!("providers.toml 中没有名为 {name:?} 的供应商"))?;

            // 用 rpassword 读取，输入不回显、不进 shell 历史
            let key = rpassword::prompt_password(format!("请输入 {name} 的 API key（不回显）："))
                .context("读取 API key 失败")?;
            if key.trim().is_empty() {
                anyhow::bail!("API key 不能为空");
            }

            crate::vault::set(&entry.keychain_account, key.trim()).map_err(anyhow::Error::msg)?;
            println!("✓ 已写入本地加密 vault：{}", entry.keychain_account);
            Ok(())
        }
    }
}

/// `ares chat`：本机纯对话（M1 的原始 REPL 入口）。
async fn cmd_chat() -> Result<()> {
    paths::ensure_dirs()?;
    let agent = build_agent(
        vec![HostId::localhost()],
        Arc::new(LocalExecutor::new()),
        Arc::new(CliApprover::new()),
        vec![],
    )
    .await?;
    repl::run(agent).await?;
    Ok(())
}

/// 公共构建：策略引擎 / provider / 审计 / 上下文。
///
/// executor 由调用方决定：CLI 用 LocalExecutor；GUI 用
/// TerminalSessionExecutor（agent 的命令注入当前终端会话）。
/// approver 同理：CLI 用 CliApprover（终端 y/n），GUI 用确认弹窗。
pub(crate) async fn build_agent(
    scope: Vec<HostId>,
    executor: Arc<dyn Executor>,
    approver: Arc<dyn Approver>,
    extra_tools: Vec<Arc<dyn Tool>>,
) -> Result<AgentLoop> {
    let hosts_cfg = HostsConfig::load()?;
    let policy_cfg = PolicyConfig::load()?;
    let policy = Arc::new(PolicyEngine::new(policy_cfg, hosts_cfg)?);

    let providers = ProvidersConfig::load()?;
    let (name, entry) = providers.active_entry()?;
    let api_key = crate::vault::get(&entry.keychain_account).with_context(|| {
        format!(
            "vault 中没有 {}。请先运行：ares provider add {name}",
            entry.keychain_account
        )
    })?;

    let provider: Arc<dyn Provider> = match entry.kind {
        ProviderKind::Openai => Arc::new(OpenAiProvider::new(name, &entry.base_url, api_key)),
        ProviderKind::Anthropic => Arc::new(AnthropicProvider::new(name, &entry.base_url, api_key)),
    };

    let audit = Arc::new(Mutex::new(AuditWriter::open()?));
    let session_id = format!("sess-{}", ares_audit::now_rfc3339());

    let ctx = ToolContext::new(executor, policy, audit, scope, session_id, "agent");

    // MCP 工具等动态工具注入注册表（2026-08-05 批次4）
    let mut registry = default_registry();
    for t in extra_tools {
        registry.register(t);
    }

    Ok(AgentLoop::new(
        provider,
        registry,
        ctx,
        approver,
        &entry.model,
    )?)
}

#[cfg(test)]
mod gpu_stress_tests {
    //! GPU 渲染回归测试：独立 wgpu 设备 + 各种屏幕内容 → render() 抓 panic。
    //! `cargo test -p ares --bin ares gpu_stress -- --nocapture`

    use crate::gui::gpu::GpuTerminalRenderer;

    #[test]
    fn render_all_content_types() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .expect("no adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("stress"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        ))
        .expect("no device");
        let device = std::sync::Arc::new(device);
        let queue = std::sync::Arc::new(queue);
        let mut egui_renderer =
            egui_wgpu::Renderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb, None, 1, false);
        let mut gpu = GpuTerminalRenderer::new(device.clone(), queue.clone(), 2.0);
        let theme = crate::gui::themes::builtin_themes()
            .into_iter()
            .find(|t| t.name == "Default")
            .expect("Default theme");

        let cases: Vec<String> = vec![
            "hello world => != :: ## **".to_string(), // 连字
            "🎉 🚀 🐳 😀".to_string(),                // emoji
            "你好，世界 中文测试".to_string(),        // CJK
            "\u{1b}[31mred\u{1b}[0m \u{1b}[38;5;208morange\u{1b}[0m".to_string(), // 256 色
            "wide: ＡＢＣ".to_string(),               // 全角
            "a".repeat(200),                          // 超长行
            "\u{1b}[44mblue bg\u{1b}[0m \u{1b}[1mbold\u{1b}[0m \u{1b}[4munder\u{1b}[0m".to_string(),
            "\u{1b}[2J\u{1b}[H".to_string(), // 清屏
        ];
        for (i, content) in cases.iter().enumerate() {
            let mut p = vt100::Parser::new(24, 80, 10000);
            p.set_scrollback(10000);
            p.process(content.as_bytes());
            p.process(b"\r\n");
            let screen = p.screen().clone();
            let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(640.0, 432.0));
            let _ = gpu.render(
                &screen,
                &theme,
                None,
                "block",
                false,
                rect,
                &mut egui_renderer,
            );
            let sel = crate::gui::term::SelectRange::normalized(0, 0, 2, 10);
            let _ = gpu.render(
                &screen,
                &theme,
                Some(&sel),
                "beam",
                true,
                rect,
                &mut egui_renderer,
            );
            let _ = gpu.render(
                &screen,
                &theme,
                Some(&sel),
                "underline",
                true,
                rect,
                &mut egui_renderer,
            );
            eprintln!("case {i} ok");
        }
        // resize（target 重建路径）
        let screen = vt100::Parser::new(24, 80, 10000).screen().clone();
        let rect2 = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1200.0, 700.0));
        let _ = gpu.render(
            &screen,
            &theme,
            None,
            "block",
            false,
            rect2,
            &mut egui_renderer,
        );
        eprintln!("resize ok");
    }
}
