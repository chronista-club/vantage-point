// @vitest-environment happy-dom
/**
 * VP の app field（doc 87 §5 / §8.3）— region の field と、3 つの registry が同梱した field。
 *
 * ## ⚠️ ここで守るもの
 *
 * - **既存の field id は 1 本も消さない**。保存済みのカスタムテーマ（`vp:editor:themes:v1`）は field id を
 *   key に値を持つので、id が消えると、その調整は黙って読まれなくなる
 * - **registry に `fields` 付きの行を 1 行足すと `appFields()` に出る**（PL §5.2 の open-closed）。rail /
 *   edge resident / pane kind のどの表に足しても同じで、region（= Editor の置き場と D-2 の semantic）は
 *   表から決まる
 * - **幅の field は `:root` に書かない**（cssVar を持たない）。`#sidebar-root` / `#app-shell` が同じ変数を
 *   inline で持つので、`:root` に書いても届かない。書き口は shell-layout.ts（`shell-layout.dom.test.ts`）
 */
import { describe, expect, it } from "vitest";
import { APP_PANE_REGISTRY } from "../app-pane-registry";
import { EDGE_RESIDENT_REGISTRY } from "../edge-residents";
import { SHELL_RAILS } from "../src/sidebar/shell-rails";
import { RIGHT_MAX_PCT, RIGHT_MIN_PCT, SIDEBAR_MAX, SIDEBAR_MIN } from "../shell-layout";
import { APP_FIELD_REGISTRIES, SHELL_LAYOUT_FIELDS, appFieldRegion, appFieldSpecs, appFields, isEditorManagedVar } from "./app-fields";
import { semanticOfRegion } from "./field-spec";

/** slice 3 より前からある field（保存済みテーマのキー）。ここから消してはいけない。 */
const LEGACY_IDS = [
	"sb.text.base",
	"sb.text.hint",
	"sb.text.meta",
	"sb.text.micro",
	"sb.background",
	"sb.panel",
	"sb.text.primary",
	"sb.text.secondary",
	"sb.text.tertiary",
	"sb.border",
	"sb.conn.auto",
	"sb.selection.bg",
	"sb.selection.border",
	"sb.selection.width",
	"sb.activity.thinking",
	"sb.activity.working",
	"sb.activity.waiting",
	"sb.activity.completed",
	"sb.activity.idle",
	"sb.activity.error",
	"chat.text.body",
	"chat.text.user",
	"chat.text.tool",
	"chat.text.meta",
	"chat.text.micro",
	"shell.resizer.breathe.ms",
	"shell.resizer.breathe.dip",
];

const byId = (id: string) => appFields().find((f) => f.id === id);

describe("既存の field", () => {
	it("id は全部残る（保存済みテーマのキー互換）", () => {
		const ids = appFields().map((f) => f.id);
		for (const id of LEGACY_IDS) expect(ids, id).toContain(id);
	});

	it("sb.* は左 edge に属する（semantic = source）。chat.* と境界の動きは全体（global）", () => {
		for (const f of appFields()) {
			if (f.id.startsWith("sb.")) expect(f.semantic, f.id).toBe("source");
			if (f.id.startsWith("chat.") || f.id.startsWith("shell.resizer.")) expect(f.semantic, f.id).toBe("global");
		}
	});

	it("地に重ねる薄い光（--lg-veil）は左 edge の配色の 1 本 — 地を明るくした時に hover の重ねも合わせられる", () => {
		expect(byId("sb.veil")).toMatchObject({ cssVar: "--lg-veil", type: "color", semantic: "source", group: "背景と文字" });
	});

	it("appFieldRegion — Editor のタブを決める region は VP の宣言から引く（VP の field でなければ無い）", () => {
		expect(appFieldRegion("sb.text.hint")).toBe("leftEdge");
		expect(appFieldRegion("shell.leftEdge.peek.width")).toBe("leftEdge");
		expect(appFieldRegion("shell.rightEdge.width")).toBe("rightEdge");
		expect(appFieldRegion("pane.lane.terminal.font.size")).toBe("center");
		expect(appFieldRegion("shell.resizer.breathe.ms")).toBeUndefined();
		expect(appFieldRegion("spacing.m")).toBeUndefined();
	});

	it("id は重複しない（基本の宣言 + 同梱 + 幅）", () => {
		const ids = appFields().map((f) => f.id);
		expect(new Set(ids).size).toBe(ids.length);
	});
});

describe("region の幅 — 持ち主は shell-layout.ts", () => {
	it("左 = peek の幅（drag と同じ 180〜640px）、右 = R の幅（15〜60%）", () => {
		const left = byId("shell.leftEdge.peek.width");
		expect(left).toMatchObject({
			type: "number",
			semantic: "source",
			initial: 280,
			constraints: { min: SIDEBAR_MIN, max: SIDEBAR_MAX, unit: "px" },
		});
		const right = byId("shell.rightEdge.width");
		expect(right).toMatchObject({
			type: "number",
			semantic: "tool",
			initial: 25,
			constraints: { min: RIGHT_MIN_PCT, max: RIGHT_MAX_PCT, unit: "%" },
		});
	});

	it("幅の field は cssVar を持たない（:root に書いても #sidebar-root / #app-shell の inline に負ける）", () => {
		for (const spec of SHELL_LAYOUT_FIELDS) expect(byId(spec.id)?.cssVar, spec.id).toBeUndefined();
	});

	it("center の名札の高さは :root の token（全 pane の名札が共有）", () => {
		expect(byId("shell.center.nameplate.height")).toMatchObject({
			cssVar: "--vp-nameplate-h",
			semantic: "global",
			initial: 28,
		});
	});
});

describe("registry が同梱した field", () => {
	it("pane kind の lane 行が持つ端末の 3 本が出る（region = center）", () => {
		const lane = APP_PANE_REGISTRY.find((p) => p.id === "lane")!;
		for (const knob of lane.fields) {
			expect(byId(`pane.lane.${knob.key}`), knob.key).toMatchObject({
				cssVar: knob.cssVar,
				initial: knob.initial,
				semantic: "global",
			});
		}
	});

	it("3 つの registry のどれに fields 付きの行を 1 行足しても、appFields() に出る", () => {
		// 本物の 3 表がそれぞれの region で繋がっている（写しや空の表に差し替わっていない）
		expect(APP_FIELD_REGISTRIES.map((r) => r.prefix)).toEqual(["rail", "resident", "pane"]);
		expect(APP_FIELD_REGISTRIES.map((r) => r.region)).toEqual(["leftEdge", "rightEdge", "center"]);
		expect(APP_FIELD_REGISTRIES[0].entries).toBe(SHELL_RAILS);
		expect(APP_FIELD_REGISTRIES[1].entries).toBe(EDGE_RESIDENT_REGISTRY);
		expect(APP_FIELD_REGISTRIES[2].entries).toBe(APP_PANE_REGISTRY);
		APP_FIELD_REGISTRIES.forEach((source, i) => {
			const extra = {
				id: "probe",
				fields: [{ key: "knob", label: "試し", group: "試し", cssVar: "--vp-probe-knob", initial: 3 }],
			};
			const registries = APP_FIELD_REGISTRIES.map((r, j) => (j === i ? { ...r, entries: [...r.entries, extra] } : r));
			const field = appFields(registries).find((f) => f.id === `${source.prefix}.probe.knob`);
			expect(field, source.prefix).toMatchObject({
				cssVar: "--vp-probe-knob",
				initial: 3,
				semantic: semanticOfRegion(source.region),
			});
			// 足していない表（本物）には出ない
			expect(appFields().some((f) => f.id === `${source.prefix}.probe.knob`)).toBe(false);
		});
	});
});

describe("isEditorManagedVar — テーマを切り替える時に :root の inline から消す変数", () => {
	it("宣言の cssVar（registry の同梱分を含む）を消す", () => {
		for (const spec of appFieldSpecs()) expect(isEditorManagedVar(spec.cssVar), spec.cssVar).toBe(true);
		expect(isEditorManagedVar("--terminal-font-size")).toBe(true);
	});

	it("creo-ui の token と component の knob も消す（従来どおり）", () => {
		for (const name of ["--color-brand-primary", "--color-surface-surface", "--typography-scale", "--layout-gap-sibling", "--_btn__pad-x"]) {
			expect(isEditorManagedVar(name), name).toBe(true);
		}
	});

	it("幅は :root に書かないので対象外。関係の無い変数も消さない", () => {
		for (const name of ["--vp-peek-w", "--vp-rsb-pct", "--frame-transition-ms", "--color-text-primary"]) {
			expect(isEditorManagedVar(name), name).toBe(false);
		}
	});
});

/**
 * field の cssVar と stylesheet の約束（doc 87 §8.3）。Editor は値を `:root` の inline style に書くので:
 *
 * - 同じ変数を `:root` 以外（要素の selector・要素の inline style）で宣言していると、子孫には届かない
 *   （`#sidebar-root{--vp-peek-w:…}` がその形 — だから幅は cssVar を持たず shell-layout の書き口を通る）
 * - 数の field の `initial` は `:root` の既定値と同じ（違うと「初期値に戻す」が別の値を書く）
 *
 * CSS は**ファイルからそのまま**読む（main_area.rs の静的 HTML・webview の CSS 定数・assets の css）。
 */
describe("field の cssVar と stylesheet の約束", async () => {
	const { readFileSync, readdirSync } = await import("node:fs");
	const { dirname, join } = await import("node:path");
	const { fileURLToPath } = await import("node:url");
	const webview = join(dirname(fileURLToPath(import.meta.url)), "..");
	const walk = (dir: string): string[] =>
		readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
			e.name === "node_modules" ? [] : e.isDirectory() ? walk(join(dir, e.name)) : [join(dir, e.name)],
		);
	const sources: Array<[string, string]> = [
		join(webview, "../src/webview/main_area.rs"),
		...walk(join(webview, "../assets")).filter((f) => f.endsWith(".css")),
		...walk(webview).filter((f) => /\.tsx?$/.test(f) && !/\.test\.tsx?$/.test(f)),
	].map((f) => [f.slice(webview.length - "webview".length), readFileSync(f, "utf8")]);
	const esc = (s: string) => s.replace(/[-]/g, "\\-");
	/** CSS の宣言 block（selector と中身）。`{` から始めて探す（selector 側から探すと大きな file で遅い）。 */
	// ⚠️ コメントは先に除く — コメントの中の括弧（main_area.rs の :root にある `new Terminal({...})` など）が
	// block を割ると、その block の宣言が見えなくなる（検査が黙って通る）
	const uncomment = (text: string) => text.replace(/\/\*[\s\S]*?\*\//g, "");
	const blocks = sources.map(([file, raw]) => ({ file, text: uncomment(raw) })).map(({ file, text }) => ({
		file,
		text,
		blocks: [...text.matchAll(/\{([^{}]*)\}/g)]
			.filter((m) => m[1].includes("--"))
			.map((m) => ({
				selector:
					text
						.slice(Math.max(0, m.index - 300), m.index)
						.split(/[{};\n`]/)
						.map((l) => l.trim())
						.filter(Boolean)
						.pop() ?? "",
				body: m[1],
			})),
	}));

	it("どの field の変数も :root 以外では宣言しない・要素の inline に書かない", () => {
		const shadowed: string[] = [];
		for (const spec of appFieldSpecs()) {
			const decl = new RegExp(`(?:^|[;{\\s])${esc(spec.cssVar)}\\s*:`);
			// 要素への inline の書き込み: style.setProperty("--x", …)（整形で改行が入っても拾う）と、Solid の
			// style={{ "--x": … }}。documentElement（= :root）への書き込みだけは許す
			const inline = new RegExp(
				`setProperty\\(\\s*["'\`]${esc(spec.cssVar)}["'\`]|["'\`]${esc(spec.cssVar)}["'\`]\\s*:`,
				"g",
			);
			for (const { file, text, blocks: list } of blocks) {
				for (const { selector, body } of list) {
					if (decl.test(body) && !/^:root\b/.test(selector)) shadowed.push(`${file}: ${selector} { ${spec.cssVar} }`);
				}
				for (const m of text.matchAll(inline)) {
					const before = text.slice(Math.max(0, m.index - 120), m.index);
					if (!before.includes("documentElement")) shadowed.push(`${file}: ${text.slice(m.index, m.index + 60).replace(/\s+/g, " ")}`);
				}
			}
		}
		expect(shadowed).toEqual([]);
	});

	it("数の field の initial は :root の既定値と同じ", () => {
		const drift: string[] = [];
		const compared = new Set<string>();
		for (const spec of appFieldSpecs()) {
			if (typeof spec.initial !== "number") continue;
			const decl = new RegExp(`(?:^|[;{\\s])${esc(spec.cssVar)}\\s*:\\s*([^;}]+)`);
			for (const { file, blocks: list } of blocks) {
				for (const { selector, body } of list) {
					const value = selector === ":root" ? body.match(decl)?.[1] : undefined;
					if (value !== undefined) compared.add(spec.id);
					if (value !== undefined && Number.parseFloat(value) !== spec.initial) {
						drift.push(`${file}: ${spec.id} initial=${spec.initial} / :root ${spec.cssVar}: ${value.trim()}`);
					}
				}
			}
		}
		expect(drift).toEqual([]);
		// 検査が空振りしていない（main_area.rs / Shell.tsx / chatview.tsx の :root を全部読めている）
		for (const id of ["sb.text.hint", "chat.text.body", "shell.resizer.breathe.ms", "shell.center.nameplate.height", "pane.lane.terminal.font.size"]) {
			expect(compared, id).toContain(id);
		}
	});
});
