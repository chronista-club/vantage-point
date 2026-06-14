// SSOT: crates/vp-app/schema/vp-sidebar.kdl
// 再生成: cargo test -p vp-app --test sidebar_ipc_codegen
// Auto-generated TypeScript definitions
// DO NOT EDIT MANUALLY

export type Timestamp = string; // ISO-8601 format
export type UUID = string;
export type LanguageCode = string; // ISO 639-1 format

// Namespace: vp.sidebar
// Version: 1.0.0

// ════════════════════════════════════════════════
// Channel: ipc (backend=stream)
// ════════════════════════════════════════════════

/** Request "process:toggle" */
export interface ProcessToggle {
  path: string;
  expanded: boolean;
}

/** Request "process:reorder" */
export interface ProcessReorder {
  order: string[];
}

/** Request "process:restart" */
export interface ProcessRestart {
  path: string;
}

/** Request "process:stop" */
export interface ProcessStop {
  path: string;
}

/** Request "process:delete" */
export interface ProcessDelete {
  path: string;
}

/** Request "process:add" — empty payload */
export interface ProcessAdd {}

/** Request "lane:select" */
export interface LaneSelect {
  path: string;
  address: string;
}

/** Request "lane:delete" */
export interface LaneDelete {
  path: string;
  address: string;
}

/** Request "lane:restart" */
export interface LaneRestart {
  path: string;
  address: string;
}

/** Request "lane:add_performer" */
export interface LaneAddPerformer {
  path: string;
  name: string;
  branch?: string;
  stand?: string;
}

/** Request "stands:fetch" */
export interface StandsFetch {
  path: string;
}

/** Request "stand:select" */
export interface StandSelect {
  path: string;
  kind: string;
}

/** Request "project:clone:pickFolder" — empty payload */
export interface ProjectClonePickFolder {}

/** Request "files:list" */
export interface FilesList {
  path: string;
  address: string;
}

/** Request "files:open" */
export interface FilesOpen {
  path: string;
  address: string;
  rel_path: string;
}

/** Event name → 生成 interface の map for "ipc" (= type-narrowing 用) */
export type IpcChannelEventTypes = Record<string, never>;

/** Request name → { request, response } 生成 interface の map for "ipc" */
export type IpcChannelRequestTypes = {
  ProcessToggle: { request: ProcessToggle; response: void };
  ProcessReorder: { request: ProcessReorder; response: void };
  ProcessRestart: { request: ProcessRestart; response: void };
  ProcessStop: { request: ProcessStop; response: void };
  ProcessDelete: { request: ProcessDelete; response: void };
  ProcessAdd: { request: ProcessAdd; response: void };
  LaneSelect: { request: LaneSelect; response: void };
  LaneDelete: { request: LaneDelete; response: void };
  LaneRestart: { request: LaneRestart; response: void };
  LaneAddPerformer: { request: LaneAddPerformer; response: void };
  StandsFetch: { request: StandsFetch; response: void };
  StandSelect: { request: StandSelect; response: void };
  ProjectClonePickFolder: { request: ProjectClonePickFolder; response: void };
  FilesList: { request: FilesList; response: void };
  FilesOpen: { request: FilesOpen; response: void };
};

/** Channel metadata for "ipc" (= Phase 2 runtime SDK 用 type-narrowing 入力) */
export const IpcChannelMeta = {
  name: "ipc" as const,
  backend: "stream" as const,
  from: "client" as const,
  lifetime: "transient" as const,
  events: [] as const,
  requests: {
    ProcessToggle: { request: "process:toggle" as const, response: "void" as const },
    ProcessReorder: { request: "process:reorder" as const, response: "void" as const },
    ProcessRestart: { request: "process:restart" as const, response: "void" as const },
    ProcessStop: { request: "process:stop" as const, response: "void" as const },
    ProcessDelete: { request: "process:delete" as const, response: "void" as const },
    ProcessAdd: { request: "process:add" as const, response: "void" as const },
    LaneSelect: { request: "lane:select" as const, response: "void" as const },
    LaneDelete: { request: "lane:delete" as const, response: "void" as const },
    LaneRestart: { request: "lane:restart" as const, response: "void" as const },
    LaneAddPerformer: { request: "lane:add_performer" as const, response: "void" as const },
    StandsFetch: { request: "stands:fetch" as const, response: "void" as const },
    StandSelect: { request: "stand:select" as const, response: "void" as const },
    ProjectClonePickFolder: { request: "project:clone:pickFolder" as const, response: "void" as const },
    FilesList: { request: "files:list" as const, response: "void" as const },
    FilesOpen: { request: "files:open" as const, response: "void" as const },
  } as const,
  __types: undefined as unknown as { events: IpcChannelEventTypes; requests: IpcChannelRequestTypes },
} as const;

/** Envelope union for channel "ipc" — discriminated on "t". */
export type IpcEnvelope =
  | ({ t: "process:toggle" } & ProcessToggle)
  | ({ t: "process:reorder" } & ProcessReorder)
  | ({ t: "process:restart" } & ProcessRestart)
  | ({ t: "process:stop" } & ProcessStop)
  | ({ t: "process:delete" } & ProcessDelete)
  | ({ t: "process:add" } & ProcessAdd)
  | ({ t: "lane:select" } & LaneSelect)
  | ({ t: "lane:delete" } & LaneDelete)
  | ({ t: "lane:restart" } & LaneRestart)
  | ({ t: "lane:add_performer" } & LaneAddPerformer)
  | ({ t: "stands:fetch" } & StandsFetch)
  | ({ t: "stand:select" } & StandSelect)
  | ({ t: "project:clone:pickFolder" } & ProjectClonePickFolder)
  | ({ t: "files:list" } & FilesList)
  | ({ t: "files:open" } & FilesOpen);


