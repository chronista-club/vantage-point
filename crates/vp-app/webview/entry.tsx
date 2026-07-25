/**
 * vp-app WebView 用 entry point.
 *
 * SolidJS + @chronista-club/creo-ui-editor-host を bundle して、main WebView の `<div id="editor-root">`
 * に EditorLayer を mount する。
 *
 * 起動: Ctrl+Shift+E で Editor Mode が toggle される (@chronista-club/creo-ui-editor-host の default keybind)。
 *
 * 主要 features (@chronista-club/creo-ui-editor-host から継承):
 * - DOM auto-discover: 既知の CSS 変数 (--typography-family-mono など) を自動 bind
 * - DevTools Console REPL: window.creoEditor.slider(...) 等で field 動的追加
 * - URL shareable state: #creo=... で URL 1 本で共有
 * - Cross-tab sync: 同 origin の複数 tab で values 追従
 * - Theme switching: 8 theme (mint-dark/light, sora-*, contrast-*, oldschool-*)
 *
 * Build:
 *   cd crates/vp-app/webview && bun install && bun run build
 *
 * 出力: ../assets/editor-host.bundle.js (vp-app の Rust 側で include_str!)
 */

// VP-140 diagnostic: bundle が parse + execute されたことを最速で confirm する。
// import 後のいかなる runtime error があっても、 この 1 行は console に出る。
console.info("[vp-bundle] booting (VP-140 diagnostic)");
(
	window as unknown as { vpBundleStatus?: Record<string, boolean> }
).vpBundleStatus = {
	booted: true,
	importsResolved: false,
	vpFrameSet: false,
};
// console bridge: webview の console.* を vp-app log (app.kdl.log) に転送する。
// agent が DevTools を開かずに console を Read/watch_file で観測するための経路。
// Rust 側 handle_ipc_message の `console` arm が tracing で書く。 console 自体は
// 壊さない (orig を必ず呼ぶ + 転送失敗は握り潰す)。
//
// ⚠️ install は無条件、 ipc は **call 時に lookup**: この IIFE は module-eval (早期) で
// 走り、 その時点で wry の window.ipc が未注入のことがある。 install 時に capture すると
// 永久に無効化されるため、 各 console 呼び出し時に毎回引く (注入後の console.* も拾える)。
(() => {
	const levels = ["log", "info", "warn", "error", "debug"] as const;
	for (const level of levels) {
		const orig = console[level].bind(console);
		console[level] = (...args: unknown[]) => {
			orig(...args);
			const ipc = (
				window as unknown as { ipc?: { postMessage(m: string): void } }
			).ipc;
			if (!ipc) return;
			try {
				// 引数ごとに guard — circular ref で 1 個 throw しても他の引数は残す
				const text = args
					.map((a) => {
						if (typeof a === "string") return a;
						try {
							return JSON.stringify(a);
						} catch {
							return "[unserializable]";
						}
					})
					.join(" ");
				ipc.postMessage(JSON.stringify({ t: "console", level, text }));
			} catch {
				/* 転送失敗は無視 — console は既に出力済 */
			}
		};
	}
})();

window.addEventListener("error", (e) => {
	console.error(
		"[vp-bundle] window.error",
		e.message,
		e.filename,
		e.lineno,
		e.error,
	);
});
window.addEventListener("unhandledrejection", (e) => {
	console.error("[vp-bundle] unhandledrejection", e.reason);
});

import { render } from "solid-js/web";
import {
	EditorHostProvider,
	EditorLayer,
	bind,
	color,
	cssVarNumberTarget,
	cssVarTarget,
	number,
	useEditorHost,
} from "@chronista-club/creo-ui-editor-host";
import {
	applyAppScene,
	closeAppPaneVisit,
	installAppPanes,
	restoreAppStateFor,
	saveAppStateFor,
	visitAppPane,
} from "./app-panes";
import { layoutEngine } from "./layout-host";
import { installGallery } from "./gallery-panes";
import { attachKeybindings } from "./keybindings";
import { renderPP, clearPP, appendPP } from "./pp";
import { installConsole, focusedOf, sessionActOf } from "./console";
// doc 46 P1 → doc 49 LE-P4 PR2: lane 内 tiling（creo-ui-layout の lane scope）。
// + New（engine × Act で新 session）は EchoesHeader へ移設済み（doc 51 §1 A1 — 帯の退役）。
import { chatHostId, installLanePanes } from "./lane-panes";
import { installChatView, CHATVIEW_CSS, handoffKey } from "./chatview";
import {
	mountEchoesHeader,
	ECHOES_HEADER_CSS,
	type EchoesHeaderApi,
} from "./EchoesHeader";
import { renderDevices as renderBastetDevices } from "./bastet";
import {
	handleMessage as handleBoardMessage,
	setActiveLaneName,
	clearActiveBoard,
	getCanvasState,
} from "./board-handler";
// ink（対話面、doc 52 §3）: board item の上に描いて明示送信で snapshot + 一行を会話へ。
import { installInk } from "./ink";
import { mountHistoryStrip, HISTORY_STRIP_CSS } from "./HistoryStrip";
import { mountResyncLoader, RESYNC_LOADER_CSS } from "./resync-loader";

console.info("[vp-bundle] imports resolved");
(window as unknown as { vpBundleStatus?: Record<string, boolean> })
	.vpBundleStatus!.importsResolved = true;

// ===== doc 49 LE-P4 PR1: app pane 配置を creo-ui-layout の場へ =====
// 旧 FrameEngine（VP-140 の Scene engine）の後継。preset / DOM 反映は app-panes.ts に
// 集約され、ここは購読の install と setActivePane bridge（下方）だけを持つ。
//
// data-frame-id 規約 (main_area.rs HTML 側で付与):
//   echoes  → pane-terminal      (Echoes Stand = lane workbench。console/chat/board の tiling を内包)
//   ge      → pane-gold-experience (Gold Experience 🌿)
//   bs      → pane-bastet         (Bastet 🧲 / device 一覧)
//   preview → pane-preview        (iframe preview)
//   empty   → pane-empty          (no selection)
//   doc 52 §10 wave 0: pp（Paisley Park）は app pane を退役 → lane tiling の board pane (#lane-board)
// 注: 旧 data-pane-id (main_area.rs inline JS が Lane address 等に書き換える native overlay sync 用)
// と attribute を分離。 同名にすると Lane click で legacy 側が hijack して配置 lookup が
// undefined → 非表示投影で pane が見えなくなる回帰を起こすため (VP-141 fix)。

// DOM 反映（engine 購読）+ keybindings hook
installAppPanes(document);
attachKeybindings(window);
// WebView 統合 (step 3a): 旧 installMainViewDirectiveBridge は削除。
// sidebar + main が 1 DOM になったため、 directive は sidebar bundle の in-process
// handler (src/sidebar/keybindings.ts の installDirectiveHandler) が同一 window で
// 直接捕捉する。 IPC 往復 bridge を残すと 1 回の Cmd hold + key で二重発火する。

// ===== legacy setActivePane bridge + per-Lane 配置記憶 =====
// 既存 main_area.rs JS が定義する window.setActivePane を wrap して、
// 旧 logic (showLane / preview iframe src 切替 / sendSlotRect) を保ったまま
// app-panes に配置切替を発火させる。
//
// per-Lane 配置の記憶 (VP-141 follow-up の後継):
// - kind=terminal Lane 切替時に旧 Lane の配置 snapshot を save、 新 Lane の保存済
//   snapshot (or default lead-focus) を restore する → user が Lane を跨いでも
//   Side Review / PP Overlay 等の選択 + share 調整の形が記憶される（app-panes.ts 所有）
// - kind != terminal (PP/GE/Bastet click 等) は Lane を跨がない fixed-Pane focus、 記憶は更新しない
const KIND_TO_PANE: Record<string, string> = {
	terminal: "echoes",
	gold_experience: "ge",
	bastet: "bs",
	preview: "preview",
	empty: "empty",
	// doc 52 §10 wave 0: paisley_park → pp は退役（board pane = lane tiling へ移設）。
	// LE-P4 PR1: 幽霊の hermit_purple → hp（DOM 不在）を落とし、DOM に居た bastet → bs を
	// 補充（旧体系では unknown kind → empty に落ちて Bastet pane が見えなかった）
};

interface SetActivePaneInfo {
	kind?: string | null;
	pane_id?: string | null;
	preview_url?: string | null;
	/** doc 33: chat lane (Act II) フラグ（Rust push_active_view 由来）。 */
	chat?: boolean;
	// Echoes 共通ヘッダ用 lane 文脈（setActivePane 相乗り、creo memo `vp-pane-common-header`）
	lane_name?: string | null;
	cwd?: string | null;
	branch?: string | null;
	/** active engine の session id（Act I の session chip 供給路。Act II は event が上書き）。 */
	session_id?: string | null;
	/** root session の stand（= slot に載る engine 種別、chip prefix 導出用: "echoes" / "codex" /
	 *  "grok" 等）。doc 39 P4-C: Rust push_active_view が engine_stand（root の engine）優先で解決
	 *  済み（cross-engine root でも chip prefix が slot の engine を映す）。無ければ lane 固定 stand。 */
	stand?: string | null;
}

/** 現 active Lane の address (Lane 跨ぎの save+restore base). null = まだ Lane click していない. */
let activeLaneAddress: string | null = null;

/** Echoes 共通ヘッダ（pane-host 上端 strip）。mount は vpConsole install 後（下方）、
 *  setActivePane bridge が lane 文脈を届ける。null = mount 点不在（graceful skip）。 */
let echoesHeader: EchoesHeaderApi | null = null;

/**
 * LaneAddress::Display 形を board-handler が使う flat lane_name に翻訳する。
 * `null` = conductor（lead）、`string` = performer 名。
 *
 * D2 統一: 語彙は root/performer。rename 途上のため legacy `lead`/`wing` も受理する:
 * - `<project>/root` / `<project>/lead` → `null`（root/lead）
 * - `<project>/performer/<name>` / `<project>/wing/<name>` → `<name>`（performer）
 *
 * この値は (a) pp-content-persist の SurrealDB record key、(b) per-lane PP の
 * canvas filter token（`null`→`conductor` に正規化して producer の lane と突合）に使う。
 */
function laneNameFromAddress(addr: string | null): string | null {
	if (!addr) return null;
	if (addr.endsWith("/root") || addr.endsWith("/lead")) return null;
	const m = addr.match(/\/(?:performer|wing)\/(.+)$/);
	if (m) return m[1] ?? null;
	return null;
}
const installSetActivePaneBridge = (): void => {
	const w = window as unknown as {
		setActivePane?: (info: SetActivePaneInfo | null) => void;
	};
	const original = w.setActivePane;
	w.setActivePane = (info) => {
		// 旧 logic を先に呼ぶ (showLane / preview iframe / sendSlotRect 等)
		if (typeof original === "function") {
			try {
				original(info);
			} catch (e) {
				console.warn("[app-panes] legacy setActivePane error", e);
			}
		}
		// app-panes に配置を発火
		if (!info || !info.kind || info.kind === "empty") {
			applyAppScene("empty");
			// lane 無し = Echoes 共通ヘッダも空へ（chips は presence-driven）。
			echoesHeader?.setLane(null);
			return;
		}
		// kind=terminal: Lane 切替判定 + 保存済配置の restore + show-subscriber 付替
		if (info.kind === "terminal" && info.pane_id) {
			const newLane = info.pane_id;
			// ⚠️ restore は **lane が本当に変わった時だけ**。header refresh（engine_session_id /
			// branch 変化等）は同一 lane に setActivePane を再送してくるため、無条件 restore だと
			// hotkey で選んだ配置（save 未経由）が黙って巻き戻る（team-b review #1）。
			const laneChanged = activeLaneAddress !== newLane;
			// Lane が変わったら旧 Lane の配置 snapshot を save（empty が主役なら app-panes 側で skip）
			if (activeLaneAddress && laneChanged) {
				saveAppStateFor(activeLaneAddress);
			}
			activeLaneAddress = newLane;
			// Echoes 共通ヘッダを当該 lane の文脈に更新（kind != terminal では触らない =
			// PP 等を眺めている間も直前の lane 文脈が載り続ける）。
			echoesHeader?.setLane({
				addr: newLane,
				name: info.lane_name ?? null,
				cwd: info.cwd ?? null,
				branch: info.branch ?? null,
				chat: !!info.chat,
				sessionId: info.session_id ?? null,
				stand: info.stand ?? null,
			});
			// wiremsg Stage 2: canvas (PP body) の供給は Rust 側 spawn_canvas_subscription が
			// per-SP で担うため、Lane 切替時の JS 側 WS 付替は不要 (旧 setWantedLane を撤去)。
			// 保存済配置を restore、 初訪問 Lane は lead-focus を default にする
			if (laneChanged) restoreAppStateFor(newLane);
			// doc 50 §4.6 A6: lane の表示を開く（roster 同期 + focus）。旧実装は
			// 'vp:console-mode'（lane 単位 mode の到着）が契機だったが、見え方が session の
			// 属性になったので lane 切替そのものが契機になる。
			if (laneChanged) applyLaneView(newLane);
			// board モデル: lane 切替時に active lane を更新する。 lane board は canvas channel で既に
			// retained 受信済みなので、 setActiveLaneName で表示 board を切り替えるだけでよい（別 load 不要）。
			// LaneAddress::Display 形 (`<project>/lead` or `<project>/wing/<name>`) を flat lane_name に翻訳。
			const laneName = laneNameFromAddress(newLane);
			setActiveLaneName(laneName);
			return;
		}
		// kind != terminal (PP/GE/Bastet/preview click 等): stand pane の**訪問**（一時 view）。
		// Lane の配置記憶には焼き込まず、✕（close-pane）で出発点の配置に戻れる
		const paneId = KIND_TO_PANE[info.kind];
		if (!paneId) {
			console.warn("[app-panes] unknown kind for setActivePane:", info.kind);
			applyAppScene("empty");
			return;
		}
		visitAppPane(paneId);
	};
};

// wiremsg Stage 2: Rust 注入口。Rust 側 spawn_canvas_subscription が active project の
// canvas ProcessMessage ごとに `window.vpBoard.handleMessage(msg)` を evaluate_script で呼ぶ。
// DevTools から手動 trigger も可: window.vpBoard.handleMessage({type:'show',content:{markdown:'# hi'}})
(
	window as unknown as {
		vpBoard: { handleMessage: typeof handleBoardMessage };
	}
).vpBoard = {
	handleMessage: handleBoardMessage,
};

// doc 19 PP Canvas Stack Model: HistoryStrip CSS を head に注入 + DOMContentLoaded で mount。
// PP pane の DOM (#pp-history-strip) は main_area.rs HTML 側で保証される。
const historyStripStyle = document.createElement("style");
historyStripStyle.textContent = HISTORY_STRIP_CSS;
document.head.appendChild(historyStripStyle);

// Act II 再同期コーナーローダー: CSS を head 注入（mount は applyDefaultScene で）。
const resyncLoaderStyle = document.createElement("style");
resyncLoaderStyle.textContent = RESYNC_LOADER_CSS;
document.head.appendChild(resyncLoaderStyle);

// PP Canvas font — font zero-start (2026-07-11): 旧 ルイカ等幅 (TLT-RuikaMono-02、
// 2026-06-01 の console look 意匠) の font-family 注入を撤去。 PP の書体は main_area.rs 側の
// principal token (本文 = --vp-font-sans / code = --typography-family-mono) に従う。
// この style block には font 以外の意匠 (font-size 段上げ / mermaid 余白) が残るため維持する。
const ppFontStyle = document.createElement("style");
ppFontStyle.textContent = `
/* PP body の base font-size を 1 段上げる (= creoui token chain: base → l)。
   fallback で 1.125em (= 16→18px 相当)。 #pp-content scope 内のみ override し
   sidebar 等の別 webview には波及しない。 marked-based revert (#477) で .creo-md
   wrapper 廃止に伴い selector を #pp-content 直に向け直し (= 2528097 の復活)。 */
#pp-content {
  font-size: var(--typography-size-l, 1.125em);
}
/* mermaid SVG wrapper の余白 — code block 置換後の見栄え */
#pp-content .creo-md-mermaid { margin: 1em 0; }
#pp-content .creo-md-mermaid svg { max-width: 100%; height: auto; }
#pp-content .creo-md-mermaid-error {
  font-family: var(--vp-font-mono),var(--typography-family-mono);
  color: var(--color-text-secondary, #c66);
  background: var(--color-surface-bg-subtle, #1a1a22);
  padding: 8px; border-radius: 4px; white-space: pre-wrap;
}
`;
document.head.appendChild(ppFontStyle);

// 起動時 default Scene apply + HistoryStrip mount
const applyDefaultScene = (): void => {
	installSetActivePaneBridge();
	const ok = applyAppScene("lead-focus");
	const paneCount = document.querySelectorAll("[data-frame-id]").length;
	// 診断 log: 配置が apply された事実と、 data-frame-id 要素の存在を確認できるようにする。
	// user 環境で 「画面が黒い」 等の issue 時に DevTools console で path を即時切り分けできるよう常時出力。
	console.info(
		`[app-panes] applied default scene = lead-focus (ok=${ok}); panes detected = ${paneCount}`,
	);
	// doc 19: PP body 下の history strip を SolidJS で mount。
	mountHistoryStrip();
	// Act II 再同期コーナーローダーを body 直下に mount（active lane の replaying に追従）。
	mountResyncLoader();
};
if (document.readyState === "loading") {
	document.addEventListener("DOMContentLoaded", applyDefaultScene, {
		once: true,
	});
} else {
	applyDefaultScene();
}
// Unison WebTransport echo probe (GUI redesign 北極星 step 2/3、 protocol close)。
// DevTools console から手動 trigger: `await window.vpUnisonEcho('<CERT_HASH>')`。
// echo server: `cargo run -p club-unison --example webtransport_echo_server -- '[::1]:4433'`。
// 動的 import で SDK を遅延ロードし、 通常 boot path の bundle 初期化を汚さない。
(
	window as unknown as {
		vpUnisonEcho: (certHash: string, url?: string) => Promise<unknown>;
	}
).vpUnisonEcho = async (certHash: string, url?: string) => {
	const { runUnisonEchoProbe } = await import("./unison-echo-probe");
	return runUnisonEchoProbe(certHash, url);
};
// auto-run: vp-app が VP_UNISON_ECHO_CERT 付きで起動すると init script が
// window.__VP_ECHO_CERT__ を注入する。 検出したら probe を自動実行し、 結果を
// console (= bridge 経由で app.kdl.log) に出す。 agent が log を読んで観測する。
{
	const echoCert = (window as unknown as { __VP_ECHO_CERT__?: string })
		.__VP_ECHO_CERT__;
	if (echoCert) {
		(window as unknown as { vpUnisonEcho: (c: string) => Promise<unknown> })
			.vpUnisonEcho(echoCert)
			.then((r) => console.log("[echo-probe-result]", JSON.stringify(r)))
			.catch((e) => console.error("[echo-probe-result] error", String(e)));
	}
}

// DevTools 検査用 (window.vpAppLayout.applyScene('ge-focus') 等で手動 trigger 可能)
(window as unknown as { vpAppLayout: unknown }).vpAppLayout = {
	engine: layoutEngine,
	applyScene: applyAppScene,
};
// vpFrameSet は main_area.rs の boot 診断 field 名（旧 FrameEngine 由来）。意味は
// 「layout 配線まで bundle init が到達した」— Rust 側 field 名の churn を避けて据え置く。
(window as unknown as { vpBundleStatus?: Record<string, boolean> })
	.vpBundleStatus!.vpFrameSet = true;
console.info("[vp-bundle] vpAppLayout attached to window — bundle init complete");

// ===== VP-141 / PR-ε-2: PP markdown render API =====
// window.vpPP で PP body の renderPP / clearPP / appendPP を公開。 PR-ε-3 で /ws/show 経由
// mcp__show が来た時の inject point として使う。 DevTools console から手動 trigger 可能:
//   window.vpPP.renderPP("# Hello\n\n**bold**")
(
	window as unknown as {
		vpPP: {
			renderPP: typeof renderPP;
			clearPP: typeof clearPP;
			appendPP: typeof appendPP;
		};
	}
).vpPP = {
	renderPP,
	clearPP,
	appendPP,
};

// ===== Echoes Act II (doc 33): Console facade + ChatView =====
// window.vpConsole を公開（EchoesEvent の per-lane ring buffer + ChatView renderer 接続点）。
// Rust event loop が `window.vpConsole.handleEvent(lane, event)` で EchoesEvent を届け、
// `window.vpConsole.setMode(lane, mode)` でエンジンモードを通知する。
// DevTools 検分: window.vpConsole.peek("<project>/root")
const vpConsole = installConsole();

// ink（対話面、doc 52 §3）を board pane に配線する。lane 文脈は closure で注入:
//   - getItemId    = 表示中 board item（board-handler の cursor）
//   - getLaneAddress = 現 active lane の address（setActivePane bridge が更新する module 変数）
//   - getFocusedSession / getSessionAct = console.ts の per-lane registry
// server は触らない（既存 IPC echoes:submit / term:write を撃つだけ、doc 52 §3 = 状態ゼロの往復）。
installInk({
	getItemId: () => getCanvasState().cursor,
	getLaneAddress: () => activeLaneAddress,
	getFocusedSession: (laneAddr) => focusedOf(laneAddr),
	// doc 50 §4.6 A6: 送り先は focused session の act（旧 lane 単位 vpConsole.getMode）。
	getSessionAct: (laneAddr, session) => sessionActOf(laneAddr, session),
});

// ChatView (C2 → doc 50 P1): scoped CSS を注入し、mount 管理 API を得る。
// SessionChatView の mount 先は lane-panes が session ごとに生やす動的 host
// （旧 #console-chat-host 固定 1 枚は session ↔ Pane 1:1 で退役）。
const chatStyle = document.createElement("style");
chatStyle.textContent = CHATVIEW_CSS;
document.head.appendChild(chatStyle);
const chatView = installChatView(vpConsole);

// ===== Echoes 共通ヘッダ（creo memo `vp-pane-common-header`）=====
// pane-host 上端の #echoes-header（main_area.rs が mount 点だけ提供）に strip を mount。
// Act I/II のどちらを表示していても同一ヘッダが載り続ける（lane の Echoes に帰属）。
const headerStyle = document.createElement("style");
headerStyle.textContent = ECHOES_HEADER_CSS;
document.head.appendChild(headerStyle);
const echoesHeaderHost = document.getElementById("echoes-header");
if (echoesHeaderHost) {
	echoesHeader = mountEchoesHeader(echoesHeaderHost, vpConsole);
} else {
	console.warn(
		"[vp-bundle] #echoes-header が見つかりません — 共通ヘッダ mount をスキップ",
	);
}

// doc 46 P1 → doc 49 LE-P4 PR2 → doc 50 P1 → doc 51 §1 A1: lane 内 tiling は creo-ui-layout の
// lane scope が担い、pane の顔ぶれは session 一覧 × console_mode から動的に導く
// （lane-panes.ts の lanePaneRefs が SSOT）。下端の帯（#pane-tabs）は退役 — pane chip は
// tiling 既定で存在理由が消え、+ New は EchoesHeader（lane の名札）右端へ移設した。
// ⚠️ xterm（lane-host）の**中身**には触れず、host 要素の style / class だけを操る。
// chat session host は lane-panes が生成し、中身は chatView.mountSession が入れる。
const paneFrame = document.getElementById("pane-terminal");
const lanePanesEl = document.getElementById("lane-panes");
const lanePanes =
	paneFrame && lanePanesEl
		? installLanePanes({
				hostOf: (id: string) => document.getElementById(id),
				container: lanePanesEl,
				mountChat: (host, lane, session) =>
					chatView.mountSession(host, lane, session),
				// doc 50 §4.6 A6 ②: term pane にも名札（素性 + kind badge）を出す。
				mountTermPlate: (host, lane, session) =>
					chatView.mountTermPlate(host, lane, session),
			})
		: null;
if (lanePanes && paneFrame) {
	// 要件 3: click で focus が移る。Pane の中身の click は素通しさせたいので capture で拾う。
	paneFrame.addEventListener(
		"click",
		(e) => {
			const host = (e.target as HTMLElement | null)?.closest(
				"#lane-host, .chat-session-host",
			);
			if (host?.id) lanePanes.focusPane(host.id);
		},
		true,
	);
}

// mode に応じて表示を追従させる（表示は既定 tiling、doc 51 §1 — mako 2026-07-24 同時注視。
// 旧「1 枚ずつ = showOnly」は doc 47 §1 決着までの暫定だった）。
//
// lane の表示を開く（doc 50 §4.6 A6 — 旧 `applyConsoleMode` の後継）。
//
// pane の顔ぶれ（roster）は lane-panes.ts が session 一覧 × 各 session の act から導出する
// （'vp:echoes-sessions' / 'vp:session-act' 購読）。ここは lane 切替と focus だけを担う。
// 旧実装は lane 単位 mode を引数に取っていたが、見え方が session の属性になったので
// 「lane を開く」操作から mode の概念が消えた。
const applyLaneView = (lane: string): void => {
	// `showLane` は**必ず**呼ぶ（renderer attach + sessions_fetch）。session 一覧が届かないと
	// pane の顔ぶれ（lanePaneRefs）が空のままになる。
	chatView.showLane(lane);
	// doc 47 §3: Pane 構成は **lane ごと**（= engine の lane scope）。DOM host は app 共有
	// なので、lane が変わったら新 lane の配置を DOM へ写し直す（これが無いと「どの lane に
	// 移動しても前の構成のまま」= doc 46 P1 の実機で観測された症状）。
	lanePanes?.setActiveLane(lane);
	// focus 先 = focused session の pane（session ↔ Pane 1:1）。その session の act で
	// host が決まる（term なら xterm、chat なら ChatView）。focused は console.ts の
	// registry が真値（echoes_session_list で同期済み。未知 lane は 1 = 旧 SP 互換）。
	// pane がまだ生えていない boot 窓は lane-panes 側が pendingFocus で救済する。
	lanePanes?.focusPane(chatHostId(focusedOf(lane)));
	// doc 38 §4.3: 再同期ローダー（global fixed 要素）は lane 切替で必ず下ろす。
	// resync-loader は activeLane の replaying を読むだけなので、stuck した replaying が
	// 新しい表示の上に居座るのを防ぐ。
	chatView.clearReplaying(lane);
};

// doc 33 §9: Act I⇄II 切替の progress overlay + switch lock。
// 「resume 確定まで切替を見せる + 二重切替を防ぐ」= 安全なハンドオフ。
const switchingOverlay = document.getElementById("console-switching");
const switchingMsg = switchingOverlay?.querySelector(
	".console-switching-msg",
) as HTMLElement | undefined;
// 進行中の handoff。null = idle。set 中は同 session の再切替をロックする。
// doc 50 §4.6 A6: 切替は session 単位（名札 kind badge）になったので、lock も session を持つ。
// 進行中の handoff を **pane（= (lane, session)）ごと**に持つ（doc 50 §4.6 A6）。
//
// ⚠️ 単一 slot（`{lane, session} | null`）だと「どれか 1 つでも進行中なら全部弾く」になり、
// **無関係な pane の badge click を無言で落とす**（A6 で全 pane が badge を持つので実際に
// 起こる。team-b review 2026-07-25 score 85 — 解除側は (lane, session) を照合していたのに
// 入口だけ素の存在チェックで、入口と出口が非対称だった）。Map なら独立に始めて独立に終わる。
const handoffPending = new Map<string, "tui" | "chat">();
let handoffTimer: number | undefined;

const beginHandoff = (
	lane: string,
	session: number,
	target: "tui" | "chat",
): void => {
	handoffPending.set(handoffKey(lane, session), target);
	if (switchingMsg) {
		switchingMsg.textContent =
			target === "chat"
				? "Act II にセッションを引き継ぎ中…"
				: "Act I にセッションを引き継ぎ中…";
	}
	switchingOverlay?.classList.add("active");
	// safety: ready 信号が来なくても 30s で全解除（stuck 防止の網。正常系は個別に解除される）。
	if (handoffTimer) clearTimeout(handoffTimer);
	handoffTimer = window.setTimeout(() => {
		handoffPending.clear();
		endHandoffIfIdle();
	}, 30000);
};
/** 1 つの handoff を終える。**全部終わってから** overlay を畳む（他が進行中なら出したまま）。 */
const endHandoff = (lane: string, session: number): void => {
	handoffPending.delete(handoffKey(lane, session));
	endHandoffIfIdle();
};
const endHandoffIfIdle = (): void => {
	if (handoffPending.size > 0) return;
	if (handoffTimer) {
		clearTimeout(handoffTimer);
		handoffTimer = undefined;
	}
	switchingOverlay?.classList.remove("active");
};

// act 適用（tui=PTY respawn 済 / chat=engine スロット確定）で overlay を clear。
// doc 33 §9 改訂（Act I レベルに合わせる）: chat 行きも session_init を待たず、
// act 適用で即解除する。Act I の「切替は即・claude の load は非同期」と同じ哲学で、
// overlay が engine 起動（resume 確定）を gate して固まるのを防ぐ。切替を表示した
// のと同じ 'vp:session-act' で overlay も畳むので、ハングが構造的に起きない。
document.addEventListener("vp:session-act", (e) => {
	const d = (
		e as CustomEvent<{ lane: string; session: number; act: "tui" | "chat" }>
	).detail;
	if (!d?.lane || !d.session) return;
	// 自分が始めた切替（同じ (lane, session) で同じ target）だけを終える。
	if (handoffPending.get(handoffKey(d.lane, d.session)) === d.act) {
		endHandoff(d.lane, d.session);
	}
});
// chat: engine が resume を確定 (session_init) したら、act 適用より早ければ先に clear
// する belt-and-suspenders（overlay の完了条件ではなく「更に早い解除」の位置づけ）。
// ⚠️ この event は lane しか運ばないので、当該 lane で **chat 行きの** handoff を畳む
// （session を特定できないため、chat 待ちのものだけを対象にする）。
document.addEventListener("vp:console-ready", (e) => {
	const detail = (e as CustomEvent<{ lane: string }>).detail;
	if (!detail?.lane) return;
	for (const [key, target] of [...handoffPending]) {
		if (target === "chat" && key.startsWith(`${detail.lane}#`)) {
			handoffPending.delete(key);
		}
	}
	endHandoffIfIdle();
});

// Act 切替（見え方の乗り換え、doc 50 §4.6 A6 ②）: 入口は **各 pane の名札 kind badge**。
// 「この pane が何であるか」の一部なので名札（上段）が住処 — §3.1 の「下段右端は消えるまでの
// 置き場」の終着点がここ。旧 lane-level Act toggle（帯）は doc 51 §1 A1 で、root picker の
// 「見え方」行（pre-A6 の仮住まい）は本 A6 で退役した。
// handoff overlay / 二重切替 lock はここ（entry.tsx）が持ち続け、名札からは event で依頼される
// — overlay の DOM / timer と名札の実装を絡ませない。
document.addEventListener("vp:act-switch-request", (e) => {
	const d = (
		e as CustomEvent<{ lane: string; session: number; target: "tui" | "chat" }>
	).detail;
	if (!d?.lane || !d.session || !d.target) return;
	// resume 確定前の二重切替をロック（中間状態を作らない）。**同じ pane の**再クリックだけを
	// 弾く — 他の pane の切替は独立に始められる（A6 で全 pane が badge を持つ）。
	if (handoffPending.has(handoffKey(d.lane, d.session))) return;
	// 押下で即 progress を出す（round-trip 前に反応 = 待ち時間を可視化）。
	beginHandoff(d.lane, d.session, d.target);
	const ipc = (
		window as unknown as { ipc?: { postMessage(m: string): void } }
	).ipc;
	// 宛先 session は引数で運ぶ（doc 50 §4.3 — focus 依存の分割はレース）。
	ipc?.postMessage(
		JSON.stringify({
			t: "session:set_act",
			lane: d.lane,
			session: d.session,
			act: d.target,
		}),
	);
});

// ===== Bastet 🧲 device 一覧 render API =====
// window.vpBastet.renderDevices(devices) で Bastet pane (pane-bastet) に接続中 device を render。
// Rust が device event 時に main_view.evaluate_script で呼ぶ (= world-device bridge の出口)。
(
	window as unknown as {
		vpBastet: {
			renderDevices: typeof renderBastetDevices;
		};
	}
).vpBastet = {
	renderDevices: renderBastetDevices,
};
// boot 窓 catch-up: world-device の接続時 snapshot は bundle ロード前に届いて renderDevices
// guard で落ちている可能性がある — view の誕生時に一覧を pull する（lanes:ensure-all の同型。
// 逆順（bundle が先）でも fetch は空を返すだけで、後から届く snapshot の push が埋める）
(window as unknown as { ipc?: { postMessage(m: string): void } }).ipc?.postMessage(
	JSON.stringify({ t: "bastet:devices_fetch" }),
);

// board pane の boot 窓 catch-up（doc 52 §10 wave 0）: board の retained BoardUpdated は
// bundle ロード前に届いて `window.vpBoard &&` guard で落ちる → reopen で board pane が出ない。
// vpBoard install 済のこの時点で Rust に保持済み board snapshot の再配信を要求する
// （bastet:devices_fetch と同型。activeLane 未設定でも boardByLane に presence が積まれ、
//  lane 選択時に board pane が生える）。
(window as unknown as { ipc?: { postMessage(m: string): void } }).ipc?.postMessage(
	JSON.stringify({ t: "board:demand" }),
);

// ===== Pane action button delegation =====
// 各 pane の `[data-action]` button を click delegation で hook。 S2 では Clear のみ実装、
// data-target 属性で対象 surface を識別 (`pp` = Paisley Park body)。 将来的に Pin / Lane 切替
// 等を追加する場合も同 delegation で wire 可能。
document.addEventListener(
	"click",
	(event) => {
		const target = event.target as HTMLElement | null;
		const btn = target?.closest("[data-action]") as HTMLElement | null;
		if (!btn) return;
		const action = btn.dataset.action;
		const dataTarget = btn.dataset.target;
		if (action === "close-pane") {
			// stand pane の ✕ — 訪問を閉じて出発点の配置へ（2026-07-23 dogfood:
			// 「Bastet が出っ放しで close できない」の根治）
			closeAppPaneVisit();
			return;
		}
		if (action === "clear") {
			if (dataTarget === "pp") {
				// doc 19 PP Canvas Stack Model: clear は items + cursor + DOM の 3 つを全 reset
				// する semantic。 `clearPP()` 直叩きだと canvasState (items / cursor) が残り、
				// strip は表示されたまま main だけ空になる非対称が起きる (= team-b review で発覚)。
				// board-handler の `handleMessage({type:'clear'})` 経路で stack 含めて全 reset する。
				clearActiveBoard();
			} else {
				console.warn("[vp-bundle] clear: unknown target", dataTarget);
			}
		}
	},
	// bubbling で取る (capture せず) — pane-header 内 button click は default で bubble する
	false,
);

// ===== sidebar Live Token の恒久 bind (2026-07-11 Editor Mode 作業台化) =====
// auto-discover は :root の既知 prefix (--color- 等) しか拾わないため、 vp-app 固有の
// --sb-text-* 4 token をここで明示 bind する (REPL 手動 creoEditor.slider() の恒久化)。
// 定義は sidebar/Shell.tsx の SHELL_CSS `:root` ブロック — editor-host の cssVarTarget が
// documentElement.style.setProperty で書くのと同 scope に揃えてある (#sidebar-root 定義の
// ままだと「近い祖先が勝つ」で slider 書き込みがマスクされる)。
// bind() は useEditorHost() を呼ぶので EditorHostProvider ツリー内で実行が必須 —
// component の setup phase で回す。 UI は持たないので null を返す。
function SidebarTokenBinds() {
	// text scale 4 段。 range は現値 ±数 px の演奏域 (heuristic 任せにしない)。
	const tokens: Array<{
		id: string;
		cssVar: string;
		value: number;
		min: number;
		max: number;
	}> = [
		{
			id: "sb.text.base",
			cssVar: "--sb-text-base",
			value: 13,
			min: 10,
			max: 18,
		},
		{
			id: "sb.text.hint",
			cssVar: "--sb-text-hint",
			value: 12,
			min: 9,
			max: 16,
		},
		{
			id: "sb.text.meta",
			cssVar: "--sb-text-meta",
			value: 11,
			min: 8,
			max: 15,
		},
		{
			id: "sb.text.micro",
			cssVar: "--sb-text-micro",
			value: 10,
			min: 7,
			max: 14,
		},
	];
	tokens.forEach((t, i) => {
		bind<number>({
			target: cssVarNumberTarget(t.id, t.cssVar, t.value, "px"),
			control: number({
				min: t.min,
				max: t.max,
				step: 0.5,
				unit: "px",
				variant: "slider",
			}),
			placement: {
				label: t.id,
				semantic: "tool",
				group: "sidebar",
				order: 100 + i,
				role: "dev",
			},
		});
	});

	// connector 演奏 knob (2026-07-11 lead 指示 + Step 7 Light Grid): mako の Editor Mode
	// 探索がそのまま connector / photon 設計になるよう slider 化。 default は Light Grid
	// 視覚仕様 (artifact c203944c) の値。 unit が px / s / ms で混在するため各 entry に持たせる。
	const connNumbers: Array<{
		id: string;
		cssVar: string;
		value: number;
		min: number;
		max: number;
		step: number;
		unit: string;
	}> = [
		{
			id: "sb.conn.width",
			cssVar: "--sb-conn-width",
			value: 2,
			min: 0.5,
			max: 4,
			step: 0.25,
			unit: "px",
		},
		{
			id: "sb.conn.slot",
			cssVar: "--sb-conn-slot",
			value: 22,
			min: 12,
			max: 32,
			step: 1,
			unit: "px",
		},
		{
			// idle 破線 tap の dash 長。
			id: "sb.conn.dash",
			cssVar: "--sb-conn-dash",
			value: 4,
			min: 2,
			max: 12,
			step: 0.5,
			unit: "px",
		},
		{
			// HITL diamond pulse の 1 beat。 default = creo-ui timeline BPM 82.7 (60/82.7)。
			id: "sb.conn.flow.beat",
			cssVar: "--sb-conn-flow-beat",
			value: 0.7255,
			min: 0.15,
			max: 2,
			step: 0.05,
			unit: "s",
		},
		{
			// photon が spine を root→末端に走る周期 (Light Grid の signature motion)。
			id: "sb.conn.photon.period",
			cssVar: "--sb-photon-period",
			value: 1800,
			min: 600,
			max: 4000,
			step: 100,
			unit: "ms",
		},
		{
			// 発光半径 (photon / node / diamond の glow に波及)。
			id: "sb.conn.glow",
			cssVar: "--sb-glow",
			value: 6,
			min: 0,
			max: 14,
			step: 0.5,
			unit: "px",
		},
	];
	connNumbers.forEach((t, i) => {
		bind<number>({
			target: cssVarNumberTarget(t.id, t.cssVar, t.value, t.unit),
			control: number({
				min: t.min,
				max: t.max,
				step: t.step,
				unit: t.unit,
				variant: "slider",
			}),
			placement: {
				label: t.id,
				semantic: "tool",
				group: "sidebar-connector",
				order: 110 + i,
				role: "dev",
			},
		});
	});

	// connector 状態色 = Light Grid の 2 hue: needs-you の magenta / working・current の cyan。
	// picker で書くと :root inline に concrete 値が入り stylesheet 定義を上書きする (探索用)。
	const connColors: Array<{ id: string; cssVar: string; value: string }> = [
		{ id: "sb.conn.hitl", cssVar: "--sb-conn-hitl", value: "#FF4A2D" },
		{ id: "sb.conn.auto", cssVar: "--sb-conn-auto", value: "#FFF76B" },
	];
	connColors.forEach((t, i) => {
		bind<string>({
			target: cssVarTarget(t.id, t.cssVar, t.value),
			control: color({ variant: "picker" }),
			placement: {
				label: t.id,
				semantic: "tool",
				group: "sidebar-connector",
				order: 120 + i,
				role: "dev",
			},
		});
	});
	return null;
}

// doc 48 Phase 2 (editor bridge): MCP → vp-app が評価する JS が provider の外から
// host に触るための明示 expose。creoEditor console API は localhost hostname heuristic で
// expose されるため vp-asset:// origin では当てにできない — bridge はこの global 一本に依存する。
// 読み手: app.rs `editor_bridge_js`（`window.vpEditorHost.mcp` を呼ぶ）。UI は持たない。
function ExposeEditorHostForBridge() {
	const host = useEditorHost();
	(window as unknown as { vpEditorHost?: unknown }).vpEditorHost = host;
	return null;
}

function App() {
	return (
		<EditorHostProvider>
			<SidebarTokenBinds />
			<ExposeEditorHostForBridge />
			<EditorLayer />
		</EditorHostProvider>
	);
}

const root = document.getElementById("editor-root");
if (root) {
	render(() => <App />, root);
} else {
	console.warn(
		"[vp-app] #editor-root が見つかりません — EditorLayer mount をスキップ",
	);
}

// Component Gallery mode（doc 48 Phase 3 → doc 49 LE-P2 で creo-ui-layout の pane 化）。
// EditorHostProvider とは独立に生きる（gallery 中も Ctrl+Shift+E / editor_set が効く）。
installGallery();
