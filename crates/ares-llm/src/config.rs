//! providers.toml 加载。
//!
//! API key 不写在配置文件里，只记 Keychain 账户名 ——
//! 配置文件可以进 git，密钥不行。

use ares_core::{paths, AresError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// OpenAI 兼容协议：DeepSeek / 豆包 / Kimi / Qwen / OpenRouter / vLLM / Ollama
    Openai,
    Anthropic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub kind: ProviderKind,
    /// API base，如 https://api.deepseek.com/v1
    pub base_url: String,
    /// 默认模型 ID
    pub model: String,
    /// Keychain 中的账户名，形如 `llm:deepseek`
    pub keychain_account: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProvidersConfig {
    /// 当前选用的 provider 名
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderEntry>,
}

impl ProvidersConfig {
    pub fn load() -> Result<Self> {
        Self::load_from(paths::config_dir().join("providers.toml"))
    }

    pub fn load_from(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(AresError::Config(format!(
                "未找到 {}。请先创建该文件并用 `ares provider add` 写入 API key。",
                path.display()
            )));
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| AresError::Config(format!("无法读取 {}: {e}", path.display())))?;
        let cfg: Self = toml::from_str(&text)
            .map_err(|e| AresError::Config(format!("解析 {} 失败：{e}", path.display())))?;

        if cfg.providers.is_empty() {
            return Err(AresError::Config(
                "providers.toml 中没有配置任何供应商".into(),
            ));
        }
        if !cfg.active.is_empty() && !cfg.providers.contains_key(&cfg.active) {
            return Err(AresError::Config(format!(
                "active 指向的供应商 {:?} 不存在",
                cfg.active
            )));
        }
        Ok(cfg)
    }

    /// 当前激活的 provider。未指定 active 时取字典序第一个。
    pub fn active_entry(&self) -> Result<(&str, &ProviderEntry)> {
        if !self.active.is_empty() {
            let e = self
                .providers
                .get(&self.active)
                .ok_or_else(|| AresError::Config("active 供应商不存在".into()))?;
            return Ok((self.active.as_str(), e));
        }
        self.providers
            .iter()
            .next()
            .map(|(k, v)| (k.as_str(), v))
            .ok_or_else(|| AresError::Config("没有可用的供应商".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(c: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(c.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    const SAMPLE: &str = r#"
active = "deepseek"

[providers.deepseek]
kind = "openai"
base_url = "https://api.deepseek.com/v1"
model = "deepseek-chat"
keychain_account = "llm:deepseek"

[providers.claude]
kind = "anthropic"
base_url = "https://api.anthropic.com"
model = "claude-opus-5"
keychain_account = "llm:anthropic"
"#;

    #[test]
    fn parses_multiple_providers() {
        let f = write_temp(SAMPLE);
        let cfg = ProvidersConfig::load_from(f.path()).unwrap();
        assert_eq!(cfg.providers.len(), 2);
        assert_eq!(cfg.providers["deepseek"].kind, ProviderKind::Openai);
        assert_eq!(cfg.providers["claude"].kind, ProviderKind::Anthropic);
    }

    #[test]
    fn active_entry_follows_active_field() {
        let f = write_temp(SAMPLE);
        let cfg = ProvidersConfig::load_from(f.path()).unwrap();
        let (name, e) = cfg.active_entry().unwrap();
        assert_eq!(name, "deepseek");
        assert_eq!(e.model, "deepseek-chat");
    }

    #[test]
    fn config_contains_no_api_key_field() {
        // 结构体里根本没有 api_key 字段，密钥不可能被误写进配置文件
        let f = write_temp(SAMPLE);
        let cfg = ProvidersConfig::load_from(f.path()).unwrap();
        let round = toml::to_string(&cfg).unwrap();
        assert!(!round.contains("api_key"));
        assert!(round.contains("keychain_account"));
    }

    #[test]
    fn missing_file_gives_actionable_error() {
        let err = ProvidersConfig::load_from("/nonexistent/providers.toml").unwrap_err();
        assert!(err.to_string().contains("provider add"));
    }

    #[test]
    fn dangling_active_is_rejected() {
        let f = write_temp(
            r#"
active = "nope"
[providers.a]
kind = "openai"
base_url = "x"
model = "m"
keychain_account = "k"
"#,
        );
        assert!(ProvidersConfig::load_from(f.path()).is_err());
    }
}
