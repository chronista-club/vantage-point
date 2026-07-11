/**
 * sidebar の shell layout component。
 *
 * v1.0 柱 2。 3 段 layout (header / scrollable list / World widget) の骨格と、
 * 全 Project を 1 リストで accordion + Lane ツリーとして描画する。
 *
 * 旧「稼働中 / 一時停止中」 タブ分割は撤去した (2026-07-10)。 SP presence の再起動フラップで
 * project がタブ間を移動して見ているタブから消える体感バグを構造的に断つため、 全 project を
 * 常時 1 リストに出す。 停止中 project の起動 (▶) は ProjectAccordion が per-project で扱う。
 *
 * - PR-1: shell layout + Solid store の最小可視化。
 * - PR-2: Project accordion + Lane ツリー
 *   (stand icon / status / awaiting dot / mailbox icon / performer git meta)。
 *   操作 (click 選択・context menu・restart/delete・Add Performer form・DnD) は PR-3。
 *   World widget 本体は後続 increment。
 */
import { For, Show, createEffect, createMemo } from "solid-js";
import { CreoIcon } from "creoui-icons-web";
import { sidebar } from "./store";
import { sendIpc } from "./ipc";
import { resolveProjectOrder } from "./dnd";
import { ContextMenu } from "./ContextMenu";
import {
	deleteHintLabel,
	deleteHintVisible,
	laneSelectHintLabel,
	laneSelectHintVisible,
} from "./keybindings";
import { FileExplorer, FILE_EXPLORER_CSS } from "./FileExplorer";
import { LanePicker, LANE_PICKER_CSS } from "./LanePicker";
import { CommandPalette, COMMAND_PALETTE_CSS } from "./CommandPalette";
import { ProjectAccordion } from "./ProjectAccordion";
import { WorldWidget } from "./WorldWidget";

/**
 * 指定 path の project accordion を view にスクロールして一瞬 flash させる。
 * タブ切替後の DOM 反映を待つため requestAnimationFrame 越しに実行する。
 * path は slash や特殊文字を含むので querySelector 属性セレクタのエスケープを避け、
 * 全 `.vp-proj` を走査して `data-path` 一致で引く。
 */
function flashProject(path: string): void {
	requestAnimationFrame(() => {
		const target = Array.from(
			document.querySelectorAll<HTMLElement>(".vp-proj"),
		).find((el) => el.getAttribute("data-path") === path);
		if (!target) return;
		target.scrollIntoView({ block: "nearest", behavior: "smooth" });
		target.classList.remove("vp-proj-flash");
		// reflow を挟んで animation を確実に再スタートさせる。
		void target.offsetWidth;
		target.classList.add("vp-proj-flash");
	});
}

export function Shell() {
	// D&D 並べ替え順 (`currents_order`) を適用した全 Project を 1 リストで表示する。
	// `currents_order` は Rust が `process:reorder` で永続化する並び順 — これを読まないと
	// 並べ替え結果が re-push で消えてしまう (#124)。
	//
	// 旧「稼働中 / 一時停止中」 タブ分割は撤去した (2026-07-10)。 SP presence は再起動で
	// フラップするため、 project が running↔paused を行き来するたびタブ間を移動し、 見ている
	// タブから消える (= 「サイドバーから project が消えた」 体感バグの一因)。 全 project を
	// 常時 1 リストに出せば分類フラップが構造的に消える。 停止中 project の起動 affordance
	// (▶) は ProjectAccordion が per-project の state で出し分けるので影響しない。
	const ordered = createMemo(() =>
		resolveProjectOrder(sidebar.processes, sidebar.currents_order),
	);

	// 新規追加 project の discoverability: セッション途中で追加された project を flash して
	// 見失わせない (タブが無くなったので tab 切替は不要、 scroll + highlight だけ)。
	//
	// 初回 populate (= app 再起動時の一括ロード / 復元) は「追加」ではないので flash しない。
	// prevPaths を跨いで持ち、 最初に project 群を受け取った push を初期ロードとして素通り
	// させ、 それ以降の push で現れた差分だけを「追加」と見なす。
	let prevPaths = new Set<string>();
	let sawProjects = false;
	createEffect(() => {
		const procs = sidebar.processes;
		const cur = new Set(procs.map((p) => p.path));
		if (!sawProjects) {
			// 初回ロード確定は「非空の push を初めて受けた時」。 それまで (mount 直後の空 state
			// や project 0 件) は prev を更新して待つ。
			if (cur.size > 0) sawProjects = true;
			prevPaths = cur;
			return;
		}
		const added = procs.filter((p) => !prevPaths.has(p.path));
		prevPaths = cur;
		if (added.length === 0) return;
		// 複数同時追加は稀。 最後に現れた 1 件を代表として reveal する。
		flashProject(added[added.length - 1].path);
	});

	return (
		<div class="vp-sidebar-shell">
			<header class="vp-sidebar-header">
				<span class="vp-sidebar-title">CURRENTs</span>
				{/* project 追加: process:add IPC → Rust 側 native folder picker → 登録 (VP-203)。 */}
				<button
					class="vp-sidebar-add"
					title="プロジェクトを追加"
					onClick={() => sendIpc({ t: "process:add" })}
				>
					<CreoIcon name="ph:plus" size={13} />
				</button>
			</header>

			<div class="vp-sidebar-list">
				<Show
					when={sidebar.processes.length > 0}
					fallback={<div class="vp-sidebar-empty">プロジェクトなし</div>}
				>
					<For each={ordered()}>
						{(proc) => <ProjectAccordion proc={proc} />}
					</For>
				</Show>
			</div>

			<WorldWidget />

			{/* 右クリック context menu (Lane 行 / project ヘッダ 共通、 singleton、 VP-204 PR-1)。 */}
			<ContextMenu />

			{/* File Explorer overlay picker (singleton)。 LaneRow のフォルダボタン or Cmd+F で
          window.vpFilePicker.open(address) を呼ぶと、 lane workdir 全体を被せる overlay が
          出現してファイルを選べる。 選択すると Canvas (PP) に投げて dismiss する ephemeral。 */}
			<FileExplorer />

			{/* PR 445 `s` directive: Lane / project switcher picker overlay (singleton)。
          Cmd hold s で window.vpLanePicker.open() が呼ばれて出現、 lane / project を fuzzy 検索 + 選択。 */}
			<LanePicker />

			{/* GPUI 借用 #2: Command Palette (⌘K)。 全 Action (directive registry) を fuzzy 検索 + 実行。 */}
			<CommandPalette />

			{/* PR 445 `d` directive: 2-click delete confirm hint bar。 pending state 中だけ
          sidebar 下端に表示、 1 秒以内に 2 回目で execute、 timeout で auto-dismiss。 */}
			<Show when={deleteHintVisible()}>
				<div class="vp-delete-hint">
					<span class="vp-delete-hint-icon">⚠️</span>
					<span class="vp-delete-hint-label">{deleteHintLabel()}</span>
				</div>
			</Show>

			{/* PR 447 `l` directive: lane number switcher mode hint bar。 mode 中だけ表示。
          1-9 のキー押下で expanded project 内 lane を上から N 番目で lane:select。 5 秒 timeout。 */}
			<Show when={laneSelectHintVisible()}>
				<div class="vp-lane-select-hint">
					<span class="vp-lane-select-hint-icon">🔢</span>
					<span class="vp-lane-select-hint-label">{laneSelectHintLabel()}</span>
					<span class="vp-lane-select-hint-help">Esc to cancel</span>
				</div>
			</Show>
		</div>
	);
}

/**
 * shell layout の CSS。 creoui token (`var(--color-*)` / `var(--spacing-*)`) は
 * SIDEBAR_HTML_V2 が `creo-tokens.css` を inline 済なので、 ここは layout のみ定義する。
 */
export const SHELL_CSS = `
/* sidebar Live Token (--sb-*) の定義は :root に置く。 適用 (use site) は #sidebar-root
   以下に閉じているので他 pane を汚染しない。 :root 定義にする理由 = creo-ui Editor Mode
   (editor-host) の cssVarTarget が document.documentElement.style.setProperty で書くため:
   #sidebar-root 側に定義があると「近い祖先の定義が勝つ」で :root への書き込みがマスクされ、
   Ctrl+Shift+E の slider が効かなくなる (2026-07-11 Editor Mode 作業台化)。
   text scale 4 段 (Live Token): base=行タイトル/summary、 hint=行本文/menu/input、
   meta=ラベル/ヘッダ/stats、 micro=badge/kbd/footer/git meta。
   connector 系 (--sb-conn-*) は lane tree connector の演奏 knob: width=線幅 (縦棒/横枝/
   proj spine 共通)、 slot=connector box 幅 (= 横枝の長さ)、 dash=自走破線の dash 長、
   flow-beat=破線が 1 周期流れる秒数 (default = creo-ui timeline BPM 82.7 の 1 beat)。
   色 (hitl/auto) は探索用の flat token — 初期値は creo status token と同値。 */
:root{--sb-text-base:13px;--sb-text-hint:12px;--sb-text-meta:11px;--sb-text-micro:10px;
  --sb-conn-width:1.5px;--sb-conn-slot:16px;--sb-conn-dash:4px;--sb-conn-flow-beat:0.7255s;
  --sb-conn-hitl:var(--color-status-warning,#d49b3f);
  --sb-conn-auto:var(--color-status-info,#3fb9d4);}
html,body{margin:0;height:100%;overflow:hidden;}
/* SolidJS mount point。 height chain (html→body→#sidebar-root→shell) を繋ぐ。
   この規則が無いと shell が content 高さに collapse し、 window 下部に gap が出る。
   脱 TUI (2026-07): font / color / bg を #sidebar-root スコープに閉じる。 旧 html,body
   直書きは単一 WebView の document 全体を汚染し、 pane header まで 'VPMono' 12px に
   mono 化していた。 サイドバーを sans 全面化しつつ pane header への波及を断つ。 */
#sidebar-root{height:100%;
  background:var(--color-surface-bg-subtle);color:var(--color-text-primary);
  /* サイドバー全面 sans。 var() 2 段 fallback は vp-tokens.css 規約 (WKWebView が var()
     chain を invalidate するため use site で並べる)。 Mizolet 不在でも creo sans stack
     (-apple-system 等) に縮退し proper sans で描画される。 */
  font-family:var(--vp-font-sans),var(--typography-family-sans);
  /* sidebar 内の font-size は全て --sb-text-* 4 token を参照する (glyph 一点物 9px/14px を
     除く)。 定義は上の :root ブロック (Editor Mode の書き込み先と揃えるため)。 */
  font-size:var(--sb-text-base,13px);line-height:1.45;}
/* position:relative は FileExplorer overlay の inset:0 を sidebar 領域に閉じるために必要。
   無いと overlay が viewport 基準になり、 sidebar 外の領域 (= ContextMenu と重なる場所) に
   描画されて検索 input が見えなくなる (PR #439 dogfood feedback)。 */
.vp-sidebar-shell{position:relative;display:flex;flex-direction:column;height:100%;}
.vp-sidebar-header{flex:0 0 auto;display:flex;align-items:center;gap:6px;
  padding:10px var(--spacing-sm,10px);font-size:var(--sb-text-meta,11px);letter-spacing:.06em;
  font-weight:var(--typography-weight-semibold,600);color:var(--color-text-tertiary);
  border-bottom:1px solid var(--color-surface-border,#1f2233);user-select:none;}
.vp-sidebar-title{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-sidebar-add{margin-left:auto;display:inline-flex;align-items:center;padding:2px;
  border:none;background:transparent;color:var(--color-text-tertiary);cursor:pointer;
  border-radius:3px;flex:0 0 auto;transition:background .12s ease,color .12s ease;}
.vp-sidebar-add:hover{background:var(--color-surface-bg-emphasis);
  color:var(--color-brand-primary);}
.vp-sidebar-list{flex:1;overflow-y:auto;padding:var(--spacing-xs,4px) 0;}
.vp-sidebar-empty{padding:var(--spacing-sm,8px);color:var(--color-text-tertiary);
  font-size:var(--sb-text-meta,11px);}

/* Project accordion */
.vp-proj{margin:0;}
/* project が所有する tree spine (= 縦ライン)。 connector の縦棒 (::before) と同じ x に重ね、
   行間 padding の隙間を埋めて proj 領域から伸びる 1 本の連続縦線に見せる (SoC: 縦は proj、
   枝は connector)。 top:0 = summary 直下から、 bottom = 最後の lane 中央で止める。 */
.vp-proj-content{position:relative;}
.vp-proj-content::before{content:"";position:absolute;left:10.5px;top:0;bottom:17px;
  width:var(--sb-conn-width,1.5px);
  background:color-mix(in oklch,var(--color-brand-primary),transparent 62%);
  pointer-events:none;}
.vp-proj + .vp-proj{border-top:1px solid var(--color-surface-border,#1f2233);}
.vp-proj-summary{list-style:none;display:flex;align-items:center;gap:6px;
  padding:6px var(--spacing-sm,8px);cursor:pointer;font-size:var(--sb-text-base,13px);user-select:none;
  transition:background .1s ease;}
.vp-proj-summary::-webkit-details-marker{display:none;}
.vp-proj-summary:hover{background:var(--color-surface-bg-emphasis);}
.vp-proj-name{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-proj-hint{padding:6px 12px 6px 20px;font-size:var(--sb-text-meta,11px);
  color:var(--color-text-tertiary);font-style:italic;}
/* 新規追加 project の reveal flash — auto tab-switch と併用して見失わせない
   (Shell の createEffect が対象に .vp-proj-flash を付与)。summary 背景を一瞬 brand 色に。 */
@keyframes vp-proj-flash{0%{background:var(--color-brand-primary-subtle);}
  100%{background:transparent;}}
.vp-proj-flash > .vp-proj-summary{animation:vp-proj-flash 1.3s ease-out;}

/* Project D&D 並べ替え (#124) — summary を掴んで他 Project の上下に落とす。
   draggable は details 要素 (.vp-proj) に付く (WebKit の summary 活性化対策)。
   dragging = 掴み中を半透明、 drop-before/after = 挿入先を brand 色の線で示す。 */
.vp-proj-summary{cursor:grab;}
.vp-proj.dragging{opacity:.4;}
.vp-proj.drop-before{box-shadow:inset 0 2px 0 0 var(--color-brand-primary);}
.vp-proj.drop-after{box-shadow:inset 0 -2px 0 0 var(--color-brand-primary);}

/* Lane 行 */
/* ミニマム 1 行 (2026-05-30): icon + session title + 右端 block (meta/awaiting/files/mailbox)。
   2 段目 / "—" placeholder / Conductor ラベルは廃止、 nowrap で 1 行固定。 */
/* tree connector — 脱 TUI hybrid (2026-07, mako 選択): box-drawing glyph (VPMono) を
   CSS pseudo-element 描画に置換。 「線種 = control surrender FSM」 の意味論 (2026-05-30) は
   そのまま継承し、 質感だけ GUI 化 (zoom 非依存で crisp、 font metrics に揃え不要)。
   ::before = 縦棒 (tree corner: ├ は行全高 / └ = .last は中央止め、 常に solid)、
   ::after = 横枝 (線種 slot: conductor/HITL solid、 休眠 dotted、 自走 = 流れる破線)。
   縦棒 x=10.5px は .vp-proj-content::before の proj spine と同座標 (row padding-left 8px
   + left 2.5px)。 */
.vp-lane-connector{position:relative;flex:0 0 var(--sb-conn-slot,16px);align-self:stretch;
  user-select:none;}
.vp-lane-connector::before{content:"";position:absolute;left:2.5px;
  width:var(--sb-conn-width,1.5px);top:0;bottom:0;background:currentColor;}
.vp-lane-connector.last::before{bottom:50%;}
.vp-lane-connector::after{content:"";position:absolute;left:2.5px;right:2px;top:50%;
  margin-top:calc(var(--sb-conn-width,1.5px) / -2);
  border-top:var(--sb-conn-width,1.5px) solid currentColor;}
.vp-lane-connector.conn-dead::after{border-top-style:dotted;}
/* 自走 = 破線が右へ流れる (= control を手放して自分で進んでいる)。 周期 (--sb-conn-flow-beat)
   の default は creo-ui internal timeline BPM 82.7 の 1 beat = 60 / 82.7 ≈ 0.7255s。
   1 beat で 1 dash 周期 (= dash 長 × 2) 進む。 */
.vp-lane-connector.conn-auto::after{border-top:none;height:var(--sb-conn-width,1.5px);
  background:repeating-linear-gradient(90deg,currentColor 0 var(--sb-conn-dash,4px),
    transparent var(--sb-conn-dash,4px) calc(var(--sb-conn-dash,4px) * 2));
  animation:vp-conn-flow var(--sb-conn-flow-beat,.7255s) linear infinite;}
@keyframes vp-conn-flow{to{background-position:calc(var(--sb-conn-dash,4px) * 2) 0;}}
@media (prefers-reduced-motion:reduce){
  .vp-lane-connector.conn-auto::after{animation:none;}}
.vp-lane-connector.conn-conductor{
  color:color-mix(in oklch,var(--color-brand-primary),transparent 30%);}
.vp-lane-connector.conn-run{color:var(--color-text-tertiary);}
.vp-lane-connector.conn-dead{
  color:color-mix(in oklch,var(--color-text-tertiary),transparent 50%);}
.vp-lane-connector.conn-hitl{color:var(--sb-conn-hitl,var(--color-status-warning,#d49b3f));}
.vp-lane-connector.conn-auto{color:var(--sb-conn-auto,var(--color-status-info,#3fb9d4));}
.vp-lane-row{position:relative;display:flex;flex-wrap:nowrap;align-items:center;
  gap:4px;padding:8px var(--spacing-sm,8px) 8px 8px;font-size:var(--sb-text-hint,12px);cursor:pointer;
  transition:background .1s ease;}
.vp-lane-row:hover{background:var(--color-surface-bg-emphasis);}
.vp-lane-row + .vp-lane-row{border-top:1px solid
  color-mix(in oklch, var(--color-surface-border,#1f2233), transparent 60%);}
/* active lane は横線 (上下 border) で認識させる (= 縦線でなく横方向の帯で demarcate、 2026-05-30)。
   文字は brand-primary を少しだけ明るく (= white 16% 混合) して存在感を上げる。 */
.vp-lane-row.active{background:var(--color-brand-primary-subtle);
  color:color-mix(in oklch,var(--color-brand-primary),white 16%);font-weight:500;
  box-shadow:inset 0 2px 0 0 var(--color-brand-primary),
             inset 0 -2px 0 0 var(--color-brand-primary);}
.vp-lane-row.inactive{color:color-mix(in oklch, var(--color-text-secondary),
  transparent 45%);cursor:default;}
/* Conductor / Performer の indent 差は connector (縦棒 + 横枝) が担うため padding override 不要。 */
.vp-lane-icon{display:inline-flex;width:18px;justify-content:center;flex:0 0 auto;}
.vp-lane-row.inactive .vp-lane-icon{opacity:0.55;}
/* session title (= icon の右、 flex:1 で伸びて右端 block を押し出す)。 */
.vp-lane-title{flex:1 1 auto;min-width:0;overflow:hidden;text-overflow:ellipsis;
  white-space:nowrap;color:var(--color-text-secondary);}
/* fallback (= session title 未設定で proj 名 / performer 名を出す時) は dimmed で控えめに。 */
.vp-lane-title.is-fallback{color:var(--color-text-tertiary);opacity:0.7;}
.vp-lane-row.active .vp-lane-title{color:var(--color-brand-primary);opacity:1;}
/* 右端 block: meta / awaiting / files / mailbox を右寄せで横並び。 */
.vp-lane-right{display:flex;align-items:center;gap:5px;flex:0 0 auto;margin-left:auto;}
/* files / mailbox は hover 時のみ表示 (= noise 減)。 ただし mailbox unread と
   awaiting dot は signal なので常時表示。 */
.vp-lane-msg{display:inline-flex;color:var(--color-text-tertiary);opacity:0;
  transition:opacity .1s ease;}
.vp-lane-row:hover .vp-lane-msg{opacity:0.55;}
.vp-lane-msg.unread{color:var(--color-brand-primary);opacity:1;}
.vp-lane-row:hover .vp-lane-msg.unread{opacity:1;}
.vp-lane-meta{display:flex;gap:5px;font-size:var(--sb-text-micro,10px);color:var(--color-text-tertiary);
  white-space:nowrap;}
.vp-lane-meta .ahead{color:var(--color-status-info,#3fb9d4);}
.vp-lane-meta .behind{color:var(--color-status-warning,#d49b3f);}
.vp-lane-meta .dirty{color:var(--color-status-warning,#d49b3f);font-weight:500;}
.vp-lane-awaiting{width:6px;height:6px;border-radius:50%;
  background:var(--color-status-warning,#d49b3f);flex:0 0 auto;}

/* Add Performer「+」(active project) / Start「▶」(一時停止中 project) — summary 右端の
   action ボタン。 レイアウトは共通、 Start は起動 affordance として常時 brand 色。 */
.vp-proj-addperformer,.vp-proj-start{margin-left:auto;display:inline-flex;align-items:center;
  padding:2px;border:none;background:transparent;color:var(--color-text-tertiary);
  cursor:pointer;border-radius:3px;flex:0 0 auto;
  transition:background .12s ease,color .12s ease;}
.vp-proj-addperformer:hover,.vp-proj-addperformer.open,.vp-proj-start:hover{
  background:var(--color-surface-bg-emphasis);color:var(--color-brand-primary);}
.vp-proj-start{color:var(--color-brand-primary);}
.vp-add-performer-form{display:flex;flex-direction:column;gap:5px;
  padding:4px var(--spacing-sm,8px) 6px 14px;}
.vp-add-performer-input{padding:5px 8px;border:1px solid var(--color-surface-border,#1f2233);
  background:var(--color-surface-bg-base);color:var(--color-text-primary);
  border-radius:var(--radius-sm,6px);font-family:inherit;font-size:var(--sb-text-meta,11px);
  box-sizing:border-box;}
.vp-add-performer-input:focus{outline:none;border-color:var(--color-brand-primary);}
.vp-add-performer-actions{display:flex;justify-content:flex-end;gap:6px;}
.vp-add-performer-actions button{padding:3px 10px;
  border:1px solid var(--color-surface-border,#1f2233);background:transparent;
  color:var(--color-text-secondary);border-radius:var(--radius-sm,6px);cursor:pointer;
  font-size:var(--sb-text-micro,10px);font-family:inherit;transition:background .12s ease,color .12s ease;}
.vp-add-performer-actions button:hover{background:var(--color-surface-bg-emphasis);
  color:var(--color-text-primary);}
.vp-add-performer-actions button.primary{background:var(--color-brand-primary-subtle);
  color:var(--color-brand-primary);border-color:var(--color-brand-primary-subtle);}

/* World widget (sidebar 最下部、 collapsed 1 行 + expanded 詳細の accordion) */
.vp-world{flex:0 0 auto;border-top:1px solid var(--color-surface-border,#1f2233);
  background:var(--color-surface-bg-base);}
.vp-world-summary{list-style:none;display:flex;align-items:center;gap:6px;
  padding:var(--spacing-xs,4px) var(--spacing-sm,8px);cursor:pointer;
  font-size:var(--sb-text-meta,11px);color:var(--color-text-secondary);user-select:none;}
.vp-world-summary::-webkit-details-marker{display:none;}
.vp-world-summary:hover{background:var(--color-surface-bg-emphasis);}
.vp-world-dot{width:6px;height:6px;border-radius:50%;flex:0 0 auto;
  background:var(--color-status-success,#3fb950);}
.vp-world-dot.offline{background:var(--color-status-error,#d4444c);}
/* L1 lifecycle: project 行の SP presence dot（●◐○）。daemon-canonical の接続状態を可視化。
   default は unregistered 相当（dim）、 各 state class で色付け。 connecting は pulse。 */
.vp-proj-presence-dot{width:6px;height:6px;border-radius:50%;flex:0 0 auto;
  background:var(--color-text-tertiary,#6e7681);opacity:.5;}
.vp-proj-presence-dot.connected{background:var(--color-status-success,#3fb950);opacity:1;}
.vp-proj-presence-dot.connecting{background:var(--color-status-warning,#d49b3f);opacity:1;
  animation:vp-presence-pulse 1.1s ease-in-out infinite;}
.vp-proj-presence-dot.disconnected{background:var(--color-status-error,#d4444c);opacity:1;}
.vp-proj-presence-dot.unregistered{background:var(--color-text-tertiary,#6e7681);opacity:.5;}
@keyframes vp-presence-pulse{0%,100%{opacity:1;}50%{opacity:.35;}}
.vp-world-line{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;
  font-variant-numeric:tabular-nums;}
.vp-world-detail{padding:var(--spacing-xs,4px) var(--spacing-sm,8px);
  border-top:1px dashed var(--color-surface-border,#1f2233);}
.vp-world-stat{display:flex;justify-content:space-between;font-size:var(--sb-text-meta,11px);
  padding:1px 0;}
.vp-world-stat .k{color:var(--color-text-tertiary);}
.vp-world-stat .v{color:var(--color-text-primary);font-weight:500;
  font-variant-numeric:tabular-nums;}

/* Bastet 🧲 — World scope の Devices セクション (stand row + device count badge) */
.vp-devices{flex:0 0 auto;border-top:1px solid var(--color-surface-border,#1f2233);}
.vp-stand-row{position:relative;display:flex;align-items:center;gap:6px;
  padding:5px var(--spacing-sm,10px);cursor:pointer;font-size:var(--sb-text-hint,12px);
  color:var(--color-text-secondary);}
.vp-stand-row:hover{background:var(--color-surface-bg-emphasis);}
.vp-stand-row.active{background:var(--color-brand-primary-subtle);
  color:var(--color-brand-primary);}
.vp-stand-icon{display:flex;align-items:center;flex:0 0 auto;}
.vp-stand-title{flex:1 1 auto;overflow:hidden;text-overflow:ellipsis;
  white-space:nowrap;}
.vp-stand-badge{flex:0 0 auto;font-size:var(--sb-text-micro,10px);padding:1px 6px;border-radius:8px;
  background:var(--color-brand-primary-subtle);color:var(--color-brand-primary);
  font-variant-numeric:tabular-nums;}

/* Lane 行 右クリック context menu (VP-204 PR-1、 singleton popup) */
.vp-ctx-backdrop{position:fixed;inset:0;z-index:9998;}
.vp-ctx-menu{position:fixed;z-index:9999;min-width:180px;
  background:var(--color-surface-bg-base);
  border:1px solid var(--color-surface-border,#1f2233);
  border-radius:var(--radius-md,6px);box-shadow:0 8px 24px rgba(0,0,0,.4);
  padding:4px 0;font-size:var(--sb-text-hint,12px);user-select:none;}
.vp-ctx-header{padding:4px 14px 6px;font-size:var(--sb-text-micro,10px);
  color:var(--color-text-tertiary);
  border-bottom:1px solid var(--color-surface-border,#1f2233);
  margin-bottom:4px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-ctx-item{padding:6px 14px;cursor:pointer;display:flex;align-items:center;
  gap:8px;color:var(--color-text-secondary);
  transition:background .1s ease,color .1s ease;}
.vp-ctx-item:hover{background:var(--color-surface-bg-emphasis);
  color:var(--color-text-primary);}
.vp-ctx-item.danger:hover{background:var(--color-status-error,#d4444c);color:#fff;}
.vp-ctx-item.danger.confirming{background:var(--color-status-error,#d4444c);
  color:#fff;}

/* FileExplorer overlay の z-index は ContextMenu (.vp-ctx-backdrop=9998 / .vp-ctx-menu=9999)
   より上に置く。 ContextMenu は position:fixed で WebView 全体を起点とするため、 overlay の
   z-index が低いと picker 上に context menu が突き抜けて描画される
   (moody-blues PR #439 final review Issue 1、 dogfood で実機目撃済)。 */
/* Lane row のフォルダピッカー起動ボタン (FileExplorer overlay を開く trigger) */
.vp-lane-files-btn{display:inline-flex;align-items:center;padding:1px 3px;
  border:none;background:transparent;color:var(--color-text-tertiary);
  cursor:pointer;border-radius:3px;flex:0 0 auto;opacity:0;
  transition:background .12s ease,color .12s ease,opacity .12s ease;}
.vp-lane-row:hover .vp-lane-files-btn{opacity:1;}
.vp-lane-files-btn:hover{background:var(--color-surface-bg-emphasis);
  color:var(--color-brand-primary);}
${FILE_EXPLORER_CSS}
${LANE_PICKER_CSS}
${COMMAND_PALETTE_CSS}
`;
