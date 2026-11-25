/**
 * lead 行（RepoAccordion）の折りたたみ — 開閉 caret と、畳んでいる間の見え方（mem_1ChNtdQKHrSB78Ppek2etg）。
 *
 * mako 2026-11-25 実機:「折り畳み、、、実装されてたね。見やすくできたらいいなぁ。」
 * 旧形は toggle の中身が活動ドット（ResponsePoint）だけで、押せることも開閉の状態も見えなかった。
 *
 * ここで固定する契約:
 * - toggle（`.vp-proj-toggle`）は caret（`ph:caret-right`）を持ち、`aria-expanded="true"` で 90° 回る
 * - caret は lead 行の最左（`.vp-lane-disclosure`）。活動ドットは toggle の外、従来の `.vp-lane-leading`
 * - 畳む中身が無い（lead 1 本だけ）時は toggle が `.is-empty`（控えめ）
 * - 畳んでいる間は `.vp-proj.folded`、lane 数の chip に sub の状態ドットと内訳の title
 * - 既存: `process:toggle` IPC / `aria-expanded` / `aria-label` / toggle の click は行の選択にならない
 */
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { build } from "esbuild";
import { solidPlugin } from "esbuild-plugin-solid";
import { Window } from "happy-dom";
import { fileURLToPath } from "node:url";

type Setup = {
	expanded: boolean;
	subs?: number;
	state?: string;
	rootExtraSessions?: number;
};
type Probe = Window & {
	sent: { t: string; path?: string; expanded?: boolean }[];
	setup(s: Setup): void;
};

let bundle: string;
const windows: Window[] = [];

beforeAll(async () => {
	const result = await build({
		stdin: {
			contents: `
        import { render } from 'solid-js/web';
        import { createComponent } from 'solid-js';
        import { RepoAccordion } from './RepoAccordion';
        import { SHELL_CSS } from './Shell';
        import { applySidebarState, emptyState, sidebar } from './store';
        const style = document.createElement('style');
        style.textContent = SHELL_CSS;
        document.head.append(style);
        window.sent = [];
        window.ipc = { postMessage: m => window.sent.push(JSON.parse(m)) };
        const session = key => ({ key, agent: 'claude', mode: 'tui' });
        const lane = (name, extra = 0) => ({
          address: { repo: 'vp', name, key: 'vp/lane/' + name },
          state: 'running', agent: 'claude', pid: 1, flow_state: null,
          sessions: { root: 1, focused: 1,
            sessions: [session(1), ...Array.from({ length: extra }, (_, i) => session(i + 2))] },
        });
        window.setup = ({ expanded, subs = 0, state = 'running', rootExtraSessions = 0 }) =>
          applySidebarState({ ...emptyState(),
            processes: [{ path: '/vp', name: 'vantage-point', state, expanded, port: 33000 }],
            lanes_by_repo: { '/vp': [
              lane('lead', rootExtraSessions),
              ...Array.from({ length: subs }, (_, i) => lane('sub-' + i)),
            ] },
            lane_sub_state: { '/vp': 'ready' },
          });
        window.setup({ expanded: true });
        render(() => createComponent(RepoAccordion, { get proc() { return sidebar.processes[0]; } }),
          document.getElementById('root'));
      `,
			resolveDir: fileURLToPath(new URL(".", import.meta.url)),
			loader: "tsx",
		},
		bundle: true,
		write: false,
		format: "iife",
		platform: "browser",
		preserveSymlinks: true,
		plugins: [solidPlugin()],
		define: { "process.env.NODE_ENV": '"production"' },
	});
	bundle = result.outputFiles[0].text;
});

function fixture(s: Setup): Probe {
	const win = new Window({
		settings: {
			enableJavaScriptEvaluation: true,
			suppressInsecureJavaScriptEnvironmentWarning: true,
		},
	}) as unknown as Probe;
	windows.push(win);
	win.document.body.innerHTML = '<div id="sidebar-root"><div id="root"></div></div>';
	win.eval(bundle);
	win.setup(s);
	return win;
}

afterEach(async () => {
	for (const win of windows.splice(0)) await win.happyDOM.abort();
});

const q = (win: Probe, sel: string) =>
	win.document.querySelector(sel) as unknown as HTMLElement | null;
// q は DOM lib の HTMLElement を返す（読みやすさ優先）。happy-dom の getComputedStyle へ渡す時だけ型を戻す
const css = (win: Probe, el: HTMLElement) =>
	win.getComputedStyle(el as unknown as Parameters<Window["getComputedStyle"]>[0]);
const toggle = (win: Probe) => q(win, ".vp-proj-summary .vp-proj-toggle")!;
const caret = (win: Probe) => q(win, ".vp-proj-toggle .vp-proj-caret");

describe("開閉 caret — 押せることと、今の開閉が見える", () => {
	it("toggle は caret（ph:caret-right）を持つ", () => {
		const win = fixture({ expanded: true, subs: 2 });
		const c = caret(win);
		expect(c).not.toBeNull();
		expect((c as unknown as { icon: string }).icon).toBe("ph:caret-right");
	});

	it("caret は lead 行の最左に居る（木の開閉の定位置）", () => {
		const win = fixture({ expanded: true, subs: 2 });
		const row = q(win, ".vp-proj-summary .vp-lane-row")!;
		const first = row.firstElementChild as HTMLElement;
		expect(first.classList.contains("vp-lane-disclosure")).toBe(true);
		expect(first.querySelector(".vp-proj-toggle")).not.toBeNull();
	});

	it("開いている間は caret が 90° 回り、畳むと戻る（aria-expanded が SSOT）", () => {
		const win = fixture({ expanded: true, subs: 2 });
		expect(toggle(win).getAttribute("aria-expanded")).toBe("true");
		expect(css(win, caret(win)!).transform).toBe("rotate(90deg)");
		win.setup({ expanded: false, subs: 2 });
		expect(toggle(win).getAttribute("aria-expanded")).toBe("false");
		expect(css(win, caret(win)!).transform).not.toBe("rotate(90deg)");
	});

	it("aria-label は従来どおり（開閉の動詞を名乗る）", () => {
		const win = fixture({ expanded: true, subs: 1 });
		expect(toggle(win).getAttribute("aria-label")).toBe("vantage-point を折りたたむ");
		win.setup({ expanded: false, subs: 1 });
		expect(toggle(win).getAttribute("aria-label")).toBe("vantage-point を展開する");
	});

	it("click は process:toggle を送り、行の選択（lane:select）にはならない", () => {
		const win = fixture({ expanded: true, subs: 2 });
		toggle(win).click();
		expect(win.sent).toEqual([{ t: "process:toggle", path: "/vp", expanded: false }]);
		win.sent.length = 0;
		win.setup({ expanded: false, subs: 2 });
		toggle(win).click();
		expect(win.sent).toEqual([{ t: "process:toggle", path: "/vp", expanded: true }]);
	});

	it("活動ドットは toggle の外、従来の leading スロットに残る（役割を分ける）", () => {
		const win = fixture({ expanded: true, subs: 2 });
		expect(q(win, ".vp-proj-toggle .vp-activity-point")).toBeNull();
		expect(q(win, ".vp-proj-summary .vp-lane-leading .vp-activity-point")).not.toBeNull();
	});

	it("sub 行は caret を持たず、空の leading スロットのまま（配置を変えない）", () => {
		const win = fixture({ expanded: true, subs: 2 });
		const subs = Array.from(
			win.document.querySelectorAll(".vp-proj-content .vp-lane-row"),
		) as unknown as HTMLElement[];
		expect(subs).toHaveLength(2);
		for (const row of subs) {
			expect(row.querySelector(".vp-lane-disclosure")).toBeNull();
			expect(row.querySelector(".vp-lane-leading")!.childElementCount).toBe(0);
		}
	});
});

describe("畳む中身の有無で caret の濃さを変える", () => {
	it("lead 1 本だけ（子が 0 件）なら控えめ（.is-empty）", () => {
		const win = fixture({ expanded: true });
		expect(toggle(win).classList.contains("is-empty")).toBe(true);
		// ⚠️ 「1 でない」では当てない — happy-dom は規則が無いと "" を返すので、規則を消しても緑になる。
		// 値は宣言の文字列のまま（".35"）返るので数値で比べる
		expect(Number(css(win, toggle(win)).opacity)).toBe(0.35);
	});

	it("sub lane がある時ははっきり出す", () => {
		const win = fixture({ expanded: true, subs: 1 });
		expect(toggle(win).classList.contains("is-empty")).toBe(false);
		expect(Number(css(win, toggle(win)).opacity)).not.toBe(0.35);
	});

	it("sub が無くても追加セッションがあれば中身あり", () => {
		const win = fixture({ expanded: true, rootExtraSessions: 1 });
		expect(toggle(win).classList.contains("is-empty")).toBe(false);
	});
});

describe("畳んでいる間 — 中に sub が畳まれていると読める", () => {
	it("畳むと .vp-proj.folded、lane 数の chip に sub の状態ドットと内訳が載る", () => {
		const win = fixture({ expanded: false, subs: 3 });
		expect(q(win, ".vp-proj")!.classList.contains("folded")).toBe(true);
		const count = q(win, ".vp-lane-count")!;
		expect(count.querySelectorAll(".vp-sub-points .vp-sub-point")).toHaveLength(3);
		expect(count.getAttribute("title")).toBe("sub lane 3 本を畳んでいます");
		// chip の形（hairline の pill）
		expect(css(win, count).borderRadius).toBe("999px");
	});

	it("開いている間は folded でなく、状態ドットも出さない（sub 行そのものが見えている）", () => {
		const win = fixture({ expanded: true, subs: 3 });
		expect(q(win, ".vp-proj")!.classList.contains("folded")).toBe(false);
		expect(q(win, ".vp-sub-points")).toBeNull();
		expect(q(win, ".vp-lane-count")!.hasAttribute("title")).toBe(false);
		expect(css(win, q(win, ".vp-lane-count")!).borderRadius).not.toBe("999px");
	});

	it("lead 1 本だけなら畳んでも folded にしない（隠れている物が無い）", () => {
		const win = fixture({ expanded: false });
		expect(q(win, ".vp-proj")!.classList.contains("folded")).toBe(false);
	});
});

describe("hint の行（repo 停止中など）", () => {
	it("placeholder 行にも caret と活動ドットが並ぶ", () => {
		const win = fixture({ expanded: true, state: "stopped" });
		expect(q(win, ".vp-proj-placeholder .vp-proj-toggle .vp-proj-caret")).not.toBeNull();
		expect(q(win, ".vp-proj-placeholder .vp-proj-toggle .vp-activity-point")).toBeNull();
		expect(q(win, ".vp-proj-placeholder > .vp-activity-point")).not.toBeNull();
	});

	it("停止中・起動処理中など、hint の間は sub のドットを出さない（畳んで隠れている lane は無い）", () => {
		// 停止しても lanes_by_repo は残る。旧形はここでドットが「N lanes」の下へ折り返した（moody-blues 指摘）
		for (const state of ["stopped", "stopping", "starting", "error"]) {
			const win = fixture({ expanded: false, subs: 2, state });
			expect(q(win, ".vp-proj")!.classList.contains("folded")).toBe(false);
			expect(q(win, ".vp-sub-points")).toBeNull();
		}
	});

	it("停止中の repo の caret は控えめ — 開くと起動要求が飛ぶので宣伝しない（再開は ▷）", () => {
		// sidebar_ipc.rs: stopped の repo を展開すると repo_spawn_request（Resume を通らない起動）
		const win = fixture({ expanded: false, subs: 2, state: "stopped" });
		expect(toggle(win).classList.contains("is-empty")).toBe(true);
		expect(toggle(win).getAttribute("title")).toContain("▷");
	});

	it("稼働中の hint（repo error など）は 1 行の案内が中身なので控えめにしない", () => {
		const win = fixture({ expanded: true, state: "error" });
		expect(toggle(win).classList.contains("is-empty")).toBe(false);
	});
});
