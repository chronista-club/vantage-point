/**
 * lane-panes（doc 49 LE-P4 PR2 → doc 51 §1 A1）の検証 — 純 calculation と lane scope の意味論。
 *
 * 旧 pane-shell.test.ts の後継。doc 51 §1 A1（tiling 既定 + 帯撤去）で固定し直した要件:
 *   既定で並ぶ = initialLaneLayout + syncPaneColumns の可視入場（enterShare）/
 *   roster は mode × root で term / chat を排他にする（同じ session を 2 枚にしない）/
 *   構成は lane ごと = engine scope。
 * DOM 反映（installLanePanes の render）は node 環境のため対象外（薄い action 層）。
 */

import { resolve, visibleIds } from "@chronista-club/creo-ui-layout";
import { describe, expect, it } from "vitest";
import {
	boardLaneKeyOf,
	chatHostId,
	enterShare,
	initialLaneLayout,
	lanePaneRefs,
	laneScope,
	newPaneChoices,
	renamePane,
	sessionOfHostId,
	hostIdForMode,
	syncPaneColumns,
	termHostId,
} from "./lane-panes";
import { layoutEngine } from "./layout-host";

/** A6: term host も session 単位（旧 `lane-host` = root 専用の静的 host は退役）。 */
const TERM = termHostId(1);
/** doc 50 P1: 固定の Chat pane は退役 — session ↔ Pane 1:1。テストは session 1 の pane で行う */
const CHAT = chatHostId(1);

describe("initialLaneLayout（A6: boot は空 — host の身元が session になった）", () => {
	it("pane ゼロ（session 一覧の到着まで id を作れない = 覆う host も存在しない）", () => {
		const r = resolve(initialLaneLayout());
		expect(Object.keys(r)).toEqual([]);
		// 旧実装は静的 `lane-host` を 1 枚全面で置いていた（root 専用 host = role ベース命名の
		// 産物）。A6 で root も session suffix になったため boot 時点では id が決まらない。
		expect(r["lane-host"]).toBeUndefined();
	});
});

describe("lanePaneRefs（roster = session 一覧 × 各 mode、doc 50 §4.6 A6）", () => {
	it("mode ごとに kind が決まる（root=tui は Console、非 root=chat は chat pane）", () => {
		expect(
			lanePaneRefs([
				{ key: 1, agent: "claude", root: true, mode: "tui" },
				{ key: 3, agent: "codex", mode: "gui" },
			]),
		).toEqual([
			{ id: "term-session-1", label: "cc#1", session: 1, kind: "term" },
			{ id: "chat-session-3", label: "cdx#3", session: 3, kind: "chat" },
		]);
	});

	it("全 session が chat（root も chat = 旧 mode==chat 相当）", () => {
		expect(
			lanePaneRefs([
				{ key: 1, agent: "claude", root: true, mode: "gui" },
				{ key: 3, agent: "codex", mode: "gui" },
			]),
		).toEqual([
			{ id: "chat-session-1", label: "cc#1", session: 1, kind: "chat" },
			{ id: "chat-session-3", label: "cdx#3", session: 3, kind: "chat" },
		]);
	});

	it("A6 の核心: 非 root も term になれる（term が 2 枚並ぶ = 旧実装では不可能だった形）", () => {
		expect(
			lanePaneRefs([
				{ key: 1, agent: "claude", root: true, mode: "tui" },
				{ key: 2, agent: "claude", mode: "tui" },
				{ key: 3, agent: "codex", mode: "gui" },
			]).map((p) => p.id),
		).toEqual(["term-session-1", "term-session-2", "chat-session-3"]);
	});

	it("mode 欠落（旧 SP）は tui に倒す（従来の既定 = tui）", () => {
		expect(
			lanePaneRefs([{ key: 1, agent: "claude", root: true }]).map((p) => p.id),
		).toEqual([TERM]);
	});

	it("session なし = 空 roster（board 無しなら pane ゼロ）", () => {
		expect(lanePaneRefs([]).map((p) => p.id)).toEqual([]);
	});

	it("board 非空: mode を問わず末尾に board pane が並ぶ（doc 52 §10 wave 0）", () => {
		expect(
			lanePaneRefs([{ key: 1, agent: "claude", root: true, mode: "tui" }], true).map(
				(p) => p.id,
			),
		).toEqual([TERM, "lane-board"]);
		expect(
			lanePaneRefs([{ key: 1, agent: "claude", root: true, mode: "gui" }], true).map(
				(p) => p.id,
			),
		).toEqual(["chat-session-1", "lane-board"]);
	});

	it("board 空（既定）は board pane を出さない", () => {
		const s = [{ key: 1, agent: "claude", root: true, mode: "tui" as const }];
		expect(lanePaneRefs(s, false).map((p) => p.id)).toEqual([TERM]);
		expect(lanePaneRefs(s).map((p) => p.id)).toEqual([TERM]);
	});

	it("kind が pane の種類を運ぶ（session の有無では判別できない）", () => {
		// ⚠️ A6 以前は「session を持つ = chat pane」で判別できたが、term も session を持つ
		// ようになったので壊れた。kind は「host を誰が作るか」（term = World A / chat =
		// SolidJS）を決める分岐なので、取り違えると xterm の host を消しかねない。
		const refs = lanePaneRefs(
			[
				{ key: 1, agent: "claude", root: true, mode: "tui" },
				{ key: 3, agent: "codex", mode: "gui" },
			],
			true,
		);
		expect(refs.map((p) => p.kind)).toEqual(["term", "chat", "board"]);
		// term と chat の両方が session を持つ = session は kind の代用にならない。
		expect(refs.filter((p) => p.session !== undefined).map((p) => p.kind)).toEqual([
			"term",
			"chat",
		]);
	});
});

describe("renamePane（in-place 変身 — doc 50 §4.6 A6 ②）", () => {
	/** 3 列・share が偏った layout（真ん中の pane を変身させる）。 */
	const base = () => ({
		structure: {
			columns: [
				{ panes: ["a"] },
				{ panes: ["chat-session-16"] },
				{ panes: ["term-session-21"] },
			],
		},
		attention: { a: 0.2, "chat-session-16": 0.5, "term-session-21": 0.3 },
	});

	it("列の位置と share を保ったまま id だけ差し替える", () => {
		const out = renamePane(base(), "chat-session-16", "term-session-16");
		// 位置: 真ん中のまま（右端に飛ばない = 実機で踏んだ症状の固定）
		expect(out.structure.columns.map((c) => c.panes)).toEqual([
			["a"],
			["term-session-16"],
			["term-session-21"],
		]);
		// share: 引き継ぐ（enterShare で作り直されない）
		expect(out.attention["term-session-16"]).toBe(0.5);
		expect(out.attention["chat-session-16"]).toBeUndefined();
		// 他の pane は無傷
		expect(out.attention.a).toBe(0.2);
		expect(out.attention["term-session-21"]).toBe(0.3);
	});

	it("居ない id / 同一 id は layout を触らない（冪等）", () => {
		const b = base();
		expect(renamePane(b, "not-there", "x")).toBe(b);
		expect(renamePane(b, "a", "a")).toBe(b);
	});

	it("往復すると元に戻る（chat→tui→chat で位置が漂わない）", () => {
		const once = renamePane(base(), "chat-session-16", "term-session-16");
		const back = renamePane(once, "term-session-16", "chat-session-16");
		expect(back).toEqual(base());
	});
});

describe("boardLaneKeyOf（address → board flat key の写像。'vp:board-presence' 突合の seam）", () => {
	it("root / lead は 'conductor'（board-handler の None→'conductor' と一致）", () => {
		expect(boardLaneKeyOf("vantage-point/root")).toBe("conductor");
		expect(boardLaneKeyOf("vantage-point/lead")).toBe("conductor");
	});
	it("performer は名前を返す", () => {
		expect(boardLaneKeyOf("vantage-point/performer/foo")).toBe("foo");
		expect(boardLaneKeyOf("vantage-point/wing/bar")).toBe("bar");
	});
	it("未知形は 'conductor' に倒す（board は最悪 conductor board に出す側）", () => {
		expect(boardLaneKeyOf("weird")).toBe("conductor");
	});
});

describe("chatHostId / sessionOfHostId（往復）", () => {
	it("往復が恒等", () => {
		expect(sessionOfHostId(chatHostId(7))).toBe(7);
	});
	it("term pane / 未知 id は null", () => {
		expect(sessionOfHostId(termHostId(1))).toBeNull();
		expect(sessionOfHostId("chat-session-x")).toBeNull();
	});
});

describe("hostIdForMode（mode → host id の唯一の写像）", () => {
	it("chat は chat host、それ以外（tui / 不明）は term host", () => {
		expect(hostIdForMode(5, "gui")).toBe(chatHostId(5));
		expect(hostIdForMode(5, "tui")).toBe(termHostId(5));
		// 旧 SP wire（mode 欠落）は tui に倒す = 従来の既定。
		expect(hostIdForMode(5, undefined)).toBe(termHostId(5));
	});

	it("**tui を chat に倒さない**（focus 側 2 箇所が chat 決め打ちだった退行の固定）", () => {
		// team-b 8 回目: `applyLaneView` と pendingFocus の fallback が mode を読まず常に
		// chat host を指していた → term が focused の lane で pane が見つからず、focus ring が
		// 挿入順の先頭に誤爆した。写像を 1 本にしたので、ここが守れば全 call site が守れる。
		for (const mode of ["tui", undefined] as const) {
			expect(hostIdForMode(9, mode)).not.toBe(chatHostId(9));
		}
	});
});

describe("syncPaneColumns（layout 列 ↔ roster の同期、tiling 既定）", () => {
	it("新しい pane は右端に列 append、**可視で入場**（enterShare = 可視 raw 平均）", () => {
		const l = syncPaneColumns(initialLaneLayout(), [TERM, CHAT]);
		expect(l.structure.columns.map((c) => c.panes)).toEqual([[TERM], [CHAT]]);
		// tiling 既定: 畳まれて生まれない。Console(1) と等しい share で並ぶ = 等分
		expect(l.attention[CHAT]).toBe(1);
		expect(l.attention[TERM]).toBe(1);
		const r = resolve(l);
		expect(r[CHAT]?.rect.w).toBeCloseTo(0.5);
	});

	it("消えた pane の列は除去される（既存の attention は保たれる）", () => {
		const grown = syncPaneColumns(initialLaneLayout(), [TERM, CHAT, chatHostId(2)]);
		const shrunk = syncPaneColumns(grown, [TERM, CHAT]);
		expect(shrunk.structure.columns.map((c) => c.panes)).toEqual([[TERM], [CHAT]]);
		expect(chatHostId(2) in shrunk.attention).toBe(false);
	});

	it("冪等（sync → sync は不動点）", () => {
		const once = syncPaneColumns(initialLaneLayout(), [TERM, CHAT]);
		expect(syncPaneColumns(once, [TERM, CHAT])).toEqual(once);
	});

	it("再入場も可視（roster から外れた pane が戻る = attention 記録は消えている → enterShare）", () => {
		// mode 切替で Console が roster を出て（chat）、戻る（tui）往復の縮図
		const noTerm = syncPaneColumns(initialLaneLayout(), [CHAT]);
		expect(TERM in noTerm.attention).toBe(false);
		const back = syncPaneColumns(noTerm, [TERM, CHAT]);
		expect(back.attention[TERM]).toBeGreaterThan(0);
	});

	it("term pane は ids に含める限り常在（refs 側が必ず先頭に置く前提の裏）", () => {
		const l = syncPaneColumns(initialLaneLayout(), [TERM]);
		expect(l.structure.columns).toEqual([{ panes: [TERM] }]);
	});
});

describe("enterShare（入場 share = 可視 raw 平均、creo-ui-layout admit と同じ規則）", () => {
	it("可視 pane が居なければ 1", () => {
		expect(enterShare({ structure: { columns: [] }, attention: {} })).toBe(1);
	});
	it("可視 pane の raw 平均（非可視 0 は数えない）", () => {
		expect(
			enterShare({
				structure: { columns: [{ panes: ["a", "b", "c"] }] },
				attention: { a: 1, b: 0.5, c: 0 },
			}),
		).toBeCloseTo(0.75);
	});
});

describe("lane scope（doc 47 §3: 構成は lane ごと）", () => {
	it("scope key は lane ごとに分かれ、配置は独立に動く", () => {
		const a = laneScope("proj/root");
		const b = laneScope("proj/performer/w1");
		expect(a).not.toBe(b);

		layoutEngine.update(a, () => syncPaneColumns(initialLaneLayout(), [TERM, CHAT]));
		layoutEngine.update(b, () => syncPaneColumns(initialLaneLayout(), [TERM, CHAT]));
		// a だけ session 1 が閉じた（roster から消えた）
		layoutEngine.update(a, (l) => syncPaneColumns(l, [TERM]));

		expect(visibleIds(layoutEngine.current(a))).toEqual([TERM]);
		expect(visibleIds(layoutEngine.current(b)).sort()).toEqual([CHAT, TERM].sort());
	});
});

describe("newPaneChoices（doc 46 P2 要件 4: Engine × Mode）", () => {
	it("chat 非対応 engine は gui を出さない（行き止まりを作らない）", () => {
		const choices = newPaneChoices([
			{ name: "claude", label: "Claude", chat_capable: true },
			{ name: "shell", label: "Shell", chat_capable: false },
		]);
		expect(choices).toEqual([
			{ engine: "claude", engineLabel: "Claude", mode: "tui" },
			{ engine: "claude", engineLabel: "Claude", mode: "gui" },
			// shell は chat host を持たないので tui だけ
			{ engine: "shell", engineLabel: "Shell", mode: "tui" },
		]);
	});

	it("label 未設定なら agent 名をそのまま出す", () => {
		const choices = newPaneChoices([{ name: "codex", chat_capable: true }]);
		expect(choices.map((c) => c.engineLabel)).toEqual(["codex", "codex"]);
	});

	it("name が空の entry は捨てる（送っても解決できない）", () => {
		expect(newPaneChoices([{ name: "", chat_capable: true }])).toEqual([]);
	});

	it("chat_capable 未指定は非対応扱い（不明なら行き止まりを作らない側に倒す）", () => {
		const choices = newPaneChoices([{ name: "unknown" }]);
		expect(choices.map((c) => c.mode)).toEqual(["tui"]);
	});
});
