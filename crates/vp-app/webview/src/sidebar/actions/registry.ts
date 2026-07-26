/**
 * Action registry — directive SSOT（`DIRECTIVE_TABLE`）を typed `Action[]` に昇格する。
 *
 * GPUI 借用 epic #2 の Phase 1。 keyboard（chord.ts）と Command Palette（⌘K）が
 * この registry の `Action.run` を共通で呼ぶ。 `run` は handlers.ts の処理本体を直接持つ
 * （decouple 済 — 旧 `dispatchDirective` 委譲を廃止し、 registry を dispatch の SSOT に）。
 */
import { DIRECTIVE_TABLE } from "../../shortcuts/chord-table";
import {
	runDelete,
	runFileExplorer,
	runLaneSelectMode,
	runNewPerformer,
	runRestart,
	runSendToPP,
	runSwitcher,
} from "./handlers";
import type { Action } from "./types";

/**
 * directive key → Command Palette 表示用の短い title。
 * 長い説明は `DIRECTIVE_TABLE[key].description`（SSOT）を使う。
 */
const DIRECTIVE_TITLES: Record<string, string> = {
	f: "File Explorer を開く",
	p: "選択を Canvas に送る",
	r: "Restart（lane / process）",
	n: "New Performer を開く",
	s: "Lane / Repo 切替",
	d: "Delete（2-click 確認）",
	l: "Lane を番号で切替",
};

/**
 * directive key → 処理本体（handlers.ts）。
 * DIRECTIVE_TABLE の全 key がここに実体を持つ（v0.8 curation で main-view fallback を撤去）。
 */
const DIRECTIVE_RUN: Record<string, () => void> = {
	f: runFileExplorer,
	p: runSendToPP,
	r: runRestart,
	n: runNewPerformer,
	d: runDelete,
	s: runSwitcher,
	l: runLaneSelectMode,
};

/**
 * 全 directive を typed Action に昇格したもの（= Command Palette の表示 source）。
 * 並びは `DIRECTIVE_TABLE` の定義順。
 */
export const ACTIONS: Action[] = Object.entries(DIRECTIVE_TABLE).map(
	([key, entry]) => ({
		id: `directive.${key}`,
		key,
		title: DIRECTIVE_TITLES[key] ?? entry.description,
		description: entry.description,
		semantic: entry.semantic,
		run:
			DIRECTIVE_RUN[key] ??
			(() => console.warn(`[directive] no handler registered for ${key}`)),
	}),
);

/** key（single char）から Action を引く。 keyboard dispatcher 用。 */
export function actionByKey(key: string): Action | undefined {
	return ACTIONS.find((a) => a.key === key);
}

/** id から Action を引く。 Command Palette / external 起動用。 */
export function actionById(id: string): Action | undefined {
	return ACTIONS.find((a) => a.id === id);
}
