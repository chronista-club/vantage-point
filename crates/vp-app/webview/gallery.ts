/**
 * Component Gallery mode（doc 48 Phase 3 → doc 49 LE-P2 で pane 化）
 *
 * `#gallery` hash で root を story 表示に切り替える dev surface。mako（目 + Editor Mode
 * slider）と AI agent（MCP editor_set / vp shot）が **同じ component を同時に触る**ための台。
 * LE-P2 からは story が creo-ui-layout の pane になり、gallery 自体が LayoutEngine の
 * **最初の VP コンテンツ = primary dogfood**（doc 49 §6 step 3）。
 *
 * 設計判断:
 * - 切替は URL hash（query でなく）: server 無接触で、Reload WebView（Cmd+R）後も
 *   hash が残る = 「tsx 編集 → watch rebuild → Cmd+R」の HMR ループ中 gallery に留まれる。
 * - 本 module は **純 data + 純 calculation のみ**（vitest node 環境でテスト可能）。
 *   DOM 反映・Solid mount・keyboard は gallery-panes.tsx（action 層）が担う。
 * - 凍結中の旧 layout 2 系統（Frame Engine / PaneLayout、doc 47 §1）には一切触れない。
 * - Editor Mode（Ctrl+Shift+E）とは同居: story が参照する CSS var は :root で live に
 *   書かれるので、slider / `editor_set` の変更が gallery 表示に即反映される。
 *
 * 入口: Ctrl+Shift+G（Editor Mode の Ctrl+Shift+E と対）or `location.hash = "#gallery"`。
 */

import {
	type Layout,
	formatNotation,
	isMember,
	memberIds,
	parseNotation,
} from "@chronista-club/creo-ui-layout";

// ---------- data ----------

/** story 1 枚 = id + 表示名 + 静的 HTML（compile-time 定数のみ、外部入力は入れない） */
export interface GalleryStory {
	id: string;
	title: string;
	/** 何を見る story か（1 行）。knob 名を書くと Editor Mode との突き合わせが速い */
	note?: string;
	html: string;
}

export const GALLERY_HASH = "#gallery";

/** 見本文（サイズ比較用に和欧数字を混ぜる） */
const SPECIMEN = "Vantage Point — 永続化された開発環境 0123456789";

/** sidebar text scale: `sb.text.*` knob（entry.tsx SidebarTokenBinds）の実寸見本 */
const TEXT_SCALE_STORY: GalleryStory = {
	id: "sidebar-text-scale",
	title: "Sidebar text scale",
	note: "knobs: sb.text.base / hint / meta / micro（Editor Mode slider ⇄ editor_set が即反映）",
	html: ["base", "hint", "meta", "micro"]
		.map(
			(k) => `
<div class="g-row">
  <code class="g-var">--sb-text-${k}</code>
  <span class="g-specimen" style="font-size:var(--sb-text-${k})">${SPECIMEN}</span>
</div>`,
		)
		.join(""),
};

/** connector / Light Grid: `sb.conn.*` / `--sb-glow` knob の見本 */
const CONNECTOR_STORY: GalleryStory = {
	id: "sidebar-connector",
	title: "Connector / Light Grid",
	note: "knobs: sb.conn.width / slot / dash / glow / hitl / auto",
	html: `
<div class="g-row">
  <code class="g-var">--sb-conn-hitl</code>
  <span class="g-swatch" style="background:var(--sb-conn-hitl)"></span>
  <span class="g-glow" style="background:var(--sb-conn-hitl);box-shadow:0 0 var(--sb-glow) var(--sb-conn-hitl)"></span>
</div>
<div class="g-row">
  <code class="g-var">--sb-conn-auto</code>
  <span class="g-swatch" style="background:var(--sb-conn-auto)"></span>
  <span class="g-glow" style="background:var(--sb-conn-auto);box-shadow:0 0 var(--sb-glow) var(--sb-conn-auto)"></span>
</div>
<div class="g-row">
  <code class="g-var">--sb-conn-width</code>
  <span class="g-line" style="height:var(--sb-conn-width);background:var(--sb-conn-auto)"></span>
</div>
<div class="g-row">
  <code class="g-var">--sb-conn-dash</code>
  <span class="g-dash" style="border-top:var(--sb-conn-width) dashed var(--sb-conn-auto)"></span>
</div>`,
};

/**
 * story registry。実 component（LaneHeader / HistoryStrip 等）は mount-function 型で
 * live 依存（VpConsole 等）を持つため、mock を整えた story から順に足す
 * （doc 48 §4「調整したい component から順に」。網羅主義にしない）。
 */
export const STORIES: readonly GalleryStory[] = [TEXT_SCALE_STORY, CONNECTOR_STORY];

// ---------- calculations ----------

/** gallery hash か（`#gallery` 単独 or `#gallery/...` の下位 path） */
export function isGalleryHash(hash: string): boolean {
	return hash === GALLERY_HASH || hash.startsWith(`${GALLERY_HASH}/`);
}

/** toggle 後の hash 値（純 calculation。適用は caller） */
export function toggleGalleryHash(hash: string): string {
	return isGalleryHash(hash) ? "" : GALLERY_HASH;
}

/** story 1 枚分の pane 内 HTML（純 calculation — vitest node 環境でそのまま検証できる） */
export function storyPaneHtml(s: GalleryStory): string {
	return `
<h2 class="g-title">${s.title}</h2>
${s.note ? `<p class="g-note">${s.note}</p>` : ""}
<div class="g-body">${s.html}</div>`;
}

export const GALLERY_CSS = `
#gallery-root{position:fixed;inset:0;z-index:500;display:flex;flex-direction:column;background:var(--color-surface-bg-base);color:var(--color-text-primary);padding:16px 20px;font-family:var(--vp-font-sans),var(--typography-family-sans);}
/* Editor Mode パネル (#editor-root、z 未指定 = auto) を gallery の不透明 overlay より
   上に明示する。これが無いと gallery 中の Ctrl+Shift+E / editor_set の UI が塗り潰され、
   「slider と gallery を同時に触る」という本義 (doc 48 Phase 3) が死ぬ。
   position:fixed の子の viewport 基準は stacking context 化では変わらないので安全。
   本 rule は gallery-style ごと注入/除去されるため通常モードには影響しない。 */
#editor-root{position:relative;z-index:600;}
#gallery-root .g-header h1{font-size:18px;margin:0 0 4px;}
#gallery-root .g-note{font-size:11px;opacity:.65;margin:0 0 8px;}
#gallery-root .g-notation{font-family:var(--vp-font-mono),monospace;opacity:.8;}
/* pane 化 (LE-P2): stage が残り全高を取り、story は PaneStage の pane host 内に住む */
#gallery-root .gp-stage{flex:1;position:relative;min-height:0;}
#gallery-root .gp-pane{width:100%;height:100%;box-sizing:border-box;overflow:auto;padding:12px 16px;border:1px solid var(--color-border-default,rgba(128,128,128,.3));border-radius:8px;background:var(--color-surface-bg-raised,rgba(128,128,128,.06));cursor:pointer;}
#gallery-root .g-title{font-size:14px;margin:0 0 2px;}
#gallery-root .g-row{display:flex;align-items:center;gap:16px;margin:10px 0;}
#gallery-root .g-var{font-family:var(--vp-font-mono),monospace;font-size:11px;opacity:.7;min-width:150px;}
#gallery-root .g-specimen{white-space:nowrap;}
#gallery-root .g-swatch{display:inline-block;width:48px;height:20px;border-radius:4px;}
#gallery-root .g-glow{display:inline-block;width:10px;height:10px;border-radius:50%;}
#gallery-root .g-line{display:inline-block;width:220px;}
#gallery-root .g-dash{display:inline-block;width:220px;height:0;}
`;

// ---------- layout bridge calculations（LE-P2 PR2、LE-15） ----------

/** 記法に使える pane id（gallery-panes / MCP 双方の guard。notation.ts の規約と同一） */
const LAYOUT_ID_RE = /^[^\s|/~]+$/;

/** 構造非所属で場に居る pane（floating）の id 列 */
export function floatingIds(layout: Layout): string[] {
	return Object.keys(layout.attention).filter(
		(id) => !isMember(layout.structure, id) && (layout.attention[id] ?? 0) > 0,
	);
}

/** Layout の記法表現（構造 + float。サイズは含まない = LE-4） */
export function layoutNotation(layout: Layout): string {
	return formatNotation(layout.structure, floatingIds(layout));
}

/** MCP layout_set の入力（全 field 任意 = 部分更新。null は「現状維持」） */
export interface LayoutSpec {
	notation?: string | null;
	attention?: Record<string, number> | null;
	locks?: Record<string, number> | null;
}

/** MCP layout_get の snapshot（純 calculation。shares は呼び手が resolve 済みを渡す） */
export function layoutSnapshot(
	scope: string,
	layout: Layout,
	shares: Record<string, number>,
): Record<string, unknown> {
	return {
		scope,
		notation: layoutNotation(layout),
		attention: { ...layout.attention },
		locks: layout.locks ? { ...layout.locks } : {},
		shares,
	};
}

/**
 * MCP layout_set の spec → 新 Layout（純 calculation）。
 * - notation 省略/null = 構造・float 集合は現状維持。指定 = total（旧 float は落ちる）
 * - attention は部分 overlay（省略 id は現状維持、0 = 非表示。新 id に > 0 = float で現れる）
 * - locks は null = 維持 / {} = 全消し / 指定 = 全置換
 * 全 pane 非表示になる spec・不正値・記法に使えない id は Error（呼び手が {error} 化）。
 */
export function applyLayoutSpec(current: Layout, spec: LayoutSpec): Layout {
	const parsed = spec.notation != null ? parseNotation(spec.notation) : null;
	const structure = parsed ? parsed.structure : current.structure;
	const floats = parsed ? [...parsed.floats] : floatingIds(current);
	const overlay = spec.attention ?? {};

	const ids = new Set([...memberIds(structure), ...floats]);
	for (const [id, v] of Object.entries(overlay)) {
		// overlay の新 id に正値 = float として現れる（2×2 の非所属 × >0）。
		// 後で記法に直列化できる id だけを許す（get の formatNotation が壊れない guard）
		if (v > 0 && !ids.has(id)) {
			if (!LAYOUT_ID_RE.test(id)) {
				throw new Error(`layout_set: pane id "${id}" に空白と | / ~ は使えない`);
			}
			ids.add(id);
		}
	}
	if (ids.size === 0) {
		throw new Error("layout_set: 空の layout は拒否（全零 guard）");
	}

	const visible = Object.values(current.attention).filter((v) => v > 0);
	const mean = visible.length > 0 ? visible.reduce((s, v) => s + v, 0) / visible.length : 1;

	const attention: Record<string, number> = {};
	for (const id of ids) {
		const o = overlay[id];
		if (o != null) {
			if (!Number.isFinite(o) || o < 0) {
				throw new Error(`layout_set: attention["${id}"] が不正な値: ${o}`);
			}
			attention[id] = o;
		} else {
			// 省略 id は**現状維持**（0 = 意図して非表示、も維持する）。mean で可視化するのは
			// 「current の場に居ない id」（notation で新規に現れた pane）だけ — 「記法に書いた
			// pane が見えない」罠の回避。旧実装は `prev > 0 ? prev : mean` で既存の 0 も蘇生
			// させていた（gallery は全 story 可視が常態で無症状、大半が 0 の app scope が
			// LE-P4 実弾で即発症した潜在バグ）
			const prev = current.attention[id];
			attention[id] = prev != null ? prev : mean;
		}
	}
	if (!Object.values(attention).some((v) => v > 0)) {
		throw new Error("layout_set: 全 pane 非表示になる spec は拒否（全零 guard）");
	}

	const locks = spec.locks != null ? spec.locks : current.locks;
	const cleanLocks = locks && Object.keys(locks).length > 0 ? { ...locks } : undefined;
	return { structure, attention, locks: cleanLocks };
}

/**
 * 末尾 limit 件（純 calculation）。`slice(-0)` が全件になる JS の罠を封じる —
 * limit 0 以下・非有限は空、非整数は切り捨て（team-b review 反映）。
 */
export function takeRecent<T>(items: readonly T[], limit: number): T[] {
	const take = Number.isFinite(limit) ? Math.max(0, Math.floor(limit)) : 0;
	return take > 0 ? items.slice(-take) : [];
}

// actions（DOM 反映 / Solid mount / keyboard / MCP bridge expose）は gallery-panes.tsx へ。
