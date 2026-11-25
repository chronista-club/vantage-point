/**
 * Editor field の宣言（pure data）と、そこからの導出（doc 87 §5 / §8.3）。
 *
 * ## ⚠️ ここで守るのは 2 つ
 *
 * - **registry の行が同梱した field は、collector を通るだけで Editor に出る**（open-closed）。
 *   field id は `<registry>.<行 id>.<key>` に collector が組み立てる — 行の書き手が id を手で書かないので、
 *   別の registry の似た名前の行（rail の `lanes` と pane の `lane`）や、将来別の表に同じ id の行が入っても衝突しない
 * - **region が Editor の置き場と D-2 の semantic を決める**。左 edge = source、右 edge = tool
 *   （creo-ui editor-mode.md D-2 の LEFT / RIGHT）。Editor の SIDEBAR タブは左右の edge を、
 *   THEME タブはそれ以外（全体・center・境界）を受ける
 */
import { describe, expect, it } from "vitest";
import {
	type AppFieldSpec,
	type FieldRegistrySource,
	collectBundledFields,
	editorSectionOf,
	managedVarsOf,
	semanticOfRegion,
} from "./field-spec";

const TERMINAL = {
	key: "terminal.font.size",
	label: "端末の文字サイズ",
	group: "Lane の端末",
	cssVar: "--terminal-font-size",
	initial: 16,
	constraints: { min: 10, max: 24, step: 0.5 },
} as const;

const sources = (extra: FieldRegistrySource["entries"] = []): FieldRegistrySource[] => [
	{ prefix: "rail", region: "leftEdge", entries: [{ id: "lanes" }, { id: "actions" }] },
	{ prefix: "resident", region: "rightEdge", entries: [{ id: "new" }] },
	{ prefix: "pane", region: "center", entries: [{ id: "lane", fields: [TERMINAL] }, ...extra] },
];

describe("collectBundledFields — registry の行が同梱した field を集める", () => {
	it("id は <registry>.<行 id>.<key>、region は registry から、残りは宣言のまま", () => {
		expect(collectBundledFields(sources())).toEqual([
			{
				id: "pane.lane.terminal.font.size",
				label: "端末の文字サイズ",
				group: "Lane の端末",
				region: "center",
				cssVar: "--terminal-font-size",
				initial: 16,
				constraints: { min: 10, max: 24, step: 0.5 },
			},
		]);
	});

	it("fields を持たない行は何も足さない", () => {
		expect(collectBundledFields(sources()).map((f) => f.id)).not.toContain("rail.lanes");
	});

	it("表に fields 付きの行を 1 行足すと、collector の出力にそのまま出る", () => {
		const extra = {
			id: "gallery",
			fields: [{ key: "gap", label: "間隔", group: "Gallery", cssVar: "--vp-gallery-gap", initial: 8 }],
		};
		const ids = collectBundledFields(sources([extra])).map((f) => f.id);
		expect(ids).toEqual(["pane.lane.terminal.font.size", "pane.gallery.gap"]);
	});

	it("rail / resident の行も同じ規則で集まる（region はそれぞれ左 edge / 右 edge）", () => {
		const knob = { key: "w", label: "幅", group: "g", cssVar: "--x", initial: 1 };
		const all = collectBundledFields([
			{ prefix: "rail", region: "leftEdge", entries: [{ id: "lanes", fields: [knob] }] },
			{ prefix: "resident", region: "rightEdge", entries: [{ id: "prs", fields: [knob] }] },
		]);
		expect(all.map((f) => [f.id, f.region])).toEqual([
			["rail.lanes.w", "leftEdge"],
			["resident.prs.w", "rightEdge"],
		]);
	});
});

describe("semanticOfRegion — region → Editor Mode D-2 の semantic", () => {
	it("左 = source、右 = tool、上 = global、下 = utility", () => {
		expect(semanticOfRegion("leftEdge")).toBe("source");
		expect(semanticOfRegion("rightEdge")).toBe("tool");
		expect(semanticOfRegion("upEdge")).toBe("global");
		expect(semanticOfRegion("downEdge")).toBe("utility");
	});

	it("center と region 無し（全体・境界）は global", () => {
		expect(semanticOfRegion("center")).toBe("global");
		expect(semanticOfRegion(undefined)).toBe("global");
	});
});

describe("editorSectionOf — field が Editor のどのタブに住むか（VP の region から決める）", () => {
	const field = (id: string, scope?: "token" | "component" | "instance") => ({ id, scope });

	it("左右の edge の region を持つ field は SIDEBAR", () => {
		expect(editorSectionOf(field("sb.text.hint"), "leftEdge")).toBe("SIDEBAR");
		expect(editorSectionOf(field("shell.rightEdge.width"), "rightEdge")).toBe("SIDEBAR");
	});

	it("chat.* は CHAT、それ以外の全体・center・境界は THEME", () => {
		expect(editorSectionOf(field("chat.text.body"), "center")).toBe("CHAT");
		expect(editorSectionOf(field("typography.scale"), undefined)).toBe("THEME");
		expect(editorSectionOf(field("shell.resizer.breathe.ms"), undefined)).toBe("THEME");
		expect(editorSectionOf(field("pane.lane.terminal.font.size"), "center")).toBe("THEME");
	});

	it("component の knob は COMPONENT（選択した時だけ出る）", () => {
		expect(editorSectionOf(field("btn.pad.x", "component"), undefined)).toBe("COMPONENT");
	});

	it("VP の region を持たない field は SIDEBAR に混ざらない — creo-ui の token の自動発見（console の creoEditor.autoDiscover()）は semantic が既定で tool", () => {
		expect(editorSectionOf({ id: "spacing.m", scope: "token" }, undefined)).toBe("THEME");
	});

	it("保存済みテーマの互換のためだけに登録している field はどのタブにも出さない", () => {
		for (const id of ["sb.text.base", "typography.size.xs", "typography.size.s", "typography.size.m", "typography.size.xl", "layout.gap.sibling"]) {
			expect(editorSectionOf(field(id), id.startsWith("sb.") ? "leftEdge" : undefined)).toBeUndefined();
		}
	});
});

describe("managedVarsOf — Editor が :root に書く変数（テーマ切替で消す対象）", () => {
	it("宣言の cssVar を集める — registry の同梱 field もここに入る", () => {
		const specs: AppFieldSpec[] = [
			{ id: "sb.text.hint", label: "", group: "", region: "leftEdge", cssVar: "--sb-text-hint", initial: 12 },
			...collectBundledFields(sources()),
		];
		expect([...managedVarsOf(specs)].sort()).toEqual(["--sb-text-hint", "--terminal-font-size"]);
	});
});
