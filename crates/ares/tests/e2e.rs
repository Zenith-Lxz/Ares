//! 端到端验收。
//!
//! 这些测试跑真实的二进制与真实的本机命令，只有 LLM 是脚本化的。
//! 它们守护的是 M1 的交付标准，任何一条失败都意味着 M1 未完成。

use std::process::Command;

fn ares_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ares")
}

fn temp_env() -> (tempfile::TempDir, Vec<(String, String)>) {
    let tmp = tempfile::tempdir().unwrap();
    let env = vec![
        (
            "ARES_CONFIG_DIR".to_string(),
            tmp.path().join("cfg").to_string_lossy().into_owned(),
        ),
        (
            "ARES_DATA_DIR".to_string(),
            tmp.path().join("data").to_string_lossy().into_owned(),
        ),
    ];
    (tmp, env)
}

#[test]
fn init_creates_config_files() {
    let (tmp, env) = temp_env();
    let out = Command::new(ares_bin())
        .arg("init")
        .envs(env)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tmp.path().join("cfg").join("SOUL.md").exists());
    assert!(tmp.path().join("cfg").join("USER.md").exists());
    assert!(tmp.path().join("cfg").join("providers.toml").exists());
}

#[test]
fn audit_verify_passes_on_empty_log() {
    let (_tmp, env) = temp_env();
    let out = Command::new(ares_bin())
        .args(["audit", "verify"])
        .envs(env)
        .output()
        .unwrap();

    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("完整"));
}

#[test]
fn audit_verify_detects_tampering() {
    let (tmp, env) = temp_env();
    let audit_dir = tmp.path().join("data").join("audit");
    std::fs::create_dir_all(&audit_dir).unwrap();

    // 用库直接写三条真实记录
    let mut w = ares_audit::AuditWriter::open_at(&audit_dir).unwrap();
    for cmd in ["uptime", "df -P", "vm_stat"] {
        w.append(ares_audit::AuditRecord::new(
            ares_audit::now_rfc3339(),
            "localhost",
            "terminal_execute",
            cmd,
            Some(0),
            "ok",
            "observer",
            "agent",
            "sess-e2e",
        ))
        .unwrap();
    }
    let path = w.path().to_path_buf();
    drop(w);

    // 篡改中间一条
    let content = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    lines[1] = lines[1].replace("df -P", "rm -rf /");
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();

    let out = Command::new(ares_bin())
        .args(["audit", "verify"])
        .envs(env)
        .output()
        .unwrap();

    assert!(!out.status.success(), "篡改后 verify 必须失败");
    assert!(String::from_utf8_lossy(&out.stderr).contains("断裂"));
}

#[test]
fn missing_providers_config_gives_actionable_error() {
    let (_tmp, env) = temp_env();
    let out = Command::new(ares_bin()).envs(env).output().unwrap();

    assert!(!out.status.success());
    let msg = String::from_utf8_lossy(&out.stderr);
    assert!(
        msg.contains("providers.toml"),
        "错误信息应指出缺什么：{msg}"
    );
}
