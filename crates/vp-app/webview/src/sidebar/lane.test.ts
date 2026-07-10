/**
 * Lane 表示ヘルパーの単体テスト。
 *
 * 主眼は `isLaneAlive` — 「lane の生死を pid で測る」癖が Act II (chat lane は
 * engine-less が正常形) で崩れ、 context menu が Restart ではなく Respawn を出す
 * バグを生んだ。 その退行をここで塞ぐ。
 */
import { describe, expect, it } from "vitest";
import type { LaneInfo } from "../generated/LaneInfo";
import { isLaneAlive } from "./lane";

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
