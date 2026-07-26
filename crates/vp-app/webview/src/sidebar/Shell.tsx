/**
 * sidebar の shell layout component。
 *
 * v1.0 柱 2。 3 段 layout (header / scrollable list / Daemon widget) の骨格と、
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
 *   Daemon widget 本体は後続 increment。
 */
import { For, Show, createEffect, createMemo } from "solid-js";
import { CreoIcon } from "@chronista-club/creo-ui-icons-web";
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
import { WirePanel, WIRE_PANEL_CSS } from "./WirePanel";
import { LanePicker, LANE_PICKER_CSS } from "./LanePicker";
import { CommandPalette, COMMAND_PALETTE_CSS } from "./CommandPalette";
import { ProjectAccordion } from "./ProjectAccordion";
import { DaemonWidget } from "./DaemonWidget";

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

			<DaemonWidget />

			{/* 右クリック context menu (Lane 行 / project ヘッダ 共通、 singleton、 VP-204 PR-1)。 */}
			<ContextMenu />

			{/* File Explorer overlay picker (singleton)。 LaneRow のフォルダボタン or Cmd+F で
          window.vpFilePicker.open(address) を呼ぶと、 lane workdir 全体を被せる overlay が
          出現してファイルを選べる。 選択すると Canvas (board) に投げて dismiss する ephemeral。 */}
			<FileExplorer />

			{/* Wire Inbox overlay panel (doc 34 §4 V1、 singleton)。 LaneRow の mailbox badge click で
          window.vpWire.open(address) が呼ばれ、 選択 lane の wire 履歴 (read-only) + ack を表示する。 */}
			<WirePanel />

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
   connector 系 (--sb-conn-*) は lane tree connector の演奏 knob: width=線幅、 slot=gutter 幅、
   dash=idle 破線の dash 長、 flow-beat=HITL pulse の 1 beat (= creo-ui timeline BPM 82.7)、
   photon-period=photon が spine を root→末端に走る周期、 glow=発光半径。
   色 (hitl/auto) は Light Grid palette の magenta/cyan (Editor Mode picker で演奏可)。

   Light Grid palette (--lg-*, Step 7 = Direction B「Light Grid / TRON origin」再スキン):
   sidebar スコープ専用の静的 palette。 app 全体の paradox-violet テーマは変えない —
   適用は #sidebar-root 以下に封じ込める (定義が :root なのは Editor Mode の書き込み先と
   揃えるため。 定義自体は inert)。 視覚仕様の SSOT = artifact c203944c (mako 承認済)。
   2026-07-22: conn 2 hue を magenta/cyan → coral/yellow に更新 (doc 48 editor bridge の
   初回 dogfood で mako × CC が live 調整して確定 — auto=mako picker / hitl=CC editor_set)。 */
:root{--sb-text-base:13px;--sb-text-hint:12px;--sb-text-meta:11px;--sb-text-micro:10px;
  --sb-conn-width:2px;--sb-conn-slot:22px;--sb-conn-dash:4px;--sb-conn-flow-beat:0.7255s;
  --sb-photon-period:1800ms;--sb-glow:6px;
  --sb-conn-hitl:#FF4A2D;
  --sb-conn-auto:#FFF76B;
  --lg-void:#05070A;--lg-void-2:#080B11;--lg-panel:#0A0E15;
  --lg-grid:#0E2A33;--lg-hairline:#12222b;
  --lg-cyan-dim:#1C6C7C;--lg-hot:#EAFBFF;--lg-mute:#5C7A85;--lg-mute-2:#38525b;}
html,body{margin:0;height:100%;overflow:hidden;}
/* SolidJS mount point。 height chain (html→body→#sidebar-root→shell) を繋ぐ。
   この規則が無いと shell が content 高さに collapse し、 window 下部に gap が出る。
   脱 TUI (2026-07): font / color / bg を #sidebar-root スコープに閉じる。 旧 html,body
   直書きは単一 WebView の document 全体を汚染し、 pane header まで 'VPMono' 12px に
   mono 化していた。 サイドバーを sans 全面化しつつ pane header への波及を断つ。 */
#sidebar-root{height:100%;position:relative;
  /* Light Grid: 地は void。 sidebar スコープの再スキンはここから下の .vp-* 系にのみ効く。 */
  background:var(--lg-void,#05070A);color:var(--lg-hot,#EAFBFF);
  /* サイドバー全面 sans (font zero-start: --vp-font-sans = 'Gen Interface JP')。 var() 2 段
     fallback は vp-tokens.css 規約 (WKWebView が var() chain を invalidate するため use site
     で並べる)。 未 install 環境でも creo sans stack に縮退し proper sans で描画される。 */
  font-family:var(--vp-font-sans),var(--typography-family-sans);
  /* sidebar 内の font-size は全て --sb-text-* 4 token を参照する (glyph 一点物 9px/14px を
     除く)。 定義は上の :root ブロック (Editor Mode の書き込み先と揃えるため)。 */
  font-size:var(--sb-text-base,13px);line-height:1.45;}
/* TRON grid ambience — sidebar 背景に 1 枚だけ (course-correction 2026-07-11: project
   カード上の grid は行を横切る scanline ノイズになるため撤去、 ambience はここに集約)。
   2 軸 grid + radial mask で上部中央から溶ける。 opacity 5% = 気配だけ。 */
#sidebar-root::before{content:"";position:absolute;inset:0;pointer-events:none;
  background-image:
    linear-gradient(var(--lg-grid,#0E2A33) 1px,transparent 1px),
    linear-gradient(90deg,var(--lg-grid,#0E2A33) 1px,transparent 1px);
  background-size:44px 44px;
  -webkit-mask-image:radial-gradient(340px 480px at 50% 16%,#000 0%,transparent 76%);
  mask-image:radial-gradient(340px 480px at 50% 16%,#000 0%,transparent 76%);
  opacity:.05;}
/* position:relative は FileExplorer overlay の inset:0 を sidebar 領域に閉じるために必要。
   無いと overlay が viewport 基準になり、 sidebar 外の領域 (= ContextMenu と重なる場所) に
   描画されて検索 input が見えなくなる (PR #439 dogfood feedback)。
   (+ Light Grid: ::before の ambience grid より上に content を置く役も担う) */
.vp-sidebar-shell{position:relative;display:flex;flex-direction:column;height:100%;}
/* 横線ゼロ方針 (mako 019f50fe): 画面に残ってよい横線は session tap だけ。
   header 下線 / Daemon・Devices 上線 / detail 破線は全削除、 区切りは spacing で。 */
.vp-sidebar-header{flex:0 0 auto;display:flex;align-items:center;gap:6px;
  padding:12px 12px 8px;font-size:var(--sb-text-micro,10px);letter-spacing:.14em;
  text-transform:uppercase;font-weight:var(--typography-weight-semibold,600);
  color:var(--lg-mute,#5C7A85);user-select:none;}
.vp-sidebar-title{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-sidebar-add{margin-left:auto;display:inline-flex;align-items:center;padding:2px;
  border:none;background:transparent;color:var(--lg-mute,#5C7A85);cursor:pointer;
  border-radius:3px;flex:0 0 auto;transition:background .12s ease,color .12s ease;}
.vp-sidebar-add:hover{background:#ffffff08;
  color:var(--sb-conn-auto,#FFF76B);}
.vp-sidebar-list{flex:1;overflow-y:auto;padding:0 0 10px;}
.vp-sidebar-empty{padding:var(--spacing-sm,8px);color:var(--lg-mute,#5C7A85);
  font-size:var(--sb-text-meta,11px);}

/* Project accordion — Light Grid: project = 地 (ground)。 発光させず void に沈む静かな地形。
   faint fill (#ffffff04) + inset hairline ring のみ (course-correction 2026-07-11:
   カード上の grid テクスチャは行が透明なため文字を横切る scanline ノイズになる → 撤去、
   ambience は #sidebar-root::before の 1 枚に集約)。
   glow なし — 図 (= session の current / photon) を引き立てるため必ず後退させる。 */
.vp-proj{margin:8px 8px 0;border-radius:11px;
  background:#ffffff04;
  box-shadow:inset 0 0 0 1px #ffffff08;
  padding:2px 4px 6px;}
/* project が所有する current-spine (= 縦ライン)。 Light Grid: session が地の上を走る
   light-trail の幹。 cyan-dim → 暗 の gradient で下に減衰。 top:0 = summary 直下から、
   bottom = 最後の lane 中央で止める。 connector の tap (::before) がここから row へ分岐する。 */
.vp-proj-content{position:relative;}
.vp-proj-content::before{content:"";position:absolute;left:10.5px;top:0;bottom:17px;
  width:var(--sb-conn-width,2px);border-radius:2px;
  background:linear-gradient(var(--lg-cyan-dim,#1C6C7C),#123039);
  pointer-events:none;}
/* photon = 文字通りの「current」。 mako motion 方針 (019f50ff): 常時アニメ禁止 —
   イベント駆動の one-shot のみ。 lane が working に遷移した瞬間に ProjectAccordion が
   .photon-fire を 1 回付与 → spine を root→末端に 1 度だけ走って消える。 定常状態では
   描画しない (base opacity:0)。 glow は 1 層に抑制 (quiet pass 019f5100)。
   iteration は Live Token (--sb-photon-loop、 default 1) — REPL で infinite にも戻せる。 */
.vp-proj-content::after{content:"";position:absolute;
  left:calc(10.5px + var(--sb-conn-width,2px) / 2 - 4px);top:-4px;
  width:8px;height:8px;border-radius:50%;
  background:var(--sb-conn-auto,#FFF76B);
  box-shadow:0 0 var(--sb-glow,6px) 2px
    color-mix(in srgb,var(--sb-conn-auto,#FFF76B),transparent 40%);
  opacity:0;pointer-events:none;}
.vp-proj-content.photon-fire::after{
  animation:lg-photon var(--sb-photon-period,1800ms) cubic-bezier(.55,0,.5,1)
    var(--sb-photon-loop,1);}
@keyframes lg-photon{
  0%{top:-4px;opacity:0;}
  12%{opacity:1;}
  82%{opacity:1;}
  100%{top:calc(100% - 21px);opacity:0;}}
@media (prefers-reduced-motion:reduce){
  /* one-shot 前提なので reduced-motion では単に走らせない (定常は元々無表示)。 */
  .vp-proj-content.photon-fire::after{animation:none;}}
.vp-proj + .vp-proj{border-top:none;}
/* project ラベル = 地の目印 (quiet ground marker)。 muted uppercase の小さい tab、 発光なし。
   course-correction 2026-07-11: 「地なのに図として主張」しないようさらに小さく (10px)、
   tracking も .15em に詰める。 weight は明示 400 (body の 300 継承より一段だけ立てる)。 */
.vp-proj-summary{list-style:none;display:flex;align-items:center;gap:7px;
  padding:7px 8px 5px;cursor:pointer;user-select:none;
  font-size:10px;letter-spacing:.14em;text-transform:uppercase;font-weight:400;
  color:var(--lg-mute-2,#38525b);
  transition:color .12s ease;}
.vp-proj-summary::-webkit-details-marker{display:none;}
.vp-proj-summary:hover{color:var(--lg-mute,#5C7A85);}
.vp-proj-name{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-proj-hint{padding:6px 12px 6px 20px;font-size:var(--sb-text-meta,11px);
  color:var(--lg-mute,#5C7A85);font-style:italic;}
/* 新規追加 project の reveal flash — auto tab-switch と併用して見失わせない
   (Shell の createEffect が対象に .vp-proj-flash を付与)。summary 背景を一瞬 brand 色に。 */
@keyframes vp-proj-flash{0%{background:color-mix(in srgb,var(--sb-conn-auto,#FFF76B),transparent 92%);}
  100%{background:transparent;}}
.vp-proj-flash > .vp-proj-summary{animation:vp-proj-flash 1.3s ease-out;}

/* Project D&D 並べ替え (#124) — summary を掴んで他 Project の上下に落とす。
   draggable は details 要素 (.vp-proj) に付く (WebKit の summary 活性化対策)。
   dragging = 掴み中を半透明、 drop-before/after = 挿入先を brand 色の線で示す。 */
.vp-proj-summary{cursor:grab;}
.vp-proj.dragging{opacity:.4;}
/* drop marker は cyan。 ground の inset ring (box-shadow 単一 property) を失わないよう併記。 */
.vp-proj.drop-before{box-shadow:inset 0 2px 0 0 var(--sb-conn-auto,#FFF76B),
  inset 0 0 0 1px #ffffff08;}
.vp-proj.drop-after{box-shadow:inset 0 -2px 0 0 var(--sb-conn-auto,#FFF76B),
  inset 0 0 0 1px #ffffff08;}

/* Lane 行 */
/* ミニマム 1 行 (2026-05-30): icon + session title + 右端 block (meta/awaiting/files/mailbox)。
   2 段目 / "—" placeholder / Conductor ラベルは廃止、 nowrap で 1 行固定。 */
/* tree connector — Light Grid (Step 7): 「tap + node」言語。 spine (proj 所有の縦線) から
   row へ分岐する横 tap (::before) と、 行の結節点 node (::after) で状態を語る。
   「線種/形 = control surrender FSM」の意味論は glyph 時代 (2026-05-30) から連続:
   - working (conn-auto): solid cyan tap + cyan node (glow)
   - idle (conn-dead): dim 破線 tap + 中空 node (縁 dim)
   - needs-you (conn-hitl): magenta tap + diamond node が downbeat (725ms) で pulse
   - root (conn-conductor): spine の頭 = cyan diamond head (tap なし)
   tap 起点 x=2.5px は spine (10.5px) と同座標 (row padding-left 8px + 2.5px)。 */
.vp-lane-connector{position:relative;flex:0 0 var(--sb-conn-slot,22px);align-self:stretch;
  user-select:none;}
.vp-lane-connector::before{content:"";position:absolute;left:2.5px;right:11px;top:50%;
  height:var(--sb-conn-width,2px);
  margin-top:calc(var(--sb-conn-width,2px) / -2);border-radius:2px;}
.vp-lane-connector::after{content:"";position:absolute;right:2px;top:50%;
  width:8px;height:8px;margin-top:-4px;border-radius:50%;}
/* working = solid cyan tap + ベタ塗り小径 node。 quiet pass (019f5100): glow なし —
   cyan が許されるのはこの tap + node だけ、 光量の主張は needs-you に譲る。 */
.vp-lane-connector.conn-auto::before{background:var(--sb-conn-auto,#FFF76B);}
.vp-lane-connector.conn-auto::after{width:6px;height:6px;margin-top:-3px;
  background:var(--sb-conn-auto,#FFF76B);}
/* conn-run は FSM 上 dead path (working は conn-auto に集約) — 保険で working と同扱い */
.vp-lane-connector.conn-run::before{background:var(--sb-conn-auto,#FFF76B);}
.vp-lane-connector.conn-run::after{width:6px;height:6px;margin-top:-3px;
  background:var(--sb-conn-auto,#FFF76B);}
/* idle = ほぼ消える (quiet pass): 極薄の破線 tap + 中空 node。 */
.vp-lane-connector.conn-dead::before{background-image:repeating-linear-gradient(90deg,
  color-mix(in srgb,var(--lg-cyan-dim,#1C6C7C),transparent 45%) 0 calc(var(--sb-conn-dash,4px) / 2),
  transparent calc(var(--sb-conn-dash,4px) / 2) var(--sb-conn-dash,4px));}
.vp-lane-connector.conn-dead::after{background:#123039;
  border:1px solid color-mix(in srgb,var(--lg-cyan-dim,#1C6C7C),transparent 40%);}
/* needs-you / HITL = magenta tap + diamond node。 唯一 glow を許される状態 (quiet pass)。
   pulse は「needs-you に入った瞬間に 1 回だけ」 (one-shot、 常時 pulse 禁止 019f50ff) —
   .conn-hitl が付いた時に animation が 1 度走り、 終わると静的 glow に落ち着く。
   data source は既存 awaiting_input (laneConnector が conn-hitl を導出済) — 配線済み。 */
.vp-lane-connector.conn-hitl::before{background:var(--sb-conn-hitl,#FF4A2D);}
.vp-lane-connector.conn-hitl::after{width:9px;height:9px;margin-top:-4.5px;
  background:var(--sb-conn-hitl,#FF4A2D);border-radius:2px;transform:rotate(45deg);
  box-shadow:0 0 var(--sb-glow,6px) 1px
    color-mix(in srgb,var(--sb-conn-hitl,#FF4A2D),transparent 55%);
  animation:lg-hitl var(--sb-conn-flow-beat,.7255s) steps(1,end) var(--sb-hitl-loop,1);}
@keyframes lg-hitl{
  0%,60%{opacity:1;box-shadow:0 0 calc(var(--sb-glow,6px) * 1.4) 2px
    color-mix(in srgb,var(--sb-conn-hitl,#FF4A2D),transparent 40%);}
  61%,100%{opacity:.55;box-shadow:0 0 calc(var(--sb-glow,6px) * .6) 1px
    color-mix(in srgb,var(--sb-conn-hitl,#FF4A2D),transparent 67%);}}
@media (prefers-reduced-motion:reduce){
  .vp-lane-connector.conn-hitl::after{animation:none;}}
/* root (conductor) = spine の頭。 tap は描かず diamond を頭石として置く。 quiet pass:
   cyan は working 専用なので頭石は cyan-dim のベタ塗り (glow なし)。 */
.vp-lane-connector.conn-conductor::before{display:none;}
.vp-lane-connector.conn-conductor::after{width:9px;height:9px;margin-top:-4.5px;
  left:6.5px;right:auto;border-radius:2px;transform:rotate(45deg);
  background:var(--lg-cyan-dim,#1C6C7C);}
/* flex-wrap:wrap = cwd (地) を 2 行目へ折り返すため。 multi-line flex では align-items /
   align-self が flex line 単位で効くので、 1 行目の内部整列 (icon/title の縦位置、 connector の
   tap/node が title 中央を指すこと) は不変。 title は min-width:0 で縮むので意図せぬ折返しは起きない。 */
.vp-lane-row{position:relative;display:flex;flex-wrap:wrap;align-items:center;
  gap:4px;padding:8px var(--spacing-sm,8px) 8px 8px;font-size:var(--sb-text-hint,12px);cursor:pointer;
  border-radius:8px;transition:background .1s ease;}
.vp-lane-row:hover{background:#ffffff06;}
/* row 間の旧 border は撤去 — 地は無地 (行を横切る線は作らない)。 */
.vp-lane-row + .vp-lane-row{border-top:none;}
/* active (= 選択中) lane — 選択表現は faint tint のみ (mako 019f5114: ブラケット/
   ブロック/バー等のアクセント要素はゼロ)。 tint は判別性のため僅かに強め (8%)、
   光り物は増やさない。 「光る」 のは state (photon / node) の仕事。 */
.vp-lane-row.active{background:color-mix(in srgb,var(--sb-conn-auto,#FFF76B),transparent 92%);}
.vp-lane-row.inactive{color:var(--lg-mute,#5C7A85);cursor:default;}
/* root session (= conductor、 spine の頭)。 quiet pass (019f5100): cyan wash / glyph glow は
   撤去、 weight 600 だけで静かに立たせる (行 tint と glyph 彩色は光の総量を増やすため落とす)。 */
.vp-lane-row:not(.performer){font-weight:600;letter-spacing:-.01em;margin-top:2px;}
/* Conductor / Performer の indent 差は connector (縦棒 + 横枝) が担うため padding override 不要。 */
.vp-lane-icon{display:inline-flex;width:18px;justify-content:center;flex:0 0 auto;}
.vp-lane-row.inactive .vp-lane-icon{opacity:0.55;}
/* 開発起点マーカー (doc 44 D4)。stand icon と title の間に置く「属性」の層。
   mute 色 = git meta / cwd と同じ最も引っ込んだ階層で、光らせない
   (光 = needs-you の専有)。起点は常時真なので、目立たせると常に鳴る警告になる。 */
.vp-lane-origin{display:inline-flex;flex:0 0 auto;margin-right:4px;
  color:var(--lg-mute,#5C7A85);}
.vp-lane-row.inactive .vp-lane-origin{opacity:0.55;}
/* Lane D&D 並べ替え (doc 44 §12) — project 側 (.vp-proj) と同じ語彙で揃える:
   dragging = 掴み中を半透明、 drop-before/after = 挿入先を brand 色の線。
   落とせるのは同じ project の lane 同士だけ (帳簿は project ごとに 1 本)。 */
.vp-lane-row.dragging{opacity:.4;}
.vp-lane-row.drop-before{box-shadow:inset 0 2px 0 0 var(--sb-conn-auto,#FFF76B);}
.vp-lane-row.drop-after{box-shadow:inset 0 -2px 0 0 var(--sb-conn-auto,#FFF76B);}
/* session title (= icon の右、 flex:1 で伸びて右端 block を押し出す)。 */
.vp-lane-title{flex:1 1 auto;min-width:0;overflow:hidden;text-overflow:ellipsis;
  white-space:nowrap;color:color-mix(in srgb,var(--lg-hot,#EAFBFF),transparent 18%);}
/* fallback (= session title 未設定で proj 名 / performer 名を出す時) は dimmed で控えめに。 */
.vp-lane-title.is-fallback{color:var(--lg-mute,#5C7A85);}
.vp-lane-row.inactive .vp-lane-title{color:var(--lg-mute,#5C7A85);}
.vp-lane-row.active .vp-lane-title{color:var(--lg-hot,#EAFBFF);}
/* 地 (ground): lane の cwd。 図 (title / state) の後ろに沈める層。 mute-2 / micro / mono =
   git meta と同じ最も引っ込んだ層で、 光らせない (光 = 注意は needs-you の専有)。
   indent は connector slot + icon + gap 分 = title の左端に揃える。 */
.vp-lane-cwd{flex:0 0 100%;box-sizing:border-box;min-width:0;overflow:hidden;
  text-overflow:ellipsis;white-space:nowrap;
  padding-left:calc(var(--sb-conn-slot,22px) + 18px + 8px);
  font-family:var(--vp-font-mono),var(--typography-family-mono);
  font-size:var(--sb-text-micro,10px);color:var(--lg-mute-2,#38525b);}
/* active 行だけ僅かに持ち上げる (可読性)。 glow は足さない — 地は地のまま。 */
.vp-lane-row.active .vp-lane-cwd{color:var(--lg-mute,#5C7A85);}
/* state 文字 (working / needs you) — 右端、 mono micro uppercase。 quiet pass (019f5100):
   muted 一色、 needs-you だけ magenta。 idle は文字ごと出さない (stateLabel が null)。 */
.vp-lane-state{flex:0 0 auto;font-family:var(--vp-font-mono),var(--typography-family-mono);
  font-size:var(--sb-text-micro,10px);letter-spacing:.04em;text-transform:uppercase;
  color:var(--lg-mute-2,#38525b);font-variant-numeric:tabular-nums;white-space:nowrap;}
.vp-lane-connector.conn-hitl ~ .vp-lane-right .vp-lane-state{
  color:var(--sb-conn-hitl,#FF4A2D);}
/* 右端 block: meta / awaiting / files / mailbox を右寄せで横並び。 */
.vp-lane-right{display:flex;align-items:center;gap:5px;flex:0 0 auto;margin-left:auto;}
/* files / mailbox は hover 時のみ表示 (= noise 減)。 ただし mailbox unread と
   awaiting dot は signal なので常時表示。 */
.vp-lane-msg{display:inline-flex;color:var(--lg-mute,#5C7A85);opacity:0;
  transition:opacity .1s ease;}
.vp-lane-row:hover .vp-lane-msg{opacity:0.55;}
.vp-lane-msg.unread{color:var(--lg-cyan-dim,#1C6C7C);opacity:1;}
.vp-lane-row:hover .vp-lane-msg.unread{opacity:1;}
/* git meta (IDs & counts) は mono 面 (UDEV Gothic NF)。 ahead=cyan-dim / behind・dirty=magenta 系。 */
.vp-lane-meta{display:flex;gap:5px;font-size:var(--sb-text-micro,10px);color:var(--lg-mute-2,#38525b);
  font-family:var(--vp-font-mono),var(--typography-family-mono);
  font-variant-numeric:tabular-nums;white-space:nowrap;}
.vp-lane-meta .ahead{color:var(--lg-cyan-dim,#1C6C7C);}
.vp-lane-meta .behind{color:color-mix(in srgb,var(--sb-conn-hitl,#FF4A2D),transparent 30%);}
.vp-lane-meta .dirty{color:color-mix(in srgb,var(--sb-conn-hitl,#FF4A2D),transparent 30%);
  font-weight:500;}
/* awaiting dot — needs-you 言語 (magenta) に従属。 diamond node と同源の信号。 */
.vp-lane-awaiting{width:6px;height:6px;border-radius:50%;
  background:var(--sb-conn-hitl,#FF4A2D);flex:0 0 auto;}
/* canvas 着信 (D) — board に絵が届いた info 信号。 needs-you(magenta glow)とは別語彙。
   Light Grid「光=注意」に従い bright(--lg-hot)で目を引くが glow(pulse)は付けない
   (glow は needs-you 専用)。 easel icon は bundled subset 外で不可視だったため pure-CSS の
   小 square (canvas/frame メタファ) に。 awaiting の円 / mailbox の封筒と形で区別。 */
.vp-lane-canvas{width:7px;height:7px;border-radius:2px;
  background:var(--lg-hot,#EAFBFF);flex:0 0 auto;}

/* Add Performer「+」(active project) / Start「▶」(一時停止中 project) — summary 右端の
   action ボタン。 レイアウトは共通、 Start は起動 affordance として常時 brand 色。 */
.vp-proj-addperformer,.vp-proj-start{margin-left:auto;display:inline-flex;align-items:center;
  padding:2px;border:none;background:transparent;color:var(--lg-mute,#5C7A85);
  cursor:pointer;border-radius:3px;flex:0 0 auto;
  transition:background .12s ease,color .12s ease;}
.vp-proj-addperformer:hover,.vp-proj-addperformer.open,.vp-proj-start:hover{
  background:#ffffff08;color:var(--sb-conn-auto,#FFF76B);}
/* Start ▶ も quiet: 定常は muted、 hover 時のみ cyan (interaction feedback)。 */
.vp-proj-start{color:var(--lg-mute,#5C7A85);}
.vp-add-performer-form{display:flex;flex-direction:column;gap:5px;
  padding:4px var(--spacing-sm,8px) 6px 14px;}
.vp-add-performer-input{padding:5px 8px;border:1px solid var(--lg-hairline,#12222b);
  background:var(--lg-panel,#0A0E15);color:var(--lg-hot,#EAFBFF);
  border-radius:var(--radius-sm,6px);font-family:inherit;font-size:var(--sb-text-meta,11px);
  box-sizing:border-box;}
.vp-add-performer-input:focus{outline:none;border-color:var(--sb-conn-auto,#FFF76B);}
.vp-add-performer-stand{cursor:pointer;appearance:none;-webkit-appearance:none;
  background-image:linear-gradient(45deg,transparent 50%,var(--lg-hot,#EAFBFF) 50%),
    linear-gradient(135deg,var(--lg-hot,#EAFBFF) 50%,transparent 50%);
  background-position:calc(100% - 12px) center,calc(100% - 8px) center;
  background-size:4px 4px,4px 4px;background-repeat:no-repeat;padding-right:22px;}
.vp-add-performer-actions{display:flex;justify-content:flex-end;gap:6px;}
.vp-add-performer-actions button{padding:3px 10px;
  border:1px solid var(--lg-hairline,#12222b);background:transparent;
  color:color-mix(in srgb,var(--lg-hot,#EAFBFF),transparent 25%);border-radius:var(--radius-sm,6px);cursor:pointer;
  font-size:var(--sb-text-micro,10px);font-family:inherit;transition:background .12s ease,color .12s ease;}
.vp-add-performer-actions button:hover{background:#ffffff08;
  color:var(--lg-hot,#EAFBFF);}
.vp-add-performer-actions button.primary{background:color-mix(in srgb,var(--sb-conn-auto,#FFF76B),transparent 92%);
  color:var(--sb-conn-auto,#FFF76B);border-color:color-mix(in srgb,var(--sb-conn-auto,#FFF76B),transparent 82%);}

/* Daemon widget (sidebar 最下部) — Light Grid foot: mono 面、 muted、 dot は cyan-dim。
   地の一部なので発光させない (online の緑 dot 廃止)。 offline だけ僅かに magenta。 */
.vp-daemon{flex:0 0 auto;background:transparent;padding-top:4px;}
.vp-daemon-summary{list-style:none;display:flex;align-items:center;gap:8px;
  padding:8px var(--spacing-sm,10px);cursor:pointer;
  font-family:var(--vp-font-mono),var(--typography-family-mono);
  font-size:var(--sb-text-meta,11px);color:var(--lg-mute-2,#38525b);user-select:none;}
.vp-daemon-summary::-webkit-details-marker{display:none;}
.vp-daemon-summary:hover{color:var(--lg-mute,#5C7A85);}
.vp-daemon-dot{width:6px;height:6px;border-radius:50%;flex:0 0 auto;
  background:var(--lg-cyan-dim,#1C6C7C);}
.vp-daemon-dot.offline{background:color-mix(in srgb,var(--sb-conn-hitl,#FF4A2D),transparent 40%);}
/* L1 lifecycle: project 行の SP presence dot。 Light Grid では project = 地なので発光させない
   (「発光ドット無し」)。 semantics は残しつつ muted 表現に落とす: connected = mute-2 定常、
   connecting = mute pulse、 disconnected = magenta 60% (要注意だけが僅かに彩度を持つ)、
   unregistered = mute-2 40%。 */
.vp-proj-presence-dot{width:5px;height:5px;border-radius:50%;flex:0 0 auto;
  background:var(--lg-mute-2,#38525b);opacity:.4;}
.vp-proj-presence-dot.connected{background:var(--lg-mute-2,#38525b);opacity:1;}
.vp-proj-presence-dot.connecting{background:var(--lg-mute,#5C7A85);opacity:1;
  animation:vp-presence-pulse 1.1s ease-in-out infinite;}
.vp-proj-presence-dot.disconnected{background:var(--sb-conn-hitl,#FF4A2D);opacity:.6;}
.vp-proj-presence-dot.unregistered{background:var(--lg-mute-2,#38525b);opacity:.4;}
@keyframes vp-presence-pulse{0%,100%{opacity:1;}50%{opacity:.35;}}
.vp-daemon-line{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;
  font-variant-numeric:tabular-nums;}
.vp-daemon-detail{padding:2px var(--spacing-sm,8px) 6px;}
.vp-daemon-stat{display:flex;justify-content:space-between;font-size:var(--sb-text-meta,11px);
  font-family:var(--vp-font-mono),var(--typography-family-mono);padding:1px 0;}
.vp-daemon-stat .k{color:var(--lg-mute-2,#38525b);}
.vp-daemon-stat .v{color:var(--lg-mute,#5C7A85);font-weight:500;
  font-variant-numeric:tabular-nums;}
/* Hub available nodes — Hub 行直下に常時リスト表示。地の一部なので発光なし、muted mono。
   左 11px + dot(5px) + gap(8px) = handle が 24px（Hub 行 label の直下）に揃い、dot は Hub 行の
   dot 列に載る。per-daemon dot = hub v0.6.0 の connected liveness（presence dot と同じ muted 語彙:
   connected = mute-2 定常 / offline = magenta 60% — registry に居るが relay 不達の stale）。 */
.vp-hub-nodes{padding:0 var(--spacing-sm,10px) 4px 11px;}
.vp-hub-daemon{display:flex;align-items:center;gap:8px;
  font-size:var(--sb-text-meta,11px);font-family:var(--vp-font-mono),var(--typography-family-mono);
  padding:1px 0;}
.vp-hub-daemon-dot{width:5px;height:5px;border-radius:50%;flex:0 0 auto;
  background:var(--lg-mute-2,#38525b);}
.vp-hub-daemon-dot.offline{background:var(--sb-conn-hitl,#FF4A2D);opacity:.6;}
.vp-hub-daemon .k{flex:1 1 auto;color:var(--lg-mute,#5C7A85);overflow:hidden;text-overflow:ellipsis;
  white-space:nowrap;}
.vp-hub-daemon .v{color:var(--lg-mute-2,#38525b);flex:0 0 auto;
  font-variant-numeric:tabular-nums;}
/* in-app update: 新しい release 検知時のみ Daemon widget 直下に出る CTA 行。
   地の muted 語彙から一段持ち上げて「押せる」ことを示す (cyan accent + hover)。 */
.vp-daemon-update{display:flex;align-items:center;gap:8px;margin:2px var(--spacing-sm,10px) 6px;
  padding:6px 10px;cursor:pointer;border-radius:6px;
  font-size:var(--sb-text-hint,12px);font-family:var(--vp-font-mono),var(--typography-family-mono);
  color:var(--sb-conn-auto,#FFF76B);
  background:color-mix(in srgb,var(--sb-conn-auto,#FFF76B),transparent 90%);
  border:1px solid color-mix(in srgb,var(--sb-conn-auto,#FFF76B),transparent 78%);
  user-select:none;}
.vp-daemon-update:hover{background:color-mix(in srgb,var(--sb-conn-auto,#FFF76B),transparent 82%);}
.vp-daemon-update-label{flex:1 1 auto;font-weight:600;}
.vp-daemon-update-ver{flex:0 0 auto;color:var(--lg-mute,#5C7A85);
  font-variant-numeric:tabular-nums;}

/* Devices 🧲 — machine scope の Devices セクション (stand row + device count badge) */
.vp-devices{flex:0 0 auto;padding-bottom:4px;}
.vp-stand-row{position:relative;display:flex;align-items:center;gap:6px;
  padding:5px var(--spacing-sm,10px);cursor:pointer;font-size:var(--sb-text-hint,12px);
  color:var(--lg-mute,#5C7A85);}
.vp-stand-row:hover{background:#ffffff06;}
.vp-stand-row.active{background:color-mix(in srgb,var(--sb-conn-auto,#FFF76B),transparent 94%);
  color:var(--sb-conn-auto,#FFF76B);}
.vp-stand-icon{display:flex;align-items:center;flex:0 0 auto;}
.vp-stand-title{flex:1 1 auto;overflow:hidden;text-overflow:ellipsis;
  white-space:nowrap;}
.vp-stand-badge{flex:0 0 auto;font-size:var(--sb-text-micro,10px);padding:1px 6px;border-radius:8px;
  background:#ffffff08;color:var(--lg-mute,#5C7A85);
  font-family:var(--vp-font-mono),var(--typography-family-mono);
  font-variant-numeric:tabular-nums;}

/* Lane 行 右クリック context menu (VP-204 PR-1、 singleton popup) */
.vp-ctx-backdrop{position:fixed;inset:0;z-index:9998;}
.vp-ctx-menu{position:fixed;z-index:9999;min-width:180px;
  background:var(--lg-panel,#0A0E15);
  border:1px solid var(--lg-hairline,#12222b);
  border-radius:var(--radius-md,6px);box-shadow:0 8px 24px rgba(0,0,0,.4);
  padding:4px 0;font-size:var(--sb-text-hint,12px);user-select:none;}
.vp-ctx-header{padding:4px 14px 6px;font-size:var(--sb-text-micro,10px);
  color:var(--lg-mute,#5C7A85);
  border-bottom:1px solid var(--lg-hairline,#12222b);
  margin-bottom:4px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-ctx-item{padding:6px 14px;cursor:pointer;display:flex;align-items:center;
  gap:8px;color:color-mix(in srgb,var(--lg-hot,#EAFBFF),transparent 25%);
  transition:background .1s ease,color .1s ease;}
.vp-ctx-item:hover{background:#ffffff08;
  color:var(--lg-hot,#EAFBFF);}
.vp-ctx-item.danger:hover{background:var(--color-status-error,#d4444c);color:#fff;}
.vp-ctx-item.danger.confirming{background:var(--color-status-error,#d4444c);
  color:#fff;}

/* FileExplorer overlay の z-index は ContextMenu (.vp-ctx-backdrop=9998 / .vp-ctx-menu=9999)
   より上に置く。 ContextMenu は position:fixed で WebView 全体を起点とするため、 overlay の
   z-index が低いと picker 上に context menu が突き抜けて描画される
   (moody-blues PR #439 final review Issue 1、 dogfood で実機目撃済)。 */
/* Lane row のフォルダピッカー起動ボタン (FileExplorer overlay を開く trigger) */
.vp-lane-files-btn{display:inline-flex;align-items:center;padding:1px 3px;
  border:none;background:transparent;color:var(--lg-mute,#5C7A85);
  cursor:pointer;border-radius:3px;flex:0 0 auto;opacity:0;
  transition:background .12s ease,color .12s ease,opacity .12s ease;}
.vp-lane-row:hover .vp-lane-files-btn{opacity:1;}
.vp-lane-files-btn:hover{background:#ffffff08;
  color:var(--sb-conn-auto,#FFF76B);}
${FILE_EXPLORER_CSS}
${WIRE_PANEL_CSS}
${LANE_PICKER_CSS}
${COMMAND_PALETTE_CSS}
`;
