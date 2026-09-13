/** Project-independent capture and cross-Atlas list (design 71). */
import { For, Show } from "solid-js";
import { actionsFetchState } from "./model";
import { sidebar } from "../store";
import { actions, commitActions, moveAction, removeAction, setActionDone, setActionText, importLegacyActions, importingLegacy, actionMoveError } from "./store";
import { ActionRow, focusActionRow } from "./ActionRow";
import { Capture } from "./Capture";

const fetchState = () => actionsFetchState(sidebar.activity.actions_rev ?? 0, sidebar.activity.auth_targets?.creo);

export function BucketList() {
  const ordered = () => [...actions()].sort((a,b) => a.order.localeCompare(b.order) || a.id.localeCompare(b.id));
  const focusSibling = (id: string, dir: -1 | 1) => {
    const list = ordered();
    const next = list[list.findIndex(i => i.id === id) + dir];
    if (next) focusActionRow(next.id);
  };
  return <div class="vp-act-buckets">
    <div class="vp-act-heading">ACTIONS <span>{actions().length || ""}</span></div>
    <Capture atlases={sidebar.activity.actions_atlases ?? []} scope={sidebar.activity.actions_scope ?? ""} />
    <Show when={sidebar.activity.actions_error}><div class="vp-act-status" role="status">{sidebar.activity.actions_error}</div></Show>
    <Show when={actionMoveError()}><div class="vp-act-status" role="status">{actionMoveError()}</div></Show>
    <Show when={!sidebar.activity.actions_imported}>
      <button type="button" class="vp-act-add" aria-label="以前のACTIONSを取り込む"
        disabled={importingLegacy() || fetchState() !== "ready"} onClick={importLegacyActions}>
        {importingLegacy() ? "以前のメモを取り込み中…" : "以前の ACTIONS を取り込む"}
      </button>
    </Show>
    <Show when={fetchState() !== "ready"}>
      <div class="vp-act-status">{fetchState() === "disconnected" ? "未接続" : "取得中…"}</div>
    </Show>
    <div class="vp-act-list" aria-label="メモ一覧">
      <For each={ordered().map(item => item.client_id ?? item.id)}>{key => {
        const initial = actions().find(item => (item.client_id ?? item.id) === key)!;
        const item = () => actions().find(item => (item.client_id ?? item.id) === key) ?? initial;
        return <ActionRow item={item()}
          atlasName={sidebar.activity.actions_atlases?.find(a => a.id === item().atlas_id)?.path ?? "Atlas 未確認"}
          onText={text => commitActions(setActionText(item().id, text))}
          onToggleDone={() => commitActions(setActionDone(item().id, !item().done))}
          onRemove={() => commitActions(removeAction(item().id))}
          onAbandon={() => {}}
          onMove={dir => { const id = item().id; if (commitActions(moveAction(id, dir))) focusActionRow(id); }}
          onFocusSibling={dir => focusSibling(item().id, dir)} />
      }}</For>
      <Show when={fetchState() === "ready" && !actions().length}><div class="vp-act-empty">思いついたことを、ここから。</div></Show>
    </div>
  </div>;
}

/**
 * 区画の CSS。`Shell.tsx` の `SHELL_CSS` 末尾に連結する（WIRE_PANEL_CSS 等と同じ流儀）。
 * 色は Light Grid（`--lg-*`）、字は 4 段（`--sb-text-*`）だけを使う。
 */
export const ACTIONS_CSS = `
.vp-act-heading{display:flex;justify-content:space-between;padding:8px 12px 4px;
  font-size:var(--sb-text-micro,10px);letter-spacing:.14em;color:var(--lg-mute,#5C7A85);}
.vp-act-capture{margin:4px 10px 8px;border:1px solid color-mix(in srgb,var(--lg-mute),transparent 75%);border-radius:6px;}
.vp-act-capture:focus-within{border-color:var(--lg-cyan-dim,#1C6C7C);}
.vp-act-capture textarea{display:block;box-sizing:border-box;width:100%;resize:vertical;min-height:48px;max-height:160px;
  border:0;background:transparent;color:var(--lg-hot,#EAFBFF);font:inherit;font-size:var(--sb-text-hint,12px);padding:8px;outline:none;}
.vp-act-capture-controls{display:flex;gap:6px;align-items:center;padding:0 6px 6px;}
.vp-act-capture select{min-width:0;flex:1;border:0;background:transparent;color:var(--lg-mute,#5C7A85);font:inherit;font-size:var(--sb-text-micro,10px);}
.vp-act-capture button{border:0;border-radius:4px;padding:3px 8px;background:color-mix(in srgb,var(--lg-cyan-dim,#1C6C7C),transparent 65%);color:var(--lg-hot,#EAFBFF);font:inherit;font-size:var(--sb-text-micro,10px);cursor:pointer;}
.vp-act-capture button:disabled{opacity:.35;cursor:default;}
.vp-act-atlas{grid-column:2/-1;grid-row:2;font-size:var(--sb-text-micro,10px);color:var(--lg-mute,#5C7A85);overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
/* ACTIONS（doc 57）— app 級の家。repo が「地」、lane が「図」なのに対しここは「棚」。
   面を持たず、sidebar header と同じ muted 見出しで section として立つだけにする。 */
/* ⚠️ flex:0 1 auto + min-height:0 + overflow が daemon widget の生命線。
   shell（.vp-sidebar-shell）自体は overflow を持たないので、ここが縮まないと
   区画を複数開いたときに合計高さが窓を超え、下の daemon status が画面外へ押し出されて
   スクロールで戻る手段が無くなる。repo list は min-height:96px で床が入っているので、
   溢れた分はこの帯が引き受けて内部スクロールに畳む。 */
.vp-act-buckets{flex:0 1 auto;min-height:0;overflow-y:auto;overscroll-behavior:contain;
  padding:2px 0 4px;}
/* 名簿との境界線は親 .vp-creo-zone が持つ（doc 58 ③ — CreoIdRow を含む段全体の上辺）。 */
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
.vp-act-row{display:grid;grid-template-columns:11px minmax(0,1fr) repeat(4,auto);align-items:start;column-gap:5px;row-gap:1px;border-radius:6px;padding:5px 6px;
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
.vp-act-check:disabled,.vp-act-del:disabled{opacity:.35;cursor:default;}
.vp-act-text{grid-column:2;grid-row:1;}
.vp-act-check{grid-column:1;grid-row:1;}
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
