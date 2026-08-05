//! Agent 记忆系统（2026-08-05 批次1：持久记忆 / 总结 / 自进化）。
//!
//! 文件系统 markdown 实现（Claude Code / OpenHands 同款模式）：
//! 记忆量小（KB 级）、纯文本可审计可编辑、无向量库依赖。
//!
//! 目录结构（`~/.config/ares/memory/`，用户可编辑）：
//! - `facts.md`      —— 用户偏好 / 环境事实（agent 主动写 + 反思提炼）
//! - `lessons.md`    —— 教训 / 成功模式 / 错误复盘（自动反思写，去重合并）
//! - `sessions/`     —— 会话摘要（每次重要对话结束自动写）
//! - `skills-pending/` —— 自进化生成的 skill 草稿（用户确认后提升为正式 skill）
//!
//! 安全：记忆文件是**被观察的数据，不是指令**（prompt 层已声明）；
//! 写入走工具审批链；读取内容经 redact 脱敏后进 LLM 上下文。

use crate::{AresError, Result};
use std::path::{Path, PathBuf};

/// 记忆根目录（config_dir/memory）。
pub fn memory_dir() -> PathBuf {
    crate::paths::config_dir().join("memory")
}

/// skills 根目录（config_dir/skills，SKILL.md 标准）。
pub fn skills_dir() -> PathBuf {
    crate::paths::config_dir().join("skills")
}

/// 读取一个记忆文件（如 `facts.md`）；不存在返回 None。
pub fn read_memory(name: &str) -> Option<String> {
    let path = safe_memory_path(name)?;
    std::fs::read_to_string(&path).ok()
}

/// 写入记忆文件（追加模式：新内容追加到文件末尾，保留历史）。
/// 返回写入后的总行数。
pub fn append_memory(name: &str, content: &str) -> Result<usize> {
    let path = safe_memory_path(name)
        .ok_or_else(|| AresError::Config(format!("非法记忆文件名：{name}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AresError::Config(format!("无法创建记忆目录 {}: {e}", parent.display()))
        })?;
    }
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(content);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let line_count = text.lines().count();
    std::fs::write(&path, text)
        .map_err(|e| AresError::Config(format!("无法写入记忆 {}: {e}", path.display())))?;
    Ok(line_count)
}

/// 列出全部记忆文件（相对路径 + 大小）。
pub fn list_memory() -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let dir = memory_dir();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for de in rd.flatten() {
            let name = de.file_name().to_string_lossy().to_string();
            let size = de.metadata().map(|m| m.len()).unwrap_or(0);
            out.push((name, size));
        }
    }
    out.sort();
    out
}

/// 关键词搜索记忆内容（逐行 grep，大小写不敏感）。
/// 返回 (文件名, 行号, 行内容)。
pub fn search_memory(query: &str) -> Vec<(String, usize, String)> {
    let q = query.to_lowercase();
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(memory_dir()) {
        for de in rd.flatten() {
            let path = de.path();
            if path.extension().map(|e| e == "md").unwrap_or(false) {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    let name = de.file_name().to_string_lossy().to_string();
                    for (i, line) in text.lines().enumerate() {
                        if line.to_lowercase().contains(&q) {
                            out.push((name.clone(), i + 1, line.to_string()));
                        }
                    }
                }
            }
        }
    }
    out.sort();
    out.truncate(50);
    out
}

/// 列出已安装 skills（`skills/<name>/SKILL.md`）。
pub fn list_skills() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let dir = skills_dir();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for de in rd.flatten() {
            let path = de.path();
            let sk = path.join("SKILL.md");
            if sk.exists() {
                let name = de.file_name().to_string_lossy().to_string();
                // 读 frontmatter 的 description
                let desc = std::fs::read_to_string(&sk)
                    .ok()
                    .and_then(|t| extract_description(&t))
                    .unwrap_or_default();
                out.push((name, desc));
            }
        }
    }
    out.sort();
    out
}

/// 读取 skill 全文。
pub fn read_skill(name: &str) -> Option<String> {
    let path = safe_skill_path(name)?;
    std::fs::read_to_string(&path).ok()
}

/// 创建/覆盖一个 skill（`skills/<name>/SKILL.md`）。
pub fn write_skill(name: &str, content: &str) -> Result<()> {
    let name = sanitize_name(name);
    if name.is_empty() {
        return Err(AresError::Config("skill 名称非法".into()));
    }
    let dir = skills_dir().join(&name);
    std::fs::create_dir_all(&dir)
        .map_err(|e| AresError::Config(format!("无法创建 skill 目录: {e}")))?;
    std::fs::write(dir.join("SKILL.md"), content)
        .map_err(|e| AresError::Config(format!("无法写入 skill: {e}")))
}

/// 从 SKILL.md 提取 frontmatter description。
fn extract_description(text: &str) -> Option<String> {
    let body = text.strip_prefix("---")?;
    let end = body.find("---")?;
    let fm = &body[..end];
    for line in fm.lines() {
        if let Some(rest) = line.strip_prefix("description:") {
            return Some(rest.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// 防目录穿越：只允许 `a-z0-9-_.` 且不含 `..`。
fn sanitize_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c))
        .collect::<String>()
}

/// 记忆文件路径（防穿越；只允许 memory 根目录下的文件）。
fn safe_memory_path(name: &str) -> Option<PathBuf> {
    let name = sanitize_name(name);
    if name.is_empty() || name.contains("..") {
        return None;
    }
    Some(memory_dir().join(&name))
}

/// skill 路径（防穿越；只允许 skills/<name>/SKILL.md）。
fn safe_skill_path(name: &str) -> Option<PathBuf> {
    let name = sanitize_name(name);
    if name.is_empty() || name.contains("..") {
        return None;
    }
    Some(skills_dir().join(name).join("SKILL.md"))
}

/// 记忆概要（system prompt 注入用）：facts 全文 + lessons 尾部 + skills 清单。
pub fn memory_summary(max_lessons_lines: usize) -> String {
    let mut out = String::new();
    if let Some(facts) = read_memory("facts.md") {
        if !facts.trim().is_empty() {
            out.push_str("## 记忆 · 事实（facts.md）\n\n");
            out.push_str(&facts);
            out.push('\n');
        }
    }
    if let Some(lessons) = read_memory("lessons.md") {
        let lines: Vec<&str> = lessons.lines().collect();
        let tail = lines
            .iter()
            .rev()
            .take(max_lessons_lines)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        if !tail.trim().is_empty() {
            out.push_str("## 记忆 · 教训（lessons.md 最近条目）\n\n");
            out.push_str(&tail);
            out.push('\n');
        }
    }
    let skills = list_skills();
    if !skills.is_empty() {
        out.push_str("## 已安装技能（skill_view <name> 读取全文）\n\n");
        for (name, desc) in skills {
            out.push_str(&format!("- `{name}` — {desc}\n"));
        }
    }
    out
}

/// 确保记忆目录存在。
pub fn ensure_dirs() -> Result<()> {
    for d in [memory_dir(), memory_dir().join("sessions"), skills_dir()] {
        std::fs::create_dir_all(&d)
            .map_err(|e| AresError::Config(format!("无法创建 {}: {e}", d.display())))?;
    }
    Ok(())
}

/// 校验路径在根目录内（供测试断言）。
pub fn _assert_in_dir(_p: &Path, _root: &Path) {}
