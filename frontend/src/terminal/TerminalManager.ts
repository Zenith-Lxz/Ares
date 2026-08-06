//! 终端管理（Phase 2 移植版，Spike 验证过的实现 + resize 防抖）。
//!
//! 坑位全带（方案 §13）：
//! - #4 unicode11 加载后必须 activeVersion = '11'
//! - #5 allowTransparency: true 必须开，否则毛玻璃全废
//! - #6 context loss 必须处理，降级 DOM renderer
//! - #7 终端内容完全由 xterm.js 管，不进 React state
//! - #14 后端双线程推送（本文件只消费）
//! - #15/#16 decoration 需要 allowProposedApi + refresh 兜底

import { Terminal } from '@xterm/xterm';
import { WebglAddon } from '@xterm/addon-webgl';
import { FitAddon } from '@xterm/addon-fit';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { Channel } from '@tauri-apps/api/core';
import { sessionSubscribe, sessionWrite, sessionResize, type PtyChunk } from '../ipc/commands';

export interface TermHandle {
  id: number;
  term: Terminal;
  host: string;
  webgl?: WebglAddon;
}

export const DORIC_THEME = {
  background: 'rgba(0,0,0,0)', // 透明，让毛玻璃透出
  foreground: '#E8E0D5',
  cursor: '#C08552',
  cursorAccent: '#161412',
  selectionBackground: 'rgba(192,133,82,0.28)',
  black: '#4A453D',
  red: '#A03E3E',
  green: '#8A9A5B',
  yellow: '#C08552',
  blue: '#6B8299',
  magenta: '#9A7AA0',
  cyan: '#7FA6A0',
  white: '#E8E0D5',
  brightBlack: '#7D7668',
  brightRed: '#B85C38',
  brightGreen: '#A3B072',
  brightYellow: '#D9A066',
  brightBlue: '#8AA3B8',
  brightMagenta: '#B394B8',
  brightCyan: '#9CC0BA',
  brightWhite: '#F5F0E8',
};

function base64ToUint8Array(b64: string): Uint8Array {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

/** 创建并挂载一个终端，订阅 Channel，接通输入与尺寸。返回句柄（Terminal 不进 React）。 */
export async function createTerminal(
  container: HTMLElement,
  id: number,
  host: string,
  opts?: {
    webgl?: boolean;
    onChunk?: (bytes: number, total: number) => void;
    onContextLoss?: () => void;
    onWebglState?: (msg: string) => void;
  },
): Promise<TermHandle> {
  const term = new Terminal({
    fontSize: 14,
    fontFamily: '"Fira Code", Menlo, monospace',
    lineHeight: 1.4,
    letterSpacing: 0,
    cursorBlink: true,
    cursorStyle: 'block',
    scrollback: 5000, // 坑 #12
    allowTransparency: true, // ★ 坑 #5
    macOptionIsMeta: true,
    allowProposedApi: true, // 坑 #15
    theme: DORIC_THEME,
  });

  // 坑 #4：unicode11 必须加载且激活
  term.loadAddon(new Unicode11Addon());
  term.unicode.activeVersion = '11';

  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(container);
  fit.fit();

  const handle: TermHandle = { id, term, host };

  // WebGL（默认挂）
  if (opts?.webgl !== false) {
    const webgl = new WebglAddon();
    // 坑 #6：context lost 降级 DOM renderer，不重试
    webgl.onContextLoss(() => {
      webgl.dispose();
      handle.webgl = undefined;
      opts?.onWebglState?.('CONTEXT LOST');
      opts?.onContextLoss?.();
    });
    try {
      term.loadAddon(webgl);
      handle.webgl = webgl;
    } catch (e) {
      opts?.onWebglState?.(`loadAddon FAILED: ${String(e)}`);
    }
    // 加载后确认 addon 存活并多次强制重绘（诊断黑屏：窗口显示前创建的
    // context 可能被 WKWebView 节流，窗口显示后需要真实渲染事件恢复）
    [800, 3000, 6000].forEach((ms, i) => {
      setTimeout(() => {
        if (handle.webgl) {
          term.refresh(0, term.rows - 1);
          opts?.onWebglState?.(`alive #${i + 1}, refresh rows=${term.rows} cols=${term.cols}`);
        } else {
          opts?.onWebglState?.('disposed');
        }
      }, ms);
    });
  }

  // Channel：后端 16ms 批量 base64 → 原始字节 → term.write
  let recvTotal = 0;
  const channel = new Channel<PtyChunk>();
  channel.onmessage = (chunk) => {
    const bytes = base64ToUint8Array(chunk.data);
    recvTotal += bytes.length;
    opts?.onChunk?.(bytes.length, recvTotal);
    term.write(bytes);
  };
  await sessionSubscribe(id, channel);

  // 键盘 → 后端写 PTY
  term.onData((data) => {
    void sessionWrite(id, data);
  });

  // 尺寸变化 → fit + 通知后端 resize
  // ★ 防抖 100ms：ResizeObserver 高频触发，连续 resize 会打爆 IPC
  let resizeTimer: ReturnType<typeof setTimeout> | undefined;
  const ro = new ResizeObserver(() => {
    fit.fit();
    if (resizeTimer) clearTimeout(resizeTimer);
    resizeTimer = setTimeout(() => {
      void sessionResize(id, term.cols, term.rows);
    }, 100);
  });
  ro.observe(container);

  return handle;
}
