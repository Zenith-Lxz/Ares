---
name: log-troubleshooting
description: Use when investigating application or system errors by reading logs (journald, syslog, app logs).
---

# 日志排查

## 触发
- 服务报错 / 应用异常 / 用户反馈故障 / 需要回溯事件

## 步骤
1. 确定日志源：systemd 服务 → `journalctl`；传统 → `/var/log/*`
2. 先看最近错误，再按时间窗过滤：
```bash
journalctl -u <unit> --no-pager -n 200                    # 最近 200 行
journalctl -u <unit> --since "10 min ago" -p err          # 最近 10 分钟错误
journalctl -u <unit> --since today --until now --no-pager | grep -iE "error|fail|exception"
grep -nE "ERROR|FATAL" /var/log/<app>.log | tail -50
```
3. 关联上下文：同一时间戳的日志前后 20 行（`grep -B5 -A20`）
4. 定位根因后给出修复建议，变更操作走审批

## 陷阱
- 时区：journalctl 用服务器本地时间，先 `date` 确认
- 日志轮转：旧日志在 `.gz`，先 `zgrep`
- 大文件先 `tail`/`grep`，不要 cat 整个文件（输出预算会截断）
