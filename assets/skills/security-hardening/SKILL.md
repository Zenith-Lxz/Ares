---
name: security-hardening
description: Use when auditing server security posture: open ports, users, ssh config, failed logins, updates.
---

# 安全加固

## 触发
- 安全检查 / 可疑登录 / 对外开放端口审计 / 合规加固

## 步骤
1. 端口审计：`ss -tlnp`（对外监听端口 + 进程）
2. 登录审计：`last -20` + `grep "Failed password" /var/log/auth.log | tail -20`（无 auth.log 用 journalctl）
3. SSH 配置：`sshd -T | grep -E "PermitRootLogin|PasswordAuthentication|Port"`（只读，改动走审批）
4. 用户与 sudo：`cat /etc/passwd | grep -E "/bin/(ba)?sh"` + `getent group sudo`
5. 更新：`apt list --upgradable 2>/dev/null | head`（**先报告再让用户决定是否升级**，升级属于变更）

## 命令
```bash
ss -tlnp
last -20
grep "Failed password" /var/log/auth.log | tail -20
sshd -T | grep -E "PermitRootLogin|PasswordAuthentication|Port"
find /etc/cron* /var/spool/cron -type f 2>/dev/null | xargs ls -la
```

## 陷阱
- 只读审计（observer）直接执行；任何加固改动（关端口/改 sshd/删用户）必须确认
- 检查完报告风险等级，不要主动"顺手加固"
- auth.log 轮转：`auth.log.1` / `journalctl -u ssh -S "7 days ago"`
