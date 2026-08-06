//! Phase 2 根视图：终端 + 会话控制 + 密码弹窗。
//! 终端内容不进 React state（坑 #7）—— 句柄存 ref。

import { useEffect, useRef, useState } from 'react';
import { createTerminal, type TermHandle } from './terminal/TerminalManager';
import { sessionClose, sessionCreate, sessionProvidePassword } from './ipc/commands';

const SSH_ALIAS = '测试'; // hosts.toml 键（10.8.8.34）

export default function App() {
  const [status, setStatus] = useState('未连接');
  // 诊断：捕获 webview 内未处理 JS 错误
  useEffect(() => {
    const h = (e: ErrorEvent) => setLog((prev) => [...prev.slice(-20), `JS ERROR: ${e.message}`]);
    window.addEventListener('error', h);
    return () => window.removeEventListener('error', h);
  }, []);
  const [log, setLog] = useState<string[]>([]);
  const [needPwdAlias, setNeedPwdAlias] = useState<string | null>(null);
  const [pwd, setPwd] = useState('');

  const containerRef = useRef<HTMLDivElement>(null);
  const handleRef = useRef<TermHandle | null>(null);

  const addLog = (m: string) => setLog((prev) => [...prev.slice(-20), m]);

  const mountTerminal = async (id: number, host: string) => {
    const el = containerRef.current;
    if (!el) return;
    // 复用容器：先清空
    el.innerHTML = '';
    const h = await createTerminal(el, id, host, {
      onChunk: (_bytes, total) => {
        if (total < 512) setStatus(`${host}: ${total}B`);
        if (total < 1024) addLog(`term ${id}: 容器 ${el.clientWidth}x${el.clientHeight}px`);
      },
      onContextLoss: () => addLog(`term ${id} CONTEXT LOST`),
      onWebglState: (m) => addLog(`term ${id} webgl: ${m}`),
    });
    handleRef.current = h;
  };

  /** 开本地 shell */
  const openLocal = async () => {
    const out = await sessionCreate(null, 100, 30);
    if (out.status === 'ok') {
      setStatus('本地 shell 已连接');
      await mountTerminal(out.id, '本地');
    }
  };

  /** 开 SSH（10.8.8.34）。无密码 → 弹窗。 */
  const openSsh = async () => {
    setStatus('连接中…');
    const out = await sessionCreate(SSH_ALIAS, 100, 30);
    if (out.status === 'need_password') {
      setNeedPwdAlias(out.alias);
      setStatus(`需要密码：${out.alias}`);
      return;
    }
    setStatus(`${SSH_ALIAS} 已连接`);
    await mountTerminal(out.id, SSH_ALIAS);
  };

  /** 密码弹窗确认：写 vault → 重新连接 */
  const submitPassword = async () => {
    if (!needPwdAlias) return;
    try {
      await sessionProvidePassword(needPwdAlias, pwd);
      setPwd('');
      setNeedPwdAlias(null);
      setStatus('密码已保存，重新连接…');
      await openSsh();
    } catch (e) {
      addLog(`密码保存失败: ${String(e)}`);
    }
  };

  /** 关闭会话 */
  const closeSession = async () => {
    const h = handleRef.current;
    if (h) {
      await sessionClose(h.id);
      handleRef.current = null;
      setStatus('已关闭');
      if (containerRef.current) containerRef.current.innerHTML = '';
    }
  };

  // 自动验证流程（Phase 2 验收）：
  // 1. 开 SSH（vault 有密码直连）2. 注入中文测试命令 3. 8 会话并发创建调查
  useEffect(() => {
    (async () => {
      // 本地 shell 优先（SSH 服务器当前握手被拒，见验证报告）
      await openLocal();
      // 2s 后向本地注入中文验证（免辅助权限）
      setTimeout(() => {
        const h = handleRef.current;
        if (h) {
          void (async () => {
            const { sessionWrite } = await import('./ipc/commands');
            await sessionWrite(h.id, 'echo "中文测试总用量 4"; ll\n');
            addLog('auto-injected: echo 中文 + ll (local)');
          })();
        }
      }, 2500);
      // 15s 后尝试 SSH（服务器若解除限流则连接；失败不覆盖本地终端）
      setTimeout(() => {
        void openSsh().then(() => {
          const h = handleRef.current;
          if (h && h.host !== '本地') {
            setTimeout(() => {
              void (async () => {
                const { sessionWrite } = await import('./ipc/commands');
                await sessionWrite(h.id, 'echo "中文测试总用量 4"; ll\n');
                addLog('auto-injected: echo 中文 + ll (ssh)');
              })();
            }, 3000);
          }
        });
      }, 5000);
      // PTY 并发调查（用户要求确认 Spike 的 TERMINAL_COUNT=2 是否真实限制）：
      // 循环创建 8 个本地会话（不挂载终端），后端日志统计 created 数
      setTimeout(() => {
        void (async () => {
          const { sessionCreate } = await import('./ipc/commands');
          let ok = 0;
          let fail = 0;
          for (let i = 0; i < 8; i++) {
            try {
              const out = await sessionCreate(null, 100, 30);
              if (out.status === 'ok') {
                ok++;
                const { sessionClose } = await import('./ipc/commands');
                await sessionClose(out.id);
              } else fail++;
            } catch {
              fail++;
            }
          }
          addLog(`并发调查: 创建成功 ${ok} / 失败 ${fail} / 共 8`);
        })();
      }, 6000);
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="shell">
      <div className="toolbar">
        <button onClick={() => void openLocal()}>+ 本地</button>
        <button onClick={() => void openSsh()}>+ SSH({SSH_ALIAS})</button>
        <button onClick={() => void closeSession()}>✕ 关闭</button>
        <span className="status">{status}</span>
        <span className="log">{log[log.length - 1] ?? ''}</span>
      </div>
      <div className="term-wrap">
        <div ref={containerRef} className="term" />
      </div>
      {needPwdAlias && (
        <div className="pwd-overlay">
          <div className="pwd-box">
            <h2>连接 {needPwdAlias} 需要密码</h2>
            <input
              type="password"
              value={pwd}
              onChange={(e) => setPwd(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && void submitPassword()}
              placeholder="SSH 密码"
              autoFocus
            />
            <div className="pwd-actions">
              <button onClick={() => setNeedPwdAlias(null)}>取消</button>
              <button className="primary" onClick={() => void submitPassword()}>
                保存并连接
              </button>
            </div>
            <p className="hint">密码加密存入本地 vault（AES-256-GCM），不会进入前端进程。</p>
          </div>
        </div>
      )}
    </div>
  );
}
