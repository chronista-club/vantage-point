/**
 * Lane (Conductor / Performer) の表示ヘルパー。
 *
 * v1.0 柱 2 PR-2。 旧 SIDEBAR_HTML の `STAND_GLYPH` / `agentDisplayName` /
 * `laneLabel` / `laneAddressKey` を SolidJS sidebar 用に port したもの。
 */
import type { IconName } from "@chronista-club/creo-ui-icons-web";
import type { LaneInfo } from "../generated/LaneInfo";

/**
 * Lane Agent kind → Phosphor icon (default / active=fill weight) のペア。
 * 旧 SIDEBAR_HTML `STAND_GLYPH` の port。 `-fill` を別 literal で持ち、
 * `IconName` 型に収まるようにする (文字列連結だと型が string に広がるため)。
 */
const COMPONENT_ICON: Record<string, { default: IconName; active: IconName }> = {
	claude: { default: "ph:chats-teardrop", active: "ph:chats-teardrop-fill" },
	shell: { default: "ph:terminal-window", active: "ph:terminal-window-fill" },
	tmux: { default: "ph:presentation", active: "ph:presentation-fill" },
	board: { default: "ph:compass", active: "ph:compass-fill" },
	runner: { default: "ph:plant", active: "ph:plant-fill" },
	devices: { default: "ph:magnet", active: "ph:magnet-fill" },
};

/** Agent kind から icon 名を解決。 active 時は fill weight。 未知 agent は `null`。 */
export function agentIcon(agent: string, active: boolean): IconName | null {
	const set = COMPONENT_ICON[agent];
	if (!set) return null;
	return active ? set.active : set.default;
}

/** component の表示名 (Architecture v4 metaphor)。 */
export function agentDisplayName(agent: string): string {
	switch (agent) {
		case "claude":
		case "hd": // legacy alias (旧 Heaven's Door)
			return "Conversation";
		case "shell":
			return "Shell";
		case "tmux":
			return "Tmux";
		case "board":
			return "Board";
		case "runner":
			return "Runner";
		case "devices":
			return "Devices";
		default:
			return agent;
	}
}

/**
 * 開発起点 lane の予約名 (doc 44 D4)。
 *
 * Rust 側 `ROOT_LANE_NAME` と同値でなければ address が食い違う
 * (`laneAddressKey()` は byte-for-byte 一致が要件)。
 */
const ROOT_LANE_NAME = "root";

/**
 * Lane が開発起点でない (旧 Performer) か。
 *
 * doc 44 P2: 旧 `kind` field は撤去された。lane は全て対等で、開発起点は予約名で表される
 * ので、判定は名前の比較になった。
 */
export function isPerformerLane(lane: LaneInfo): boolean {
	return lane.address.name !== ROOT_LANE_NAME;
}

/**
 * root session の mode ("tui" | "gui") を sessions (registry snapshot) から導出する。
 *
 * doc 53 R1: 旧 lane 単位 `console_mode` field は退役 — TS 側の導出はこの 1 関数に閉じる
 * (Rust 側の対 = `app::root_mode_of`)。sessions 欠落 (boot 窓の placeholder 等) は "tui"
 * (旧 serde default と同値) に倒す。
 */
export function rootModeOf(lane: LaneInfo): string {
	const reg = lane.sessions;
	if (!reg) return "tui";
	return reg.sessions.find((s) => s.key === reg.root)?.mode ?? "tui";
}

/**
 * Lane が生きているか (= engine を持ちうる状態か)。
 *
 * ⚠️ 生死を `pid` だけで測らない。 doc 33: chat lane (gui) は engine-less (pid=null) が
 * **正常形**で、 chat engine は submit 契機の lazy spawn。 よって pid は「今 engine が
 * 生きているか」で揺れ、 pid だけの判定は同じ lane の見え方を時間で変える。
 *
 * dim 表示 (`inactive`) と context menu (Restart / Respawn の分岐) の両方がこの 1 つの
 * 述語を共有する — 片方だけ chat の手当てが漏れる形が過去のバグだった。
 */
export function isLaneAlive(lane: LaneInfo): boolean {
	return lane.pid != null || rootModeOf(lane) === "gui";
}

/** Lane の表示ラベル。 開発起点はラベルなし、 それ以外は lane 名。 */
export function laneLabel(lane: LaneInfo): string {
	// 地で判別 (A): 開発起点はラベルなし (repo folder 直下 + インデントなしで自明)、
	// それ以外は name のみ (段下げ + 左罫線で従属関係を示す)。
	if (!isPerformerLane(lane)) return "";
	return lane.address.name;
}

/**
 * lane の cwd を「地 (ground)」表示用のラベルに畳む（純粋）。
 *
 * メンタルモデル: **絶対 path は repo が持つ** (`~/repos/proj-dir`)。 lane はそこからの
 * **差分だけ**を名乗る。 こうすると絶対 path が世界に一度しか現れず、 冗長性が構造的にゼロになる。
 *
 * - conductor (cwd = repo root) → `""` = **語ることが無いので黙る** (呼び手は行ごと出さない)
 * - performer (repo 配下) → `".vp/lanes/x"` 等の相対 path
 * - repo の外に居る lane (別所の clone 等) → 差分で表せない = **驚き**なので ~ 短縮した
 *   絶対 path を full で出す (home 推定は mac `/Users/<u>/` / Linux `/home/<u>/`。 外しても
 *   絶対 path がそのまま出るだけで実害は無い — tooltip は常に完全な path)。
 */
export function laneCwdLabel(cwd: string, repoPath: string): string {
	if (!cwd) return "";
	if (cwd === repoPath) return "";
	if (repoPath && cwd.startsWith(`${repoPath}/`)) {
		return cwd.slice(repoPath.length + 1);
	}
	return cwd.replace(/^\/(?:Users|home)\/[^/]+(?=\/|$)/, "~");
}

/**
 * Lane address を Display 形 (`<repo>/root` / `<repo>/performer/<name>`) に変換。
 * Rust `LaneAddressWire::key()` と完全一致させる (active selection 比較に使うため)。
 */
export function laneAddressKey(lane: LaneInfo): string {
	// doc 44 P2: フラット化で `<repo>/<name>` の 1 形になった
	// (Rust 側 `LaneAddressWire::key()` / `LaneAddress::Display` と byte-for-byte 一致)。
	const a = lane.address;
	return `${a.repo}/${a.name}`;
}

/**
 * Lane の tree connector の状態 class を導出する (Light Grid state 言語の FSM 投影、 純関数)。
 *
 * 描画は Shell.tsx の `.vp-lane-connector` (CSS pseudo-element) が担い、 ここは意味論だけを
 * class で返す。 `awaitingInput` は OSC 99 由来の console 入力待ち (caller が
 * `sidebar.awaiting_input[addr]` を渡す — store 依存を外に出して testable に保つ)。
 *
 * - conductor: spine の頭 (頭石)
 * - working (conn-auto): flow が動いている = solid cyan tap + node
 * - needs-you (conn-hitl): ユーザ本人待ち = magenta diamond (盤面で唯一光る状態)
 * - idle (conn-dead): flow 不在 = No current (極薄破線 + 中空 node)
 *
 * FSM 投影 (2026-07-11、 mem_1Ccv39yTsb9knkjucKCP3Z): 一次 source は **server 側
 * flow_state** (LaneInfo.flow_state、 daemon が wire store から derive = vp flow progress と
 * 同一判定)。 client で再推定しない。 これで「プロンプト待ちの TUI claude は pid が生きて
 * いるため working と誤判定」 (dep symlink lane の偽 WORKING) が根治する — wire 活動の
 * 無い lane は flow_state = "idle" でほぼ消える。
 *
 * - awaiting_user → needs-you: 未 ack needs_user wire (ユーザ本人の回答待ち)。
 * - awaitingInput (OSC 99) も needs-you に残す — flow とは別軸の「console がユーザを
 *   待っている」 signal で、 active console の HITL (AskUserQuestion 等) を拾える唯一の経路。
 * - flow_state 欠落 (旧 daemon) は従来の pid heuristic に fallback。
 */
export function laneConnector(lane: LaneInfo, awaitingInput: boolean): string {
	if (!isPerformerLane(lane)) {
		return "conn-root"; // conductor は幹 = spine の頭
	}
	const fs = lane.flow_state;
	if (fs === "awaiting_user") return "conn-hitl"; // ユーザ本人待ち = needs-you
	if (awaitingInput) return "conn-hitl"; // console 入力待ち = needs-you
	if (fs != null) {
		// working / hitl_pending / stuck = flow が動いている (hitl_pending は conductor の
		// 仕事、 stuck は conductor が捌く異常 — どちらもユーザを光で呼ぶ状態ではない)
		if (fs === "working" || fs === "hitl_pending" || fs === "stuck") {
			return "conn-auto";
		}
		return "conn-dead"; // idle / completed = ほぼ消える
	}
	// fallback (旧 daemon = flow_state 未投影): engine 実在 (pid) heuristic
	if (lane.pid == null) return "conn-dead";
	return "conn-auto";
}
