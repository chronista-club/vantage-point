/**
 * Lane (Main / Sub) の表示ヘルパー。
 *
 * v1.0 柱 2 PR-2。 旧 SIDEBAR_HTML の `STAND_GLYPH` / `agentDisplayName` /
 * `laneLabel` / `laneAddressKey` を SolidJS sidebar 用に port したもの。
 */
import type { IconName } from "@chronista-club/creo-ui-icons-web";
import type { LaneInfo } from "../generated/LaneInfo";
import { isMainLaneName } from "../../lane-address";

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
	vpcode: { default: "ph:flask", active: "ph:flask-fill" },
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
		case "vpcode":
			return "vpcode";
		case "devices":
			return "Devices";
		default:
			return agent;
	}
}

/**
 * Lane が開発起点でない (旧 Sub) か。
 *
 * doc 44 P2: 旧 `kind` field は撤去された。lane は全て対等で、開発起点は予約名で表される
 * ので、判定は名前の比較になった。
 *
 * ⚠️ 判定の実体は `lane-address.ts` の [`isMainLaneName`]（Main 判定の SSOT）。
 * 以前ここにあった独自定数 `ROOT_LANE_NAME = "root"` は #1004 (root → main) の rename
 * から取り残され、daemon が `main` を発行し始めた瞬間に**全 main lane が Sub 誤判定**
 * された（2026-08-19 実機で発見）。「Rust 側と対で直す」というコメントの警告は
 * 機能しなかった — 対で直す運用ではなく、判定を 1 箇所に畳んで構造で防ぐ。
 */
export function isSubLane(lane: LaneInfo): boolean {
	return !isMainLaneName(lane.address.name);
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
	if (!isSubLane(lane)) return "";
	return lane.address.name;
}

/**
 * lane の cwd を「地 (ground)」表示用のラベルに畳む（純粋）。
 *
 * メンタルモデル: **絶対 path は repo が持つ** (`~/repos/proj-dir`)。 lane はそこからの
 * **差分だけ**を名乗る。 こうすると絶対 path が世界に一度しか現れず、 冗長性が構造的にゼロになる。
 *
 * - main (cwd = repo root) → `""` = **語ることが無いので黙る** (呼び手は行ごと出さない)
 * - sub (repo 配下) → `".vp/lanes/x"` 等の相対 path
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
 * lane の address 文字列（active selection の比較 key）。
 *
 * ## ⚠️ **組み立てない** — daemon が発行した値をそのまま返す
 *
 * 旧実装は `${repo}/${name}` を自分で組み、doc に「Rust `LaneAddressWire::key()` と
 * **byte-for-byte 一致させる**」と書いていた = **手で同期を保つ契約**。同じ写像が
 * Rust 2 実装 + TS 2 実装の計 4 箇所にあり、Rust 内ですら食い違った記録がある
 * （`vp-app/src/lane.rs` の `key_matches_display`）。
 *
 * 形式（`<repo>/lane/<name>`）を知るのは daemon の `LaneAddress::canonical` だけ。
 * ここが組み立てを持たない限り、**形式が変わっても webview は無改修**で追随する。
 *
 * ⚠️ `key` が空 = 旧 daemon の payload。その場合だけ旧 2 分節へ縮退する
 * （読み側の `parse_address` が救済する形なので、新形を組み直さない）。
 */
export function laneAddressKey(lane: LaneInfo): string {
	const a = lane.address;
	return a.key !== "" ? a.key : `${a.repo}/${a.name}`;
}

/** ショートカット番号の上限。1 打で選べる範囲＝ 1〜9（0 は使わない）。 */
export const LANE_SHORTCUT_MAX = 9;

/**
 * repo の並び順 → root lane のショートカット番号（1〜9）。範囲外は `null`。
 *
 * ## ⚠️ 番号は **repo の位置そのもの**
 *
 * 「root lane だけがショートカットを持つ」（mako 2026-08-09）＝ repo と 1:1 なので、lane を
 * 数える必要が無い。旧実装は「**展開中** repo の全 lane を上から数える」形で、repo を畳むと
 * 番号が動いた＝筋肉記憶が付かなかった。位置に固定すると、畳んでも並びを変えない限り不変。
 *
 * 表示（sidebar の `#N` badge）と `⌘ hold l` の宛先が**この 1 つの関数**から出るので、
 * 「見えている番号と飛び先が違う」が構造的に起きない。
 */
export function laneShortcutNumber(repoIndex: number): number | null {
	if (!Number.isInteger(repoIndex) || repoIndex < 0) return null;
	return repoIndex < LANE_SHORTCUT_MAX ? repoIndex + 1 : null;
}

/**
 * repo path → ショートカット番号。**表示順で数える**（`sidebar.processes` の生順ではない）。
 *
 * ⚠️ 画面は `resolveRepoOrder(processes, currents_order)` の順で描かれる（user が drag で
 * 並べ替えた順）。生順で数えると **`#N` badge と `⌘ hold l` の飛び先が食い違う**。
 * badge と宛先が同じ 1 本から出るよう、両者ともこの関数を通す。
 */
export function shortcutNumberOf(
	orderedRepoPaths: readonly string[],
	repoPath: string,
): number | null {
	return laneShortcutNumber(orderedRepoPaths.indexOf(repoPath));
}

/**
 * Lane の tree connector の状態 class を導出する (Light Grid state 言語の FSM 投影、 純関数)。
 *
 * 描画は Shell.tsx の `.vp-lane-dot` (CSS pseudo-element) が担い、 ここは意味論だけを
 * class で返す。 `awaitingInput` は OSC 99 由来の console 入力待ち (caller が
 * `sidebar.awaiting_input[addr]` を渡す — store 依存を外に出して testable に保つ)。
 *
 * - main: spine の頭 (頭石)
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
	if (!isSubLane(lane)) {
		return "conn-root"; // main は幹 = spine の頭
	}
	const fs = lane.flow_state;
	if (fs === "awaiting_user") return "conn-hitl"; // ユーザ本人待ち = needs-you
	if (awaitingInput) return "conn-hitl"; // console 入力待ち = needs-you
	if (fs != null) {
		// working / hitl_pending / stuck = flow が動いている (hitl_pending は main の
		// 仕事、 stuck は main が捌く異常 — どちらもユーザを光で呼ぶ状態ではない)
		if (fs === "working" || fs === "hitl_pending" || fs === "stuck") {
			return "conn-auto";
		}
		return "conn-dead"; // idle / completed = ほぼ消える
	}
	// fallback (旧 daemon = flow_state 未投影): engine 実在 (pid) heuristic
	if (lane.pid == null) return "conn-dead";
	return "conn-auto";
}
