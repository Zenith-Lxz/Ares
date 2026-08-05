//! 展示前清洗：一切进入终端 / 通知的用户可见字符串。

use regex::Regex;
use std::sync::LazyLock;

/// 完整 CSI 序列：ESC [ 参数字节 最终字节（含 SGR 颜色码，38;5;196m 等）。
/// 只删 ESC 本身会留下 `[2K` 这类残留文本，必须整段删除。
static CSI_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;:?]*[ -/]*[@-~]").expect("static"));

/// 清洗任意进入终端 / 通知的用户可见字符串。
///
/// - 先剔除完整 CSI 序列（`\x1b[2K` 清行、`\x1b[38;5;196m` 改色等）
/// - 剔除全部 C0/C1 控制字符与 ESC（`\x00-\x1f`、`\x7f`、`\u{80}-\u{9f}`）
/// - 剔除不可见 / 双向控制 Unicode（U+200B–U+200F、U+202A–U+202E、U+2066–U+2069）
/// - 折叠换行为空格，单行硬截断到 120 字符
pub fn sanitize(s: &str) -> String {
    let no_csi = CSI_RE.replace_all(s, "");
    let cleaned: String = no_csi
        .chars()
        .filter(|c| {
            let cp = *c as u32;
            // 换行/回车（0x0a/0x0d）**放行**并随后折叠为空格 —— 若在此删除，
            // 折叠阶段就无换行可折，多行文本会被拼成一行（"a\nb" → "ab"）。
            // 其余 C0/C1 控制字符与 ESC 一律剔除。
            !((cp <= 0x1f && cp != 0x0a && cp != 0x0d) || cp == 0x7f || (0x80..=0x9f).contains(&cp))
                && !(0x200b..=0x200f).contains(&cp)
                && !(0x202a..=0x202e).contains(&cp)
                && !(0x2066..=0x2069).contains(&cp)
        })
        .collect();
    let one_line = cleaned.replace(['\n', '\r'], " ");
    // 压缩连续空白（`\r\n` 双字符会折叠出双空格）—— 与 Task 17 的
    // sanitize_for_display 语义一致：单行、无连续空白。
    let one_line = one_line.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = one_line.chars();
    chars.by_ref().take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_full_csi_sequences() {
        // 完整 CSI 序列（含擦行/光标控制）必须整段删除，不留 [2K 残渣
        assert_eq!(sanitize("a\x1b[2Kb"), "ab");
        assert_eq!(sanitize("a\x1b[1A\x1b[0Kb"), "ab");
        // 颜色码也整段删（展示层不需要保留注入的颜色）
        assert_eq!(sanitize("a\x1b[38;5;196mred\x1b[0mb"), "aredb");
    }

    #[test]
    fn strips_control_and_invisible_chars() {
        let s = sanitize("a\x00b\x7fc\u{200b}d\u{202a}e\u{2066}f");
        assert_eq!(s, "abcdef");
        assert!(!s.contains('\u{1b}'));
    }

    #[test]
    fn collapses_newlines_and_truncates() {
        assert_eq!(sanitize("a\nb\r\nc"), "a b c");
        let long = sanitize(&"x".repeat(300));
        assert_eq!(long.chars().count(), 120);
    }
}
