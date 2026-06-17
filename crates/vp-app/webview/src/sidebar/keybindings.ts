/**
 * Sidebar WebView の directive installer（= VP 規約 v0.3+ directive dispatcher、 sidebar 側）。
 *
 * GPUI 借用 #2 の decouple 後: **処理本体は `actions/handlers.ts`、 dispatch の SSOT は
 * `actions/registry.ts`** に移設済。 本 file は **thin installer** — keydown (chord) 捕捉を
 * registry の `Action.run` に繋ぐだけ。
 *
 * WebView 統合 (step 3a) 後: 統合 DOM では sidebar が同一 window の keydown を直接捕捉するため、
 * 旧 main view bridge（`window.vpSidebar.fireDirective` / app.rs `DirectiveFire` 往復）は撤去した。
 */
import { installDirectiveHandler } from "../shortcuts/chord";
import { actionByKey } from "./actions/registry";
import {
	deleteHintLabel,
	deleteHintVisible,
	laneSelectHintLabel,
	laneSelectHintVisible,
} from "./directive-state";

// hint 信号は keybindings 利用者（Shell.tsx）からも参照されるので re-export（旧 keybindings 互換）。
export {
	deleteHintLabel,
	deleteHintVisible,
	laneSelectHintLabel,
	laneSelectHintVisible,
};

/**
 * key → registry の `Action.run`。 keyboard（chord）からの dispatch entry。
 * 旧 if-chain は actions/handlers.ts へ decouple 済（registry が key→処理 の SSOT）。
 */
export function dispatchDirective(key: string): void {
	const action = actionByKey(key);
	if (action) {
		action.run();
	} else {
		console.debug("[directive] 未登録:", key);
	}
}

export function installSidebarKeybindings(): void {
	installDirectiveHandler({ exec: dispatchDirective });
}
