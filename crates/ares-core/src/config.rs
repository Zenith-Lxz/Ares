//! 配置文件加载。
//!
//! `hosts.toml` 存放 ssh_config 表达不了的元数据：
//! 环境标签、分组、角色、备注、探测缓存。
//! 连接参数本身仍以 `~/.ssh/config` 为唯一来源（M2 引入）。

use crate::{paths, AresError, Env, HostId, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// 主机探测缓存。首次连接时自动填充。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Probe {
    #[serde(default)]
    pub os: String,
    /// 包管理器：apt / yum / dnf / apk / pacman / brew
    #[serde(default)]
    pub pkg: String,
    /// init 系统：systemd / openrc / launchd
    #[serde(default)]
    pub init: String,
    #[serde(default)]
    pub services: Vec<String>,
    /// RFC3339 时间戳
    #[serde(default)]
    pub probed_at: String,
}

/// 单台主机的扩展元数据。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostEntry {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub env: Env,
    #[serde(default)]
    pub group: String,
    /// 用于配置漂移检测的同类分组
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub note: String,
    /// key | password
    #[serde(default)]
    pub auth: String,
    /// 形如 "02:00-05:00"
    #[serde(default)]
    pub maintenance_window: String,
    #[serde(default)]
    pub probe: Probe,
    // ── 连接信息（2026-08-05 主机簿独立维护：hosts.toml 是唯一事实源，
    //    ssh_config 仅作一次性导入源）──
    /// 连接地址 / IP；缺省用 host 键名
    #[serde(default)]
    pub hostname: String,
    /// ssh 用户名；缺省用当前用户（ssh 默认）
    #[serde(default)]
    pub user: String,
    /// 端口；缺省 22
    #[serde(default)]
    pub port: Option<u16>,
}

impl HostEntry {
    /// 连接目标：`[user@]hostname[:port]`（用于 ssh 命令构造）。
    pub fn connect_target(&self, key: &str) -> String {
        let hostname = if self.hostname.is_empty() {
            key.to_string()
        } else {
            self.hostname.clone()
        };
        let base = if self.user.is_empty() {
            hostname
        } else {
            format!("{}@{}", self.user, hostname)
        };
        match self.port {
            Some(p) if p != 22 => format!("{base} -p {p}"),
            _ => base,
        }
    }
}

/// `hosts.toml` 的完整内容。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostsConfig {
    #[serde(default)]
    pub hosts: BTreeMap<String, HostEntry>,
}

impl HostsConfig {
    /// 从默认位置加载。文件不存在时返回空配置而非报错 ——
    /// 首次运行的用户不应该被一个缺失的可选文件挡住。
    pub fn load() -> Result<Self> {
        Self::load_from(paths::config_dir().join("hosts.toml"))
    }

    pub fn load_from(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| AresError::Config(format!("无法读取 {}: {e}", path.display())))?;
        toml::from_str(&text)
            .map_err(|e| AresError::Config(format!("解析 {} 失败：{e}", path.display())))
    }

    /// 查询主机的环境等级。
    ///
    /// `localhost` 固定为 `Env::Local`，且不可被配置覆盖 ——
    /// 本机不是远程主机，混淆两者会让策略判定出错。
    /// 未在配置中出现的主机回落到 `Env::Unknown`（默认值）——
    /// **未知即最严**：Unknown 的变更操作强制确认（见 §7.1），
    /// 宁可在首次使用时多确认一次，也不能把未登记主机当 dev 放行。
    /// 真正的 prod 主机必须显式标注，这是使用者的责任。
    pub fn env_of(&self, host: &HostId) -> Env {
        if host.as_str() == "localhost" {
            return Env::Local;
        }
        self.hosts
            .get(host.as_str())
            .map(|e| e.env)
            .unwrap_or_default()
    }

    /// 展开标签选择器，如 `@prod` → 所有 env=prod 或 tags 含 "prod" 的主机。
    pub fn hosts_with_tag(&self, tag: &str) -> Vec<HostId> {
        self.hosts
            .iter()
            .filter(|(_, e)| e.tags.iter().any(|t| t == tag) || e.env.to_string() == tag)
            .map(|(name, _)| HostId::new(name.clone()))
            .collect()
    }

    /// 保存到默认位置（`~/.config/ares/hosts.toml`）。
    pub fn save(&self) -> Result<()> {
        self.save_to(paths::config_dir().join("hosts.toml"))
    }

    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let text = toml::to_string_pretty(self)
            .map_err(|e| AresError::Config(format!("序列化 hosts.toml 失败：{e}")))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AresError::Config(format!("无法创建目录 {}: {e}", parent.display()))
            })?;
        }
        std::fs::write(path, text)
            .map_err(|e| AresError::Config(format!("无法写入 {}: {e}", path.display())))
    }

    /// 插入/更新一台主机（键名 = 别名）。返回是否新增。
    pub fn upsert(&mut self, key: String, entry: HostEntry) -> bool {
        let is_new = !self.hosts.contains_key(&key);
        self.hosts.insert(key, entry);
        is_new
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn missing_file_yields_empty_config() {
        let cfg = HostsConfig::load_from("/nonexistent/path/hosts.toml").unwrap();
        assert!(cfg.hosts.is_empty());
    }

    #[test]
    fn parses_full_host_entry() {
        let f = write_temp(
            r#"
[hosts.prod-web-01]
tags = ["prod", "web", "nginx"]
env = "prod"
group = "web-cluster"
role = "web"
note = "主站入口，改配置前先摘 LB"
auth = "key"
maintenance_window = "02:00-05:00"

[hosts.prod-web-01.probe]
os = "Ubuntu 22.04"
pkg = "apt"
init = "systemd"
services = ["nginx", "docker"]
probed_at = "2026-08-05T10:00:00Z"
"#,
        );
        let cfg = HostsConfig::load_from(f.path()).unwrap();
        let entry = &cfg.hosts["prod-web-01"];

        assert_eq!(entry.env, Env::Prod);
        assert_eq!(entry.tags, vec!["prod", "web", "nginx"]);
        assert_eq!(entry.role, "web");
        assert_eq!(entry.probe.pkg, "apt");
        assert_eq!(entry.probe.services, vec!["nginx", "docker"]);
    }

    #[test]
    fn minimal_entry_uses_defaults() {
        let f = write_temp("[hosts.box]\n");
        let cfg = HostsConfig::load_from(f.path()).unwrap();
        let entry = &cfg.hosts["box"];

        // 未知即最严：未标注 env 的主机是 Unknown（Task 5 `Env::default()`，
        // spec §7.1）—— 不是 Dev。Dev 会放行 auto 规则，安全语义不允许。
        assert_eq!(entry.env, Env::Unknown);
        assert!(entry.tags.is_empty());
        assert_eq!(entry.probe, Probe::default());
    }

    #[test]
    fn localhost_is_always_local_env() {
        let f = write_temp(
            r#"
[hosts.localhost]
env = "prod"
"#,
        );
        let cfg = HostsConfig::load_from(f.path()).unwrap();
        // 即使配置里写成 prod，也必须返回 Local
        assert_eq!(cfg.env_of(&HostId::localhost()), Env::Local);
    }

    #[test]
    fn unknown_host_falls_back_to_dev() {
        let cfg = HostsConfig::default();
        assert_eq!(cfg.env_of(&HostId::new("never-seen")), Env::Unknown);
    }

    #[test]
    fn tag_selector_matches_tags_and_env() {
        let f = write_temp(
            r#"
[hosts.a]
env = "prod"

[hosts.b]
tags = ["prod"]

[hosts.c]
env = "dev"
"#,
        );
        let cfg = HostsConfig::load_from(f.path()).unwrap();
        let mut got = cfg.hosts_with_tag("prod");
        got.sort();
        assert_eq!(got, vec![HostId::new("a"), HostId::new("b")]);
    }

    #[test]
    fn malformed_toml_reports_path() {
        let f = write_temp("[hosts.broken\n");
        let err = HostsConfig::load_from(f.path()).unwrap_err();
        assert!(err.to_string().contains("解析"));
    }

    #[test]
    fn connect_target_uses_hostname_user_port() {
        let e = HostEntry {
            hostname: "10.8.8.83".into(),
            user: "root".into(),
            port: Some(22022),
            ..Default::default()
        };
        assert_eq!(e.connect_target("ignored"), "root@10.8.8.83 -p 22022");
    }

    #[test]
    fn connect_target_falls_back_to_key_and_defaults() {
        let e = HostEntry::default();
        assert_eq!(e.connect_target("my-alias"), "my-alias");
        let e2 = HostEntry {
            hostname: "1.2.3.4".into(),
            ..Default::default()
        };
        assert_eq!(e2.connect_target("x"), "1.2.3.4");
        let e3 = HostEntry {
            user: "ops".into(),
            port: Some(22),
            ..Default::default()
        };
        assert_eq!(e3.connect_target("y"), "ops@y");
    }

    #[test]
    fn save_roundtrip_preserves_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        let mut cfg = HostsConfig::default();
        cfg.upsert(
            "prod-web".into(),
            HostEntry {
                hostname: "10.8.8.83".into(),
                user: "root".into(),
                port: Some(22022),
                env: Env::Prod,
                tags: vec!["web".into()],
                ..Default::default()
            },
        );
        cfg.save_to(&path).unwrap();

        let loaded = HostsConfig::load_from(&path).unwrap();
        let e = &loaded.hosts["prod-web"];
        assert_eq!(e.hostname, "10.8.8.83");
        assert_eq!(e.user, "root");
        assert_eq!(e.port, Some(22022));
        assert_eq!(e.env, Env::Prod);
    }

    #[test]
    fn upsert_reports_new_vs_update() {
        let mut cfg = HostsConfig::default();
        assert!(cfg.upsert("a".into(), HostEntry::default()));
        assert!(!cfg.upsert("a".into(), HostEntry::default()));
    }

    #[test]
    fn old_style_hosts_without_connection_fields_still_parse() {
        // 兼容：旧 hosts.toml（无 hostname/user/port）解析后字段为空/None
        let f = tempfile::tempdir().unwrap();
        let path = f.path().join("hosts.toml");
        std::fs::write(&path, "[hosts.legacy]\nenv = \"dev\"\n").unwrap();
        let cfg = HostsConfig::load_from(&path).unwrap();
        let e = &cfg.hosts["legacy"];
        assert_eq!(e.hostname, "");
        assert_eq!(e.user, "");
        assert_eq!(e.port, None);
        assert_eq!(e.env, Env::Dev);
    }
}
