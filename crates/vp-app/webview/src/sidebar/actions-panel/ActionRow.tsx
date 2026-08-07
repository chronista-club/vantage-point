/**
 * ACTIONS の 1 行（doc 57 §5）。
 *
 * ## この file の存在理由 = focus と IME を守ること
 *
 * creo-ui の `CUOutliner` をそのまま使えないのはここが理由。あちらは
 * `createMemo(() => flattenVisible(nodes()))` が毎回新しい行オブジェクトを作り、`<For>` が
 * 参照 `===` でキーイングするので、**1 文字打つたびに `<input>` ごと作り直される**
 * （focus・caret・変換中の composition が飛ぶ）。
 *
 * VP 側で守っている 2 点:
 *
 * 1. **input を自分の打鍵に対して uncontrolled にする** — `value=` を JSX 属性で渡さない。
 *    model → DOM の同期は `createEffect` で「値が食い違っているときだけ」書く。自分で打った
 *    変化は既に一致しているので書かれず、caret が末尾へ飛ばない
 * 2. **composition 中は DOM に触らない** — 変換確定前に value を書くと変換が壊れる。
 *    keydown の解釈も `actKeyIntent` が `composing` で塞ぐ（変換確定の Enter で行が増えない）
 *
 * 行そのものは `<Index>`（位置キーイング）で描くので、item が差し替わっても DOM は保たれる。
 */
import { Show, createEffect, createSignal } from "solid-js";
import { CreoIcon } from "@chronista-club/creo-ui-icons-web";
import { copyText } from "../../../clipboard";
import { isImeKeystroke } from "../../../ime";
import {
	type ActionItem,
	actKeyIntent,
	isLocalId,
	remainingOf,
	titleOf,
} from "./model";
import { beginEditing, endEditing } from "./store";

/**
 * creo の memory permalink の base（出口②、doc 57 §0）。
 *
 * `chatview.tsx` の `CREO_MEMORY_BASE` と同じ値を持つ。daemon 側の `VP_CREO_URL`
 * （API の宛先を dev で差し替える env）とは**別物** — こちらは人が開く公開 URL。
 */
const CREO_MEMORY_BASE = "https://app.creo-memories.in/m/";

/** 行に focus を移す。描画の後に走らせる要があるので `queueMicrotask` 越し。 */
export function focusActionRow(id: string): void {
	queueMicrotask(() => {
		const el = document.querySelector<HTMLTextAreaElement>(
			`[data-vp-act-row="${CSS.escape(id)}"] .vp-act-text`,
		);
		if (!el) return;
		el.focus();
		const pos = el.value.length;
		el.setSelectionRange(pos, pos);
	});
}

/**
 * textarea の高さを中身に合わせる。`null` を渡すと 1 行に畳む（= 未 focus の見え方）。
 *
 * 行は**畳んだ状態が既定**（doc 57 §2）— 一覧ではタイトルだけが見え、focus した行だけが
 * 全文に開く。`height:auto` を挟むのは、縮む方向にも追従させるため（scrollHeight は
 * 現在の height を下回らない）。
 */
function autoSize(el: HTMLTextAreaElement, open: boolean): void {
	// ⚠️ `white-space` の出し分けが要る。textarea の UA 既定は `pre-wrap` なので、畳んだ姿
	// （`height:1.5em`）のままだと長いタイトルが**折り返してタテ方向にクリップ**され、
	// 語の途中で切れた見た目になる。サイドバーの他の 1 行表現（`.vp-proj-name` 等）は
	// 例外なく nowrap なので、畳んだときはそれに揃える。
	el.style.whiteSpace = open ? "pre-wrap" : "nowrap";
	if (!open) {
		el.style.height = "";
		// nowrap だと横スクロールが残る。畳むときは先頭へ戻さないとタイトルが読めない。
		el.scrollLeft = 0;
		return;
	}
	el.style.height = "auto";
	el.style.height = `${el.scrollHeight}px`;
}

export interface ActionRowProps {
	item: ActionItem;
	onText(text: string): void;
	onToggleDone(): void;
	onRemove(): void;
	/** 何も書かずに抜けた行を捨てる（focus は動かさない）。 */
	onAbandon(): void;
	onMove(dir: -1 | 1): void;
	onFocusSibling(dir: -1 | 1): void;
}

export function ActionRow(props: ActionRowProps) {
	let el!: HTMLTextAreaElement;
	let composing = false;

	// model → DOM の一方向同期。**自分の打鍵で起きた変化は既に一致している**ので書かない
	// （書くと caret が末尾へ飛ぶ）。IME 変換中も触らない。
	// 実際に書き込まれるのは、行の入替や外からの push で item が差し替わったときだけ。
	createEffect(() => {
		const text = props.item.text;
		if (!composing && el.value !== text) {
			el.value = text;
			autoSize(el, document.activeElement === el);
		}
	});

	/** caret 位置に改行を差し込む（⌘Enter）。textarea の既定動作が無いので手で入れる。 */
	const insertNewline = (t: HTMLTextAreaElement) => {
		const at = t.selectionStart ?? t.value.length;
		const end = t.selectionEnd ?? at;
		t.value = `${t.value.slice(0, at)}\n${t.value.slice(end)}`;
		t.setSelectionRange(at + 1, at + 1);
		autoSize(t, true);
		props.onText(t.value);
	};

	const onKeyDown = (
		e: KeyboardEvent & { currentTarget: HTMLTextAreaElement },
	) => {
		const t = e.currentTarget;
		const isMac = navigator.platform.toUpperCase().includes("MAC");
		const intent = actKeyIntent(
			{
				key: e.key,
				metaKey: e.metaKey,
				ctrlKey: e.ctrlKey,
				altKey: e.altKey,
				shiftKey: e.shiftKey,
				empty: t.value === "",
				atStart: t.selectionStart === 0,
				atEnd: t.selectionStart === t.value.length,
				// ⚠️ 自前の `composing` フラグ**だけ**では WKWebView で素通りする —
				// あちらは compositionend が keydown より先に走るので、変換確定の Enter が
				// 来た時点で既に false。engine 別の痕跡は共有の `isImeKeystroke` が持つ
				// （chat 入力 #963 で同じ罠を踏んで確立した判別）。
				composing: composing || isImeKeystroke(e),
			},
			isMac,
		);
		if (intent === null) return;
		e.preventDefault();
		switch (intent) {
			case "newline":
				insertNewline(t);
				break;
			case "remove":
				props.onRemove();
				break;
			case "move-up":
				props.onMove(-1);
				break;
			case "move-down":
				props.onMove(1);
				break;
			case "focus-prev":
				props.onFocusSibling(-1);
				break;
			case "focus-next":
				props.onFocusSibling(1);
				break;
			case "commit":
				// 「書き終えた」= 元の作業へ戻る。行は畳まれてタイトルだけになる。
				t.blur();
				break;
		}
	};

	const remaining = () => remainingOf(props.item);

	// 出口①（doc 57 §0）: 整えた文章を OS clipboard へ。差し込み回避の本体 —
	// 思いついた瞬間に頼まず、区切りがついてから貼る。
	const [copied, setCopied] = createSignal(false);
	let copiedTimer: number | undefined;
	const copy = () => {
		const text = props.item.text.trim();
		if (text === "") return;
		copyText(text);
		flashCopied();
	};

	// 出口②（doc 57 §0 / Phase 4）: creo の permalink をコピーする。
	// **creo に上がった行にだけ出す** — local id の行はまだ URL を持たない
	//（blur すれば上がるので、その次の push でボタンが現れる）。
	const permalink = () => (isLocalId(props.item.id) ? null : `${CREO_MEMORY_BASE}${props.item.id}`);
	const copyUrl = () => {
		const url = permalink();
		if (!url) return;
		copyText(url);
		flashCopied();
	};

	function flashCopied(): void {
		setCopied(true);
		if (copiedTimer) clearTimeout(copiedTimer);
		copiedTimer = window.setTimeout(() => setCopied(false), 900);
	}

	return (
		<div
			class="vp-act-row"
			data-vp-act-row={props.item.id}
			data-done={props.item.done ? "" : undefined}
		>
			{/* done トグル。bullet の位置を奪っている — 280px に押せる的を 2 つ並べる余裕が
			    無いため（doc 57 §2）。Things / Workflowy と同じ発想で学習コストもない。 */}
			<button
				type="button"
				class="vp-act-check"
				role="checkbox"
				aria-checked={props.item.done ?? false}
				title={props.item.done ? "未完了に戻す（⌘Enter）" : "完了にする（⌘Enter）"}
				onClick={() => props.onToggleDone()}
			/>

			{/* ⚠️ value= を渡さない（uncontrolled）。初期値は上の createEffect が入れる。
			    textarea なのは ⌘Enter で改行を書けるようにするため（内容 = 2 行目以降）。
			    未 focus では 1 行に畳んでタイトルだけ見せる（doc 57 §2）。 */}
			<textarea
				ref={el}
				class="vp-act-text"
				rows={1}
				placeholder="やること"
				title={titleOf(props.item) || undefined}
				onInput={(e) => {
					autoSize(e.currentTarget, true);
					props.onText(e.currentTarget.value);
				}}
				onFocus={(e) => {
					autoSize(e.currentTarget, true);
					// 編集中の行は daemon push から守る（往復前の古い値で上書きさせない）。
					beginEditing(props.item.id);
				}}
				onBlur={(e) => {
					autoSize(e.currentTarget, false);
					// 何も書かずに抜けた行は残さない。⌘b で開いて「やっぱりやめた」が
					// 空行として溜まると、捕捉バッファがすぐゴミで埋まる。
					if (e.currentTarget.value.trim() === "") props.onAbandon();
					// 「書き終えた」の合図。**書きかけの新規行はここで初めて creo へ上がる**
					//（編集中は id が差し替わらないよう payload から外してある）。
					endEditing(props.item.id);
				}}
				onCompositionStart={() => {
					composing = true;
				}}
				onCompositionEnd={(e) => {
					composing = false;
					// 変換確定分は onInput が来ないブラウザがあるので、ここで拾い直す。
					autoSize(e.currentTarget, true);
					props.onText(e.currentTarget.value);
				}}
				onKeyDown={onKeyDown}
			/>

			{/* 内容がチェックリストのときだけ「残り」を出す（説明文なら出さない）。 */}
			<Show when={remaining() !== null}>
				<span class="vp-act-remain" title="未完のチェック">
					{remaining()}
				</span>
			</Show>

			{/* 出口①: pasteboard（doc 57 §0）。行 hover で現れる。 */}
			<button
				type="button"
				class="vp-act-copy"
				classList={{ copied: copied() }}
				title="コピー — 区切りがついたら貼る"
				onClick={copy}
			>
				<CreoIcon name={copied() ? "ph:check" : "ph:copy"} size={9} />
			</button>

			{/* 出口②: creo の permalink。creo に上がっている行だけに出る。 */}
			<Show when={permalink() !== null}>
				<button
					type="button"
					class="vp-act-link"
					title="URL をコピー — creo の memory を指す"
					onClick={copyUrl}
				>
					<CreoIcon name="ph:link" size={9} />
				</button>
			</Show>

			{/* ⚠️ creo の memory ごと消える（mako 裁定 2026-08-04）。取り消せない。 */}
			<button
				type="button"
				class="vp-act-del"
				title="削除 — creo の memory ごと消える"
				onClick={() => props.onRemove()}
			>
				<CreoIcon name="ph:x" size={9} />
			</button>
		</div>
	);
}
