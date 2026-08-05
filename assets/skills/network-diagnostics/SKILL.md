---
name: network-diagnostics
description: Use when diagnosing connectivity, DNS, ports, latency, or firewall issues.
---

# 网络诊断

## 触发
- 连不上 / 超时 / DNS 解析失败 / 端口不通 / 防火墙拦截

## 步骤
1. 本机视角：`ip -j addr` + `ip route` + `ss -tlnp`（端口监听）
2. 连通性：`ping -c 3 <host>` + `nc -zvw3 <host> <port>` + `curl -vI https://<host>` 
3. DNS：`cat /etc/resolv.conf` + `dig +short <domain>` + `getent hosts <domain>`
4. 防火墙：`iptables -L -n` / `ufw status` / `nft list ruleset`（先读，变更走审批）
5. 路径追踪：`traceroute -n <host>` / `mtr -r -c 5 <host>`

## 命令
```bash
ss -tlnp
ip -j addr; ip route
ping -c 3 8.8.8.8
nc -zvw3 <host> <port>
dig +short <domain> @1.1.1.1
curl -sS -o /dev/null -w "%{http_code} %{time_total}s\n" https://<domain>
```

## 陷阱
- 服务器上没 dig/nc 时用 `getent`/`/dev/tcp`：`timeout 3 bash -c '</dev/tcp/<host>/<port>'`
- 通但慢：先查 DNS 延迟（`time dig`）再查带宽/拥塞
- 防火墙改动必须确认，改完立即验证连通性
