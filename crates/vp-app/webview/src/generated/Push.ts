// SSOT: crates/vp-app/schema/vp-push.kdl
// 再生成: cargo test -p vp-app --test push_codegen
// Auto-generated TypeScript definitions
// DO NOT EDIT MANUALLY

export type Timestamp = string; // ISO-8601 format
export type UUID = string;
export type LanguageCode = string; // ISO 639-1 format

// Namespace: vp.push
// Version: 1.0.0

// ════════════════════════════════════════════════
// Channel: push (backend=stream)
// ════════════════════════════════════════════════

/** Event "term:ensure_lane" */
export interface TermEnsureLane {
  lane: string;
  session: number;
  is_root: boolean;
}

/** Event "term:show_lane" */
export interface TermShowLane {
  lane?: string;
  is_chat: boolean;
}

/** Event "term:remove_lane" */
export interface TermRemoveLane {
  lane: string;
}

/** Event "term:remove_session" */
export interface TermRemoveSession {
  lane: string;
  session: number;
}

/** Event "term:paste" */
export interface TermPaste {
  text: string;
}

/** Event "devices:render" */
export interface DevicesRender {
  devices: any[];
}

/** Event "console:session_list" */
export interface ConsoleSessionList {
  lane: string;
  payload: any;
}

/** Event "console:event" */
export interface ConsoleEvent {
  lane: string;
  event: any;
  session: number;
}

/** Event "console:mode_applied" */
export interface ConsoleModeApplied {
  lane: string;
  session: number;
  mode: string;
}

/** Event "console:agents" */
export interface ConsoleAgents {
  lane: string;
  payload: any;
  req?: string;
}

/** Event "ink:snapshot" */
export interface InkSnapshot {
  path: string;
}

/** Event "ink:snapshot_error" */
export interface InkSnapshotError {
  message: string;
}

/** Event "board:message" */
export interface BoardMessage {
  message: any;
}

/** Event "debuglog:lines" */
export interface DebuglogLines {
  source: string;
  reset: boolean;
  lines: string[];
}

/** Event name → 生成 interface の map for "push" (= type-narrowing 用) */
export type PushChannelEventTypes = {
  TermEnsureLane: TermEnsureLane;
  TermShowLane: TermShowLane;
  TermRemoveLane: TermRemoveLane;
  TermRemoveSession: TermRemoveSession;
  TermPaste: TermPaste;
  DevicesRender: DevicesRender;
  ConsoleSessionList: ConsoleSessionList;
  ConsoleEvent: ConsoleEvent;
  ConsoleModeApplied: ConsoleModeApplied;
  ConsoleAgents: ConsoleAgents;
  InkSnapshot: InkSnapshot;
  InkSnapshotError: InkSnapshotError;
  BoardMessage: BoardMessage;
  DebuglogLines: DebuglogLines;
};

/** Request name → { request, response } 生成 interface の map for "push" */
export type PushChannelRequestTypes = Record<string, never>;

/** Channel metadata for "push" (= Phase 2 runtime SDK 用 type-narrowing 入力) */
export const PushChannelMeta = {
  name: "push" as const,
  backend: "stream" as const,
  from: "server" as const,
  lifetime: "persistent" as const,
  events: ["term:ensure_lane", "term:show_lane", "term:remove_lane", "term:remove_session", "term:paste", "devices:render", "console:session_list", "console:event", "console:mode_applied", "console:agents", "ink:snapshot", "ink:snapshot_error", "board:message", "debuglog:lines"] as const,
  requests: {} as const,
  __types: undefined as unknown as { events: PushChannelEventTypes; requests: PushChannelRequestTypes },
} as const;

/** Envelope union for channel "push" — discriminated on "t". */
export type PushEventEnvelope =
  | ({ t: "term:ensure_lane" } & TermEnsureLane)
  | ({ t: "term:show_lane" } & TermShowLane)
  | ({ t: "term:remove_lane" } & TermRemoveLane)
  | ({ t: "term:remove_session" } & TermRemoveSession)
  | ({ t: "term:paste" } & TermPaste)
  | ({ t: "devices:render" } & DevicesRender)
  | ({ t: "console:session_list" } & ConsoleSessionList)
  | ({ t: "console:event" } & ConsoleEvent)
  | ({ t: "console:mode_applied" } & ConsoleModeApplied)
  | ({ t: "console:agents" } & ConsoleAgents)
  | ({ t: "ink:snapshot" } & InkSnapshot)
  | ({ t: "ink:snapshot_error" } & InkSnapshotError)
  | ({ t: "board:message" } & BoardMessage)
  | ({ t: "debuglog:lines" } & DebuglogLines);


