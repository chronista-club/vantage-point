/**
 * ShellRail — 左 edge の Rail 列 + peek（doc 87 §2 / §8 slice 1）。
 *
 * ## これは creo-ui `CURail` の仮実装
 *
 * creo-ui 0.30.0 の `CURail` は VP の左 edge を 3 点で表せない（1 は doc 87 §6 の要望②、2・3 は
 * slice 1 の実装で見つけた追加の要望候補 — doc 87 §8.1）:
 *
 * 1. peek の幅が 240px 固定 — VP は 180〜640px の drag resize と永続が要る（要望②）
 * 2. 選択中の rail が在る間は peek が必ず開く — VP の slim（rail は選んだまま peek だけ畳む、
 *    `Cmd+[`）を表せない
 * 3. 列に registry の icon しか描けない — slim の repo badge 列と daemon dot の置き場が無く、
 *    下起点（歯車）も持てない
 *
 * そこで **DOM の契約は CURail と同じ**まま（`[data-region="leftEdge"]` / `nav[aria-label="Rail"]` /
 * `button[data-rail][aria-current]` / peek `[data-expanded]`）、足りない口だけ足した仮実装を置く。
 * 選択の導出は creo-ui の `selectRailId`（最長 prefix 一致）と `railsByOrder` をそのまま使う
 * — 規則を VP 側で書き直さない。要望が着地したら CURail へ差し替える（契約は
 * `ShellRail.dom.test.ts` が固定している）。
 *
 * ## CURail と違うところ（差し替え時に要望として渡すもの）
 *
 * - `peekOpen`: 選択とは別に peek の開閉を受ける（slim）
 * - `anchor`: rail ごとに列の上 / 下どちらの起点に属するか（doc 82 の 3 起点の規約）
 * - `columnCenter` / `columnBottom`: 列の中央と下端に consumer の要素を置く slot
 * - `peekFooter`: peek の下端に、どの rail でも同じ位置に残る帯を置く slot（machine 帯）
 * - peek は **全 rail 分を mount したまま**、選択外を `hidden` で隠す。CURail は選択中の
 *   1 枚だけを描く（切り替えると unmount）が、VP では rail を行き来するたびに ACTIONS の
 *   下書き・名簿の展開状態・`n` directive の登録（RepoAccordion の mount 時）が消えてしまう
 * - peek の幅は親（`#sidebar-root`）の幅から決まる（flex で残りを埋める）。幅の SSOT は
 *   `shell-layout.ts`
 */
import { For, Show, createMemo, type Component, type JSX } from "solid-js";
import { Dynamic } from "solid-js/web";
import { type RailDef, railsByOrder, selectRailId } from "@chronista-club/creo-ui/shells";
import type { RailAnchor } from "./route";

/** creo-ui の RailDef に、列の中の起点（上 / 下）を足したもの。 */
export interface ShellRailDef extends RailDef {
	anchor: RailAnchor;
}

interface ShellRailProps {
	/** app 供給の registry（PL-3 — primitive は rail を hardcode しない）。 */
	registry: readonly ShellRailDef[];
	/** 今の pseudo-route（PL-4: 選択の唯一の SSOT）。 */
	pathname: string;
	/** peek を開いているか（full = true / slim = false）。 */
	peekOpen: boolean;
	/** icon click。選択を変えるのは呼び手（route の持ち主）— ここは rail id を渡すだけ。 */
	onSelect: (railId: string) => void;
	/** 列の中央 slot（上下の rail の間。溢れたらこの中で scroll）。 */
	columnCenter?: JSX.Element;
	/** 列の下端 slot（下起点の rail の直上）。 */
	columnBottom?: JSX.Element;
	/** peek の下端 slot（どの rail を選んでも同じ位置に残る帯。slim では描かない）。 */
	peekFooter?: JSX.Element;
}

export const ShellRail: Component<ShellRailProps> = (props) => {
	const ordered = createMemo(() => railsByOrder(props.registry) as ShellRailDef[]);
	const selectedId = createMemo(() => selectRailId(props.pathname, props.registry));
	const expanded = () => props.peekOpen && selectedId() !== undefined;

	const RailButton = (p: { rail: ShellRailDef }) => (
		<button
			type="button"
			class="vp-rail-btn"
			data-rail={p.rail.id}
			aria-label={p.rail.labelKey}
			title={p.rail.labelKey}
			aria-current={p.rail.id === selectedId() ? "page" : undefined}
			onClick={() => props.onSelect(p.rail.id)}
		>
			<Dynamic component={p.rail.icon} />
		</button>
	);

	return (
		<div class="vp-rail" data-region="leftEdge">
			<nav aria-label="Rail" class="vp-rail-column tri-anchor">
				<div class="anchor-top">
					<For each={ordered().filter((r) => r.anchor === "top")}>
						{(rail) => <RailButton rail={rail} />}
					</For>
				</div>
				<div class="anchor-center">{props.columnCenter}</div>
				<div class="anchor-bottom">
					{props.columnBottom}
					<For each={ordered().filter((r) => r.anchor === "bottom")}>
						{(rail) => <RailButton rail={rail} />}
					</For>
				</div>
			</nav>
			<div class="vp-rail-peek" data-expanded={expanded() ? "true" : "false"}>
				{/* slim の間は peek を描かない（旧 slim 帯と同じ — 形は「一覧の見せ方」で、
				    畳んでいる間に見えない中身を抱えない）。開いている間は全 rail 分を
				    mount したまま、選択外を hidden にする（冒頭「CURail と違うところ」）。 */}
				<Show when={props.peekOpen}>
					<For each={ordered()}>
						{(rail) => (
							<section
								class="vp-rail-peek-body"
								data-peek={rail.id}
								hidden={rail.id !== selectedId()}
							>
								<Dynamic component={rail.peek} />
							</section>
						)}
					</For>
					{props.peekFooter}
				</Show>
			</div>
		</div>
	);
};

/** Rail の CSS（`SHELL_CSS` に連結される）。幅（列 48px + peek）は main_area.rs の `#sidebar-root`。 */
export const SHELL_RAIL_CSS = `
/* .vp-sidebar-shell（flex 列）の中で残りの高さを埋める（上にエラー帯が出ても溢れない）。 */
.vp-rail{display:flex;flex:1 1 0;min-height:0;}
/* 列 = 起点の帯。幅は #sidebar-root の --vp-rail-w（main_area.rs）と同じ値を読む。 */
.vp-rail-column{flex:0 0 var(--vp-rail-w,48px);width:var(--vp-rail-w,48px);box-sizing:border-box;
  padding:8px 0;gap:6px;border-right:1px solid var(--lg-hairline,#12222b);}
.vp-rail-column>.anchor-top,.vp-rail-column>.anchor-bottom{align-items:center;gap:6px;}
.vp-rail-column>.anchor-center{align-items:center;gap:6px;overflow-y:auto;overflow-x:hidden;
  padding:4px 0;}
.vp-rail-btn{position:relative;display:flex;align-items:center;justify-content:center;
  width:36px;height:36px;flex:none;padding:0;border:1px solid transparent;border-radius:8px;
  background:transparent;color:var(--lg-mute,#5C7A85);cursor:pointer;
  transition:background .12s ease,color .12s ease,border-color .12s ease;}
.vp-rail-btn:hover{color:var(--lg-hot,#EAFBFF);background:#ffffff08;}
/* 選択中の rail は選択行（横タブ）と同じ面の色で示す — 光らせない（光 = 注意は needs-you の専有）。 */
.vp-rail-btn[aria-current="page"]{color:var(--lg-hot,#EAFBFF);background:var(--sb-selection-bg);
  border-color:var(--sb-selection-border);}
.vp-rail-btn:focus-visible{outline:1px solid var(--sb-selection-border);outline-offset:1px;}
/* rail icon の用事 dot（Lanes = input 待ちの lane が在る）— slim badge の awaiting と同じ語彙。 */
.vp-rail-flag{position:absolute;top:3px;right:3px;width:7px;height:7px;border-radius:50%;
  background:var(--sb-conn-hitl,#FF4A2D);}
.vp-rail-peek{flex:1 1 auto;min-width:0;display:flex;flex-direction:column;overflow:hidden;}
.vp-rail-peek[data-expanded="false"]{display:none;}
.vp-rail-peek-body{flex:1 1 auto;min-height:0;display:flex;flex-direction:column;}
.vp-rail-peek-body[hidden]{display:none;}
`;
