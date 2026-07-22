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
 * story registry。実 component（EchoesHeader / HistoryStrip 等）は mount-function 型で
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

// actions（DOM 反映 / Solid mount / keyboard）は gallery-panes.tsx へ（LE-P2 で分離）。
