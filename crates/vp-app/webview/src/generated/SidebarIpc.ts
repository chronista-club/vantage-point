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

/** Event "sub:create_result" */
export interface SubCreateResult {
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

/** Event "wire:result" */
export interface WireResult {
  payload: any;
}

/** Event "clone:path_picked" */
export interface ClonePathPicked {
  path: string;
}

/** Event "settings:result" */
export interface SettingsResult {
  developer_mode: boolean;
  developer_mode_locked: boolean;
  default_repo_root?: string;
  resolved_repo_root?: string;
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

/** Request "lane:add_sub" */
export interface LaneAddSub {
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

/** Request "settings:fetch" — empty payload */
export interface SettingsFetch {}

/** Request "settings:save" */
export interface SettingsSave {
  developer_mode?: boolean;
  default_repo_root?: string;
}

/** Request "settings:pick_repo_root" — empty payload */
export interface SettingsPickRepoRoot {}

/** Request "daemon:restart" — empty payload */
export interface DaemonRestart {}

/** Request "actions:persist" */
export interface ActionsPersist {
  items: any[];
  removed: string[];
}

/** Event name → 生成 interface の map for "ipc" (= type-narrowing 用) */
export type IpcChannelEventTypes = {
  SidebarState: SidebarState;
  SidebarError: SidebarError;
  SubCreateResult: SubCreateResult;
  AgentsResult: AgentsResult;
  WireResult: WireResult;
  ClonePathPicked: ClonePathPicked;
  SettingsResult: SettingsResult;
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
  LaneAddSub: { request: LaneAddSub; response: void };
  AgentsFetch: { request: AgentsFetch; response: void };
  StandSelect: { request: StandSelect; response: void };
  RepoClonePickFolder: { request: RepoClonePickFolder; response: void };
  WireFetch: { request: WireFetch; response: void };
  WireAck: { request: WireAck; response: void };
  UpdateApply: { request: UpdateApply; response: void };
  AuthLogin: { request: AuthLogin; response: void };
  AuthLogout: { request: AuthLogout; response: void };
  SettingsFetch: { request: SettingsFetch; response: void };
  SettingsSave: { request: SettingsSave; response: void };
  SettingsPickRepoRoot: { request: SettingsPickRepoRoot; response: void };
  DaemonRestart: { request: DaemonRestart; response: void };
  ActionsPersist: { request: ActionsPersist; response: void };
};

/** Channel metadata for "ipc" (= Phase 2 runtime SDK 用 type-narrowing 入力) */
export const IpcChannelMeta = {
  name: "ipc" as const,
  backend: "stream" as const,
  from: "client" as const,
  lifetime: "transient" as const,
  events: ["sidebar:state", "sidebar:error", "sub:create_result", "agents:result", "wire:result", "clone:path_picked", "settings:result"] as const,
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
    LaneAddSub: { request: "lane:add_sub" as const, response: "void" as const },
    AgentsFetch: { request: "agents:fetch" as const, response: "void" as const },
    StandSelect: { request: "stand:select" as const, response: "void" as const },
    RepoClonePickFolder: { request: "repo:clone:pickFolder" as const, response: "void" as const },
    WireFetch: { request: "wire:fetch" as const, response: "void" as const },
    WireAck: { request: "wire:ack" as const, response: "void" as const },
    UpdateApply: { request: "update:apply" as const, response: "void" as const },
    AuthLogin: { request: "auth:login" as const, response: "void" as const },
    AuthLogout: { request: "auth:logout" as const, response: "void" as const },
    SettingsFetch: { request: "settings:fetch" as const, response: "void" as const },
    SettingsSave: { request: "settings:save" as const, response: "void" as const },
    SettingsPickRepoRoot: { request: "settings:pick_repo_root" as const, response: "void" as const },
    DaemonRestart: { request: "daemon:restart" as const, response: "void" as const },
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
  | ({ t: "lane:add_sub" } & LaneAddSub)
  | ({ t: "agents:fetch" } & AgentsFetch)
  | ({ t: "stand:select" } & StandSelect)
  | ({ t: "repo:clone:pickFolder" } & RepoClonePickFolder)
  | ({ t: "wire:fetch" } & WireFetch)
  | ({ t: "wire:ack" } & WireAck)
  | ({ t: "update:apply" } & UpdateApply)
  | ({ t: "auth:login" } & AuthLogin)
  | ({ t: "auth:logout" } & AuthLogout)
  | ({ t: "settings:fetch" } & SettingsFetch)
  | ({ t: "settings:save" } & SettingsSave)
  | ({ t: "settings:pick_repo_root" } & SettingsPickRepoRoot)
  | ({ t: "daemon:restart" } & DaemonRestart)
  | ({ t: "actions:persist" } & ActionsPersist);

/** Envelope union for channel "ipc" — discriminated on "t". */
export type IpcEventEnvelope =
  | ({ t: "sidebar:state" } & SidebarState)
  | ({ t: "sidebar:error" } & SidebarError)
  | ({ t: "sub:create_result" } & SubCreateResult)
  | ({ t: "agents:result" } & AgentsResult)
  | ({ t: "wire:result" } & WireResult)
  | ({ t: "clone:path_picked" } & ClonePathPicked)
  | ({ t: "settings:result" } & SettingsResult);


