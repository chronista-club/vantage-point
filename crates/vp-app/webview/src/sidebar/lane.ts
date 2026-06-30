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
