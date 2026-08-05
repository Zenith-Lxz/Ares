---
name: backup-recovery
description: Use when backing up files/databases or recovering from accidental deletion or corruption.
---

# 备份与恢复

## 触发
- 需要备份 / 误删文件 / 数据恢复 / 迁移前快照

## 步骤
1. 明确范围：文件？数据库？哪个路径？（先问清楚或列出候选）
2. 备份（变更走审批）：
```bash
# 文件
tar czf /var/backups/<name>-$(date +%Y%m%d-%H%M).tar.gz -C /path/to/src .
# 数据库（MySQL/MariaDB）
mysqldump --single-transaction -u root <db> | gzip > /var/backups/<db>.sql.gz
# PostgreSQL
pg_dump -Fc <db> -f /var/backups/<db>.dump
```
3. 验证备份完整性（`tar tzf` 抽查 / `zcat | head`）
4. 恢复（**必须确认**，恢复是覆盖操作）：先备份当前状态再覆盖

## 陷阱
- 备份永远先验证可读性，否则等于没备份
- 恢复前把当前损坏状态也存一份（`cp` 快照），恢复失败可回退
- mysqldump 用 `--single-transaction` 避免锁表
- 大数据库用 `--quick` 防止内存溢出
