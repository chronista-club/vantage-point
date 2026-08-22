/**
 * code pane（コードブラウザ P1）の view 状態 SSOT — board-view.ts の縮小鏡。
 *
 * ## board-view との違い（意図的に小さい）
 *
 * - **float form が無い**（P1 は docked のみ）。board の drag / resize / rect 機構が
 *   丸ごと不要になり、状態は「lane ごとの open」1 bit だけ
 * - **DOM に一切触らない**。`#lane-code` の display を書くのは lane-panes 一本
 *   （roster render + stray 掃除）。board の「float は board-view / docked は lane-panes」
 *   という二重所有をここに持ち込まない
 *
 * ## キー
 *
 * `(repo, lane)` 合成キー（`boardKeyOf`）。⚠️ **repo 次元を落とさない** — 全 repo の
 * root lane は同名なので、lane 名だけで持つと「repo A で開いたまま B へ移ると B でも
 * 開いている」になる（2026-08-04 の board 混線と同じ穴）。
 *
 * ## 永続化
 *
 * 無し（in-memory、board と同じ）。doc 50 P5 の lane layout 永続に乗せ替える。
 */

import { boardKeyOf } from "./lane-panes";

// ============================================================================
// data（module-local）
// ============================================================================

export type CodeViewState = {
	open: boolean;
};

/** 既定 = 閉。表示は user が起こす（Cmd+F / File menu / LaneRow フォルダ）。 */
export const DEFAULT_CODE_VIEW: CodeViewState = { open: false };

/** code キー（boardKeyOf と同じ key 系）→ view 状態。 */
const states = new Map<string, CodeViewState>();

function stateOf(key: string): CodeViewState {
	return states.get(key) ?? DEFAULT_CODE_VIEW;
}

// ============================================================================
// calculations（純関数 — vitest 対象）
// ============================================================================

/** 開閉 toggle。 */
export function toggleOpen(s: CodeViewState): CodeViewState {
	return { ...s, open: !s.open };
}

// ============================================================================
// actions（installCodeView + 入口）
// ============================================================================

interface CodeViewController {
	/** 表示 lane の切替（address、null = lane 不在）。roster へ現状態を再通知する。 */
	setActiveLane(address: string | null): void;
	/** 開閉 toggle（Cmd+F / File menu）。lane 不在は no-op。 */
	toggle(): void;
	/** 指定 lane の pane を開く（LaneRow フォルダ — lane 切替と併用する片方向）。 */
	openFor(address: string): void;
}

/** singleton（keybindings / entry / dispatch から module 関数越しに触る）。 */
let controller: CodeViewController | null = null;

/** 開閉 toggle。未 install / lane 不在は no-op（例外を出さない）。 */
export function toggleCodeOpen(): void {
	controller?.toggle();
}

export function installCodeView(): CodeViewController {
	let activeKey: string | null = null;

	/** roster への出入りを lane-panes へ知らせる（tiling 反映は向こうの仕事）。 */
	const dispatchView = (key: string): void => {
		document.dispatchEvent(
			new CustomEvent("vp:code-view", {
				detail: { lane: key, open: stateOf(key).open },
			}),
		);
	};

	const setOpen = (key: string, next: CodeViewState): void => {
		states.set(key, next);
		dispatchView(key);
	};

	controller = {
		setActiveLane(address: string | null): void {
			const key = address === null ? null : boardKeyOf(address);
			if (key === activeKey) return;
			activeKey = key;
			// 新 lane の現状態を roster へ再通知（開いたまま戻ってきた lane の復元）。
			if (key !== null) dispatchView(key);
		},
		toggle(): void {
			if (activeKey === null) {
				console.debug("[code-view] lane 不在のため toggle no-op");
				return;
			}
			setOpen(activeKey, toggleOpen(stateOf(activeKey)));
		},
		openFor(address: string): void {
			const key = boardKeyOf(address);
			if (!stateOf(key).open) setOpen(key, { open: true });
		},
	};
	return controller;
}
