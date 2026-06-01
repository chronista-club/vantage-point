/**
 * Sidebar WebView の directive installer（= VP 規約 v0.3+ directive dispatcher、 sidebar 側）。
 *
 * GPUI 借用 #2 の decouple 後: **処理本体は `actions/handlers.ts`、 dispatch の SSOT は
 * `actions/registry.ts`** に移設済。 本 file は **thin installer** — keydown 捕捉を registry の
 * `Action.run` に繋ぎ、 main view bridge（`window.vpSidebar.fireDirective`）を公開するだけ。
 *
 * sidebar 内発火（chord）も main view bridge 発火（app.rs `DirectiveFire`）も、 同じ
 * `dispatchDirective` → `actionByKey(key).run()` を通って挙動を一元化する。
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

declare global {
	interface Window {
		/**
		 * main view bridge 経由で sidebar 内 directive を発火する公開 API。
		 * `app.rs` の `AppEvent::DirectiveFire` arm が `sidebar.evaluate_script` で呼ぶ。
		 */
		vpSidebar?: {
			fireDirective(key: string): void;
		};
	}
}

/**
 * key → registry の `Action.run`。 keyboard（chord）と main view bridge 共通の dispatch entry。
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
	// main view bridge から呼ばれる経路: app.rs `AppEvent::DirectiveFire` arm が
	// `sidebar.evaluate_script("window.vpSidebar.fireDirective('<key>')")` を発火。
	// sidebar 内発火と同じ `dispatchDirective` を通すことで挙動を一元化する。
	window.vpSidebar = {
		fireDirective: dispatchDirective,
	};
}
