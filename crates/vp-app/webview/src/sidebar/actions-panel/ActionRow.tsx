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
import { Show, createEffect } from "solid-js";
import { CreoIcon } from "@chronista-club/creo-ui-icons-web";
import {
	type ActionItem,
	actKeyIntent,
	remainingOf,
	titleOf,
} from "./model";

/** 行に focus を移す。描画の後に走らせる要があるので `queueMicrotask` 越し。 */
export function focusActionRow(id: string): void {
	queueMicrotask(() => {
		const el = document.querySelector<HTMLInputElement>(
			`[data-vp-act-row="${CSS.escape(id)}"] .vp-act-text`,
		);
		if (!el) return;
		el.focus();
		const pos = el.value.length;
		el.setSelectionRange(pos, pos);
	});
}

export interface ActionRowProps {
	item: ActionItem;
	onText(text: string): void;
	onToggleDone(): void;
	onInsert(): void;
	onRemove(): void;
	onMove(dir: -1 | 1): void;
	onFocusSibling(dir: -1 | 1): void;
}

export function ActionRow(props: ActionRowProps) {
	let el!: HTMLInputElement;
	let composing = false;

	// model → DOM の一方向同期。**自分の打鍵で起きた変化は既に一致している**ので書かない
	// （書くと caret が末尾へ飛ぶ）。IME 変換中も触らない。
	// 実際に書き込まれるのは、行の入替や外からの push で item が差し替わったときだけ。
	createEffect(() => {
		const text = props.item.text;
		if (!composing && el.value !== text) el.value = text;
	});

	const onKeyDown = (e: KeyboardEvent & { currentTarget: HTMLInputElement }) => {
		const input = e.currentTarget;
		const isMac = navigator.platform.toUpperCase().includes("MAC");
		const intent = actKeyIntent(
			{
				key: e.key,
				metaKey: e.metaKey,
				ctrlKey: e.ctrlKey,
				altKey: e.altKey,
				shiftKey: e.shiftKey,
				empty: input.value === "",
				atStart: input.selectionStart === 0,
				composing,
			},
			isMac,
		);
		if (intent === null) return;
		e.preventDefault();
		switch (intent) {
			case "insert":
				props.onInsert();
				break;
			case "toggle-done":
				props.onToggleDone();
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
			case "blur":
				input.blur();
				break;
		}
	};

	const remaining = () => remainingOf(props.item);

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

			{/* ⚠️ value= を渡さない（uncontrolled）。初期値は上の createEffect が入れる。 */}
			<input
				ref={el}
				class="vp-act-text"
				placeholder="やること"
				title={titleOf(props.item) || undefined}
				onInput={(e) => props.onText(e.currentTarget.value)}
				onCompositionStart={() => {
					composing = true;
				}}
				onCompositionEnd={(e) => {
					composing = false;
					// 変換確定分は onInput が来ないブラウザがあるので、ここで拾い直す。
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

			<button
				type="button"
				class="vp-act-del"
				title="削除"
				onClick={() => props.onRemove()}
			>
				<CreoIcon name="ph:x" size={9} />
			</button>
		</div>
	);
}
