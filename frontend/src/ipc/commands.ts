//! IPC 类型安全封装（与 Rust 侧 commands 类型逐一对齐，方案 §9 ipc/）。
//!
//! Phase 1：mock 类型（Rust 侧返回 mock）；Phase 2 起随后端逐步对齐。

import { invoke, Channel } from '@tauri-apps/api/core';

// ── session（§5.1）────────────────────────────────────────────

export type SessionId = number;

export interface SessionInfo {
  id: SessionId;
  kind: 'local' | 'ssh';
  host_alias: string | null;
  title: string;
  connected: boolean;
  cols: number;
  rows: number;
}

export interface PtyChunk {
  id: SessionId;
  /** base64 原始 PTY 字节 */
  data: string;
}

export function sessionCreate(
  hostAlias: string | null,
  cols: number,
  rows: number,
): Promise<SessionInfo> {
  return invoke('session_create', { hostAlias, cols, rows });
}

export function sessionSubscribe(
  id: SessionId,
  channel: Channel<PtyChunk>,
): Promise<void> {
  return invoke('session_subscribe', { id, channel });
}

export function sessionWrite(id: SessionId, data: string): Promise<void> {
  return invoke('session_write', { id, data });
}

export function sessionResize(id: SessionId, cols: number, rows: number): Promise<void> {
  return invoke('session_resize', { id, cols, rows });
}

export function sessionClose(id: SessionId): Promise<void> {
  return invoke('session_close', { id });
}

export function sessionList(): Promise<SessionInfo[]> {
  return invoke('session_list');
}

// ── guard（§5.2）──────────────────────────────────────────────

export type CommandVerdict =
  | { kind: 'allow' }
  | { kind: 'confirm'; reason: string }
  | { kind: 'touchid'; reason: string }
  | { kind: 'deny'; reason: string };

export function commandCheck(id: SessionId, command: string): Promise<CommandVerdict> {
  return invoke('command_check', { id, command });
}

export function commandAuthorize(id: SessionId, command: string): Promise<boolean> {
  return invoke('command_authorize', { id, command });
}

// ── host（§5.3）───────────────────────────────────────────────

export interface HostEntry {
  alias: string;
  hostname: string;
  port: number;
  user: string;
  env: string;
  tags: string[];
  note: string;
  reachable: boolean | null;
}

export function hostList(): Promise<HostEntry[]> {
  return invoke('host_list');
}

export function hostGet(alias: string): Promise<HostEntry> {
  return invoke('host_get', { alias });
}

export function hostProbe(aliases: string[]): Promise<void> {
  return invoke('host_probe', { aliases });
}

// ── agent（§5.4）──────────────────────────────────────────────

export type AgentEvent =
  | { type: 'token'; text: string }
  | { type: 'tool_start'; tool: string; summary: string }
  | { type: 'tool_result'; tool: string; display: string; success: boolean }
  | {
      type: 'approval_required';
      approval_id: number;
      host: string;
      env: string;
      command: string;
      decision: string;
      host_count: number;
      reason: string;
    }
  | { type: 'turn_end'; input_tokens: number; output_tokens: number }
  | { type: 'error'; message: string };

export function agentSubscribe(channel: Channel<AgentEvent>): Promise<void> {
  return invoke('agent_subscribe', { channel });
}

export function agentSend(message: string): Promise<void> {
  return invoke('agent_send', { message });
}

export function agentInterrupt(): Promise<void> {
  return invoke('agent_interrupt');
}

export function agentApprove(approvalId: number, approved: boolean): Promise<void> {
  return invoke('agent_approve', { approvalId, approved });
}

export function agentSetScope(aliases: string[]): Promise<void> {
  return invoke('agent_set_scope', { aliases });
}

// ── audit（§5.5）──────────────────────────────────────────────

export interface AuditRecord {
  seq: number;
  ts: string;
  actor: string;
  action: string;
  host: string;
  summary: string;
}

export interface VerifyReport {
  ok: boolean;
  checked: number;
  broken: number[];
}

export function auditQuery(filter?: string): Promise<AuditRecord[]> {
  return invoke('audit_query', { filter });
}

export function auditVerify(): Promise<VerifyReport> {
  return invoke('audit_verify');
}

// ── config（§5.5）─────────────────────────────────────────────

export interface AppConfig {
  font_size: number;
  line_height: number;
  theme: string;
  scrollback: number;
  command_guard: boolean;
  glass_blur: number;
  glass_opacity: number;
}

export interface ThemeInfo {
  name: string;
  dark: boolean;
}

export function configGet(): Promise<AppConfig> {
  return invoke('config_get');
}

export function configSet(config: AppConfig): Promise<void> {
  return invoke('config_set', { config });
}

export function themeList(): Promise<ThemeInfo[]> {
  return invoke('theme_list');
}

// ── vault（§5.5，安全铁律：无 vault_get）──────────────────────

export function vaultHas(alias: string): Promise<boolean> {
  return invoke('vault_has', { alias });
}

export function vaultSet(alias: string, secret: string): Promise<void> {
  return invoke('vault_set', { alias, secret });
}
