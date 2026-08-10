/**
 * R sidebar — 右の帯（edge rail）のフル幅形 = debug log viewer（sidebar view modes、2026-08-01）。
 *
 * `Cmd+]`（directive `]`、発火は sidebar bundle の handlers.ts → 共有 bus
 * `vp:right-sidebar-toggle`）で rail ⇄ R sidebar を行き来する。旧 WebUI の
 * 「デバッグ右パネル」（doc 44 P1 で撤去）の現代版 — 中身は vp-app 自身と daemon の
 * log file tail（`~/.local/state/vp/log/{app,daemon}.kdl.log`）。
 *
 * ## 層の分離
 *
 * - **data**: `open` / `source`（この module が SSOT）。行 buffer は DOM（#rsb-log）が持つ
 * - **calculations**: `trimToTail`（行 cap の純関数、vitest 済）
 * - **actions**: `installRightSidebar`（toggle 購読 + tab click + watch 依頼）と
 *   push envelope `debuglog:lines` の受け口（`handleLines`）
 *
 * ## tail の購読 model（消費者主導）
 *
 * 開いた時だけ `debuglog:watch` を送り、Rust が該当 file の tail を始める（閉 = unwatch で
 * 停止 — 見ていない log を読み続けない）。source 切替も watch の送り直しで、Rust 側は
 * **最後の watch が勝つ**単一 tail。応答は毎回 `reset=true` の backlog から始まるので、
 * 取りこぼしという概念が無い（stream だが保留箱に頼らない — vp-push.kdl の注記と対）。
 */

export type DebugLogSource = "app" | "daemon";

/** 表示を保つ最大行数。超えたら古い側から捨てる（tail の意味論）。 */
const MAX_LINES = 2000;
/** trim の hysteresis: 毎 chunk で split しないための余裕。 */
const TRIM_AT = MAX_LINES + 500;

/** 全文 text から末尾 `max` 行だけ残す（純関数）。 */
export function trimToTail(text: string, max: number): string {
	const lines = text.split("\n");
	if (lines.length <= max) return text;
	return lines.slice(lines.length - max).join("\n");
}

export interface RightSidebarDeps {
	/** #rsb-log（行の表示先 + scroll container）。 */
	log: HTMLElement;
	/** .rsb-tab 群（data-src="app" | "daemon"）。 */
	tabs: HTMLElement[];
}

export interface RightSidebarApi {
	/** push envelope `debuglog:lines` の受け口（dispatch.ts から接続）。 */
	handleLines(source: string, reset: boolean, lines: string[]): void;
}

export function installRightSidebar(deps: RightSidebarDeps): RightSidebarApi {
	let open = false;
	let source: DebugLogSource = "app";

	const sendIpc = (payload: Record<string, unknown>): void => {
		const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } })
			.ipc;
		ipc?.postMessage(JSON.stringify(payload));
	};

	/** 開閉の反映: body class（CSS が rail と排他表示）+ tail の購読開始/停止。 */
	const applyOpen = (): void => {
		document.body.classList.toggle("rsb-open", open);
		// shell layout（shell-layout.ts）へ「開閉が変わった」を伝える。あちらが取っ手の
		// 出し入れと永続化を持つ。⚠️ **この dispatch が無いと取っ手が出ない**（開いたことを
		// 知らないので display:none のまま = R を掴めない）。
		document.dispatchEvent(
			new CustomEvent("vp:right-sidebar-state", { detail: { open } }),
		);
		if (open) {
			sendIpc({ t: "debuglog:watch", source });
		} else {
			sendIpc({ t: "debuglog:unwatch" });
		}
	};

	// `]` directive（sidebar bundle の handlers.ts）からの依頼。統合 WebView の
	// 同一 document なので共有 bus（doc 47 §6）がそのまま届く。
	document.addEventListener("vp:right-sidebar-toggle", () => {
		open = !open;
		applyOpen();
	});

	// 復元（shell-layout.ts の `vp:shell-restore`）。boot で 1 回だけ来る。
	// ⚠️ 既に同じ状態なら `applyOpen` を呼ばない — 呼ぶと `debuglog:watch` を無駄撃ちする。
	document.addEventListener("vp:shell-restore", (e) => {
		const d = (e as CustomEvent<{ rightOpen?: boolean }>).detail;
		if (typeof d?.rightOpen !== "boolean" || d.rightOpen === open) return;
		open = d.rightOpen;
		applyOpen();
	});

	for (const tab of deps.tabs) {
		tab.addEventListener("click", () => {
			const next = tab.dataset.src as DebugLogSource;
			if (next === source) return;
			source = next;
			for (const t of deps.tabs) t.classList.toggle("active", t === tab);
			// watch の送り直し（Rust は最後の watch が勝つ）。応答の reset=true が表示を差し替える。
			sendIpc({ t: "debuglog:watch", source });
		});
	}

	return {
		handleLines(src, reset, lines) {
			// source 切替直後に届く旧 source の残 chunk は捨てる（表示の混線防止）。
			if (src !== source) return;
			// tail -f の流儀: 末尾に居た時だけ追従する（遡って読んでいる最中は動かさない）。
			const el = deps.log;
			const atBottom =
				reset || el.scrollHeight - el.scrollTop - el.clientHeight < 8;
			if (reset) el.textContent = "";
			el.append(`${lines.join("\n")}\n`);
			if (el.textContent && el.textContent.split("\n").length > TRIM_AT) {
				el.textContent = trimToTail(el.textContent, MAX_LINES);
			}
			if (atBottom) el.scrollTop = el.scrollHeight;
		},
	};
}
