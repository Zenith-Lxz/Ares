//! 内置策略清单。
//!
//! 这些条目**不可通过配置删除**，只能追加。理由很直接：
//! 一个能被配置关掉的安全护栏，在需要它的那一刻大概率已经被关掉了。

/// 硬禁止 —— 命中即拒绝，不提供任何确认途径。
///
/// 收录标准：后果不可逆且几乎不存在合法的 Agent 使用场景。
/// 需要做这些事的时候，人应该自己去做。
///
/// **关于 `rm -rf` 的语义分层**（重要，三处文档必须一致）：
/// - deny：删除根本身或**根下顶级目录**（`/etc`、`/var`、`/usr` …）—— 无合法场景
/// - confirm（默认级别）：任意路径的递归删除（`/tmp/*`、`/var/log/old` 等）—— 需显式确认
/// - critical（极高危）：涉及整机或数据不可逆的操作 —— 确认之外还要求输入完整主机名
///
/// 不要用 `rm -rf /*` 这样的模式：glob 的 `*` 跨 `/`，`rm -rf /*` 会匹配
/// `rm -rf /tmp/build`，把「允许但需确认的清理」误杀成「不可覆盖的 deny」。
pub const DENY_COMMANDS: &[&str] = &[
    "rm -rf /",
    "rm -fr /",
    "rm -rf --no-preserve-root /",
    // 根下顶级目录（删除整个目录 = 毁机，无合法 Agent 场景）
    "rm -rf /bin",
    "rm -rf /boot",
    "rm -rf /dev",
    "rm -rf /etc",
    "rm -rf /home",
    "rm -rf /lib",
    "rm -rf /lib64",
    "rm -rf /media",
    "rm -rf /mnt",
    "rm -rf /opt",
    "rm -rf /proc",
    "rm -rf /root",
    "rm -rf /run",
    "rm -rf /sbin",
    "rm -rf /srv",
    "rm -rf /sys",
    "rm -rf /usr",
    "rm -rf /var",
    "mkfs*",
    "dd if=* of=/dev/*",
    "dd of=/dev/*",
    "> /dev/sda*",
    "> /dev/nvme*",
    ":(){ :|:& };:",
    "chmod -R 000 /",
    "mv /* *",
    "shred /dev/*",
];

/// 极高危命令 —— 命中时 Confirm 之外还要追加输入确认（手打主机名，spec §14.2）。
///
/// 收录标准：做错的代价是「不可逆的破坏面」（删库/删卷/销毁基础设施），
/// 但存在合法使用场景（清理 /var 子目录、正式销毁环境）。
/// **注意**：不要收录 `rm -rf /*` —— 它已在 DENY_COMMANDS 中（删除根下顶级目录），
/// deny 优先于一切，收录进 critical 只会让 critical_is_disjoint_from_deny 失败
/// 且这些模式永远走不到审批流程（死代码）。
pub const CRITICAL_COMMANDS: &[&str] = &[
    "rm -rf /var/*",
    "rm -rf /etc/*",
    "rm -rf /home/*",
    "DROP DATABASE*",
    "TRUNCATE *",
    "docker system prune -a*",
    "docker volume rm *",
    "kubectl delete namespace *",
    "kubectl delete pv *",
    "helm uninstall *",
    "terraform destroy*",
    "userdel -r *",
    "lvremove *",
    "vgremove *",
    "zpool destroy *",
];

// 危险命令无需单独清单 —— 默认级别 confirm 已覆盖一切变更操作。
// 只有两种显式清单：deny（硬禁止）与 critical（极高危，追加手打主机名）。
// 2026-08-05 用户决策：移除 Touch ID 后，原 TOUCHID_COMMANDS 全部并入
// confirm 默认级别，不再需要独立收录（保留清单只会制造「为什么这个危险
// 命令不用确认」的假象）。

/// 无副作用的只读命令 —— 自动执行。
///
/// 收录标准：**不修改任何状态**。有任何写入可能的命令都不在此列。
/// 这个白名单存在的唯一理由是可用性：若连查看状态都要逐条确认，
/// Agent 探测一台主机要点十几次确认，产品直接不可用。
/// 可用 `observer.strict = true` 整体关闭。
pub const OBSERVER_COMMANDS: &[&str] = &[
    // 磁盘与文件系统
    "df",
    "df *",
    "du *",
    "lsblk*",
    "mount",
    "findmnt*",
    // 内存与负载
    "free",
    "free *",
    "uptime",
    "vmstat*",
    "iostat*",
    // 身份与主机
    "uname*",
    "hostname",
    "hostnamectl",
    "id",
    "id *",
    "whoami",
    "who",
    "w",
    // 进程
    "ps",
    "ps *",
    "pgrep *",
    "pidof *",
    "top -bn1*",
    // 网络（只读；`ss *` 删除：`ss -K` 可强杀 TCP 连接；`ss -t*`/`ss -u*` 同样可带
    // `-K` 杀连接，只保留无变更选项的精确形态；`ip -j *` 删除：`-j` 是全局选项，
    // `ip -j addr add`/`ip -j link set eth0 down` 都是变更操作）
    "ss",
    "ss -tulpn",
    "ss -tulpn *",
    "netstat *",
    "ip a",
    "ip addr",
    "ip route",
    "ping -c *",
    "dig *",
    "nslookup *",
    "host *",
    "traceroute *",
    // systemd（只读子命令）
    "systemctl status*",
    "systemctl is-active *",
    "systemctl is-enabled *",
    "systemctl show *",
    "systemctl list-units*",
    "systemctl list-timers*",
    // `journalctl*` 收窄：`--vacuum-size` 删日志、`--rotate` 轮转都是数据销毁
    "journalctl -u *",
    "journalctl -b*",
    "journalctl -f*",
    "journalctl --since *",
    "journalctl --until *",
    "journalctl -n*",
    // 包管理（只读子命令）
    "dpkg -l*",
    "dpkg -s *",
    "rpm -q*",
    "apt list*",
    "apk info*",
    // 容器与编排（只读子命令；`kubectl get *` 删除：kubectl get secrets -o yaml
    // 输出 base64 凭据，且 get 支持 create/delete 外的变更动词如 scale）
    "docker ps*",
    "docker images*",
    "docker logs *",
    "docker inspect *",
    "docker stats --no-stream*",
    "podman ps*",
    "kubectl describe *",
    "kubectl logs *",
    "kubectl get pods*",
    "kubectl get nodes*",
    "kubectl get services*",
    "kubectl get deployments*",
    // 文本读取（`awk *`/`grep *` 等通配面过宽 —— 保留精确只读前缀；
    // 任意文件读取类（cat /etc/shadow 等）本来只靠脱敏兜底，见 §7.3）
    "cat *",
    "head *",
    "tail *",
    "less *",
    "wc *",
    "sort *",
    "uniq *",
    "grep *",
    "egrep *",
    "rg *",
    // `sed -n *` 删除：`-n` 与 `-i` 可同用（sed -n -i 's/x/y/' file 原地改文件）
    "sed -n s*",
    "sed -n p*",
    // 文件信息（只读）
    "ls",
    "ls *",
    "stat *",
    "file *",
    "readlink *",
    "realpath *",
    "md5 *",
    "shasum *",
    "sha256sum *",
    // 时间与环境（`date *` 删除：`date -s` 改系统时间；`env`/`printenv*` 删除：
    // 输出整个进程环境，可能含未脱敏的敏感变量，且对判断问题无帮助）
    "date",
    "timedatectl",
    "locale",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::CommandPattern;

    #[test]
    fn all_builtin_patterns_compile() {
        for list in [DENY_COMMANDS, CRITICAL_COMMANDS, OBSERVER_COMMANDS] {
            for pat in list {
                CommandPattern::new(pat)
                    .unwrap_or_else(|e| panic!("内置模式 {pat:?} 编译失败：{e}"));
            }
        }
    }

    #[test]
    fn observer_list_has_no_mutating_commands() {
        // 守护性测试：任何人往 observer 清单里加入含以下动词的命令都会被挡下
        let forbidden = [
            "rm ", "mv ", "cp ", "chmod", "chown", "mkdir", "touch ", "restart", "stop", "start",
            "install", "remove", "delete", "write", "kill", "reboot", "shutdown", "truncate",
            "dd ",
        ];
        for cmd in OBSERVER_COMMANDS {
            let lower = cmd.to_lowercase();
            for bad in forbidden {
                assert!(
                    !lower.contains(bad),
                    "observer 清单中的 {cmd:?} 含有变更性动词 {bad:?}，\
                     只读白名单里不允许出现任何可能修改状态的命令"
                );
            }
        }
    }

    #[test]
    fn deny_list_is_not_empty() {
        assert!(!DENY_COMMANDS.is_empty());
        assert!(DENY_COMMANDS.contains(&"rm -rf /"));
    }
}
