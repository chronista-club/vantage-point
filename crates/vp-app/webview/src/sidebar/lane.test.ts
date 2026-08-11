/**
 * Lane 表示ヘルパーの単体テスト。
 *
 * 主眼は `isLaneAlive` — 「lane の生死を pid で測る」癖が gui (chat lane は
 * engine-less が正常形) で崩れ、 context menu が Restart ではなく Respawn を出す
 * バグを生んだ。 その退行をここで塞ぐ。
 */
import { describe, expect, it } from "vitest";
import type { LaneInfo } from "../generated/LaneInfo";
import {
	laneAddressKey,
	laneShortcutNumber,
	shortcutNumberOf, isLaneAlive, laneConnector, laneCwdLabel } from "./lane";

/** 最小の LaneInfo。 テストが着目する field だけ上書きする。 */
function lane(over: Partial<LaneInfo> = {}): LaneInfo {
	return {
		id: "",
		address: { repo: "vp", name: "root" },
		state: "running",
		agent: "claude",
		created_at: "2026-07-10T00:00:00Z",
		pid: null,
		cwd: "/tmp",
		...over,
	} as LaneInfo;
}

/** root session の mode を sessions（registry snapshot）で表現する（doc 53 R1 — 旧
 *  console_mode field は退役。mode は wire の sessions だけが運ぶ）。 */
function withRootMode(mode: string): Partial<LaneInfo> {
	return {
		sessions: {
			root: 1,
			focused: 1,
			sessions: [{ key: 1, agent: "claude", mode }],
		},
	} as Partial<LaneInfo>;
}

describe("isLaneAlive", () => {
	it("tui lane は pid の有無で生死が決まる", () => {
		expect(isLaneAlive(lane({ ...withRootMode("tui"), pid: 1234 }))).toBe(true);
		// PTY spawn 失敗 = Dead lane (dim 表示 + Respawn menu)
		expect(isLaneAlive(lane({ ...withRootMode("tui"), pid: null }))).toBe(false);
	});

	it("sessions 欠落（boot 窓の placeholder）は tui 扱い = pid が生死を決める", () => {
		// doc 53 R1: 導出の fallback は旧 serde default（"tui"）と同値。
		expect(isLaneAlive(lane({ pid: null }))).toBe(false);
		expect(isLaneAlive(lane({ pid: 99 }))).toBe(true);
	});

	it("chat lane は engine-less (pid=null) でも生きている", () => {
		// doc 33: chat engine は submit 契機の lazy spawn。 pid=null は正常形であって Dead ではない。
		expect(isLaneAlive(lane({ ...withRootMode("gui"), pid: null }))).toBe(true);
	});

	it("chat lane の生死は engine の起動状態で揺れない", () => {
		// engine 起動中 (pid あり) と idle (pid なし) で判定が変わると、
		// 同じ lane の context menu が時間で Restart / Respawn に化ける。
		const idle = isLaneAlive(lane({ ...withRootMode("gui"), pid: null }));
		const running = isLaneAlive(lane({ ...withRootMode("gui"), pid: 4321 }));
		expect(idle).toBe(running);
	});

	it("root の mode で判定する — 非 root に chat が居ても root=tui なら pid が真実", () => {
		// doc 53 R1 の壊し方テスト（root=tui のまま非 root だけ chat の A6 正規構成）:
		// lane 単位の要約に落ちると「chat が 1 枚でもあれば生存」に化ける。
		const mixed = {
			sessions: {
				root: 1,
				focused: 2,
				sessions: [
					{ key: 1, agent: "claude", mode: "tui" },
					{ key: 2, agent: "claude", mode: "gui" },
				],
			},
		} as Partial<LaneInfo>;
		expect(isLaneAlive(lane({ ...mixed, pid: null }))).toBe(false);
	});
});

/** sub の最小 LaneInfo (laneConnector 用)。 */
function sub(over: Partial<LaneInfo> = {}): LaneInfo {
	return lane({
		address: { repo: "vp", name: "feat" },
		...over,
	} as Partial<LaneInfo>);
}

describe("laneConnector (FSM 投影)", () => {
	it("root は spine の頭 (state を持たない)", () => {
		expect(laneConnector(lane(), false)).toBe("conn-root");
	});

	it("flow_state が一次 source: プロンプト待ちの TUI claude (pid あり + idle) は消える", () => {
		// 偽 WORKING の根治対象: dep symlink lane 等、 wire 活動が無いのに pid が生きている lane。
		expect(
			laneConnector(sub({ pid: 1234, flow_state: "idle" }), false),
		).toBe("conn-dead");
		expect(
			laneConnector(sub({ pid: 1234, flow_state: "completed" }), false),
		).toBe("conn-dead");
	});

	it("working / hitl_pending / stuck = flow が動いている (cyan)", () => {
		for (const fs of ["working", "hitl_pending", "stuck"]) {
			expect(laneConnector(sub({ flow_state: fs }), false)).toBe(
				"conn-auto",
			);
		}
	});

	it("awaiting_user = needs-you (盤面で唯一光る状態)", () => {
		// pid や他の signal に関係なく magenta diamond。
		expect(
			laneConnector(
				sub({ flow_state: "awaiting_user", pid: null }),
				false,
			),
		).toBe("conn-hitl");
	});

	it("OSC awaiting_input も needs-you (console HITL の別軸 signal)", () => {
		expect(laneConnector(sub({ flow_state: "working" }), true)).toBe(
			"conn-hitl",
		);
	});

	it("flow_state 欠落 (旧 daemon) は pid heuristic に fallback", () => {
		expect(
			laneConnector(sub({ flow_state: null, pid: 1234 }), false),
		).toBe("conn-auto");
		expect(
			laneConnector(sub({ flow_state: null, pid: null }), false),
		).toBe("conn-dead");
	});
});

describe("laneCwdLabel — 絶対 path は repo が持ち、 lane は差分だけを名乗る", () => {
	const proj = "/Users/makoto/repos/vantage-point";

	it("root (cwd = repo root) は空 = 語ることが無いので黙る", () => {
		expect(laneCwdLabel(proj, proj)).toBe("");
	});

	it("sub は repo root 起点の相対 path", () => {
		expect(laneCwdLabel(`${proj}/.vp/lanes/act2`, proj)).toBe(".vp/lanes/act2");
	});

	it("repo の外に居る lane は絶対 path を full で出す (= 驚きにはインクを払う)", () => {
		expect(laneCwdLabel("/Users/makoto/work/other-clone", proj)).toBe(
			"~/work/other-clone",
		);
		// home 推定が効かない形はそのまま (実害なし — tooltip は常に完全な path)。
		expect(laneCwdLabel("/opt/work/proj", proj)).toBe("/opt/work/proj");
	});

	it("prefix が途中まで一致するだけの兄弟 dir を誤って相対化しない", () => {
		// `<proj>-old` は `<proj>/` 配下ではない。 startsWith の境界に `/` を要求する所以。
		expect(laneCwdLabel(`${proj}-old`, proj)).toBe("~/repos/vantage-point-old");
	});

	it("cwd が空なら空 (防御)", () => {
		expect(laneCwdLabel("", proj)).toBe("");
	});
});

/**
 * ⚠️ **番号の出どころは 1 本**（sidebar の `#N` badge と `⌘ hold l` の宛先）。
 *
 * 2 箇所で数えると「見えている番号と飛び先が違う」になる。特に並びは user が drag で
 * 決めた表示順（`resolveRepoOrder`）で、`sidebar.processes` の生順ではない。
 */
describe("laneShortcutNumber / shortcutNumberOf（root lane の Index）", () => {
	it("1 始まり。9 個目まで", () => {
		expect(laneShortcutNumber(0)).toBe(1);
		expect(laneShortcutNumber(8)).toBe(9);
	});

	it("⚠️ 10 個目以降は番号を持たない（1 打で選べない）", () => {
		expect(laneShortcutNumber(9)).toBeNull();
		expect(laneShortcutNumber(100)).toBeNull();
	});

	it("不正な位置は null（-1 = 並びに居ない repo）", () => {
		expect(laneShortcutNumber(-1)).toBeNull();
		expect(laneShortcutNumber(1.5)).toBeNull();
	});

	it("表示順の位置で引く", () => {
		const order = ["/a", "/b", "/c"];
		expect(shortcutNumberOf(order, "/a")).toBe(1);
		expect(shortcutNumberOf(order, "/c")).toBe(3);
	});

	it("⚠️ 並びに無い repo は null（生順で数えると番号がずれる側）", () => {
		expect(shortcutNumberOf(["/a", "/b"], "/zzz")).toBeNull();
	});
});

/**
 * ⚠️ **address は daemon が発行する。client は組み立てない。**
 *
 * 旧実装は `${repo}/${name}` を自分で組み、doc に「Rust の `key()` と byte-for-byte
 * 一致させる」と書く**手動の契約**だった。同じ写像が Rust 2 + TS 2 の計 4 箇所にあり、
 * Rust 内ですら食い違った記録がある。ここが組み立てに戻ると、daemon が形式を変えた日に
 * **active lane の強調が無音で消える**（比較は byte 一致なので）。
 */
describe("laneAddressKey（daemon 発行の key を運ぶ）", () => {
	it("daemon が発行した key をそのまま返す", () => {
		const l = lane({
			address: { repo: "vp", name: "foo", key: "vp/lane/foo" },
		} as Partial<LaneInfo>);
		expect(laneAddressKey(l)).toBe("vp/lane/foo");
	});

	it("⚠️ 組み立て直さない — repo/name と食い違う key でも key が勝つ", () => {
		// 組み立てに戻ると、この test が「vp/name」を返して落ちる。
		const l = lane({
			address: { repo: "vp", name: "name", key: "daemon/lane/issued" },
		} as Partial<LaneInfo>);
		expect(laneAddressKey(l)).toBe("daemon/lane/issued");
	});

	it("key が空 = 旧 daemon の payload。旧 2 分節へ縮退する", () => {
		// ⚠️ ここで新形を組み直さないのが要点 — 形式の知識を client に持たせない。
		// 旧形なので読み側の `parse_address` が救済する。
		const l = lane({
			address: { repo: "vp", name: "foo", key: "" },
		} as Partial<LaneInfo>);
		expect(laneAddressKey(l)).toBe("vp/foo");
	});
});
