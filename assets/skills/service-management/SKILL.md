---
name: service-management
description: Use when checking, starting, stopping, restarting, or enabling systemd services.
---

# 服务管理

## 触发
- 服务挂了 / 需要重启 / 开机自启 / 查看服务状态

## 步骤
1. 状态：`systemctl status <unit> --no-pager -l`
2. 查看原因：`journalctl -u <unit> -n 50 --no-pager`
3. 变更操作（restart/stop/enable/disable）**必须走确认审批**
4. 验证：`systemctl is-active <unit>` + 健康检查（curl/端口）

## 命令
```bash
systemctl status <unit> --no-pager -l
systemctl is-active <unit>; systemctl is-enabled <unit>
systemctl restart <unit>          # 需要确认
systemctl enable --now <unit>     # 需要确认
ss -tlnp | grep <port>            # 验证端口
```

## 陷阱
- restart 前先看日志 —— 重启只是掩盖症状
- 依赖关系：restart 后检查依赖服务（`systemctl list-dependencies`）
- 守护进程配置改了要先 `systemctl daemon-reload`
