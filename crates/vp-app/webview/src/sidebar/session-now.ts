/**
 * 「今なにを」(now-line) の sidebar 側 store（doc 58 ②-a）。
 *
 * ## ⚠️ なぜ `store.ts`（SidebarState mirror）に入れないか
 *
 * `applySidebarState` は Rust push を `reconcile(next)` で**全体差し替え**する。
 * client-local な field をあの store に足すと、次の push で無言で消える。
 * ACTIONS（actions-panel/store.ts）と同じ理由の分離 — Rust mirror と client-local は
 * store を分ける。
 *
 * 供給元は editor-host bundle の chatview（`session-now-bridge` 経由の tee）。
 * 鍵は `sessionNowKey(lane, session)` = `<address>#<session>`。
 */
import { createStore } from "solid-js/store";
import { sessionNowKey } from "../../session-now-bridge";

const [sessionNow, setSessionNow] = createStore<
	Record<string, string | undefined>
>({});

/** 名簿が読む reactive な map。`sessionNowKey` で引く。 */
export { sessionNow };

/**
 * tee 1 件を map に畳む。text=null は「今は無い」= 鍵ごと消す
 * （「今」は turn より長生きしない — doc 51 §1 A3）。
 */
export function applySessionNow(
	lane: string,
	session: number,
	text: string | null,
): void {
	setSessionNow(sessionNowKey(lane, session), text ?? undefined);
}
