//! 配置与数据目录解析。
//!
//! 配置（人写的）与数据（程序写的）严格分开：
//! 配置目录可以放进 git 管理，数据目录不应该。

use crate::{AresError, Result};
use std::path::PathBuf;

/// 配置目录，默认 `~/.config/ares`。
///
/// 可用环境变量 `ARES_CONFIG_DIR` 覆盖 —— 测试与多环境切换依赖此。
pub fn config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ARES_CONFIG_DIR") {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .expect("home directory must exist on macOS")
        .join(".config")
        .join("ares")
}

/// 数据目录，默认 `~/.local/share/ares`。
///
/// 存放审计日志、落盘输出、主机档案。可用 `ARES_DATA_DIR` 覆盖。
pub fn data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ARES_DATA_DIR") {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .expect("home directory must exist on macOS")
        .join(".local")
        .join("share")
        .join("ares")
}

/// 审计日志目录。
pub fn audit_dir() -> PathBuf {
    data_dir().join("audit")
}

/// 大输出落盘目录。
pub fn outputs_dir() -> PathBuf {
    data_dir().join("outputs")
}

/// 创建所有必需目录。幂等。
pub fn ensure_dirs() -> Result<()> {
    for d in [config_dir(), data_dir(), audit_dir(), outputs_dir()] {
        std::fs::create_dir_all(&d)
            .map_err(|e| AresError::Config(format!("无法创建目录 {}: {e}", d.display())))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_takes_effect() {
        // 用唯一的临时路径，避免与其他测试的环境变量竞争
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        std::env::set_var("ARES_CONFIG_DIR", &path);
        assert_eq!(config_dir(), PathBuf::from(&path));
        std::env::remove_var("ARES_CONFIG_DIR");
    }

    #[test]
    fn default_paths_are_under_home() {
        std::env::remove_var("ARES_CONFIG_DIR");
        std::env::remove_var("ARES_DATA_DIR");
        let home = dirs::home_dir().unwrap();
        assert!(config_dir().starts_with(&home));
        assert!(data_dir().starts_with(&home));
    }

    #[test]
    fn ensure_dirs_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("ARES_CONFIG_DIR", tmp.path().join("cfg"));
        std::env::set_var("ARES_DATA_DIR", tmp.path().join("data"));

        ensure_dirs().unwrap();
        ensure_dirs().unwrap(); // 第二次不应报错

        assert!(tmp.path().join("data").join("audit").is_dir());
        assert!(tmp.path().join("data").join("outputs").is_dir());

        std::env::remove_var("ARES_CONFIG_DIR");
        std::env::remove_var("ARES_DATA_DIR");
    }
}
