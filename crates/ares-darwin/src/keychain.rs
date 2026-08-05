//! macOS Keychain 读写。
//!
//! 所有条目使用统一的 service 名，account 由调用方指定，
//! 形如 `ssh:prod-web-01` / `sudo:prod-web-01` / `llm:anthropic`。

use anyhow::{Context, Result};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

/// Keychain 中所有 ARES 条目共用的 service 名。
const SERVICE: &str = "ares";

/// 写入或覆盖一条凭据。
pub fn set_secret(account: &str, secret: &str) -> Result<()> {
    set_generic_password(SERVICE, account, secret.as_bytes())
        .with_context(|| format!("failed to store secret for account {account}"))
}

/// 读取一条凭据。不存在时返回 `Ok(None)` 而非错误。
pub fn get_secret(account: &str) -> Result<Option<String>> {
    match get_generic_password(SERVICE, account) {
        Ok(bytes) => {
            let s = String::from_utf8(bytes)
                .with_context(|| format!("secret for {account} is not valid UTF-8"))?;
            Ok(Some(s))
        }
        Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
        Err(e) => Err(e).with_context(|| format!("failed to read secret for {account}")),
    }
}

/// 删除一条凭据。不存在时视为成功。
pub fn delete_secret(account: &str) -> Result<()> {
    match delete_generic_password(SERVICE, account) {
        Ok(()) => Ok(()),
        Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
        Err(e) => Err(e).with_context(|| format!("failed to delete secret for {account}")),
    }
}

/// `errSecItemNotFound` —— Security.framework 中「条目不存在」的错误码。
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用 account 前缀，避免污染真实凭据。
    const TEST_ACCOUNT: &str = "test:ares-keychain-roundtrip";

    #[test]
    fn secret_roundtrip() {
        // 前置清理，保证测试可重复运行
        delete_secret(TEST_ACCOUNT).unwrap();

        assert_eq!(get_secret(TEST_ACCOUNT).unwrap(), None);

        set_secret(TEST_ACCOUNT, "hunter2").unwrap();
        assert_eq!(get_secret(TEST_ACCOUNT).unwrap(), Some("hunter2".into()));

        // 覆盖写
        set_secret(TEST_ACCOUNT, "hunter3").unwrap();
        assert_eq!(get_secret(TEST_ACCOUNT).unwrap(), Some("hunter3".into()));

        delete_secret(TEST_ACCOUNT).unwrap();
        assert_eq!(get_secret(TEST_ACCOUNT).unwrap(), None);
    }

    #[test]
    fn delete_missing_is_ok() {
        delete_secret("test:ares-never-existed").unwrap();
    }
}
