// @vitest-environment happy-dom
// ⚠️ happy-dom なのは route.ts が `@chronista-club/creo-ui/shells` の `selectRailId` を借りるため。
// shells の index は module 評価時に Solid の delegateEvents（window 参照）を走らせるので、
// 純関数だけ使う場合でも window が要る。
/**
 * pseudo-route（doc 87 §3）— 左 rail の選択と center の Scene を 1 本の文字列から導く。
 *
 * ## ⚠️ ここで守るのは「選択の SSOT が 1 本」であること
 *
 * VP に router は無い。かわりに `/lanes/<addr>` / `/actions` / `/settings/<section>` の文字列を
 * 1 本だけ持ち、rail の選択（aria-current）も「設定を開いているか」もここから導く。
 * 旧形は `SettingsPanel` の `visible` signal と sidebar の描画分岐が別々に「今どこか」を持っていた
 * — 2 本あると片方だけ進む状態（設定を開いたのに rail が Lanes のまま等）が作れてしまう。
 */
import { describe, expect, it } from "vitest";
import {
	DEFAULT_ROUTE,
	SHELL_RAILS,
	closeSettings,
	initialRouteState,
	laneOf,
	laneRoute,
	navigate,
	normalizeRoute,
	onWorkspaceSelected,
	railClick,
	railOf,
	restoreAfterTransient,
	sceneOf,
	sectionOf,
	settingsRoute,
} from "./route";

describe("rail の導出（route = 唯一の SSOT、PL-4）", () => {
	it("各 rail の主 route はその rail を選ぶ", () => {
		expect(railOf("/lanes")).toBe("lanes");
		expect(railOf("/actions")).toBe("actions");
		expect(railOf("/settings")).toBe("settings");
	});

	it("下位の path も同じ rail（最長 prefix 一致）", () => {
		expect(railOf("/lanes/vantage-point/lane/shell-3x3-slice1")).toBe("lanes");
		expect(railOf("/settings/updates")).toBe("settings");
	});

	it("⚠️ segment 境界で判定する（`/lanesx` は Lanes ではない）", () => {
		expect(railOf("/lanesx")).toBeUndefined();
		expect(railOf("/settingsfoo")).toBeUndefined();
	});

	it("どの rail にも当たらなければ未選択", () => {
		expect(railOf("/")).toBeUndefined();
		expect(railOf("")).toBeUndefined();
	});

	it("registry は Lanes / Actions / Settings の 3 本、Settings だけが下起点", () => {
		expect(SHELL_RAILS.map((r) => r.id)).toEqual(["lanes", "actions", "settings"]);
		expect(SHELL_RAILS.filter((r) => r.anchor === "bottom").map((r) => r.id)).toEqual([
			"settings",
		]);
	});
});

describe("Scene の導出（開発 ⇄ 設定）", () => {
	it("Settings rail の下は設定、それ以外は開発", () => {
		expect(sceneOf("/settings")).toBe("settings");
		expect(sceneOf("/settings/updates")).toBe("settings");
		expect(sceneOf("/lanes")).toBe("dev");
		expect(sceneOf("/lanes/vp/root")).toBe("dev");
		expect(sceneOf("/actions")).toBe("dev");
	});

	it("未知の route は開発に倒す（設定は明示して入る場所）", () => {
		expect(sceneOf("/nowhere")).toBe("dev");
	});
});

describe("route の組み立てと分解", () => {
	it("lane address は `/` を含んだまま `/lanes/` の後ろに載る", () => {
		expect(laneRoute("vantage-point/lane/shell-3x3-slice1")).toBe(
			"/lanes/vantage-point/lane/shell-3x3-slice1",
		);
		expect(laneOf("/lanes/vantage-point/lane/shell-3x3-slice1")).toBe(
			"vantage-point/lane/shell-3x3-slice1",
		);
	});

	it("lane 未選択は `/lanes` だけ", () => {
		expect(laneRoute(null)).toBe("/lanes");
		expect(laneRoute(undefined)).toBe("/lanes");
		expect(laneRoute("")).toBe("/lanes");
		expect(laneOf("/lanes")).toBeUndefined();
	});

	it("lanes 以外の route から lane は取れない", () => {
		expect(laneOf("/actions")).toBeUndefined();
		expect(laneOf("/settings/vp/root")).toBeUndefined();
	});

	it("設定の section", () => {
		expect(settingsRoute()).toBe("/settings");
		expect(settingsRoute("updates")).toBe("/settings/updates");
		expect(sectionOf("/settings/updates")).toBe("updates");
		expect(sectionOf("/settings")).toBeUndefined();
		expect(sectionOf("/lanes/x")).toBeUndefined();
	});

	it("未知の route は既定（Lanes）に正規化する", () => {
		expect(DEFAULT_ROUTE).toBe("/lanes");
		expect(normalizeRoute("/nowhere")).toBe("/lanes");
		expect(normalizeRoute("")).toBe("/lanes");
		expect(normalizeRoute("/actions")).toBe("/actions");
		expect(normalizeRoute("/settings/updates")).toBe("/settings/updates");
	});
});

describe("遷移 — 設定を解くと前の開発に戻る（doc 87 §3）", () => {
	it("初期状態は Lanes の開発 Scene", () => {
		const s = initialRouteState();
		expect(s.route).toBe("/lanes");
		expect(s.devReturn).toBe("/lanes");
	});

	it("開発の中の移動は戻り先も一緒に進む", () => {
		const s = navigate(initialRouteState(), "/actions");
		expect(s).toEqual({ route: "/actions", devReturn: "/actions" });
	});

	it("設定に入っても戻り先は直前の開発 route のまま", () => {
		const dev = navigate(initialRouteState(), "/lanes/vp/root");
		const inSettings = navigate(dev, "/settings");
		expect(inSettings.route).toBe("/settings");
		expect(inSettings.devReturn).toBe("/lanes/vp/root");
		// 設定の中で section を移っても戻り先は動かない
		const section = navigate(inSettings, "/settings/updates");
		expect(section.devReturn).toBe("/lanes/vp/root");
	});

	it("設定を閉じると直前の開発 route に戻る", () => {
		const s = closeSettings(navigate(navigate(initialRouteState(), "/actions"), "/settings"));
		expect(s).toEqual({ route: "/actions", devReturn: "/actions" });
	});

	it("開発中に閉じても何も変わらない", () => {
		const dev = navigate(initialRouteState(), "/lanes/vp/root");
		expect(closeSettings(dev)).toBe(dev);
	});

	it("未知の route への navigate は既定に正規化される", () => {
		expect(navigate(initialRouteState(), "/nowhere").route).toBe("/lanes");
	});
});

describe("作業の選択（lane / component）が route に及ぼすもの", () => {
	it("Lanes にいれば選んだ lane が route に載る", () => {
		const s = onWorkspaceSelected(initialRouteState(), "vp/sub/x");
		expect(s).toEqual({ route: "/lanes/vp/sub/x", devReturn: "/lanes/vp/sub/x" });
	});

	it("⚠️ 設定中に作業を選んだら設定を抜けてその lane へ（旧 SettingsPanel の dismiss と同じ）", () => {
		const inSettings = navigate(initialRouteState(), "/settings");
		const s = onWorkspaceSelected(inSettings, "vp/root");
		expect(s.route).toBe("/lanes/vp/root");
		expect(sceneOf(s.route)).toBe("dev");
	});

	it("設定中に lane 以外（devices 等）を選んだら、直前の開発 route へ戻る", () => {
		const inSettings = navigate(navigate(initialRouteState(), "/actions"), "/settings");
		const s = onWorkspaceSelected(inSettings, null);
		expect(s.route).toBe("/actions");
	});

	it("⚠️ 設定中に lane 以外を選んで Lanes に戻る時は、古い lane を route に残さない", () => {
		const inSettings = navigate(navigate(initialRouteState(), "/lanes/vp/root"), "/settings");
		const s = onWorkspaceSelected(inSettings, null);
		expect(s.route).toBe("/lanes");
	});

	it("Actions にいる間は lane を選んでも Actions のまま（peek を奪わない）", () => {
		const onActions = navigate(initialRouteState(), "/actions");
		expect(onActions).toBe(onWorkspaceSelected(onActions, "vp/root"));
	});

	it("同じ選択の配り直しでは状態を作り直さない（identity が保たれる）", () => {
		const s = onWorkspaceSelected(initialRouteState(), "vp/root");
		expect(onWorkspaceSelected(s, "vp/root")).toBe(s);
	});
});

describe("rail の icon click（route と peek の開閉）", () => {
	const lanes = navigate(initialRouteState(), "/lanes/vp/root");

	it("別の rail を押す → その rail へ移り、peek を開く", () => {
		const r = railClick(lanes, "slim", "actions", "vp/root");
		expect(r.state.route).toBe("/actions");
		expect(r.form).toBe("full");
	});

	it("選択中の rail を full で押す → peek を畳む（route は動かない）", () => {
		const r = railClick(lanes, "full", "lanes", "vp/root");
		expect(r.state).toBe(lanes);
		expect(r.form).toBe("slim");
	});

	it("選択中の rail を slim で押す → peek を開く", () => {
		const r = railClick(lanes, "slim", "lanes", "vp/root");
		expect(r.state).toBe(lanes);
		expect(r.form).toBe("full");
	});

	it("Lanes rail に移るときは今の active lane を route に載せる", () => {
		const onActions = navigate(initialRouteState(), "/actions");
		const r = railClick(onActions, "full", "lanes", "vp/sub/x");
		expect(r.state.route).toBe("/lanes/vp/sub/x");
	});

	it("Settings の歯車 → 設定 Scene。戻り先は押す前の開発 route", () => {
		const r = railClick(lanes, "full", "settings", "vp/root");
		expect(sceneOf(r.state.route)).toBe("settings");
		expect(r.state.devReturn).toBe("/lanes/vp/root");
		expect(r.form).toBe("full");
	});

	it("設定中に Actions を押す → 設定を抜けて Actions（Scene は開発）", () => {
		const inSettings = railClick(lanes, "full", "settings", "vp/root").state;
		const r = railClick(inSettings, "full", "actions", "vp/root");
		expect(r.state.route).toBe("/actions");
		expect(sceneOf(r.state.route)).toBe("dev");
	});
});

describe("一時遷移（`b` の capture）からの戻り", () => {
	const lanes = navigate(initialRouteState(), "/lanes/vp/root");
	const placed = navigate(lanes, "/actions");

	it("capture が置いた route のままなら、入る前に戻す", () => {
		expect(restoreAfterTransient(placed, lanes, "/actions", null, false)).toBe(lanes);
	});

	it("⚠️ その間に user が別の場所へ移っていたら、戻さない（後から打ち消さない）", () => {
		const moved = navigate(placed, "/settings");
		expect(restoreAfterTransient(moved, lanes, "/actions", null, false)).toBe(moved);
	});

	it("⚠️ その間に作業の選択が変わっていたら、戻り先にも同じ規則を当てる", () => {
		// Lanes から入った → 選んだ lane が route に載る
		expect(restoreAfterTransient(placed, lanes, "/actions", "vp/sub/x", true).route).toBe(
			"/lanes/vp/sub/x",
		);
		// 設定から入った → 作業を選んだので設定は開き直さない
		const inSettings = navigate(lanes, "/settings");
		const fromSettings = navigate(inSettings, "/actions");
		const back = restoreAfterTransient(fromSettings, inSettings, "/actions", "vp/sub/x", true);
		expect(sceneOf(back.route)).toBe("dev");
		expect(back.route).toBe("/lanes/vp/sub/x");
	});
});
