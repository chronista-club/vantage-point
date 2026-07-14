/**
 * Lane 表示ヘルパーの単体テスト。
 *
 * 主眼は `isLaneAlive` — 「lane の生死を pid で測る」癖が Act II (chat lane は
 * engine-less が正常形) で崩れ、 context menu が Restart ではなく Respawn を出す
 * バグを生んだ。 その退行をここで塞ぐ。
 */
import { describe, expect, it } from "vitest";
import type { LaneInfo } from "../generated/LaneInfo";
import { isLaneAlive, laneConnector, laneCwdLabel } from "./lane";

/** 最小の LaneInfo。 テストが着目する field だけ上書きする。 */
function lane(over: Partial<LaneInfo> = {}): LaneInfo {
	return {
		id: "",
		address: { kind: "conductor", project: "vp" },
		kind: "conductor",
		state: "running",
		stand: "echoes",
		created_at: "2026-07-10T00:00:00Z",
		pid: null,
		cwd: "/tmp",
		console_mode: "tui",
		...over,
	} as LaneInfo;
}

describe("isLaneAlive", () => {
	it("tui lane は pid の有無で生死が決まる", () => {
		expect(isLaneAlive(lane({ console_mode: "tui", pid: 1234 }))).toBe(true);
		// PTY spawn 失敗 = Dead lane (dim 表示 + Respawn menu)
		expect(isLaneAlive(lane({ console_mode: "tui", pid: null }))).toBe(false);
	});

	it("chat lane は engine-less (pid=null) でも生きている", () => {
		// doc 33: chat engine は submit 契機の lazy spawn。 pid=null は正常形であって Dead ではない。
		expect(isLaneAlive(lane({ console_mode: "chat", pid: null }))).toBe(true);
	});

	it("chat lane の生死は engine の起動状態で揺れない", () => {
		// engine 起動中 (pid あり) と idle (pid なし) で判定が変わると、
		// 同じ lane の context menu が時間で Restart / Respawn に化ける。
		const idle = isLaneAlive(lane({ console_mode: "chat", pid: null }));
		const running = isLaneAlive(lane({ console_mode: "chat", pid: 4321 }));
		expect(idle).toBe(running);
	});
});

/** performer の最小 LaneInfo (laneConnector 用)。 */
function performer(over: Partial<LaneInfo> = {}): LaneInfo {
	return lane({
		kind: "performer",
		address: { kind: "performer", project: "vp", name: "feat" },
		name: "feat",
		...over,
	} as Partial<LaneInfo>);
}

describe("laneConnector (FSM 投影)", () => {
	it("conductor は spine の頭 (state を持たない)", () => {
		expect(laneConnector(lane(), false)).toBe("conn-conductor");
	});

	it("flow_state が一次 source: プロンプト待ちの TUI claude (pid あり + idle) は消える", () => {
		// 偽 WORKING の根治対象: dep symlink lane 等、 wire 活動が無いのに pid が生きている lane。
		expect(
			laneConnector(performer({ pid: 1234, flow_state: "idle" }), false),
		).toBe("conn-dead");
		expect(
			laneConnector(performer({ pid: 1234, flow_state: "completed" }), false),
		).toBe("conn-dead");
	});

	it("working / hitl_pending / stuck = flow が動いている (cyan)", () => {
		for (const fs of ["working", "hitl_pending", "stuck"]) {
			expect(laneConnector(performer({ flow_state: fs }), false)).toBe(
				"conn-auto",
			);
		}
	});

	it("awaiting_user = needs-you (盤面で唯一光る状態)", () => {
		// pid や他の signal に関係なく magenta diamond。
		expect(
			laneConnector(
				performer({ flow_state: "awaiting_user", pid: null }),
				false,
			),
		).toBe("conn-hitl");
	});

	it("OSC awaiting_input も needs-you (console HITL の別軸 signal)", () => {
		expect(laneConnector(performer({ flow_state: "working" }), true)).toBe(
			"conn-hitl",
		);
	});

	it("flow_state 欠落 (旧 daemon) は pid heuristic に fallback", () => {
		expect(
			laneConnector(performer({ flow_state: null, pid: 1234 }), false),
		).toBe("conn-auto");
		expect(
			laneConnector(performer({ flow_state: null, pid: null }), false),
		).toBe("conn-dead");
	});
});

describe("laneCwdLabel — 絶対 path は project が持ち、 lane は差分だけを名乗る", () => {
	const proj = "/Users/makoto/repos/vantage-point";

	it("conductor (cwd = project root) は空 = 語ることが無いので黙る", () => {
		expect(laneCwdLabel(proj, proj)).toBe("");
	});

	it("performer は project root 起点の相対 path", () => {
		expect(laneCwdLabel(`${proj}/.vp/lanes/act2`, proj)).toBe(".vp/lanes/act2");
	});

	it("project の外に居る lane は絶対 path を full で出す (= 驚きにはインクを払う)", () => {
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
