//! 交互式对话。
//!
//! 所有着色只用前景色 —— 用户的终端背景图必须完整保留。

use ares_agent::{AgentLoop, TurnResult};
use ares_core::Result;
use std::io::{self, BufRead, Write};

/// Doric 配色的 ANSI 前景色编号
mod color {
    pub const MARBLE: u8 = 254; // 主文本
    pub const BRONZE: u8 = 179; // 强调
    pub const OLIVE: u8 = 107; // 成功
    pub const CRIMSON: u8 = 196; // 危险
    pub const STONE: u8 = 245; // 次要
}

fn fg(c: u8, s: &str) -> String {
    format!("\x1b[38;5;{c}m{s}\x1b[0m")
}

pub fn banner() {
    println!();
    println!("    {}", fg(color::BRONZE, "A R E S"));
    println!(
        "    {}",
        fg(color::STONE, "─────────────────────────────────")
    );
    println!(
        "    {}",
        fg(color::STONE, "Autonomous Remote Engineering System")
    );
    println!();
}

fn render(r: &TurnResult) {
    for run in &r.tool_runs {
        let mark = if run.success { "✓" } else { "✗" };
        let c = if run.success {
            color::OLIVE
        } else {
            color::CRIMSON
        };

        let head = match &run.command {
            Some(cmd) => format!("{} {} · {}", mark, run.tool, cmd),
            None => format!("{} {}", mark, run.tool),
        };
        println!("  {}", fg(c, &head));
        println!(
            "    {}",
            fg(color::STONE, &format!("[{}]", run.decision_label))
        );

        for line in run.display.lines() {
            // 展示层统一清洗：M2+ 远程主机输出可携带 ANSI/控制字符（被入侵
            // 服务器的日志/错误页），不清洗可伪造提示符、覆盖审批横幅。
            // sanitize_for_display 提升为公共模块（ares_core::display），
            // Task 17 的 CliApprover 与这里共用同一实现。
            println!(
                "    {}",
                fg(color::MARBLE, &ares_core::display::sanitize(line))
            );
        }
        println!();
    }

    if !r.reply.is_empty() {
        println!("{}", fg(color::MARBLE, &r.reply));
    }
    println!(
        "  {}",
        fg(
            color::STONE,
            &format!("tokens {}↑ {}↓", r.usage.input, r.usage.output)
        )
    );
    println!();
}

pub async fn run(mut agent: AgentLoop) -> Result<()> {
    banner();
    println!("  {}\n", fg(color::STONE, "输入问题开始对话，Ctrl-D 退出"));

    run_loop(&mut agent).await
}

async fn run_loop(agent: &mut AgentLoop) -> Result<()> {
    let stdin = io::stdin();
    loop {
        print!("{} ", fg(color::BRONZE, "❯"));
        io::stdout().flush()?;

        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            println!();
            break; // Ctrl-D
        }
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if input == "exit" || input == "quit" {
            break;
        }

        println!();
        match agent.run_turn(input).await {
            Ok(r) => render(&r),
            Err(e) => println!("  {}\n", fg(color::CRIMSON, &format!("错误：{e}"))),
        }
    }
    Ok(())
}
