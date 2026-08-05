---
name: process-analysis
description: Use when investigating CPU/memory hogs, zombies, runaway processes, or load spikes.
---

# 进程分析

## 触发
- 负载高 / 某进程占满 CPU/内存 / 僵尸进程 / 进程反复崩溃

## 步骤
1. 总览：`top -bn1 | head -20` 或 `ps aux --sort=-%cpu | head -12`
2. 定位：按 CPU/内存排序找异常进程，记录 PID
3. 深挖：`ps -o pid,ppid,user,%cpu,%mem,etime,cmd -p <pid>` + `/proc/<pid>/status`
4. 处置（变更走审批）：kill/重启/查日志
5. 验证：`ps -p <pid>` 消失 / 资源回落

## 命令
```bash
ps aux --sort=-%cpu | head -12
ps aux --sort=-%mem | head -12
top -bn1 | head -20
ps -o pid,ppid,user,%cpu,%mem,etime,cmd -p <pid>
cat /proc/<pid>/status
# 僵尸进程
ps aux | awk '$8 ~ /Z/ {print}'
```

## 陷阱
- 先看进程的父进程（PPID）—— 僵尸进程要处理的是父进程
- 杀进程前确认不是关键服务（`systemctl status <pid>` 关联 unit）
- 高负载不一定是 CPU：查 `vmstat 1 5`（r 队列）与 `iostat`（IO 等待）
