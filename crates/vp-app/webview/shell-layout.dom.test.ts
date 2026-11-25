// @vitest-environment happy-dom
/**
 * `shell-layout.ts` の DOM への当て方（doc 87 §8 slice 1 — 額縁が grid になった後の形）。
 *
 * ## ⚠️ 守るもの
 *
 * - L の保存値は **peek の幅**。DOM には `#sidebar-root` の `--vp-peek-w` として当て、creo-ui の
 *   `CURail` が `peekWidth` で受ける（`#sidebar-root` は中身の幅）。inline の `width` を書くと、
 *   slim（CURail が peek を畳む）でも peek 分の幅が残る
 * - R の幅（%）は `#app-shell` の `--vp-rsb-pct` に当てる（grid の right track が読む）。
 *   grid item の % 幅は track に対して解決され auto の track では効かないので、`#right-sidebar` の
 *   inline `width` には書かない
 */
import { beforeEach, describe, expect, it } from "vitest";
import { applyShellLayout } from "./shell-layout";

beforeEach(() => {
	document.body.innerHTML =
		'<div id="app-shell"><div id="sidebar-root"></div><div id="shell-resizer-left"></div>' +
		'<div id="host"></div><div id="shell-resizer-right"></div><div id="right-sidebar"></div></div>';
});

const restore = (over: Partial<Parameters<typeof applyShellLayout>[0]> = {}) =>
	applyShellLayout({
		sidebar_width: 300,
		right_sidebar_width: 420,
		sidebar_form: "full",
		right_sidebar_open: true,
		right_sidebar_pct: 30,
		...over,
	});

describe("復元の当て方", () => {
	it("L は peek の幅を --vp-peek-w に書き、inline の width は書かない", () => {
		restore();
		const left = document.getElementById("sidebar-root")!;
		expect(left.style.getPropertyValue("--vp-peek-w")).toBe("300px");
		expect(left.style.width).toBe("");
	});

	it("slim でも peek の幅は保持したまま（full に戻した時の幅）、取っ手は隠す", () => {
		restore({ sidebar_form: "slim" });
		expect(document.getElementById("sidebar-root")!.style.getPropertyValue("--vp-peek-w")).toBe(
			"300px",
		);
		expect(document.getElementById("shell-resizer-left")!.style.display).toBe("none");
	});

	it("R の % は #app-shell の --vp-rsb-pct に書き、#right-sidebar の width には書かない", () => {
		restore();
		expect(document.getElementById("app-shell")!.style.getPropertyValue("--vp-rsb-pct")).toBe(
			"30%",
		);
		expect(document.getElementById("right-sidebar")!.style.width).toBe("");
	});
});
