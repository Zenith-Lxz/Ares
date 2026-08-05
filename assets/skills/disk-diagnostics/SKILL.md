---
name: disk-diagnostics
description: Use when diagnosing disk usage, inodes, filesystem health, or storage capacity issues on a server.
---

# 磁盘诊断

## 触发
- 磁盘占用高 / 空间不足 / inode 耗尽 / 文件系统只读 / IO 慢

## 步骤
1. 总览：`df -P`（容量）+ `df -i`（inode）—— 先看哪个挂载点满
2. 定位大目录：`du -h --max-depth=1 /var 2>/dev/null | sort -rh | head -15`
3. 删除前**先确认**（confirm 审批链）：清理日志/临时文件/旧备份
4. 验证：`df -P` 复查

## 命令
```bash
df -P
df -i
du -h --max-depth=1 / | sort -rh | head -15
# 找超过 1G 的文件
find / -xdev -type f -size +1G -exec ls -lh {} \; 2>/dev/null | head -20
# 被删除但仍在占用（释放后空间才回来）
lsof +L1 2>/dev/null | head -20
```

## 陷阱
- `du` 在 NFS/大目录上慢，加 `--max-depth` 限制
- 删文件后空间没释放 = 有进程持有已删除的 fd（lsof +L1）
- 只读文件系统先查 `mount` 与 dmesg（可能磁盘错误）
