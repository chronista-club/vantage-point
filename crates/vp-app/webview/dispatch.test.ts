/**
 * `dispatch.ts` — Rust → webview 押し込みの単一受け口。
 *
 * ここで守るのは **「押し込みが黙って落ちない」** という、この module の存在理由そのもの。
 * 旧来 `window.X && window.X.y(...)` は bundle 準備前なら no-op で「成功」し、VP はその穴を
 * feature ごとの pull（旧 `lanes:ensure-all` 等、退役済）で埋めてきた。保留と順序が壊れると
 * 同じ穴が戻る。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { PushEventEnvelope } from "./src/generated/Push";
import { installDispatch, openDispatch, type PushHandlers } from "./dispatch";

/** 呼ばれた動詞を到着順に記録する handler。 */
function recordingHandlers(): { calls: string[]; handlers: PushHandlers } {
	const calls: string[] = [];
	return {
		calls,
		handlers: {
			ensureLane: (lane, session, isRoot) =>
				calls.push(`ensure:${lane}#${session}:${isRoot}`),
			showLane: (lane, isChat) => calls.push(`show:${lane}:${isChat}`),
			removeLane: (lane) => calls.push(`removeLane:${lane}`),
			removeLaneSession: (lane, session) =>
				calls.push(`removeSession:${lane}#${session}`),
			deliverPaste: (text) => calls.push(`paste:${text}`),
			renderDevices: (devices) =>
				calls.push(`devices:${devices.length}`),
			handleBoardMessage: (message) =>
				calls.push(`board:${(message as { type?: string }).type}`),
			consoleSessionList: (lane, _payload) => calls.push(`roster:${lane}`),
			consoleEvent: (lane, _event, session) =>
				calls.push(`ev:${lane}#${session}`),
			consoleActApplied: (lane, session, act) =>
				calls.push(`act:${lane}#${session}:${act}`),
			consoleStands: (lane, _payload, req) =>
				calls.push(`stands:${lane}:${req}`),
		},
	};
}

const dispatch = (msg: PushEventEnvelope): void => {
	(window as unknown as { vpDispatch: (m: PushEventEnvelope) => void }).vpDispatch(
		msg,
	);
};

describe("dispatch", () => {
	beforeEach(() => {
		// vitest の environment は node（`vitest.config.ts`）。dispatch.ts が触るのは
		// `window.vpDispatch` だけなので、jsdom を持ち込まず最小の stub で足りる。
		(globalThis as unknown as { window: unknown }).window = globalThis;
		vi.resetModules();
	});

	it("install 前に届いた push を到着順で流す", async () => {
		// module state（pending / handlers）は module 単位なので毎回読み直す。
		const mod = await import("./dispatch");
		mod.openDispatch();

		dispatch({ t: "term:ensure_lane", lane: "vp/root", session: 1, is_root: true });
		dispatch({ t: "term:show_lane", lane: "vp/root", is_chat: false });
		dispatch({ t: "term:paste", text: "hi" });

		const { calls, handlers } = recordingHandlers();
		// install 前は 1 件も実行されていない（= 落としてもいない）。
		expect(calls).toEqual([]);

		mod.installDispatch(handlers);
		expect(calls).toEqual([
			"ensure:vp/root#1:true",
			"show:vp/root:false",
			"paste:hi",
		]);
	});

	it("install 後は保留を経由せず直通で呼ばれる", async () => {
		const mod = await import("./dispatch");
		mod.openDispatch();
		const { calls, handlers } = recordingHandlers();
		mod.installDispatch(handlers);

		dispatch({ t: "term:remove_lane", lane: "vp/root" });
		// 直通なので install を待たずその場で実行される。
		expect(calls).toEqual(["removeLane:vp/root"]);

		dispatch({ t: "term:remove_session", lane: "vp/root", session: 7 });
		expect(calls).toEqual(["removeLane:vp/root", "removeSession:vp/root#7"]);
	});

	it("保留分と直通分が 1 本の順序に並ぶ", async () => {
		// 保留を先に流してから直通へ切り替わること（= install 境界で順序が入れ替わらない）。
		const mod = await import("./dispatch");
		mod.openDispatch();
		dispatch({ t: "term:paste", text: "queued" });

		const { calls, handlers } = recordingHandlers();
		mod.installDispatch(handlers);
		dispatch({ t: "term:paste", text: "live" });

		expect(calls).toEqual(["paste:queued", "paste:live"]);
	});

	it("lane 省略は null として渡る（lane 未選択）", async () => {
		// schema の `optional` field。旧実装の `window.showLane(null, false)` に対応する。
		const mod = await import("./dispatch");
		mod.openDispatch();
		const { calls, handlers } = recordingHandlers();
		mod.installDispatch(handlers);

		dispatch({ t: "term:show_lane", is_chat: false });
		expect(calls).toEqual(["show:null:false"]);
	});

	it("catch-up pull で埋めていた 2 面も保留される", async () => {
		// 旧 `bastet:devices_fetch` / `board:demand` は「boot 窓で押し込みが落ちる」ことへの
		// feature 別 pull だった（両方とも `ready` の replay に畳んで退役）。install 前の窓は
		// **この保留が引き継ぐ**ので、ここが落ちると畳んだぶんの穴がそのまま開く。
		const mod = await import("./dispatch");
		mod.openDispatch();

		dispatch({ t: "devices:render", devices: [{}, {}] });
		dispatch({ t: "board:message", message: { type: "show" } });

		const { calls, handlers } = recordingHandlers();
		expect(calls).toEqual([]);

		mod.installDispatch(handlers);
		expect(calls).toEqual(["devices:2", "board:show"]);
	});

	it("optional field の省略は null として渡る（stands の相関 id）", async () => {
		// `req` 省略 = 「応答を誰も拾わない」。旧実装は JS へ `null` を直書きしていた。
		const mod = await import("./dispatch");
		mod.openDispatch();
		const { calls, handlers } = recordingHandlers();
		mod.installDispatch(handlers);

		dispatch({ t: "console:stands", lane: "vp/root", payload: {} });
		dispatch({ t: "console:stands", lane: "vp/root", payload: {}, req: "r1" });
		expect(calls).toEqual(["stands:vp/root:null", "stands:vp/root:r1"]);
	});

	it("install 前に届いた分は二重に流れない", async () => {
		// `pending` を null にしてから flush するので、flush 中に届いた分も直通に乗る。
		const mod = await import("./dispatch");
		mod.openDispatch();
		dispatch({ t: "term:paste", text: "once" });

		const { calls, handlers } = recordingHandlers();
		mod.installDispatch(handlers);
		expect(calls).toEqual(["paste:once"]);

		// 2 度目の install でも保留は空なので、過去分が再生されない。
		mod.installDispatch(handlers);
		expect(calls).toEqual(["paste:once"]);
	});
});
