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

/** Event "sidebar:state" */
export interface SidebarState {
  state: any;
}

/** Event "sidebar:error" */
export interface SidebarError {
  message: string;
}

/** Event "performer:create_result" */
export interface PerformerCreateResult {
  repo_path: string;
  name: string;
  error?: string;
}

/** Event "agents:result" */
export interface AgentsResult {
  repo_path: string;
  agents: any[];
  error?: string;
}

/** Event "files:list_result" */
export interface FilesListResult {
  address: string;
  entries: any[];
  truncated: boolean;
}

/** Event "wire:result" */
export interface WireResult {
  payload: any;
}

/** Event "clone:path_picked" */
export interface ClonePathPicked {
  path: string;
}

/** Event "file_picker:open" */
export interface FilePickerOpen {
  address: string;
}

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

/** Request "repo:delete" */
export interface RepoDelete {
  path: string;
}

/** Request "repo:add" — empty payload */
export interface RepoAdd {}

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
  fresh?: boolean;
}

/** Request "lane:new_root" */
export interface LaneNewRoot {
  path: string;
  address: string;
}

/** Request "lane:set_origin" */
export interface LaneSetOrigin {
  path: string;
  address: string;
}

/** Request "lane:reorder" */
export interface LaneReorder {
  path: string;
  order: string[];
}

/** Request "lane:add_performer" */
export interface LaneAddPerformer {
  path: string;
  name: string;
  branch?: string;
  agent?: string;
}

/** Request "agents:fetch" */
export interface AgentsFetch {
  path: string;
}

/** Request "stand:select" */
export interface StandSelect {
  path: string;
  kind: string;
}

/** Request "repo:clone:pickFolder" — empty payload */
export interface RepoClonePickFolder {}

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

/** Request "wire:fetch" */
export interface WireFetch {
  path: string;
  address: string;
}

/** Request "wire:ack" */
export interface WireAck {
  path: string;
  address: string;
  message_id: string;
}

/** Request "update:apply" */
export interface UpdateApply {
  version: string;
}

/** Request "auth:login" */
export interface AuthLogin {
  target?: string;
}

/** Request "auth:logout" */
export interface AuthLogout {
  target?: string;
}

/** Request "actions:persist" */
export interface ActionsPersist {
  items: any[];
  removed: string[];
}

/** Event name → 生成 interface の map for "ipc" (= type-narrowing 用) */
export type IpcChannelEventTypes = {
  SidebarState: SidebarState;
  SidebarError: SidebarError;
  PerformerCreateResult: PerformerCreateResult;
  AgentsResult: AgentsResult;
  FilesListResult: FilesListResult;
  WireResult: WireResult;
  ClonePathPicked: ClonePathPicked;
  FilePickerOpen: FilePickerOpen;
};

/** Request name → { request, response } 生成 interface の map for "ipc" */
export type IpcChannelRequestTypes = {
  ProcessToggle: { request: ProcessToggle; response: void };
  ProcessReorder: { request: ProcessReorder; response: void };
  ProcessRestart: { request: ProcessRestart; response: void };
  ProcessStop: { request: ProcessStop; response: void };
  RepoDelete: { request: RepoDelete; response: void };
  RepoAdd: { request: RepoAdd; response: void };
  LaneSelect: { request: LaneSelect; response: void };
  LaneDelete: { request: LaneDelete; response: void };
  LaneRestart: { request: LaneRestart; response: void };
  LaneNewRoot: { request: LaneNewRoot; response: void };
  LaneSetOrigin: { request: LaneSetOrigin; response: void };
  LaneReorder: { request: LaneReorder; response: void };
  LaneAddPerformer: { request: LaneAddPerformer; response: void };
  AgentsFetch: { request: AgentsFetch; response: void };
  StandSelect: { request: StandSelect; response: void };
  RepoClonePickFolder: { request: RepoClonePickFolder; response: void };
  FilesList: { request: FilesList; response: void };
  FilesOpen: { request: FilesOpen; response: void };
  WireFetch: { request: WireFetch; response: void };
  WireAck: { request: WireAck; response: void };
  UpdateApply: { request: UpdateApply; response: void };
  AuthLogin: { request: AuthLogin; response: void };
  AuthLogout: { request: AuthLogout; response: void };
  ActionsPersist: { request: ActionsPersist; response: void };
};

/** Channel metadata for "ipc" (= Phase 2 runtime SDK 用 type-narrowing 入力) */
export const IpcChannelMeta = {
  name: "ipc" as const,
  backend: "stream" as const,
  from: "client" as const,
  lifetime: "transient" as const,
  events: ["sidebar:state", "sidebar:error", "performer:create_result", "agents:result", "files:list_result", "wire:result", "clone:path_picked", "file_picker:open"] as const,
  requests: {
    ProcessToggle: { request: "process:toggle" as const, response: "void" as const },
    ProcessReorder: { request: "process:reorder" as const, response: "void" as const },
    ProcessRestart: { request: "process:restart" as const, response: "void" as const },
    ProcessStop: { request: "process:stop" as const, response: "void" as const },
    RepoDelete: { request: "repo:delete" as const, response: "void" as const },
    RepoAdd: { request: "repo:add" as const, response: "void" as const },
    LaneSelect: { request: "lane:select" as const, response: "void" as const },
    LaneDelete: { request: "lane:delete" as const, response: "void" as const },
    LaneRestart: { request: "lane:restart" as const, response: "void" as const },
    LaneNewRoot: { request: "lane:new_root" as const, response: "void" as const },
    LaneSetOrigin: { request: "lane:set_origin" as const, response: "void" as const },
    LaneReorder: { request: "lane:reorder" as const, response: "void" as const },
    LaneAddPerformer: { request: "lane:add_performer" as const, response: "void" as const },
    AgentsFetch: { request: "agents:fetch" as const, response: "void" as const },
    StandSelect: { request: "stand:select" as const, response: "void" as const },
    RepoClonePickFolder: { request: "repo:clone:pickFolder" as const, response: "void" as const },
    FilesList: { request: "files:list" as const, response: "void" as const },
    FilesOpen: { request: "files:open" as const, response: "void" as const },
    WireFetch: { request: "wire:fetch" as const, response: "void" as const },
    WireAck: { request: "wire:ack" as const, response: "void" as const },
    UpdateApply: { request: "update:apply" as const, response: "void" as const },
    AuthLogin: { request: "auth:login" as const, response: "void" as const },
    AuthLogout: { request: "auth:logout" as const, response: "void" as const },
    ActionsPersist: { request: "actions:persist" as const, response: "void" as const },
  } as const,
  __types: undefined as unknown as { events: IpcChannelEventTypes; requests: IpcChannelRequestTypes },
} as const;

/** Envelope union for channel "ipc" — discriminated on "t". */
export type IpcEnvelope =
  | ({ t: "process:toggle" } & ProcessToggle)
  | ({ t: "process:reorder" } & ProcessReorder)
  | ({ t: "process:restart" } & ProcessRestart)
  | ({ t: "process:stop" } & ProcessStop)
  | ({ t: "repo:delete" } & RepoDelete)
  | ({ t: "repo:add" } & RepoAdd)
  | ({ t: "lane:select" } & LaneSelect)
  | ({ t: "lane:delete" } & LaneDelete)
  | ({ t: "lane:restart" } & LaneRestart)
  | ({ t: "lane:new_root" } & LaneNewRoot)
  | ({ t: "lane:set_origin" } & LaneSetOrigin)
  | ({ t: "lane:reorder" } & LaneReorder)
  | ({ t: "lane:add_performer" } & LaneAddPerformer)
  | ({ t: "agents:fetch" } & AgentsFetch)
  | ({ t: "stand:select" } & StandSelect)
  | ({ t: "repo:clone:pickFolder" } & RepoClonePickFolder)
  | ({ t: "files:list" } & FilesList)
  | ({ t: "files:open" } & FilesOpen)
  | ({ t: "wire:fetch" } & WireFetch)
  | ({ t: "wire:ack" } & WireAck)
  | ({ t: "update:apply" } & UpdateApply)
  | ({ t: "auth:login" } & AuthLogin)
  | ({ t: "auth:logout" } & AuthLogout)
  | ({ t: "actions:persist" } & ActionsPersist);

/** Envelope union for channel "ipc" — discriminated on "t". */
export type IpcEventEnvelope =
  | ({ t: "sidebar:state" } & SidebarState)
  | ({ t: "sidebar:error" } & SidebarError)
  | ({ t: "performer:create_result" } & PerformerCreateResult)
  | ({ t: "agents:result" } & AgentsResult)
  | ({ t: "files:list_result" } & FilesListResult)
  | ({ t: "wire:result" } & WireResult)
  | ({ t: "clone:path_picked" } & ClonePathPicked)
  | ({ t: "file_picker:open" } & FilePickerOpen);


