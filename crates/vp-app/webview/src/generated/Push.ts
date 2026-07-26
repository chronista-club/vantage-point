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

/** Event name → 生成 interface の map for "push" (= type-narrowing 用) */
export type PushChannelEventTypes = {
  TermEnsureLane: TermEnsureLane;
  TermShowLane: TermShowLane;
  TermRemoveLane: TermRemoveLane;
  TermRemoveSession: TermRemoveSession;
  TermPaste: TermPaste;
};

/** Request name → { request, response } 生成 interface の map for "push" */
export type PushChannelRequestTypes = Record<string, never>;

/** Channel metadata for "push" (= Phase 2 runtime SDK 用 type-narrowing 入力) */
export const PushChannelMeta = {
  name: "push" as const,
  backend: "stream" as const,
  from: "server" as const,
  lifetime: "persistent" as const,
  events: ["term:ensure_lane", "term:show_lane", "term:remove_lane", "term:remove_session", "term:paste"] as const,
  requests: {} as const,
  __types: undefined as unknown as { events: PushChannelEventTypes; requests: PushChannelRequestTypes },
} as const;

/** Envelope union for channel "push" — discriminated on "t". */
export type PushEventEnvelope =
  | ({ t: "term:ensure_lane" } & TermEnsureLane)
  | ({ t: "term:show_lane" } & TermShowLane)
  | ({ t: "term:remove_lane" } & TermRemoveLane)
  | ({ t: "term:remove_session" } & TermRemoveSession)
  | ({ t: "term:paste" } & TermPaste);


