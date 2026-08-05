//! 凭据脱敏。
//!
//! 任何进入审计日志或 LLM 上下文的文本都必须先过这里。
//! 宁可误伤（把不是密钥的东西也遮掉），不可漏放 ——
//! 密钥一旦进了模型上下文或日志，就再也收不回来了。

use once_cell::sync::Lazy;
use regex::Regex;

/// 替换后的占位符。
const MASK: &str = "«已脱敏»";

struct Rule {
    re: Regex,
    /// 替换模板。`$1` 等捕获组会被保留，用于留下可辨识的前缀。
    replacement: &'static str,
}

static RULES: Lazy<Vec<Rule>> = Lazy::new(|| {
    let rules: Vec<(&str, &'static str)> = vec![
        // PEM 私钥块 —— 整块替换，包括头尾
        (
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
            "-----BEGIN PRIVATE KEY-----«已脱敏»-----END PRIVATE KEY-----",
        ),
        // OpenAI 风格 key
        (r"\bsk-[A-Za-z0-9_\-]{16,}", MASK),
        // shadow 密码哈希（/etc/shadow 行：user:$6$salt$hash:...）——
        // observer 允许 cat /etc/shadow，哈希裸奔进 LLM 上下文离线可破解。
        // 算法标识 $[0-9A-Za-z] 覆盖 $1$/$6$/$y$(yescrypt)/$gy$ 等
        (
            r"(?m)^[A-Za-z0-9_.-]+:\$[0-9A-Za-z]\$[A-Za-z0-9./$]{8,}:[^:\n]*:",
            "«shadow 行已脱敏»",
        ),
        // Anthropic
        (r"\bsk-ant-[A-Za-z0-9_\-]{16,}", MASK),
        // GitHub token（ghp_ / gho_ / ghs_ / ghr_ / github_pat_）
        (r"\bgh[pousr]_[A-Za-z0-9]{16,}", MASK),
        (r"\bgithub_pat_[A-Za-z0-9_]{20,}", MASK),
        // AWS access key id 与 secret
        (r"\bAKIA[0-9A-Z]{16}\b", MASK),
        (
            r"(?i)\b(aws_secret_access_key\s*[=:]\s*)\S+",
            "${1}«已脱敏»",
        ),
        // Slack
        (r"\bxox[baprs]-[A-Za-z0-9\-]{10,}", MASK),
        // JWT
        (
            r"\beyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+",
            MASK,
        ),
        // 通用 key=value 形式的密码类字段
        (
            r"(?i)\b(password|passwd|pwd|secret|token|api[_\-]?key|access[_\-]?key)(\s*[=:]\s*)\S+",
            "${1}${2}«已脱敏»",
        ),
        // 带前缀的环境变量赋值：PGPASSWORD、MYSQL_PWD、REDIS_PASSWORD、
        // AWS_SECRET_ACCESS_KEY、MYAPP_API_TOKEN 等（运维输出里最常见的泄露形态）。
        // 要求「大写前缀 + 密码类词根」，单独一个 `PWD=` 不匹配（shell 普通变量，
        // 误伤会让 env 输出不可读）；但 `MYSQL_PWD=` 的 PWD 前有 `MYSQL_` 前缀 → 命中。
        (
            r"(?i)([A-Z][A-Z0-9_]*(?:PASSWORD|PASSWD|PWD|SECRET|TOKEN|API[_\-]?KEY|ACCESS[_\-]?KEY))(\s*[=:]\s*)\S+",
            "${1}${2}«已脱敏»",
        ),
        // 命令行参数形式
        (
            r"(?i)(--(?:password|token|secret|api-key)[=\s]+)\S+",
            "${1}«已脱敏»",
        ),
        // URL 中的 basic auth
        (r"://[^:/@\s]+:[^@/\s]+@", "://«已脱敏»@"),
    ];

    rules
        .into_iter()
        .map(|(pat, rep)| Rule {
            re: Regex::new(pat).expect("built-in redaction pattern must compile"),
            replacement: rep,
        })
        .collect()
});

/// 对文本做脱敏。
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for rule in RULES.iter() {
        out = rule.re.replace_all(&out, rule.replacement).into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_masked(input: &str, must_not_contain: &str) {
        let out = redact(input);
        assert!(
            !out.contains(must_not_contain),
            "脱敏失败：输出中仍含有 {must_not_contain:?}\n输出：{out}"
        );
    }

    #[test]
    fn masks_openai_key() {
        assert_masked(
            "export OPENAI_API_KEY=sk-proj-abcdefghij1234567890ABCDEF",
            "sk-proj-abcdefghij1234567890ABCDEF",
        );
    }

    #[test]
    fn masks_anthropic_key() {
        assert_masked(
            "ANTHROPIC_API_KEY=sk-ant-api03-xxxxxxxxxxxxxxxxxxxx",
            "sk-ant-api03-xxxxxxxxxxxxxxxxxxxx",
        );
    }

    #[test]
    fn masks_github_tokens() {
        assert_masked("token: ghp_12...ghij", "ghp_12...ghij");
        // 真实格式：github_pat_ 前缀 + 22 位以上。
        // 注意：占位符形态（github...2345）不满足 {20,} 长度要求，
        // 测试必须用真实格式才能验证规则真的生效。
        assert_masked(
            "GITHUB_TOKEN=github_pat_11ABCDEFGHIJKLMNOPQRSTUVWXYZ012345",
            "github_pat_11ABCDEFGHIJKLMNOPQRSTUVWXYZ012345",
        );
        assert_masked(
            "gho_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
            "gho_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
        );
    }

    #[test]
    fn masks_aws_credentials() {
        // 真实格式：AKIA + 16 位（AWS 官方文档示例；占位符 AKIAIO...MPLE
        // 不满足 [0-9A-Z]{16} 的精确长度，测试必须用真实形态）
        assert_masked("AKIAIOSFODNN7EXAMPLE", "AKIAIOSFODNN7EXAMPLE");
        assert_masked(
            "aws_secret_access_key=wJalrXUtnFEMI/K7MDENG/bPxRfiCY",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCY",
        );
    }

    #[test]
    fn masks_private_key_block() {
        // 真实 PEM 块：整块替换为 [REDACTED PRIVATE KEY]。
        // 旧测试把「替换结果」当输入（输入就是 [REDACTED PRIVATE KEY]），
        // 断言恒真/恒假，测试本身无效 —— 必须输入真实 PEM 块。
        let input = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAAC0VFRElURUQ=
secretline-token-here
-----END OPENSSH PRIVATE KEY-----";
        let out = redact(input);
        assert!(!out.contains("b3BlbnNzaC1rZXktdjEAAAAA"));
        assert!(!out.contains("secretline-token-here"));
        // PEM 规则替换为保留框架的占位（-----BEGIN PRIVATE KEY-----«已脱敏»-----END PRIVATE KEY-----）
        assert!(out.contains("«已脱敏»"));
    }

    #[test]
    fn masks_password_assignments() {
        assert_masked("mysql -u root --password=hunter2", "hunter2");
        assert_masked("PGPASSWORD: s3cr3tpass", "s3cr3tpass");
        assert_masked("api_key = abcdef123456", "abcdef123456");
    }

    #[test]
    fn masks_shadow_hash_rows() {
        // /etc/shadow 行（observer 允许 cat /etc/shadow，哈希必须脱敏）
        assert_masked(
            "root:$6$saltsalt$abcdefghijklmnopqrstuvwxyz0123456789:19000:0:99999:7:::",
            "$6$saltsalt$abcdefghijklmnopqrstuvwxyz0123456789",
        );
        // 普通用户行同样脱敏
        assert_masked(
            "www-data:$y$j9T$salt$hashhashhashhashhashhashhashhash:19001:0:99999:7:::",
            "$y$j9T$salt$hashhashhashhashhashhashhashhash",
        );
    }

    #[test]
    fn masks_url_basic_auth() {
        assert_masked("https://admin:hunter2@example.com/api", "hunter2");
    }

    #[test]
    fn masks_jwt() {
        assert_masked(
            "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NSJ9.dBjftJeZ4CVPmB92K27uhbUJU1p1r",
            "dBjftJeZ4CVPmB92K27uhbUJU1p1r",
        );
    }

    #[test]
    fn preserves_ordinary_output() {
        // 正常的命令输出不应被破坏 —— 过度脱敏会让审计日志失去价值
        let input = "Filesystem  1024-blocks  Used Available Capacity Mounted on\n/dev/disk3s5  971350180 412883640 543210112 44% /";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn preserves_field_name_for_context() {
        // 保留字段名让人能看出「这里原本有个密码」，而不是凭空消失
        let out = redact("password=hunter2");
        assert!(out.contains("password"));
        assert!(!out.contains("hunter2"));
    }

    #[test]
    fn handles_multiple_secrets_in_one_text() {
        let out = redact("sk-abcdefghij1234567890 and ghp_abcdefghij1234567890");
        assert!(!out.contains("sk-abcdefghij1234567890"));
        assert!(!out.contains("ghp_abcdefghij1234567890"));
    }

    #[test]
    fn empty_input_is_safe() {
        assert_eq!(redact(""), "");
    }
}
