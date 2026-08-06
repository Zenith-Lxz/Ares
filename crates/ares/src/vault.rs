//! 本地加密 vault：SSH 密码 / LLM key 存储（替代 macOS keychain 授权弹窗）。
//!
//! - 文件：`~/.config/ares/vault.bin`（AES-256-GCM 加密，600 权限）
//! - 密钥：SHA-256(应用盐 + hostname + username) —— 本机免密解密（无 keychain 弹窗）
//! - 明文格式：`alias\0secret\0` 序列
//! - 迁移：`get` 未命中时回退读旧 keychain（一次授权后自动写入 vault）

use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, OsRng};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use sha2::{Digest, Sha256};

const MAGIC: &[u8] = b"ARESVLT1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;

fn vault_path() -> std::path::PathBuf {
    // 测试/自定义路径覆盖
    if let Ok(p) = std::env::var("ARES_VAULT_PATH") {
        return std::path::PathBuf::from(p);
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("~").join(".config"));
    let base = if base.starts_with("~") {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        std::path::PathBuf::from(home).join(base.strip_prefix("~").unwrap())
    } else {
        base
    };
    base.join("ares").join("vault.bin")
}

fn derive_key(salt: &[u8]) -> [u8; 32] {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "ares-host".into());
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "ares-user".into());
    let mut h = Sha256::new();
    h.update(b"ares-vault-v1");
    h.update(salt);
    h.update(host.as_bytes());
    h.update(user.as_bytes());
    h.finalize().into()
}

/// 解密整个 vault → 条目列表（文件缺失/损坏 → None）。
fn decrypt_all() -> Option<Vec<(String, String)>> {
    let data = std::fs::read(vault_path()).ok()?;
    if data.len() < MAGIC.len() + SALT_LEN + NONCE_LEN + 16 || &data[..MAGIC.len()] != MAGIC {
        return None;
    }
    let salt = &data[MAGIC.len()..MAGIC.len() + SALT_LEN];
    let nonce = &data[MAGIC.len() + SALT_LEN..MAGIC.len() + SALT_LEN + NONCE_LEN];
    let ct = &data[MAGIC.len() + SALT_LEN + NONCE_LEN..];
    let cipher = Aes256Gcm::new_from_slice(&derive_key(salt)).ok()?;
    let plain = cipher.decrypt(Nonce::from_slice(nonce), ct).ok()?;
    let mut entries = Vec::new();
    let mut i = 0usize;
    while i < plain.len() {
        let Some(nul) = plain[i..].iter().position(|&b| b == 0) else {
            break;
        };
        let a = std::str::from_utf8(&plain[i..i + nul]).ok()?;
        let rest = &plain[i + nul + 1..];
        let Some(nul2) = rest.iter().position(|&b| b == 0) else {
            break;
        };
        let s = std::str::from_utf8(&rest[..nul2]).ok()?;
        entries.push((a.to_string(), s.to_string()));
        i += nul + 1 + nul2 + 1;
    }
    Some(entries)
}

/// 读取 alias 对应 secret。
pub fn get(alias: &str) -> Option<String> {
    decrypt_all()?
        .into_iter()
        .find(|(a, _)| a == alias)
        .map(|(_, s)| s)
}

/// 读取（含旧 keychain 迁移）：vault 未命中 → keychain → 自动写入 vault。
pub fn get_migrate(alias: &str) -> Option<String> {
    if let Some(s) = get(alias) {
        return Some(s);
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(Some(s)) = ares_darwin::keychain::get_secret(alias) {
            if !s.is_empty() {
                let _ = set(alias, &s);
                return Some(s);
            }
        }
    }
    None
}

/// 写入/更新 alias → secret（原子写，600 权限）。
pub fn set(alias: &str, secret: &str) -> Result<(), String> {
    let mut entries = decrypt_all().unwrap_or_default();
    entries.retain(|(a, _)| a != alias);
    entries.push((alias.to_string(), secret.to_string()));
    let mut plain = Vec::new();
    for (a, s) in &entries {
        plain.extend_from_slice(a.as_bytes());
        plain.push(0);
        plain.extend_from_slice(s.as_bytes());
        plain.push(0);
    }
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce);
    let cipher = Aes256Gcm::new_from_slice(&derive_key(&salt)).map_err(|e| e.to_string())?;
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plain.as_slice())
        .map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(MAGIC.len() + SALT_LEN + NONCE_LEN + ct.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    let path = vault_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &out).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// 删除 alias（不存在时 OK）。
#[allow(dead_code)]
pub fn remove(alias: &str) -> Result<(), String> {
    let mut entries = decrypt_all().unwrap_or_default();
    let before = entries.len();
    entries.retain(|(a, _)| a != alias);
    if entries.len() == before {
        return Ok(());
    }
    // 复用 set 的序列化逻辑：写入空条目集也行，但需保留现有条目
    set_entries(&entries)
}

#[allow(dead_code)]
fn set_entries(entries: &[(String, String)]) -> Result<(), String> {
    let mut plain = Vec::new();
    for (a, s) in entries {
        plain.extend_from_slice(a.as_bytes());
        plain.push(0);
        plain.extend_from_slice(s.as_bytes());
        plain.push(0);
    }
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce);
    let cipher = Aes256Gcm::new_from_slice(&derive_key(&salt)).map_err(|e| e.to_string())?;
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plain.as_slice())
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    let path = vault_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &out).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// vault 文件路径（诊断/展示用）。
#[allow(dead_code)]
pub fn path_display() -> String {
    vault_path().display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("ares_vault_test_{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        std::env::set_var("ARES_VAULT_PATH", tmp.to_str().unwrap());
        // 验证加密/解密 + 文件路径逻辑
        let entries = vec![
            ("ssh-pw:测试".to_string(), "secret123".to_string()),
            ("llm:deepseek".to_string(), "sk-abc".to_string()),
        ];
        set_entries(&entries).expect("set");
        let got = decrypt_all().expect("decrypt");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].1, "secret123");
        assert_eq!(got[1].1, "sk-abc");
        // 更新
        set("ssh-pw:测试", "newpass").expect("update");
        let got = decrypt_all().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(
            got.iter().find(|(a, _)| a == "ssh-pw:测试").unwrap().1,
            "newpass"
        );
        // 删除
        remove("llm:deepseek").expect("remove");
        let got = decrypt_all().unwrap();
        assert_eq!(got.len(), 1);
    }
}
