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
import { FrameEngine, type PaneId, type SceneId } from "./frame-engine";
import { installGallery } from "./gallery";
import { DEFAULT_SCENES, EMPTY_SCENE, generateAllFocusScenes } from "./scenes";
import { attachRenderer } from "./renderer";
import { attachKeybindings } from "./keybindings";
import { renderPP, clearPP, appendPP } from "./pp";
import {
	installConsole,
	nextRequestId,
	isMyResponse,
	type BusRequestId,
	type EchoesStandsDetail,
} from "./console";
// doc 46 P1: lane 内 tiling（Pane の並び / 縮小 / focus）。
import { PaneShell, newPaneChoices } from "./pane-shell";
// `vp:echoes-stands` bus が運ぶ stand entry（chatview の StandOption と同形）。
type PaneStand = { name: string; label?: string; chat_capable?: boolean };
import { installChatView, CHATVIEW_CSS } from "./chatview";
import {
	mountEchoesHeader,
	ECHOES_HEADER_CSS,
	type EchoesHeaderApi,
} from "./EchoesHeader";
import { renderDevices as renderBastetDevices } from "./bastet";
import {
	handleMessage as handleCanvasMessage,
	setActiveLaneName,
	clearActiveBoard,
} from "./canvas-handler";
import { mountHistoryStrip, HISTORY_STRIP_CSS } from "./HistoryStrip";
import { mountResyncLoader, RESYNC_LOADER_CSS } from "./resync-loader";

console.info("[vp-bundle] imports resolved");
(window as unknown as { vpBundleStatus?: Record<string, boolean> })
	.vpBundleStatus!.importsResolved = true;

// ===== VP-140 / PR-ε-1: 3D Frame Layout Engine init =====
// EditorLayer mount より前に Pane / Scene を register しておき、 DOMContentLoaded で
// default Scene を apply する。 setActivePane bridge も window に登録 (legacy 互換)。
//
// data-frame-id 規約 (main_area.rs HTML 側で付与):
//   echoes  → pane-terminal      (Echoes Stand = lane terminal host)
//   pp      → pane-paisley-park  (Paisley Park 🧭 / Information Router、 PP body = Smart Canvas surface)
//   ge      → pane-gold-experience (Gold Experience 🌿)
//   hp      → pane-hermit-purple   (Hermit Purple 🍇)
//   preview → pane-preview        (iframe preview)
//   empty   → pane-empty          (no selection)
// 注: 旧 data-pane-id (main_area.rs inline JS が Lane address 等に書き換える native overlay sync 用)
// と attribute を分離。 同名にすると Lane click で legacy 側が hijack して Frame Engine の Scene lookup が
// undefined → HIDDEN_TRANSFORM で pane が見えなくなる回帰を起こすため (VP-141 fix)。
//
// VP-142 cleanup (PR-ε-4): legacy "canvas" pane 削除。 VP-42 era の placeholder だったが、 PR-ε-3 で
// PP body (`pane-paisley-park` 内 `<div id="pp-content">`) が Smart Canvas surface を物理化したため
// vestigial。 doc 13 §10 Q-3 (Smart Canvas 配置) も WebView 主 = PP body で確定済。
const FRAME_PANE_IDS: PaneId[] = [
	"echoes",
	"pp",
	"ge",
	"hp",
	"preview",
	"empty",
];
const FOCUSABLE_PANE_IDS: PaneId[] = ["echoes", "pp", "ge", "hp", "preview"];

const frameEngine = new FrameEngine();
FRAME_PANE_IDS.forEach((id) => frameEngine.registerPane({ id, kind: id }));
DEFAULT_SCENES.forEach((s) => frameEngine.registerScene(s));
frameEngine.registerScene(EMPTY_SCENE);
generateAllFocusScenes(FOCUSABLE_PANE_IDS).forEach((s) =>
	frameEngine.registerScene(s),
);

// DOM 反映 + keybindings hook
attachRenderer(frameEngine, document);
attachKeybindings(frameEngine, window);
// WebView 統合 (step 3a): 旧 installMainViewDirectiveBridge は削除。
// sidebar + main が 1 DOM になったため、 directive は sidebar bundle の in-process
// handler (src/sidebar/keybindings.ts の installDirectiveHandler) が同一 window で
// 直接捕捉する。 IPC 往復 bridge を残すと 1 回の Cmd hold + key で二重発火する。

// ===== legacy setActivePane bridge + per-Lane Scene state =====
// 既存 main_area.rs JS が定義する window.setActivePane を wrap して、
// 旧 logic (showLane / preview iframe src 切替 / sendSlotRect) を保ったまま
// Frame Engine に Scene 切替を発火させる。
//
// per-Lane Scene state preservation (VP-141 follow-up):
// - 各 Lane が独立に「最後にいた Scene」 を覚える Map
// - kind=terminal Lane 切替時に旧 Lane の Scene を save、 新 Lane の保存済 Scene (or default lead-focus)
//   を restore する → user が Lane を跨いでも Side Review / PP Overlay 等の layout 選択が記憶される
// - onSceneChange listener で manual Scene 切替 (Cmd+Shift+N) も active Lane の state に反映
// - kind != terminal (PP/GE/HP click 等) は Lane を跨がない fixed-Pane focus、 laneScenes は更新しない
const KIND_TO_PANE: Record<string, PaneId> = {
	terminal: "echoes",
	paisley_park: "pp",
	gold_experience: "ge",
	hermit_purple: "hp",
	preview: "preview",
	empty: "empty",
	// VP-142 cleanup: legacy "canvas" kind 削除 (Smart Canvas surface = PP body に物理化済)
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
 * LaneAddress::Display 形を canvas-handler が使う flat lane_name に翻訳する。
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
/** Lane address → 最後にその Lane が table に乗っていた SceneId. */
const laneScenes = new Map<string, SceneId>();

// onSceneChange で active Lane の Scene state を継続 update。
// bridge 内で applyScene を呼ぶ場合も含めて全 Scene 切替で fire するが、 同じ値を再 set しても
// 害なし (idempotent)、 manual hotkey 切替時にも自然に反映される。
frameEngine.onSceneChange((sceneId) => {
	if (activeLaneAddress && sceneId !== "empty") {
		laneScenes.set(activeLaneAddress, sceneId);
	}
});

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
				console.warn("[frame-engine] legacy setActivePane error", e);
			}
		}
		// Frame Engine に Scene を発火
		if (!info || !info.kind || info.kind === "empty") {
			frameEngine.applyScene("empty");
			// lane 無し = Echoes 共通ヘッダも空へ（chips は presence-driven）。
			echoesHeader?.setLane(null);
			return;
		}
		// kind=terminal: Lane 切替判定 + 保存済 Scene の restore + show-subscriber 付替
		if (info.kind === "terminal" && info.pane_id) {
			const newLane = info.pane_id;
			// Lane が変わった場合、 旧 Lane の現 Scene を save (`onSceneChange` でも save される筈だが
			// 二重 set は idempotent、 timing race に対する保険として明示)
			if (activeLaneAddress && activeLaneAddress !== newLane) {
				const currentScene = frameEngine.getCurrentSceneId();
				if (currentScene && currentScene !== "empty") {
					laneScenes.set(activeLaneAddress, currentScene);
				}
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
			// 保存済 Scene を restore、 初訪問 Lane は lead-focus を default にする
			const target = laneScenes.get(newLane) ?? "lead-focus";
			frameEngine.applyScene(target);
			// board モデル: lane 切替時に active lane を更新する。 lane board は canvas channel で既に
			// retained 受信済みなので、 setActiveLaneName で表示 board を切り替えるだけでよい（別 load 不要）。
			// LaneAddress::Display 形 (`<project>/lead` or `<project>/wing/<name>`) を flat lane_name に翻訳。
			const laneName = laneNameFromAddress(newLane);
			setActiveLaneName(laneName);
			return;
		}
		// kind != terminal (PP/GE/HP/canvas/preview click 等): fixed-Pane focus、 Lane state は更新しない
		const paneId = KIND_TO_PANE[info.kind];
		if (!paneId) {
			console.warn("[frame-engine] unknown kind for setActivePane:", info.kind);
			frameEngine.applyScene("empty");
			return;
		}
		frameEngine.applyScene(`${paneId}-focus`);
	};
};

// DevTools 検査用 (window.vpLaneScenes で per-Lane state を inspect 可能)
(window as unknown as { vpLaneScenes: Map<string, SceneId> }).vpLaneScenes =
	laneScenes;

// wiremsg Stage 2: Rust 注入口。Rust 側 spawn_canvas_subscription が active project の
// canvas ProcessMessage ごとに `window.vpCanvas.handleMessage(msg)` を evaluate_script で呼ぶ。
// DevTools から手動 trigger も可: window.vpCanvas.handleMessage({type:'show',content:{markdown:'# hi'}})
(
	window as unknown as {
		vpCanvas: { handleMessage: typeof handleCanvasMessage };
	}
).vpCanvas = {
	handleMessage: handleCanvasMessage,
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
	const ok = frameEngine.applyScene("lead-focus");
	const paneCount = document.querySelectorAll("[data-frame-id]").length;
	// 診断 log: Frame Engine が apply された事実と、 data-frame-id 要素の存在を確認できるようにする。
	// user 環境で 「画面が黒い」 等の issue 時に DevTools console で path を即時切り分けできるよう常時出力。
	console.info(
		`[frame-engine] applied default scene = lead-focus (ok=${ok}); panes detected = ${paneCount}`,
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

// DevTools 検査用 (window.vpFrame.applyScene('side-review') 等で手動 trigger 可能)
(window as unknown as { vpFrame: FrameEngine }).vpFrame = frameEngine;
(window as unknown as { vpBundleStatus?: Record<string, boolean> })
	.vpBundleStatus!.vpFrameSet = true;
console.info("[vp-bundle] vpFrame attached to window — bundle init complete");

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

// ChatView (C2): #console-chat-host に SolidJS ChatView を mount。scoped CSS を注入。
const chatStyle = document.createElement("style");
chatStyle.textContent = CHATVIEW_CSS;
document.head.appendChild(chatStyle);
const chatHost = document.getElementById("console-chat-host");
const chatView = chatHost ? installChatView(chatHost, vpConsole) : null;

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

// 現在 active な lane（toggle が console_set_mode を送る宛先）。
let consoleActiveLane: string | null = null;
// 現在 active な lane の Act（toggle の label / 次の宛先を決める）。
//
// ⚠️ **CSS class から読まない。** 旧実装は `#console-chat-host` の `.active` を
// 「chat 表示中か」の判定に流用していたが、doc 46 P1 で `.active` は「Pane として
// 描画対象か」に意味が変わり、両 Pane が常に並ぶので mode の指標にならなくなった。
// 見た目の class と状態を兼務させると、片方の意味を変えた時にもう片方が静かに壊れる。
let consoleActiveMode: "tui" | "chat" = "tui";

// doc 46 P1: Pane shell。lane の表示領域を「Act I か Act II」の排他から tiling へ。
// ⚠️ Pane の**中身**（xterm / ChatView）には触れず、host 要素の class だけを操る。
const paneTabs = document.getElementById("pane-tabs");
const paneFrame = document.getElementById("pane-terminal");
const paneShell =
	paneTabs && paneFrame
		? new PaneShell(
				(id: string) => document.getElementById(id),
				paneTabs,
				paneFrame,
			)
		: null;
if (paneShell && paneFrame && paneTabs) {
	const tabsEl: HTMLElement = paneTabs;
	// 要件 1: 既定で左右に並べる。順は「操る（console）→ 視る（chat）」。
	paneShell.dock({ id: "lane-host", label: "Console" });
	paneShell.dock({ id: "console-chat-host", label: "Chat" });
	// doc 46 P2 要件 4/5: 「+ New」= Engine × Act を選んで**新しい session** を作る。
	// 一覧は既存の `vp:echoes-stands` bus（stands_list の結果）から受ける — 新配信路は作らない。
	const newBtn = document.createElement("button");
	newBtn.type = "button";
	newBtn.className = "pane-tab pane-new";
	newBtn.textContent = "+ New";
	newBtn.title = "Engine と Act を選んで新しいコンソールを作る";
	let paneMenu: HTMLElement | null = null;
	const closePaneMenu = (): void => {
		paneMenu?.remove();
		paneMenu = null;
	};
	const openPaneMenu = (stands: PaneStand[]): void => {
		closePaneMenu();
		const lane = activeLaneAddress ?? consoleActiveLane;
		if (!lane) return;
		const menu = document.createElement("div");
		menu.className = "pane-new-menu";
		const choices = newPaneChoices(stands);
		if (choices.length === 0) {
			const empty = document.createElement("div");
			empty.className = "pane-new-empty";
			empty.textContent = "engine なし";
			menu.appendChild(empty);
		}
		for (const c of choices) {
			const item = document.createElement("button");
			item.type = "button";
			item.className = "pane-new-item";
			// Act は「Console（Act I）」「Chat（Act II）」で見せる — 内部語（tui/chat）は出さない。
			item.textContent = `${c.engineLabel} · ${c.act === "chat" ? "Chat" : "Console"}`;
			item.addEventListener("click", () => {
				closePaneMenu();
				// 要件 5: backend が新しい session id を発行し、lane の cwd から始める。
				const ipc = (
					window as unknown as { ipc?: { postMessage(m: string): void } }
				).ipc;
				ipc?.postMessage(
					JSON.stringify({
						t: "console:new_session",
						lane,
						engine: c.engine,
						act: c.act,
					}),
				);
			});
			menu.appendChild(item);
		}
		const rect = newBtn.getBoundingClientRect();
		menu.style.left = `${rect.left}px`;
		menu.style.bottom = `${window.innerHeight - rect.top + 4}px`;
		document.body.appendChild(menu);
		paneMenu = menu;
	};
	// stands の一覧は非同期で返るので、click で要求 → 応答で menu を開く。
	// doc 47 §6: `vp:echoes-stands` は共有 bus。要求ごとに相関 id を採番し、
	// **自分の要求への応答だけ**を拾う（chat の「+」の要求では開かない）。
	// 副次: 連打しても最新 id 以外の応答は捨てられる。
	let paneMenuReq: BusRequestId | null = null;
	document.addEventListener("vp:echoes-stands", (e) => {
		const d = (e as CustomEvent<EchoesStandsDetail<PaneStand>>).detail;
		if (!isMyResponse(paneMenuReq, d?.req)) return;
		paneMenuReq = null;
		openPaneMenu(d?.stands ?? []);
	});
	newBtn.addEventListener("click", (ev) => {
		// ⚠️ 無条件に止めない（#836 / #837 の教訓）。document の click は
		// EchoesHeader の root picker を閉じる経路でもあるので、こちらが握り潰すと
		// picker が開いたまま 2 つの浮動メニューが並ぶ。開いている時だけ止める。
		if (paneMenu) {
			ev.stopPropagation();
			closePaneMenu();
			return;
		}
		const lane = activeLaneAddress ?? consoleActiveLane;
		if (!lane) return;
		// 要求元タグ = 相関 id。応答（handleStands → bus）まで往復する。
		paneMenuReq = nextRequestId("pane-new");
		const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } })
			.ipc;
		ipc?.postMessage(
			JSON.stringify({ t: "echoes:stands_fetch", lane, req: paneMenuReq }),
		);
	});
	document.addEventListener("click", () => closePaneMenu());
	tabsEl.appendChild(newBtn);
	// PaneShell.render は tabs を作り直すので、「+ New」は毎回付け直す。
	const paneObserver = new MutationObserver(() => {
		if (!tabsEl.contains(newBtn)) tabsEl.appendChild(newBtn);
	});
	paneObserver.observe(tabsEl, { childList: true });

	// 要件 3: click で focus が移る。Pane の中身の click は素通しさせたいので capture で拾う。
	paneFrame.addEventListener(
		"click",
		(e) => {
			const host = (e.target as HTMLElement | null)?.closest(
				"#lane-host, #console-chat-host",
			);
			if (host?.id) paneShell.focus(host.id);
		},
		true,
	);
}

// mode に応じて chat Pane を開き、focus を移す。
//
// doc 46 §1.4: Act は将来 Pane の kind に畳まれる（lane の mode ではなくなる）が、
// P1 では既存の `console_mode` を**初期 Pane 構成 + focus 先**に写して残置する。
// いきなり撤去すると Act 切替の全経路（doc 33 / doc 38 の資産）が同時に壊れるため。
//
// 旧実装との差: 片方を `display:none` で**隠す**のをやめ、両方を並べたまま focus だけ移す。
const applyConsoleMode = (lane: string, mode: "tui" | "chat"): void => {
	consoleActiveLane = lane;
	consoleActiveMode = mode;
	// chat Pane は常に描画対象にしておく（並んでいるので「表示中の Act」に関係なく見える）。
	//
	// ⚠️ `showLane` も**必ず**呼ぶ。「表示するか」（`.active`）だけを常時化して
	// 「何を表示するか」（`showLane`）を chat 分岐に残すと、Act I の lane では
	// chat Pane が並んでいるのに中身が空のままになる（P1 の取りこぼし）。
	// Act が決めるのは **focus だけ**で、どちらの Pane も中身は常に現 lane を映す。
	chatHost?.classList.add("active");
	chatView?.showLane(lane);
	// doc 47 §3: Pane 構成は **lane ごと**。DOM host は app 共有なので、lane が
	// 変わったら新 lane の layout を DOM へ写し直す（これが無いと「どの lane に
	// 移動しても前の構成のまま」= doc 46 P1 の実機で観測された症状）。
	paneShell?.setLane(lane);
	// ⚠️ 表示は **1 枚ずつ**（mako 2026-07-21）。内部モデル（doc 47）を整えるまで
	// view は「現状維持・シンプル・ミニマム」に倒す方針。
	//
	// tiling の内部（`LaneLayouts` / `PaneLayout`）はそのまま残し、**既定の見せ方**だけを
	// 従来の「Act I か Act II」に戻す。畳んだ側はタブ chip に残るので、`+ New` も
	// 手動での並列表示も失われない（機能を消さずに既定だけを寄せる）。
	//
	// doc 47 §1 の決着（doc 46 の Pane を FrameEngine に畳む）後、既定を tiling に戻す。
	if (mode === "chat") {
		paneShell?.focus("console-chat-host");
		paneShell?.minimizeOthers("console-chat-host");
	} else {
		paneShell?.focus("lane-host");
		paneShell?.minimizeOthers("lane-host");
		// doc 38 §4.3: Act I へ切替えたら再同期ローダー（global fixed 要素）を必ず下ろす。
		// resync-loader は activeLane の replaying を読むだけで Act を知らないため、chat→tui で
		// stuck した replaying が Act I 表示の上に居座るのを防ぐ。
		chatView?.clearReplaying(lane);
	}
};

// vpConsole.setMode(lane, mode) が投げる CustomEvent を受けて表示を切替える
// (Rust が lane 選択 / console_set_mode 成功時に setMode を呼ぶ)。
document.addEventListener("vp:console-mode", (e) => {
	const detail = (e as CustomEvent<{ lane: string; mode: "tui" | "chat" }>)
		.detail;
	if (!detail?.lane) return;
	// SP 応答待ちの間に別 lane へ移った後から届いた mode 適用で、表示ごと元の lane に
	// 引き戻さない（overlay 解除 / toggle label は別 listener なので影響なし）。lane の
	// mode 自体は vpConsole 側 map に記録済みで、再選択時の setMode 同期が正しく開く。
	if (activeLaneAddress && detail.lane !== activeLaneAddress) return;
	applyConsoleMode(detail.lane, detail.mode);
});

// doc 33 §9: Act I⇄II 切替の progress overlay + switch lock。
// 「resume 確定まで切替を見せる + 二重切替を防ぐ」= 安全なハンドオフ。
const switchingOverlay = document.getElementById("console-switching");
const switchingMsg = switchingOverlay?.querySelector(
	".console-switching-msg",
) as HTMLElement | undefined;
// 進行中の handoff。null = idle。set 中は toggle をロックする。
let handoffPending: { lane: string; target: "tui" | "chat" } | null = null;
let handoffTimer: number | undefined;

const beginHandoff = (lane: string, target: "tui" | "chat"): void => {
	handoffPending = { lane, target };
	if (switchingMsg) {
		switchingMsg.textContent =
			target === "chat"
				? "Act II にセッションを引き継ぎ中…"
				: "Act I にセッションを引き継ぎ中…";
	}
	switchingOverlay?.classList.add("active");
	// safety: ready 信号が来なくても 30s で解除（stuck 防止）。
	if (handoffTimer) clearTimeout(handoffTimer);
	handoffTimer = window.setTimeout(() => endHandoff(), 30000);
};
const endHandoff = (): void => {
	handoffPending = null;
	if (handoffTimer) {
		clearTimeout(handoffTimer);
		handoffTimer = undefined;
	}
	switchingOverlay?.classList.remove("active");
};

// mode 適用（tui=PTY respawn 済 / chat=engine スロット確定）で overlay を clear。
// doc 33 §9 改訂（Act I レベルに合わせる）: chat 行きも session_init を待たず、
// mode 適用で即解除する。Act I の「切替は即・claude の load は非同期」と同じ哲学で、
// overlay が engine 起動（resume 確定）を gate して固まるのを防ぐ。切替を表示した
// のと同じ vp:console-mode で overlay も畳むので、ハングが構造的に起きない。
document.addEventListener("vp:console-mode", (e) => {
	const detail = (e as CustomEvent<{ lane: string; mode: "tui" | "chat" }>)
		.detail;
	if (
		handoffPending &&
		detail?.lane === handoffPending.lane &&
		detail?.mode === handoffPending.target
	) {
		endHandoff();
	}
});
// chat: engine が resume を確定 (session_init) したら、mode 適用より早ければ先に clear
// する belt-and-suspenders（overlay の完了条件ではなく「更に早い解除」の位置づけ）。
document.addEventListener("vp:console-ready", (e) => {
	const detail = (e as CustomEvent<{ lane: string }>).detail;
	if (
		handoffPending?.target === "chat" &&
		detail?.lane === handoffPending.lane
	) {
		endHandoff();
	}
});

// Act toggle: #pane-terminal 右上の floating button。現在 mode を反転して console_set_mode を送る
// (World B 所有、xterm/chat どちらの表示中でも常に押せる)。root(conductor) の切替を主眼とする。
const paneTerminal = document.getElementById("pane-terminal");
if (paneTerminal) {
	const toggle = document.createElement("button");
	toggle.className = "echoes-act-toggle";
	toggle.textContent = "💬 Act II";
	const syncLabel = (): void => {
		toggle.textContent = consoleActiveMode === "chat" ? "⌨ Act I" : "💬 Act II";
	};
	document.addEventListener("vp:console-mode", syncLabel);
	toggle.addEventListener("click", () => {
		// resume 確定前の二重切替をロック（中間状態を作らない）。
		if (handoffPending) return;
		// 宛先 lane は setActivePane bridge が確実に追う activeLaneAddress を使う
		// (consoleActiveLane は起動レースで未設定のことがあり no-op になっていた)。
		const lane = activeLaneAddress ?? consoleActiveLane;
		if (!lane) {
			console.warn(
				"[console-toggle] active lane 不明 — lane を選択してから押してください",
			);
			return;
		}
		const next: "tui" | "chat" = consoleActiveMode === "chat" ? "tui" : "chat";
		// 押下で即 progress を出す（round-trip 前に反応 = 待ち時間を可視化）。
		beginHandoff(lane, next);
		const ipc = (
			window as unknown as { ipc?: { postMessage(m: string): void } }
		).ipc;
		ipc?.postMessage(
			JSON.stringify({ t: "console:set_mode", lane, mode: next }),
		);
	});

	// New Session ボタン（doc 39 §4）: 「今いる Act に出す」非破壊の New。
	//  - Act I（tui lane）: 新 session + root 張り替え + slot の bare respawn（旧会話はタブに残存）
	//  - Act II（chat lane）: 新 Draft タブ + focus（従来どおり）
	// 分岐は Rust（ConsoleNewSession の lane_is_chat）が行う。旧実装の 2 クリック armed 防爆は
	// New が破壊的（fresh restart = 全会話破棄）だった時代の名残 — 非破壊化で不要になり撤去し、
	// 全会話破棄は sidebar の Reset Lane（2-click 確認付き context menu）へ退避した。
	const newSession = document.createElement("button");
	newSession.className = "echoes-act-toggle echoes-new-session";
	newSession.textContent = "✨ New";
	newSession.addEventListener("click", () => {
		if (handoffPending) return;
		const lane = activeLaneAddress ?? consoleActiveLane;
		if (!lane) {
			console.warn(
				"[new-session] active lane 不明 — lane を選択してから押してください",
			);
			return;
		}
		const ipc = (
			window as unknown as { ipc?: { postMessage(m: string): void } }
		).ipc;
		ipc?.postMessage(JSON.stringify({ t: "console:new_session", lane }));
	});

	// 操作群コンテナ。Echoes 共通ヘッダの操作エリア（#echoes-header-actions、EchoesHeader が
	// 描く安定 div）へ集約する。ヘッダ不在（bundle 半壊等）時は旧位置 = pane-terminal 右上
	// floating に fallback（CHATVIEW_CSS の絶対配置が生きる）。
	const actions = document.createElement("div");
	actions.className = "echoes-console-actions";
	actions.appendChild(newSession);
	actions.appendChild(toggle);
	const actionsHost =
		document.getElementById("echoes-header-actions") ?? paneTerminal;
	actionsHost.appendChild(actions);
}

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
		if (action === "clear") {
			if (dataTarget === "pp") {
				// doc 19 PP Canvas Stack Model: clear は items + cursor + DOM の 3 つを全 reset
				// する semantic。 `clearPP()` 直叩きだと canvasState (items / cursor) が残り、
				// strip は表示されたまま main だけ空になる非対称が起きる (= team-b review で発覚)。
				// canvas-handler の `handleMessage({type:'clear'})` 経路で stack 含めて全 reset する。
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

// doc 48 Phase 3: Component Gallery mode（#gallery hash / Ctrl+Shift+G）。
// EditorHostProvider とは独立に生きる（gallery 中も Ctrl+Shift+E / editor_set が効く）。
installGallery();
