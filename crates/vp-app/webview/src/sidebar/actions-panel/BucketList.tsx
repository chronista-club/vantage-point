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
import { PANEL_BUCKETS, type BucketDef, countUndone, itemsIn } from "./model";
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
								onInsert={() =>
									focusActionRow(appendAction(props.def.id, row().id))
								}
								onRemove={() => removeAndFocus(row().id)}
								onMove={(dir) => commitActions(moveAction(row().id, dir))}
								onFocusSibling={(dir) => focusSibling(row().id, dir)}
							/>
						)}
					</Index>

					<Show when={items().length === 0}>
						<div class="vp-act-empty">まだ何もない</div>
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
.vp-act-buckets{flex:0 0 auto;padding:2px 0 4px;
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

.vp-act-row{display:flex;align-items:center;gap:5px;border-radius:6px;padding:2px 6px;
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

.vp-act-text{flex:1 1 auto;min-width:0;padding:0;border:none;background:transparent;
  color:inherit;font:inherit;line-height:1.5;outline:none;}
.vp-act-text::placeholder{color:var(--lg-mute-2,#38525b);}
.vp-act-remain{flex:0 0 auto;font-size:var(--sb-text-micro,10px);
  color:var(--lg-mute-2,#38525b);font-variant-numeric:tabular-nums;}
/* 行の道具（コピー / 削除）は hover で現れる。常時出すと 280px の行が道具で埋まる。 */
.vp-act-copy,.vp-act-del{flex:0 0 auto;display:inline-flex;align-items:center;padding:1px 2px;
  border:none;background:transparent;color:var(--lg-mute-2,#38525b);cursor:pointer;
  border-radius:3px;opacity:0;transition:opacity .12s ease,color .12s ease;}
.vp-act-row:hover .vp-act-copy,.vp-act-row:hover .vp-act-del{opacity:1;}
.vp-act-copy:hover{color:var(--lg-hot,#EAFBFF);}
/* コピー済みの一瞬だけ点く（hover していなくても見える = 押した手応え）。 */
.vp-act-copy.copied{opacity:1;color:var(--lg-cyan-dim,#1C6C7C);}
.vp-act-del:hover{color:var(--sb-conn-hitl,#FF4A2D);}

.vp-act-empty{padding:3px 8px;font-size:var(--sb-text-meta,11px);
  color:var(--lg-mute-2,#38525b);font-style:italic;}
.vp-act-add{display:flex;align-items:center;gap:5px;width:100%;
  padding:3px 6px;border:none;background:transparent;cursor:pointer;text-align:left;
  color:var(--lg-mute-2,#38525b);font:inherit;font-size:var(--sb-text-meta,11px);
  border-radius:6px;transition:color .12s ease,background .12s ease;}
.vp-act-add:hover{background:#ffffff06;color:var(--lg-mute,#5C7A85);}

@media (prefers-reduced-motion:reduce){
  .vp-act-caret,.vp-act-check,.vp-act-add,.vp-act-del{transition:none;}}
`;
