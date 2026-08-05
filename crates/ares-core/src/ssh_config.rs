//! `~/.ssh/config` 解析：为 TUI 主机选择页提供主机列表。
//!
//! 只解析与连接相关的四个键：`Host` / `HostName` / `User` / `Port`。
//! 通配符条目（`Host *`、`web?`、`!exclude`）被跳过 —— 它们不产生
//! 可直接选择的主机，只影响 ssh 内部的参数合并（交给 OpenSSH 处理）。

use std::path::{Path, PathBuf};

/// 一条可连接的主机。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshHost {
    /// Host 别名（取首个）。
    pub alias: String,
    /// HostName（缺省时与别名相同）。
    pub hostname: String,
    pub user: Option<String>,
    pub port: Option<u16>,
}

impl SshHost {
    /// 连接串：`user@hostname[:port]`（port 非 22 时显示）。
    pub fn connect_target(&self) -> String {
        let base = match &self.user {
            Some(u) => format!("{u}@{}", self.hostname),
            None => self.hostname.clone(),
        };
        match self.port {
            Some(p) if p != 22 => format!("{base} -p {p}"),
            _ => base,
        }
    }
}

/// 配置文件路径（`ARES_SSH_CONFIG` 可覆盖，测试用）。
pub fn config_path() -> PathBuf {
    match std::env::var("ARES_SSH_CONFIG") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".ssh").join("config")
        }
    }
}

/// 读取并解析 ssh_config。文件不存在返回空列表（不报错）。
pub fn load() -> Vec<SshHost> {
    load_from(&config_path())
}

/// 解析指定文件（测试入口）。
pub fn load_from(path: &Path) -> Vec<SshHost> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    parse(&text)
}

/// 解析配置文本。条目以 `Host` 开头，`HostName`/`User`/`Port` 累积到当前条目。
fn parse(text: &str) -> Vec<SshHost> {
    let mut hosts: Vec<SshHost> = Vec::new();
    let mut aliases: Vec<String> = Vec::new();
    let mut hostname: Option<String> = None;
    let mut user: Option<String> = None;
    let mut port: Option<u16> = None;

    // 结束当前条目（若有）
    let flush = |hosts: &mut Vec<SshHost>,
                 aliases: &mut Vec<String>,
                 hostname: &mut Option<String>,
                 user: &mut Option<String>,
                 port: &mut Option<u16>| {
        if aliases.is_empty() {
            return;
        }
        // 通配符 / 排除条目不产生可连主机
        if aliases.iter().any(|a| a.contains(['*', '?', '!'])) {
            aliases.clear();
            *hostname = None;
            *user = None;
            *port = None;
            return;
        }
        let alias = aliases.remove(0);
        hosts.push(SshHost {
            hostname: hostname.take().unwrap_or_else(|| alias.clone()),
            alias,
            user: user.take(),
            port: port.take(),
        });
    };

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, val) = match split_key_value(line) {
            Some(kv) => kv,
            None => continue,
        };
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                flush(
                    &mut hosts,
                    &mut aliases,
                    &mut hostname,
                    &mut user,
                    &mut port,
                );
                aliases = val.split_whitespace().map(|s| s.to_string()).collect();
            }
            "hostname" => hostname = Some(val.trim().to_string()),
            "user" => user = Some(val.trim().to_string()),
            "port" => port = val.trim().parse::<u16>().ok(),
            _ => {} // 其余键（IdentityFile 等）与连接目标无关，忽略
        }
    }
    flush(
        &mut hosts,
        &mut aliases,
        &mut hostname,
        &mut user,
        &mut port,
    );
    hosts
}

/// 拆 `key value` 或 `key=value`（首处空白或等号）。
fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let idx = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let key = line[..idx].trim();
    let val = line[idx..].trim_start_matches([' ', '=', '\t']).trim();
    if key.is_empty() {
        None
    } else {
        Some((key, val))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_text(t: &str) -> Vec<SshHost> {
        parse(t)
    }

    #[test]
    fn parses_basic_host_block() {
        let hosts =
            parse_text("Host prod-web-01\n  HostName 10.8.8.83\n  User root\n  Port 22022\n");
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].alias, "prod-web-01");
        assert_eq!(hosts[0].hostname, "10.8.8.83");
        assert_eq!(hosts[0].user.as_deref(), Some("root"));
        assert_eq!(hosts[0].port, Some(22022));
    }

    #[test]
    fn splits_multiple_aliases() {
        let hosts = parse_text("Host web1 web2 web3\n  HostName example.com\n");
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].alias, "web1");
    }

    #[test]
    fn skips_wildcard_and_exclude_entries() {
        let hosts = parse_text(
            "Host *\n  User admin\n\nHost web?\n  HostName 10.0.0.1\n\nHost real\n  HostName 10.0.0.2\n",
        );
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].alias, "real");
    }

    #[test]
    fn hostname_defaults_to_alias() {
        let hosts = parse_text("Host bastion\n");
        assert_eq!(hosts[0].hostname, "bastion");
    }

    #[test]
    fn user_and_port_are_optional() {
        let hosts = parse_text("Host direct\n  HostName 1.2.3.4\n");
        assert_eq!(hosts[0].user, None);
        assert_eq!(hosts[0].port, None);
        assert_eq!(hosts[0].connect_target(), "1.2.3.4");
    }

    #[test]
    fn handles_comments_and_blank_lines() {
        let hosts = parse_text("# 注释\n\nHost a\n  # 块内注释\n  HostName a.example\n\nHost b\n");
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].alias, "a");
        assert_eq!(hosts[1].hostname, "b");
    }

    #[test]
    fn supports_key_equals_value_syntax() {
        let hosts = parse_text("Host eq\nHostName=eq.example\nUser=ops\n");
        assert_eq!(hosts[0].hostname, "eq.example");
        assert_eq!(hosts[0].user.as_deref(), Some("ops"));
    }

    #[test]
    fn load_from_missing_file_is_empty() {
        assert!(load_from(Path::new("/nonexistent/ssh_config")).is_empty());
    }
}
