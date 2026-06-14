/**
 * Action — VP の semantic operation の typed 単位（GPUI の Actions pattern を borrow）。
 *
 * keyboard directive（`Cmd+key`）と Command Palette（⌘K）が **同じ `Action.run`** を通ることで、
 * 「keystroke と処理」を疎結合にする。 GPUI 借用 epic #2（`mem_1CbbyeeVfHBqiDPhFJviZR`）。
 *
 * 当面 `run` は既存 `dispatchDirective(key)` に委譲して段階移行する（registry.ts 参照）。
 * handler 本体を if-chain から `Action.run` へ移すのは follow-up。
 */
import type { DirectiveSemantic } from "../../shortcuts/chord-table";

export interface Action {
	/** 安定 id（例: `"directive.f"`）。 palette / 将来の external 起動の key。 */
	id: string;
	/** single-char の `Cmd+key` shortcut（任意。 palette 専用 action は key を持たない）。 */
	key?: string;
	/** Command Palette 表示用の短い title。 */
	title: string;
	/** 長い説明（chord-table の directive SSOT 由来）。 */
	description: string;
	/** 主たる挙動軸（focus-preserving / focus-transferring / panel-local / layout / system）。 */
	semantic: DirectiveSemantic;
	/** 実行。 当面は `dispatchDirective` に委譲、 段階的に handler を移設する。 */
	run(): void;
}
