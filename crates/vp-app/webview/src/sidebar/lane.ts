/**
 * Lane (Conductor / Performer) の表示ヘルパー。
 *
 * v1.0 柱 2 PR-2。 旧 SIDEBAR_HTML の `STAND_GLYPH` / `standDisplayName` /
 * `laneLabel` / `laneAddressKey` を SolidJS sidebar 用に port したもの。
 */
import type { IconName } from "creoui-icons-web";
import type { LaneInfo } from "../generated/LaneInfo";

/**
 * Lane Stand kind → Phosphor icon (default / active=fill weight) のペア。
 * 旧 SIDEBAR_HTML `STAND_GLYPH` の port。 `-fill` を別 literal で持ち、
 * `IconName` 型に収まるようにする (文字列連結だと型が string に広がるため)。
 */
const STAND_ICON: Record<string, { default: IconName; active: IconName }> = {
	echoes: { default: "ph:chats-teardrop", active: "ph:chats-teardrop-fill" },
	hd: { default: "ph:chats-teardrop", active: "ph:chats-teardrop-fill" }, // legacy alias
	shell: { default: "ph:terminal-window", active: "ph:terminal-window-fill" },
	tmux: { default: "ph:presentation", active: "ph:presentation-fill" },
	paisley_park: { default: "ph:compass", active: "ph:compass-fill" },
	gold_experience: { default: "ph:plant", active: "ph:plant-fill" },
	hermit_purple: { default: "ph:plug", active: "ph:plug-fill" },
	bastet: { default: "ph:magnet", active: "ph:magnet-fill" },
};

/** Stand kind から icon 名を解決。 active 時は fill weight。 未知 stand は `null`。 */
export function standIcon(stand: string, active: boolean): IconName | null {
	const set = STAND_ICON[stand];
	if (!set) return null;
	return active ? set.active : set.default;
}

/** Stand の表示名 (Architecture v4 metaphor)。 */
export function standDisplayName(stand: string): string {
	switch (stand) {
		case "echoes":
		case "hd": // legacy alias (旧 Heaven's Door)
			return "Echoes";
		case "shell":
			return "Shell";
		case "tmux":
			return "Tmux";
		case "paisley_park":
			return "Paisley Park";
		case "gold_experience":
			return "Gold Experience";
		case "hermit_purple":
			return "Hermit Purple";
		case "bastet":
			return "Bastet";
		default:
			return stand;
	}
}

/** Lane kind が Performer か。 */
function isPerformerKind(kind: string): boolean {
	return kind === "performer";
}

/** Lane が Performer か (Conductor との対)。 */
export function isPerformerLane(lane: LaneInfo): boolean {
	return isPerformerKind(lane.kind) || isPerformerKind(lane.address.kind);
}

/**
 * Lane が生きているか (= engine を持ちうる状態か)。
 *
 * ⚠️ 生死を `pid` だけで測らない。 doc 33: chat lane (Act II) は engine-less (pid=null) が
 * **正常形**で、 chat engine は submit 契機の lazy spawn。 よって pid は「今 engine が
 * 生きているか」で揺れ、 pid だけの判定は同じ lane の見え方を時間で変える。
 *
 * dim 表示 (`inactive`) と context menu (Restart / Respawn の分岐) の両方がこの 1 つの
 * 述語を共有する — 片方だけ chat の手当てが漏れる形が過去のバグだった。
 */
export function isLaneAlive(lane: LaneInfo): boolean {
	return lane.pid != null || lane.console_mode === "chat";
}

/** Lane の表示ラベル。 Conductor はそのまま、 Performer は `Performer: <name>`。 */
export function laneLabel(lane: LaneInfo): string {
	const kind = lane.kind || lane.address.kind;
	// 地で判別 (A): Conductor はラベルなし (project folder 直下 + インデントなしで自明)、
	// Performer は name のみ ("Performer:" prefix を省略、 段下げ + 左罫線で Performer と判別)。
	if (kind === "conductor") return "";
	if (isPerformerKind(kind)) return lane.name ?? lane.address.name ?? "?";
	return kind;
}

/**
 * Lane address を Display 形 (`<project>/conductor` / `<project>/performer/<name>`) に変換。
 * Rust `LaneAddressWire::key()` と完全一致させる (active selection 比較に使うため)。
 */
export function laneAddressKey(lane: LaneInfo): string {
	const a = lane.address;
	if (isPerformerKind(a.kind)) {
		return `${a.project}/performer/${a.name ?? "<unnamed>"}`;
	}
	return `${a.project}/${a.kind || "conductor"}`;
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
 * flow_state** (LaneInfo.flow_state、 TheWorld が wire store から derive = vp flow progress と
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
		return "conn-conductor"; // conductor は幹 = spine の頭
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
