//! Phase 1 临时调试页：遍历 invoke 全部 command，验证前后端签名对齐。
//! Phase 2 起替换为真实布局（终端区 + Agent 侧栏 + 状态栏）。

import { useEffect, useState } from 'react';
import { Channel } from '@tauri-apps/api/core';
import * as ipc from './ipc/commands';

interface ProbeRow {
  name: string;
  status: 'ok' | 'fail';
  detail: string;
}

export default function App() {
  const [rows, setRows] = useState<ProbeRow[]>([]);
  const [running, setRunning] = useState(false);

  const run = async () => {
    setRunning(true);
    const out: ProbeRow[] = [];

    const probe = async (name: string, fn: () => Promise<unknown>) => {
      try {
        const r = await fn();
        out.push({ name, status: 'ok', detail: JSON.stringify(r).slice(0, 120) });
      } catch (e) {
        out.push({ name, status: 'fail', detail: String(e).slice(0, 120) });
      }
    };

    await probe('session_create', () => ipc.sessionCreate(null, 100, 30));
    await probe('session_subscribe', () =>
      ipc.sessionSubscribe(1, new Channel<ipc.PtyChunk>()),
    );
    await probe('session_write', () => ipc.sessionWrite(1, 'll\n'));
    await probe('session_resize', () => ipc.sessionResize(1, 100, 30));
    await probe('session_close', () => ipc.sessionClose(1));
    await probe('session_list', () => ipc.sessionList());
    await probe('command_check', () => ipc.commandCheck(1, 'rm -rf /'));
    await probe('command_authorize', () => ipc.commandAuthorize(1, 'rm -rf /'));
    await probe('host_list', () => ipc.hostList());
    await probe('host_get', () => ipc.hostGet('测试'));
    await probe('host_probe', () => ipc.hostProbe(['测试']));
    await probe('agent_subscribe', () =>
      ipc.agentSubscribe(new Channel<ipc.AgentEvent>()),
    );
    await probe('agent_send', () => ipc.agentSend('检查磁盘'));
    await probe('agent_interrupt', () => ipc.agentInterrupt());
    await probe('agent_approve', () => ipc.agentApprove(1, true));
    await probe('agent_set_scope', () => ipc.agentSetScope(['测试']));
    await probe('audit_query', () => ipc.auditQuery());
    await probe('audit_verify', () => ipc.auditVerify());
    await probe('config_get', () => ipc.configGet());
    await probe('config_set', () => ipc.configSet({
      font_size: 14,
      line_height: 1.4,
      theme: 'Doric',
      scrollback: 5000,
      command_guard: true,
      glass_blur: 24,
      glass_opacity: 0.72,
    }));
    await probe('theme_list', () => ipc.themeList());
    await probe('vault_has', () => ipc.vaultHas('ssh-pw:测试'));
    await probe('vault_set', () => ipc.vaultSet('ssh-pw:test', 'x'));

    setRows(out);
    setRunning(false);
  };

  useEffect(() => {
    void run();
  }, []);

  const ok = rows.filter((r) => r.status === 'ok').length;
  const fail = rows.length - ok;

  return (
    <div className="probe">
      <h1>ARES Phase 1 — command 探针</h1>
      <p>
        结果：<span className="ok">ok {ok}</span> / <span className="fail">fail {fail}</span>
        {running && '（运行中…）'}
      </p>
      <button onClick={() => void run()}>重新遍历</button>
      <table>
        <thead>
          <tr>
            <th>command</th>
            <th>状态</th>
            <th>返回</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r) => (
            <tr key={r.name}>
              <td>{r.name}</td>
              <td className={r.status}>{r.status}</td>
              <td className="detail">{r.detail}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
