/**
 * Lane (Lead / Wing) の表示ヘルパー。
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
		default:
			return stand;
	}
}

/** Lane kind が Wing か。 */
function isWingKind(kind: string): boolean {
	return kind === "wing";
}

/** Lane が Wing か (Lead との対)。 */
export function isWingLane(lane: LaneInfo): boolean {
	return isWingKind(lane.kind) || isWingKind(lane.address.kind);
}

/** Lane の表示ラベル。 Lead はそのまま、 Wing は `Wing: <name>`。 */
export function laneLabel(lane: LaneInfo): string {
	const kind = lane.kind || lane.address.kind;
	// 地で判別 (A): Lead はラベルなし (project folder 直下 + インデントなしで自明)、
	// Wing は name のみ ("Wing:" prefix を省略、 段下げ + 左罫線で Wing と判別)。
	if (kind === "lead") return "";
	if (isWingKind(kind)) return lane.name ?? lane.address.name ?? "?";
	return kind;
}

/**
 * Lane address を Display 形 (`<project>/lead` / `<project>/wing/<name>`) に変換。
 * Rust `LaneAddressWire::key()` と完全一致させる (active selection 比較に使うため)。
 */
export function laneAddressKey(lane: LaneInfo): string {
	const a = lane.address;
	if (isWingKind(a.kind)) {
		return `${a.project}/wing/${a.name ?? "<unnamed>"}`;
	}
	return `${a.project}/${a.kind || "lead"}`;
}

/** Lane の制御状態 (control surrender FSM の表示用 3 値)。 */
export type LaneFsm = "auto" | "hitl" | "idle";

/**
 * Lane の FSM 状態を導出 (= 状態アイコンの出し分け)。
 * - idle: pid null (= Pane 不在、 休眠/未起動)
 * - hitl: 入力待ち (= 人の入力を待つ、 control を握っている)
 * - auto: それ以外で稼働中 (= AI 主導で自走、 control を手放した)
 */
export function laneFsm(lane: LaneInfo, awaiting: boolean): LaneFsm {
	if (lane.pid == null) return "idle";
	if (awaiting) return "hitl";
	return "auto";
}

/** FSM 状態 → 状態アイコン (主体メタファー: 誰が主体か)。 */
export const FSM_ICON: Record<LaneFsm, IconName> = {
	auto: "ph:robot", // AI 主導で自走
	hitl: "ph:keyboard", // 人の入力待ち
	idle: "ph:circle-dashed", // 休眠 (実体が薄い)
};

/** FSM 状態 → tooltip ラベル。 */
export const FSM_LABEL: Record<LaneFsm, string> = {
	auto: "自走中 (AI 主導)",
	hitl: "入力待ち (人の操作待ち)",
	idle: "休眠",
};
