/**
 * Light Grid の色を token に畳む（doc 87 §7 / §8.3）。
 *
 * ## ⚠️ ここで守るもの
 *
 * - **左 edge の補助色は palette の token から引く**。直書きの色は Editor で背景・文字の色を変えても動かず、
 *   それだけが浮いて残る（明るい背景で白い hover が消える、暗い丸だけが残る）。token にしたことで、プリセットでは
 *   creo-ui の token に従い、Light Grid（既定）では `--lg-veil` を Editor の `sb.veil` で合わせられる。⚠️ Light Grid の
 *   `--lg-cyan-deep` / `--lg-cyan` / `--lg-shadow` は literal のまま（既定の見た目を bit 単位で保つため。doc 87 §8.3）
 * - **既定の見た目は 1 bit も変えない**。白い重ね（旧 `#ffffff04` / `06` / `08` / `0c`）は `--lg-veil` を
 *   同じ不透明度で混ぜた値で、8 bit に丸めると旧値と同じ alpha になる
 * - **literal の色を持つ `--lg-*` は、Light Grid 以外のプリセットで creo-ui の token に写される**
 *   （`editor/styles.ts`）。写し忘れた token はプリセットを変えても暗いまま残る
 *
 * CSS は**ファイルからそのまま**読む（Shell.tsx を import すると creo-ui の Solid の部品まで評価される）。
 */
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { EDITOR_CSS } from "../../editor/styles";

const here = dirname(fileURLToPath(import.meta.url));
const read = (rel: string) => readFileSync(join(here, rel), "utf8");

/** file の `*_CSS` 定数（template literal）の中身。コメントは除く。 */
function cssConstants(src: string): Array<{ name: string; css: string }> {
	return [...src.matchAll(/const\s+(\w*CSS\w*)\s*=\s*`([\s\S]*?)`/g)].map((m) => ({
		name: m[1],
		css: m[2].replace(/\/\*[\s\S]*?\*\//g, ""),
	}));
}

/** `--name:value;` の並びを Map に。 */
function declarations(block: string): Map<string, string> {
	const out = new Map<string, string>();
	for (const m of block.matchAll(/(--[\w-]+)\s*:\s*([^;]+);?/g)) out.set(m[1], m[2].trim());
	return out;
}

const SHELL_CSS = cssConstants(read("Shell.tsx")).find((c) => c.name === "SHELL_CSS")!.css;
/** Light Grid の palette（`:root{--sb-text-base…}` の宣言 block）。 */
const PALETTE_BLOCK = SHELL_CSS.match(/:root\{(--sb-text-base[\s\S]*?)\}/)![1];
const PALETTE = declarations(PALETTE_BLOCK);
/** Light Grid 以外のプリセットでの写し先（`editor/styles.ts`）。 */
const PRESET = declarations(
	EDITOR_CSS.match(/:root\[data-vp-editor-theme\]:not\(\[data-vp-editor-theme="contrast-dark"\]\)\s*\{([^}]*)\}/)![1],
);

/** src/sidebar の全 tsx の `*_CSS` 定数（左 edge の CSS）。 */
const SIDEBAR_CSS = (function walk(dir: string): string[] {
	return readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
		e.isDirectory() ? walk(join(dir, e.name)) : e.name.endsWith(".tsx") ? [join(dir, e.name)] : [],
	);
})(here).flatMap((file) =>
	cssConstants(readFileSync(file, "utf8")).map(({ name, css }) => ({ where: `${file.slice(here.length + 1)} ${name}`, css })),
);

/** 色の literal（hex / rgb(a) / hsl / oklch）。var() の fallback は除いてから探す。 */
function colorLiterals(css: string): string[] {
	const stripped = css.replace(/var\(--[\w-]+,\s*(?:#[0-9a-fA-F]{3,8}|rgba?\([^)]*\)|oklch\([^)]*\))\)/g, "VAR");
	return [...stripped.matchAll(/#[0-9a-fA-F]{3,8}\b|rgba?\([^)]*\)|hsla?\([^)]*\)|oklch\([^)]*\)/g)].map((m) => m[0]);
}

describe("Shell.tsx の CSS — 直書きの色は palette の宣言だけ", () => {
	it("palette の外に色の literal が無い（例外: mask の #000 と、危険色の塗りに載せる白の文字）", () => {
		const outside = SHELL_CSS.replace(PALETTE_BLOCK, "");
		const leftovers: string[] = [];
		for (const rule of outside.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
			const [selector, body] = [rule[1].trim(), rule[2]];
			for (const decl of body.split(";")) {
				const [prop, ...rest] = decl.split(":");
				const value = rest.join(":");
				for (const literal of colorLiterals(value)) {
					const maskStop = literal === "#000" && /mask-image/.test(prop);
					const onDanger = literal === "#fff" && selector.includes(".danger") && prop.trim() === "color";
					if (!maskStop && !onDanger) leftovers.push(`${selector} { ${decl.trim()} }`);
				}
			}
		}
		expect(leftovers).toEqual([]);
	});
});

describe("白い重ね（Lucid Glass の veil）", () => {
	it("--lg-veil は白、--lg-veil-1〜4 はそれを旧 #ffffff04 / 06 / 08 / 0c と同じ alpha で混ぜる", () => {
		expect(PALETTE.get("--lg-veil")).toBe("#fff");
		const alphaBytes = [4, 6, 8, 12];
		alphaBytes.forEach((byte, i) => {
			const value = PALETTE.get(`--lg-veil-${i + 1}`);
			const pct = value?.match(/^color-mix\(in srgb,var\(--lg-veil\) ([\d.]+)%,transparent\)$/)?.[1];
			expect(pct, `--lg-veil-${i + 1}: ${value}`).toBeDefined();
			expect(Math.round((Number(pct) / 100) * 255), `--lg-veil-${i + 1}`).toBe(byte);
		});
	});

	it("src/sidebar のどの CSS にも、使われる色として 8 桁の白（#ffffffXX）が残っていない", () => {
		// var() の fallback は除く — 定義のある token の控えで、描画には使われない（次の test が「定義がある」を守る）
		const hits = SIDEBAR_CSS.flatMap(({ where, css }) =>
			colorLiterals(css)
				.filter((c) => /^#ffffff[0-9a-f]{2}$/i.test(c))
				.map((c) => `${where}: ${c}`),
		);
		expect(hits).toEqual([]);
	});

	it("CSS が引く --lg-* は全部 palette に定義がある（未定義の token は fallback の直書きが描かれる）", () => {
		// 実例: AtlasPicker は未定義の --lg-hover を引いていて、fallback の #ffffff0c がそのまま描かれていた
		const undefinedRefs = SIDEBAR_CSS.flatMap(({ where, css }) =>
			[...css.matchAll(/var\((--lg-[\w-]+)/g)].filter((m) => !PALETTE.has(m[1])).map((m) => `${where}: ${m[1]}`),
		);
		expect(undefinedRefs).toEqual([]);
	});
});

describe("palette と プリセットの写し", () => {
	it("切れた接続の丸の地は --lg-cyan-deep（旧 #123039 の直書き）", () => {
		expect(PALETTE.get("--lg-cyan-deep")).toBe("#123039");
	});

	it("context menu の影は --lg-shadow（旧 rgba(0,0,0,.4) の直書き）— creo-ui の --color-shadow-base を直に引かない", () => {
		// ⚠️ 起動時の data-theme は mint-dark（sidebar.tsx が VpEditor の後に書く）で、creo-ui の --color-shadow-base は
		// mint-dark では 0.3。contrast-dark の 0.4 を前提に creo の token を引くと、既定の影が薄くなる
		expect(PALETTE.get("--lg-shadow")).toBe("rgba(0,0,0,.4)");
		expect(SHELL_CSS).toMatch(/\.vp-ctx-menu\{[^}]*box-shadow:0 8px 24px var\(--lg-shadow\)/);
	});

	it("literal の色を持つ --lg-* は、Light Grid 以外のプリセットで全部写される", () => {
		const literal = [...PALETTE].filter(([name, value]) => name.startsWith("--lg-") && !value.includes("var("));
		expect(literal.length).toBeGreaterThan(0);
		for (const [name] of literal) expect(PRESET.has(name), name).toBe(true);
	});

	it("プリセットの veil は文字の色（--lg-hot）に従う — 明るい地では暗い重ねになる", () => {
		expect(PRESET.get("--lg-veil")).toBe("var(--lg-hot)");
	});
});
