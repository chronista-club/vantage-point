/**
 * ACTIONS の区画（doc 57 §2）— daemon status の直上に並ぶ 5 つの `<details>`。
 *
 * doc 56 §7 が「app 級の家 = サイドバー下部・daemon status の上」として予約していた住所。
 * CURRENTs を描かないのは、そこが既存の repo 一覧そのものだから（合流は Phase 5）。
 *
 * ## 層
 *
 * - **data**: `store.ts`（木と開閉）
 * - **calculations**: `model.ts`（並び・件数・キーの意図）
 * - **actions**: この file（DOM イベント → store の書き換え → focus 移動）
 */
import { For, Index, Show } from "solid-js";
import { CreoIcon } from "@chronista-club/creo-ui-icons-web";
import {
	PANEL_BUCKETS,
	type BucketDef,
	actionsFetchState,
	countUndone,
	itemsIn,
} from "./model";
import { sidebar } from "../store";
import {
	actions,
	appendAction,
	commitActions,
	moveAction,
	openBuckets,
	removeAction,
	setActionDone,
	setActionText,
	toggleBucket,
} from "./store";
import { ActionRow, focusActionRow } from "./ActionRow";

/**
 * ACTIONS の取得状態。`/api/health` の 2 値から導く（新しい配管は無い）。
 *
 * ⚠️ `services` ではなく既に sidebar へ届いている値を読む — 読み手のいない場所に
 * 出しても意味が無い（`DaemonWidget` の艦隊スイッチ表示と同じ判断）。
 */
const fetchState = () =>
	actionsFetchState(
		sidebar.activity.actions_rev ?? 0,
		sidebar.activity.auth_targets?.creo,
	);

function Bucket(props: { def: BucketDef }) {
	const items = () => itemsIn(actions(), props.def.id);
	const undone = () => countUndone(actions(), props.def.id);
	const open = () => openBuckets().has(props.def.id);

	/** 兄弟へ focus を移す。端なら何もしない。 */
	const focusSibling = (from: string, dir: -1 | 1) => {
		const list = items();
		const at = list.findIndex((i) => i.id === from);
		const next = list[at + dir];
		if (next) focusActionRow(next.id);
	};

	/** 消したあとは前の行へ戻る（無ければ後ろ）。 */
	const removeAndFocus = (id: string) => {
		const list = items();
		const at = list.findIndex((i) => i.id === id);
		const fallback = list[at - 1] ?? list[at + 1];
		if (commitActions(removeAction(id)) && fallback) focusActionRow(fallback.id);
	};

	/**
	 * 並べ替え。**必ず focus を張り直す**のが肝。
	 *
	 * `<Index>` は位置キーイングなので、行が動いても DOM スロットは居座り、そこへ隣の Action の
	 * 中身が流れ込む。focus は DOM 側に残ったままなので、張り直さないと**気づかず別の Action を
	 * 編集する**。id は移動しても変わらないので、同じ id で引き直せば正しい行に戻る。
	 * （`onInsert` / `onRemove` が focus を扱っているのと同じ責務。ここだけ抜けていた）
	 */
	const moveAndFocus = (id: string, dir: -1 | 1) => {
		if (commitActions(moveAction(id, dir))) focusActionRow(id);
	};

	/**
	 * 何も書かずに抜けた行を捨てる。**焦点の転送が終わってから**消すのが肝。
	 *
	 * `blur` は `focusActionRow` の `el.focus()` が同期的に撃つので、その場で木を書き換えると
	 * `<Index>`（位置キーイング）の中身が 1 つずれ、**転送先の箱に別の Action が流れ込んだ状態で
	 * 焦点が着く**（転送先が最後の行なら箱ごと消えて焦点が落ちる）。空行は `atStart` と `atEnd` が
	 * 同時に真なので矢印が必ず行移動になり、この経路は普通に踏む。
	 *
	 * そこで microtask 1 つ待って転送の完了を見届け、**着地した行の id を DOM から読んでから**
	 * 消し、同じ id で焦点を張り直す。`moveAndFocus` / `removeAndFocus` と同じ
	 * 「書き換えたら張り直す」契約に揃えた形。
	 */
	const abandonIfEmpty = (id: string) => {
		queueMicrotask(() => {
			const landed = (document.activeElement as HTMLElement | null)
				?.closest?.("[data-vp-act-row]")
				?.getAttribute("data-vp-act-row");
			// まだ自分に焦点がある = 転送ではなく一時的な blur。消さない。
			if (landed === id) return;
			if (commitActions(removeAction(id)) && landed) focusActionRow(landed);
		});
	};

	return (
		<details
			class="vp-act-bucket"
			open={open()}
			onToggle={(e) => {
				// store が SSOT。値が一致していれば何もしない（echo loop 防止、
				// RepoAccordion:129-136 と同型）。
				if (e.currentTarget.open !== open()) toggleBucket(props.def.id);
			}}
		>
			<summary class="vp-act-summary" title={props.def.hint}>
				<span class="vp-act-caret">›</span>
				<span class="vp-act-label">{props.def.label}</span>
				<Show when={undone() > 0}>
					<span class="vp-act-badge">{undone()}</span>
				</Show>
			</summary>

			{/* ⚠️ `<details>` は閉じていても子を DOM に持つので、Show で中身ごと出し入れする
			    （100 行あっても閉じている間はコストゼロ）。 */}
			<Show when={open()}>
				<div class="vp-act-list">
					{/* 位置キーイング。item が差し替わっても <input> の DOM が保たれる
					    = focus と IME が飛ばない（ActionRow の doc 参照）。 */}
					<Index each={items()}>
						{(row) => (
							<ActionRow
								item={row()}
								onText={(text) => commitActions(setActionText(row().id, text))}
								onToggleDone={() =>
									commitActions(setActionDone(row().id, !row().done))
								}
								onRemove={() => removeAndFocus(row().id)}
								onAbandon={() => abandonIfEmpty(row().id)}
								onMove={(dir) => moveAndFocus(row().id, dir)}
								onFocusSibling={(dir) => focusSibling(row().id, dir)}
							/>
						)}
					</Index>

					{/* ⚠️ 未取得のときに「まだ何もない」と言わない — 空と未取得は同じ姿なので、
					    ここで言い分けないと user は「本当に空」と読む（2026-08-07 の実害）。 */}
					<Show when={items().length === 0}>
						<div class="vp-act-empty">
							{fetchState() === "ready" ? "まだ何もない" : "—"}
						</div>
					</Show>

					<button
						type="button"
						class="vp-act-add"
						onClick={() => focusActionRow(appendAction(props.def.id))}
					>
						<CreoIcon name="ph:plus" size={10} />
						追加
					</button>
				</div>
			</Show>
		</details>
	);
}

export function BucketList() {
	return (
		<div class="vp-act-buckets">
			{/* ⚠️ **区画を開かなくても見える**位置に置く。区画は既定で閉じているので、
			    中に出しても「空に見える」ままで気づけない。ready のときは何も出さない
			    （正常時に増える表示はゼロ = それとなく、の条件）。 */}
			<Show when={fetchState() !== "ready"}>
				<div
					class="vp-act-status"
					title={
						fetchState() === "disconnected"
							? "Creo ID に未接続 — 下の Creo ID 行からログインすると同期される"
							: "creo から最初の取得を待っている"
					}
				>
					{fetchState() === "disconnected" ? "未接続" : "取得中…"}
				</div>
			</Show>
			<For each={PANEL_BUCKETS}>{(def) => <Bucket def={def} />}</For>
		</div>
	);
}

/**
 * 区画の CSS。`Shell.tsx` の `SHELL_CSS` 末尾に連結する（FILE_EXPLORER_CSS 等と同じ流儀）。
 * 色は Light Grid（`--lg-*`）、字は 4 段（`--sb-text-*`）だけを使う。
 */
export const ACTIONS_CSS = `
/* ACTIONS（doc 57）— app 級の家。repo が「地」、lane が「図」なのに対しここは「棚」。
   面を持たず、sidebar header と同じ muted 見出しで section として立つだけにする。 */
/* ⚠️ flex:0 1 auto + min-height:0 + overflow が daemon widget の生命線。
   shell（.vp-sidebar-shell）自体は overflow を持たないので、ここが縮まないと
   区画を複数開いたときに合計高さが窓を超え、下の daemon status が画面外へ押し出されて
   スクロールで戻る手段が無くなる。repo list は min-height:96px で床が入っているので、
   溢れた分はこの帯が引き受けて内部スクロールに畳む。 */
.vp-act-buckets{flex:0 1 auto;min-height:0;overflow-y:auto;overscroll-behavior:contain;
  padding:2px 0 4px;
  border-top:1px solid var(--lg-hairline,#12222b);}
.vp-act-bucket{flex:0 0 auto;}
.vp-act-summary{list-style:none;display:flex;align-items:center;gap:6px;
  padding:6px 12px;cursor:pointer;user-select:none;
  font-size:var(--sb-text-micro,10px);letter-spacing:.14em;text-transform:uppercase;
  font-weight:var(--typography-weight-semibold,600);
  color:var(--lg-mute-2,#38525b);transition:color .12s ease;}
.vp-act-summary::-webkit-details-marker{display:none;}
.vp-act-summary:hover{color:var(--lg-mute,#5C7A85);}
.vp-act-caret{display:inline-block;flex:0 0 auto;width:8px;font-size:9px;line-height:1;
  color:var(--lg-mute-2,#38525b);transition:transform .12s ease;}
.vp-act-bucket[open] .vp-act-caret{transform:rotate(90deg);}
.vp-act-label{flex:1 1 auto;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-act-badge{flex:0 0 auto;padding:0 5px;border-radius:7px;background:#ffffff08;
  color:var(--lg-mute,#5C7A85);font-size:var(--sb-text-micro,10px);letter-spacing:0;
  font-family:var(--vp-font-mono),var(--typography-family-mono);
  font-variant-numeric:tabular-nums;}

/* 行リスト。上限を切って repo list（flex:1）を潰さない。
   overscroll-behavior:contain = 端まで来ても親へスクロールを渡さない。 */
.vp-act-list{max-height:min(30vh,220px);overflow-y:auto;overscroll-behavior:contain;
  padding:0 6px 2px;}

/* align-items:flex-start = 複数行に開いたとき、チェックと道具が 1 行目に揃うようにする
   （center だと縦中央に浮いて、どの行に効くのか読めなくなる）。 */
.vp-act-row{display:flex;align-items:flex-start;gap:5px;border-radius:6px;padding:2px 6px;
  font-size:var(--sb-text-hint,12px);
  color:color-mix(in srgb,var(--lg-hot,#EAFBFF),transparent 25%);}
.vp-act-row:hover{background:#ffffff06;}
/* 編集中の行だけ僅かに持ち上げる（選択表現は faint tint のみ、光り物は足さない）。 */
.vp-act-row:focus-within{background:color-mix(in srgb,var(--sb-conn-auto,#FFF76B),transparent 94%);}

/* bullet を兼ねた done トグル。静かなアクセント（cyan）を使う —
   黄（--sb-conn-auto）は「働いている lane」専用なので done には使わない。 */
.vp-act-check{flex:0 0 auto;width:11px;height:11px;padding:0;border-radius:50%;
  cursor:pointer;background:transparent;
  border:1px solid color-mix(in srgb,var(--lg-cyan-dim,#1C6C7C),transparent 35%);
  transition:background .12s ease,border-color .12s ease;}
.vp-act-check:hover{border-color:var(--lg-cyan-dim,#1C6C7C);
  background:color-mix(in srgb,var(--lg-cyan-dim,#1C6C7C),transparent 80%);}
.vp-act-row[data-done] .vp-act-check{background:var(--lg-cyan-dim,#1C6C7C);
  border-color:var(--lg-cyan-dim,#1C6C7C);}
.vp-act-row[data-done] .vp-act-text{color:var(--lg-mute-2,#38525b);text-decoration:line-through;}

/* textarea（⌘Enter で改行を書けるように）。既定の height は 1 行 = 畳んだ姿で、
   focus 中だけ ActionRow の autoSize が inline height を書いて全文に開く。
   overflow:hidden + resize:none で「入力欄らしさ」を消し、行として振る舞わせる。 */
.vp-act-text{flex:1 1 auto;min-width:0;padding:0;border:none;background:transparent;
  color:inherit;font:inherit;line-height:1.5;outline:none;
  resize:none;overflow:hidden;height:1.5em;display:block;
  /* 既定 = 畳んだ姿。textarea の UA 既定 pre-wrap のままだと長いタイトルが折り返して
     height:1.5em にタテ方向でクリップされる（サイドバーの他の 1 行表現と同じ nowrap に揃える）。
     focus 中は ActionRow の autoSize が inline で pre-wrap に戻す。 */
  white-space:nowrap;}
.vp-act-text::placeholder{color:var(--lg-mute-2,#38525b);}
/* チェックと道具は 1 行目の高さに揃える（flex-start の相方）。 */
.vp-act-check{margin-top:3px;}
.vp-act-copy,.vp-act-link,.vp-act-del{margin-top:1px;}
.vp-act-remain{flex:0 0 auto;font-size:var(--sb-text-micro,10px);
  color:var(--lg-mute-2,#38525b);font-variant-numeric:tabular-nums;}
/* 行の道具（コピー / 削除）は hover で現れる。常時出すと 280px の行が道具で埋まる。 */
.vp-act-copy,.vp-act-link,.vp-act-del{flex:0 0 auto;display:inline-flex;align-items:center;padding:1px 2px;
  border:none;background:transparent;color:var(--lg-mute-2,#38525b);cursor:pointer;
  border-radius:3px;opacity:0;transition:opacity .12s ease,color .12s ease;}
.vp-act-row:hover .vp-act-copy,.vp-act-row:hover .vp-act-link,.vp-act-row:hover .vp-act-del{opacity:1;}
.vp-act-copy:hover,.vp-act-link:hover{color:var(--lg-hot,#EAFBFF);}
/* コピー済みの一瞬だけ点く（hover していなくても見える = 押した手応え）。 */
.vp-act-copy.copied{opacity:1;color:var(--lg-cyan-dim,#1C6C7C);}
.vp-act-del:hover{color:var(--sb-conn-hitl,#FF4A2D);}

.vp-act-empty{padding:3px 8px;font-size:var(--sb-text-meta,11px);
  color:var(--lg-mute-2,#38525b);font-style:italic;}
/* 取得できていないことの「それとなく」の表明。⚠️ 警告色は使わない — 復旧は
   Creo ID 行の Login 1 つで、user を急かす種類の異常ではない。区画ラベルより
   一段沈めて、目に入るが読み飛ばせる濃度に置く。 */
.vp-act-status{padding:2px 8px 4px;font-size:var(--sb-text-meta,11px);
  color:var(--lg-mute-2,#38525b);font-style:italic;letter-spacing:.02em;}
.vp-act-add{display:flex;align-items:center;gap:5px;width:100%;
  padding:3px 6px;border:none;background:transparent;cursor:pointer;text-align:left;
  color:var(--lg-mute-2,#38525b);font:inherit;font-size:var(--sb-text-meta,11px);
  border-radius:6px;transition:color .12s ease,background .12s ease;}
.vp-act-add:hover{background:#ffffff06;color:var(--lg-mute,#5C7A85);}

@media (prefers-reduced-motion:reduce){
  .vp-act-caret,.vp-act-check,.vp-act-add,.vp-act-link,.vp-act-del{transition:none;}}
`;
