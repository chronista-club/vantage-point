/**
 * Conversation tui（xterm）の配線 — 旧 World A。
 *
 * doc 53 §6.5 の「もう半分」。2026-07-26 まで、この 976 行は `crates/vp-app/src/main_area.rs` の
 * inline `<script>` に Rust 文字列として埋め込まれていた（World A）。同じ webview の中に 2 つの
 * コードベースが並ぶ形で、doc 53 §6.5 の実測では **A6 で出た 17 バグの最大タイのクラスタ（4 件）が
 * この境界に集まっていた**（同じ概念を 2 言語で表現する / 片方だけ改修に追随する）。
 *
 * doc 33 §4 は input-doubling 調査の診断ベースライン保護のため World A を「不可侵」と宣言して
 * いたが、2026-07-26 に凍結を解除した（計器 hop A/B は 2 点とも Rust 側で xterm JS を通らない
 * ため、そもそも守っていなかった。doc 33 §4 / doc 53 §6.5.1.1）。
 *
 * ## Rust からどう届くか
 *
 * 制御面（lane/session の出現・表示切替・消滅・paste）は **`installTerm` の戻り値**として
 * `dispatch.ts` に渡し、Rust からは単一受け口 `window.vpDispatch` の envelope で届く
 * （SSOT = `crates/vp-app/schema/vp-push.kdl`、型は codegen が両側に出す）。
 * **引数の数や順序が食い違えば TS のコンパイルが落ちる** — 旧来この境界は名前で関数を呼ぶ形で、
 * 食い違っても Rust も TS も黙っていた（doc 53 §6.5.1）。
 *
 * ⚠️ window に残しているのは 2 つだけ:
 *
 * | window API | 理由 |
 * |---|---|
 * | `vpTerminal.handleOutput(lane, session, b64)` | PTY 出力は高頻度 stream。envelope 化には「buffer するか」を制御面と別に決める必要があり、移行は別 PR |
 * | `showLane(lane, isChat)` | `active-pane.ts` が kind=terminal で呼ぶ（TS 内の呼び出し）。`installTerm` の closure にあるため今は window 経由 |
 */
import { FitAddon } from "@xterm/addon-fit";
import { ProgressAddon } from "@xterm/addon-progress";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { UnicodeGraphemesAddon } from "@xterm/addon-unicode-graphemes";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import type { TermPushHandlers } from "./dispatch";

/** wry が注入する IPC。install 時ではなく **呼ぶ時に** 引く（注入が後の場合があるため）。 */
const ipc = () =>
	(window as unknown as { ipc?: { postMessage(m: string): void } }).ipc;

function post(payload: unknown): void {
	try {
		ipc()?.postMessage(JSON.stringify(payload));
	} catch (_) {
		/* IPC 不在は握り潰す（起動初期 / DevTools 単体実行） */
	}
}

function dbg(msg: string): void {
	post({ t: "debug", msg });
}

// ---------------------------------------------------------------------------
// Live Token（VP-143）— creo-ui-editor-host（Ctrl+Shift+E）が runtime 調整する 5 値
// ---------------------------------------------------------------------------

type CursorStyle = "bar" | "block" | "underline";

interface TerminalTokens {
	fontSize: number;
	lineHeight: number;
	letterSpacing: number;
	fontFamily: string;
	cursorStyle: CursorStyle;
}

const TERMINAL_FONT_SIZE_FALLBACK = 16;
const TERMINAL_LINE_HEIGHT_FALLBACK = 1.15;
const TERMINAL_LETTER_SPACING_FALLBACK = 0;
const TERMINAL_CURSOR_STYLE_FALLBACK: CursorStyle = "bar";
const TERMINAL_CURSOR_STYLES = new Set<string>(["bar", "block", "underline"]);

/** per-(lane, session) の xterm 実体。合成キー `<lane>#<session>` で引く。 */
interface LaneInstance {
	address: string;
	session: number;
	/** focus の優先だけに使う（**host の選び方には使わない** — host の身元は session）。
	 *  root は付け替えで動くので、既存 instance にも毎回焼き直す。 */
	isRootHost: boolean;
	term: Terminal;
	fitAddon: FitAddon;
	writeOutput: (bytes: Uint8Array) => void;
	sendResize: () => void;
	container: HTMLDivElement;
	ro: ResizeObserver;
	webglAddon: WebglAddon | null;
	webglCleanup: (() => void) | null;
}

export function installTerm(): TermPushHandlers {
	// Creo tokens から xterm.js theme を構築 (全 Lane instance で共有)。
	// OKLCH 値は xterm.js の内部 color parser が直接解釈できないので、
	// hidden probe で `color: var(...)` を browser に解決させて
	// `getComputedStyle().color` から rgb(R,G,B) を取得 → hex に降ろす。
	const probe = document.createElement("span");
	probe.style.position = "absolute";
	probe.style.visibility = "hidden";
	document.body.appendChild(probe);

	const resolveHex = (varName: string, fallback: string): string => {
		probe.style.color = `var(${varName}, ${fallback})`;
		const rgb = getComputedStyle(probe).color;
		const m = rgb.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
		if (!m) return fallback;
		return `#${[m[1], m[2], m[3]]
			.map((n) => Number(n).toString(16).padStart(2, "0"))
			.join("")}`;
	};

	const theme = {
		background: resolveHex("--color-surface-bg-base", "#0F1128"),
		foreground: resolveHex("--color-text-primary", "#EDEEF4"),
		cursor: resolveHex("--color-brand-primary", "#7D6BC2"),
		cursorAccent: resolveHex("--color-surface-bg-base", "#0F1128"),
		selectionBackground: resolveHex("--color-brand-primary-subtle", "#2C2843"),
		black: resolveHex("--terminal-ansi-black", "#1E1E2E"),
		red: resolveHex("--terminal-ansi-red", "#F38BA8"),
		green: resolveHex("--terminal-ansi-green", "#A6E3A1"),
		yellow: resolveHex("--terminal-ansi-yellow", "#F9E2AF"),
		blue: resolveHex("--terminal-ansi-blue", "#89B4FA"),
		magenta: resolveHex("--terminal-ansi-magenta", "#F5C2E7"),
		cyan: resolveHex("--terminal-ansi-cyan", "#94E2D5"),
		white: resolveHex("--terminal-ansi-white", "#BAC2DE"),
		brightBlack: resolveHex("--terminal-ansi-bright-black", "#585B70"),
		brightRed: resolveHex("--terminal-ansi-bright-red", "#F38BA8"),
		brightGreen: resolveHex("--terminal-ansi-bright-green", "#A6E3A1"),
		brightYellow: resolveHex("--terminal-ansi-bright-yellow", "#F9E2AF"),
		brightBlue: resolveHex("--terminal-ansi-bright-blue", "#89B4FA"),
		brightMagenta: resolveHex("--terminal-ansi-bright-magenta", "#F5C2E7"),
		brightCyan: resolveHex("--terminal-ansi-bright-cyan", "#94E2D5"),
		brightWhite: resolveHex("--terminal-ansi-bright-white", "#FFFFFF"),
	};

	// VP "principal" token + creo fallback を 2 段 var() で probe に当て、 computed font-family を
	// 完全 resolve させて xterm canvas に渡す stack を得る。 WKWebView は var() chain (declaration
	// 内 var()) を invalidate するが、 use site で並べる形なら正しく resolve する。
	probe.style.fontFamily = "var(--vp-font-mono), var(--typography-family-mono)";
	const monoFamily =
		(getComputedStyle(probe).fontFamily || "").trim() ||
		`'UDEV Gothic NF', monospace`;
	probe.remove();

	/** CSS variable から読取、不正値や未設定時は fallback (= 旧 hardcoded 値) に縮退。 */
	function readTerminalTokens(): TerminalTokens {
		const cs = getComputedStyle(document.documentElement);
		const fontSize = Number.parseFloat(cs.getPropertyValue("--terminal-font-size"));
		const lineHeight = Number.parseFloat(
			cs.getPropertyValue("--terminal-line-height"),
		);
		const letterSpacing = Number.parseFloat(
			cs.getPropertyValue("--terminal-letter-spacing"),
		);
		const fontFamilyRaw = (
			cs.getPropertyValue("--terminal-font-family") || ""
		).trim();
		const cursorRaw = (cs.getPropertyValue("--terminal-cursor-style") || "")
			.trim()
			.toLowerCase();
		return {
			fontSize:
				Number.isFinite(fontSize) && fontSize > 0
					? fontSize
					: TERMINAL_FONT_SIZE_FALLBACK,
			lineHeight:
				Number.isFinite(lineHeight) && lineHeight > 0
					? lineHeight
					: TERMINAL_LINE_HEIGHT_FALLBACK,
			letterSpacing: Number.isFinite(letterSpacing)
				? letterSpacing
				: TERMINAL_LETTER_SPACING_FALLBACK,
			fontFamily: fontFamilyRaw || monoFamily,
			cursorStyle: TERMINAL_CURSOR_STYLES.has(cursorRaw)
				? (cursorRaw as CursorStyle)
				: TERMINAL_CURSOR_STYLE_FALLBACK,
		};
	}

	// ========= per-(Lane, session) instance registry (doc 50 §4.6 A6) =========
	// doc 46 §1.5「session ↔ Pane は 1:1」に従い、xterm も **session ごと**に 1 枚持つ
	//  (旧: lane ごとに 1 枚 = 「term になれるのは root だけ」の物理制約の由来だった)。
	// transport は terminal S4 の Daemon "canvas" channel + Rust per-lane terminal session のまま。
	//  topic は lane 単位で共有し、session は message field で運ぶ (Design B) ので、購読は lane 1 本、
	//  振り分けだけがここ (handleOutput の session 引数) で起きる。
	//
	// ⚠️ key を入れ子 Map ではなく合成文字列にしているのは、この層の支配的アクセスが
	//  **全 instance 走査** (token 反映 / paste / resize) だから。lane 単位の操作 (showLane /
	//  removeLane) だけ info.address で絞る。Rust 側 (pty_slots / terminal_pumps) が入れ子なのは
	//  あちらが lane 単位 teardown と session 単位付け替えを両方回すため — 層ごとに主軸が違う。
	const laneInstances = new Map<string, LaneInstance>();

	/** (lane, session) → registry key。session は 1 以上の整数 (VP 採番)。 */
	const instKey = (address: string, session: number) => `${address}#${session}`;

	// 右クリック context menu (macOS の text actions / AutoFill / Services 等) を全面 suppress。
	//  per-Lane terminal container は別 listener で paste 動作に差替え済 (e.preventDefault + doPaste)、
	//  capture phase の document listener は preventDefault のみ呼ぶので container listener の paste も生きる。
	//  対象外: preview iframe (cross-context、 iframe 内に独立 listener が必要)。
	document.addEventListener(
		"contextmenu",
		(e) => {
			e.preventDefault();
		},
		{ capture: true },
	);

	/**
	 * (lane, session) の term host 要素を get-or-create する（doc 50 §4.6 A6）。
	 *
	 * **host の身元は session**（`#term-session-<n>`、chat 側 `chat-session-<n>` と同型）。
	 * root も例外にしない — 初版は root だけ静的 `#lane-host` を使っていたが、それは host を
	 * **role**（誰が root か）に縛る形で、root を付け替えると host id が session 間で入れ替わる:
	 * xterm 側は生成時の host を握り続け、layout 側は最新 roster から id を計算し直すので
	 * **DOM 位置と focus が旧 root に残留**する（team-b 7 回目 2026-07-25）。
	 * A6 が「非 root も term になれる / どの session でも代表にできる」を正規にした以上、role で
	 * 識別子を決める形は成立しない（replay file と同じ判断 —
	 * `daemon::pty_slot::replay_file_path_session_in`）。
	 *
	 * lane 切替では作り直すので id に lane を含めない（DOM には常に 1 lane 分しか無い）。
	 */
	function ensureTermHost(session: number): HTMLElement | null {
		const id = `term-session-${session}`;
		const found = document.getElementById(id);
		if (found) return found;
		const parent = document.getElementById("lane-panes");
		if (!parent) return null;
		const host = document.createElement("div");
		host.id = id;
		host.className = "term-session-host";
		parent.appendChild(host);
		return host;
	}

	function createLaneInstance(
		address: string,
		session: number,
		isRoot: boolean,
	): LaneInstance | null {
		const host = ensureTermHost(session);
		if (!host) {
			console.error(`createLaneInstance: term host not found (session=${session})`);
			return null;
		}
		// container は (Lane, session) あたり 1 つ、 absolute で host 全領域を埋める
		const container = document.createElement("div");
		container.className = "lane-pane";
		container.dataset.laneAddr = address;
		container.dataset.session = String(session);
		const tdiv = document.createElement("div");
		tdiv.className = "lane-term";
		container.appendChild(tdiv);
		host.appendChild(container);

		const tokens = readTerminalTokens();
		const term = new Terminal({
			// terminal S4: 明示 80×24 init (狭幅復元bug対策)。 hidden 状態で fit すると container 幅≈0
			//  → cols≈数個 に潰れ、 sendResize で PTY まで極狭化する。 init を 80×24 に固定し、 fit は
			//  「container が可視 (clientWidth>0)」 のときだけ走らせて、 不可視時は 80×24 を保つ。
			cols: 80,
			rows: 24,
			fontFamily: tokens.fontFamily,
			fontSize: tokens.fontSize,
			lineHeight: tokens.lineHeight, // V4+ visual axis (Live Token で 1.0-1.5 調整可)
			cursorStyle: tokens.cursorStyle, // V4+ visual axis (Live Token で bar/block/underline 切替可)
			scrollSensitivity: 5, // trackpad で適度 (5 確定 2026-05-11)、 mouse wheel は xterm.js 内部 limitation で 1 行扱い、 page scroll は Shift+PgUp/PgDn で代替
			smoothScrollDuration: 0, // discrete jump、 PR #247 ghost char 抑制 (= smooth 125ms + 高速 scroll で cell update skip → fragment 残骸)、 V4+ で再証明 (2026-05-11)
			scrollback: 5000, // history buffer (drift 無罪確認 2026-05-11、 default 1000 でも drift 再現 → scrollback は origin ではない)
			allowProposedApi: true, // Unicode11Addon 等の proposed API 利用許可（下記 archaeology の要）
			theme,
		});

		// === FitAddon = 描画 prerequisite (baseline で確定 2026-05-10) ===
		// xterm.js は term.open(tdiv) 時に container 現 size から cols/rows 計算、
		// .lane-term は flex layout で size 確定が fitAddon.fit() 後 → これ無しだと
		// canvas/DOM が 0 描画 = 「コンソール表示されない」 状態。 baseline 必須。
		const fitAddon = new FitAddon();
		term.loadAddon(fitAddon);

		// === Unicode11Addon + UnicodeGraphemesAddon = Unicode 15 grapheme cluster + width 補正 ===
		//  ⚠️ load 順序が critical: WebglAddon load の **前** に widthProvider を確定する必要がある。
		//  理由: WebGL renderer は loadAddon 時に widthProvider を読んで glyph atlas を事前構築、
		//  後から activeVersion を切替えても atlas は古い width のまま (= upstream design limitation)。
		//  ⚠️ allowProposedApi: true (Terminal options) と pair 必須。 V6 baseline reset で proposed API
		//  gate を閉じた時、 2 addon とも silent fail で drift + 1 cell 幅の同時症状発生 (= V7/V8.1/V8.2
		//  経由で path 探索後、 V8.3 で `allowProposedApi: true` 復帰、 V9 で graphemes も再 enable)。
		//
		//  Unicode11Addon: width table 補正 (Unicode 11、 emoji / CJK 拡張 / box-drawing の cell width)
		//  UnicodeGraphemesAddon: grapheme cluster 認識 (Unicode 15、 ZWJ + skin tone + variation selector)
		//  activeVersion は graphemes が '15-graphemes' で u11 の '11' を上書き、 widthProvider を確定。
		try {
			term.loadAddon(new Unicode11Addon());
			term.unicode.activeVersion = "11";
		} catch (e) {
			console.warn(`[xterm:${address}] Unicode11Addon load failed:`, e);
		}
		try {
			term.loadAddon(new UnicodeGraphemesAddon());
			term.unicode.activeVersion = "15-graphemes";
		} catch (e) {
			console.warn(`[xterm:${address}] UnicodeGraphemesAddon load failed:`, e);
			// Fallback: Unicode 11 (= width table のみ、 grapheme 不対応)
			try {
				term.unicode.activeVersion = "11";
			} catch (_) {}
		}

		// === WebglAddon = baseline 高速 renderer (V4 確定 2026-05-10、 user 方針) ===
		// user 方針「基本 WebGL 描画」 を反映、 V3 (= DOM renderer) で drift 不再現を観察した上で
		// V4 で WebGL active 化。 過去 Phase 5-D (2026-05-02) で「WebGL 復活が正解」 と判断、 frame
		// 毎 canvas 全描画で cell recycling 起因 ghost char が原理的に発生しない property が再評価対象。
		// GPU context loss (Mac で別 app 切替時) は onContextLoss で dispose → DOM fallback。
		let webglAddon: WebglAddon | null = null;
		let webglCleanup: (() => void) | null = null;
		try {
			const addon = new WebglAddon();
			webglAddon = addon;
			term.loadAddon(addon);
			addon.onContextLoss(() => {
				console.warn(`[xterm:${address}] WebGL context loss — DOM fallback`);
				addon.dispose();
			});
			// glyph atlas 破損の自動復旧。 GPU context loss を伴わない silent な atlas
			// corruption (= 文字化けが ge app まで治らない症状) を、 app が foreground
			// に戻った時に clearTextureAtlas で atlas を作り直して wipe する。 corruption
			// の trigger (GPU 切替 / sleep-wake / メモリ圧) は app の background 化と
			// 相関するため、 visible 復帰時の再構築が実効的。 真の context loss は上の
			// onContextLoss で dispose → DOM fallback されるため別経路。
			const onVisible = () => {
				if (document.visibilityState !== "visible") return;
				try {
					addon.clearTextureAtlas();
				} catch (_) {}
			};
			document.addEventListener("visibilitychange", onVisible);
			webglCleanup = () =>
				document.removeEventListener("visibilitychange", onVisible);
		} catch (e) {
			console.warn(`[xterm:${address}] WebGL unavailable:`, e);
		}

		// === ProgressAddon = OSC 9;4 ConEmu progress event capture (V4+ enhancer) ===
		//  shell tool / build script (cargo, bun, npm) や Claude CLI が emit する progress 状態
		//  (state: 0=remove/1=normal/2=error/3=indeterminate/4=warning、 value: 0-100) を event 化。
		//  creo-ui の `.creo-progress[data-variant][data-indeterminate]` に state mapping が完全一致 →
		//  CSS 既存資産で即視覚化可。 MVP: console.log で event を確認、 後続 PR で sidebar wire。
		try {
			const progressAddon = new ProgressAddon();
			term.loadAddon(progressAddon);
			progressAddon.onChange((p) => {
				console.log(`[osc9;4:${address}] state=${p.state} value=${p.value}`);
			});
		} catch (e) {
			console.warn(`[xterm:${address}] ProgressAddon load failed:`, e);
		}

		// === WebLinksAddon = URL auto-link + cmd/ctrl+click で OS ブラウザ起動 ===
		//  default handler の window.open は WebView 内遷移になり OS ブラウザに繋がらないため、
		//  custom handler で `open-url` IPC を送り Rust (terminal::handle_ipc_message) が
		//  webbrowser::open で OS default browser を起動する (native open 経路)。
		//  cmd (Mac) / ctrl (win/linux) + click 限定 = iTerm/VSCode/Terminal.app と同じ端末慣習。
		//  素の click は cursor 位置 / text 選択に残す (誤爆防止)。
		try {
			term.loadAddon(
				new WebLinksAddon((event, uri) => {
					if (!event.metaKey && !event.ctrlKey) return;
					post({ t: "open-url", url: uri });
				}),
			);
		} catch (e) {
			console.warn(`[xterm:${address}] WebLinksAddon load failed:`, e);
		}

		// Addons (final state、 VP-162 V9 baseline 2026-05-11):
		//   ✅ FitAddon              — baseline 必須 (描画 prerequisite、 load 順序 1st)
		//   ✅ Unicode11Addon        — width table 補正 (load 順序 2nd、 activeVersion '11')
		//   ✅ UnicodeGraphemesAddon — grapheme cluster 認識 (load 順序 3rd、 activeVersion '15-graphemes')
		//   ✅ WebglAddon            — 高速 GPU 描画 (load 順序 4th、 Unicode 15 widthProvider で atlas 構築)
		//   ✅ ProgressAddon         — V4+ enhancer
		//   ✅ WebLinksAddon         — V4+ enhancer
		//   ❌ ImageAddon            — VP-162 で不要判定 (2026-05-11)。#920 で npm 依存からも外した。
		//      VP architecture は「Canvas (= board) で視る、 TUI で操る」で image は board body markdown
		//      pipeline が主路線、 terminal 内 inline (sixel/iTerm IIP/kitty graphics) は副次的。
		//      復帰 cost は低い (`@xterm/addon-image` を足して try/catch 1 block) ので必要時に revisit。
		//
		// === drift 真犯人 (= archaeology trace、 2026-05-11) ===
		//   convertEol: true — drift origin 確定:
		//     V4+++ で 4 件一括復帰した時 drift 再演、 1 件ずつ revert isolation で convertEol が真犯人と判明。
		//     仮説では「単純な \n → \r\n 変換、 cell rendering 不介入」 だったが、 実際は xterm.js の
		//     write buffer + cell index 計算と相互作用で drift 起こす (= 推測、 真因は upstream issue 候補)。
		//     modern shell (zsh / bash) は \r\n standard で disable で問題なし、 legacy shell の
		//     \n only output 対策が必要な時のみ revisit。
		//
		//   allowProposedApi: false (= V6 baseline reset の silent regression) — V7-V8.2 で発見遅延:
		//     V6 で「Terminal options 1 件ずつ active 化」 で削除した allowProposedApi が、 Unicode11Addon
		//     + UnicodeGraphemesAddon (= 両方 proposed API) を silent fail させていた。 V6 → V7 (graphemes
		//     disable) → V8.1 (Unicode11 復帰 + activeVersion '11') → V8.2 (load 順序修正) でも emoji 1 cell
		//     幅と drift が残存、 V8.3 で `allowProposedApi: true` 復帰して両症状を解消。 V7 で UnicodeGraphemes
		//     を「wrap drift 主犯」 と判定したが、 真犯人は proposed API gate の方だった (= V9 で再 enable
		//     して drift 不再現を検証)。 教訓: **proposed addon を使う時は allowProposedApi: true と必ず pair、
		//     baseline reset で削除しない**。

		term.open(tdiv);
		// 実験: terminal textarea の autocomplete を **on** に。 browser の autofill が typed commands を
		//  保存して提案する挙動を観察する。 dogfood で「過去 command の suggestion が出るか / UI の overlay が
		//  邪魔にならないか / cross-lane suggestion 混在しないか」 を実測。 問題あれば off に戻す。
		try {
			term.textarea?.setAttribute("autocomplete", "on");
		} catch (_) {}
		installOscHandlers(term, address);

		// ===== Transport: Daemon "canvas" channel 経由 (terminal S4、 doc 27 §4.1) =====
		// 旧 `/ws/terminal` browser-native WebSocket 直結を撤去し、 Rust 側 per-lane terminal session
		// (app.rs `spawn_terminal_session`) に橋渡しする IPC 経路に直切替:
		//   - 出力: Rust が daemon canvas channel から PTY bytes を受け、 `window.vpTerminal.handleOutput
		//           (address, session, base64)` で inject (下記 coalescer で 1 frame 分まとめて term.write)。
		//   - 入力: `term.onData` → IPC `{t:'term:write', lane, session, data:base64}` → Rust session → repo。
		//   - resize: `sendResize` → IPC `{t:'term:resize', lane, session, cols, rows}` → Rust session → repo。
		// 再接続は Rust session が担うので JS 側 retry/backoff/scrollback-replay は不要 (= 撤去)。

		// 出力 coalescer: 1 frame 内に届いた複数 chunk を結合して 1 回 term.write する
		//  (大量出力時の write 呼び出しオーバヘッド削減)。 64KiB 超で即 flush、 それ未満は rAF で束ねる。
		const COALESCE_MAX_BYTES = 65536;
		const outState = { queue: [] as Uint8Array[], bytes: 0, scheduled: false };
		function flushOutput(): void {
			outState.scheduled = false;
			if (outState.queue.length === 0) return;
			const merged = new Uint8Array(outState.bytes);
			let off = 0;
			for (const chunk of outState.queue) {
				merged.set(chunk, off);
				off += chunk.length;
			}
			outState.queue.length = 0;
			outState.bytes = 0;
			try {
				term.write(merged);
			} catch (_) {}
		}
		function writeOutput(bytes: Uint8Array): void {
			outState.queue.push(bytes);
			outState.bytes += bytes.length;
			if (outState.bytes >= COALESCE_MAX_BYTES) {
				flushOutput();
			} else if (!outState.scheduled) {
				outState.scheduled = true;
				requestAnimationFrame(flushOutput);
			}
		}

		function sendResize(): void {
			// doc 50 §4.6 A6: 宛先 slot は **引数で運ぶ**（pane ごとに大きさが違う）。
			post({
				t: "term:resize",
				lane: address,
				session,
				cols: term.cols,
				rows: term.rows,
			});
		}

		// ⚠️ ここに「生成直後の初回 fit」は置かない。`.lane-pane` は `display:none` で生まれ
		// （main_area.rs の CSS）、`.active` が付くのは `createLaneInstance` が**戻った後**
		// （`ensureLane`）。つまり生成時点の `clientWidth` は必ず 0 で、`clientWidth > 0` を
		// 条件にした fit は**一度も走らない**（旧実装の `fit()` 単独呼び出しも到達不能だった）。
		// サイズ合わせは `syncSize` が「可視になった契機」で行う。

		// input → IPC (Rust session → repo)。 d は xterm の UTF-16 string、 UTF-8 bytes に直して base64 化。
		term.onData((d) => {
			try {
				const bytes = new TextEncoder().encode(d);
				let bin = "";
				for (let i = 0; i < bytes.length; i++)
					bin += String.fromCharCode(bytes[i]);
				// 宛先 session を明示（「focus してから送る」型の分割はレース — doc 50 §4.3）。
				post({
					t: "term:write",
					lane: address,
					session,
					data: btoa(bin),
				});
			} catch (e) {
				dbg(`[lane:${address}#${session}] input send error: ${e}`);
			}
		});

		// OSC 52 (clipboard) intercept — Lane ごとに独立
		term.parser.registerOscHandler(52, (data) => {
			const idx = data.indexOf(";");
			if (idx < 0) return true;
			const pd = data.slice(idx + 1);
			if (pd === "?" || pd.length === 0) return true;
			try {
				const binary = atob(pd);
				const bytes = new Uint8Array(binary.length);
				for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
				post({ t: "copy", d: new TextDecoder("utf-8").decode(bytes) });
			} catch (_) {}
			return true;
		});

		// Copy/Paste (per-Lane scope)
		function doCopy(): boolean {
			const sel = term.getSelection();
			if (!sel) return false;
			navigator.clipboard.writeText(sel).catch(() => {
				post({ t: "copy", d: sel });
			});
			return true;
		}
		function doPaste(): void {
			// Phase 4-paste-fix: navigator.clipboard.readText() は webview の permission policy で
			// silent fail することがあるので、 **常に IPC fallback を併用**。 Rust 側 arboard が
			// OS clipboard を読んで `term:paste` の push で戻してくる経路。
			try {
				navigator.clipboard
					.readText()
					.then((text) => {
						if (text) term.paste(text);
					})
					.catch(() => {
						post({ t: "paste:request" });
					});
			} catch (_) {
				// navigator.clipboard 自体が undefined のケース (古い WebKit 等)
				post({ t: "paste:request" });
			}
		}
		term.attachCustomKeyEventHandler((e) => {
			if (e.type !== "keydown") return true;
			const key = (e.key || "").toLowerCase();
			if (
				(e.ctrlKey && e.key === "Insert" && !e.shiftKey) ||
				(e.metaKey && key === "c")
			) {
				if (doCopy()) return false;
			}
			if (
				(e.shiftKey && e.key === "Insert" && !e.ctrlKey) ||
				(e.ctrlKey && e.shiftKey && key === "v") ||
				(e.metaKey && key === "v")
			) {
				doPaste();
				return false;
			}
			if (e.ctrlKey && !e.shiftKey && !e.metaKey && key === "c") {
				if (term.hasSelection()) {
					doCopy();
					term.clearSelection();
					return false;
				}
			}
			return true;
		});
		container.addEventListener("contextmenu", (e) => {
			e.preventDefault();
			doPaste();
		});
		container.addEventListener("mouseup", () => {
			setTimeout(() => {
				const sel = term.getSelection();
				if (sel && sel.length > 0) doCopy();
			}, 0);
		});
		container.addEventListener("click", () => {
			try {
				term.focus();
			} catch (_) {}
		});

		// ResizeObserver (per-container): active な間だけ fit + resize 通知
		const ro = new ResizeObserver(() => {
			if (!container.classList.contains("active") || container.clientWidth === 0)
				return;
			try {
				fitAddon.fit();
				sendResize();
			} catch (_) {}
		});
		ro.observe(container);

		return {
			address,
			session,
			isRootHost: isRoot,
			term,
			fitAddon,
			writeOutput,
			sendResize,
			container,
			ro,
			webglAddon,
			webglCleanup,
		};
	}

	/** 1 つの term instance を dispose する（xterm + observer。socket は持たない）。 */
	function disposeInstance(key: string, info: LaneInstance): void {
		try {
			info.ro.disconnect();
			if (info.webglCleanup) {
				try {
					info.webglCleanup();
				} catch (_) {}
			}
			if (info.webglAddon) {
				try {
					info.webglAddon.dispose();
				} catch (_) {}
			}
			info.term.dispose();
			info.container.remove();
			// host は全 session 動的（A6 の identity 化）なので、空になったら一律で畳む。
			const host = document.getElementById(`term-session-${info.session}`);
			if (host && host.childElementCount === 0) host.remove();
		} catch (e) {
			console.error("disposeInstance error:", e);
		}
		laneInstances.delete(key);
	}

	/** 今どの lane を見せているか（`showLane` が書く level）。`ensureLane` が読む —
	 *  表示中 lane に後から生まれた instance を active にするため。 */
	let shownLane: string | null = null;

	// -------------------------------------------------------------------------
	// Rust → JS の window API（名前で呼ばれる契約。冒頭の doc comment 参照）
	// -------------------------------------------------------------------------

	/**
	 * 可視になった instance を実サイズへ合わせ、PTY にも伝える（`fit()` と `sendResize()` は対）。
	 *
	 * **level 駆動**（#918 と同型）: 「サイズが変わった」という edge ではなく「今この instance が
	 * 可視である」という level を契機に撃つ。container の ResizeObserver は size **変化**でしか
	 * 発火しないので、生成後に誰もサイズを動かさなければ一度も同期されない — PTY は
	 * `spawn_agent(&cmd, 120, 48)`（`lane_reconcile.rs`）の 120×48 のまま取り残される。
	 *
	 * rAF 2 段なのは、`display` 切替の layout flush 前に走ると fit が 0 幅で潰れるため
	 * （= 狭幅復元bug の intermittent 原因）。幅がまだ 0 なら見送り、80×24 を保つ。
	 *
	 * @param focus 真なら fit 後に 1 枚へ focus する（root 優先、無ければ先頭）。
	 *   キーボード入力の宛先は 1 つなので、focus を撃つのは lane 表示の契機だけ。
	 */
	const syncSize = (targets: LaneInstance[], focus = false): void => {
		if (targets.length === 0) return;
		requestAnimationFrame(() =>
			requestAnimationFrame(() => {
				for (const info of targets) {
					try {
						if (info.container.clientWidth > 0) {
							info.fitAddon.fit();
							info.sendResize();
						}
					} catch (_) {}
				}
				if (!focus) return;
				try {
					(targets.find((i) => i.isRootHost) || targets[0]).term.focus();
				} catch (_) {}
			}),
		);
	};

	// doc 50 §4.6 A6: (lane, session) ごとに xterm を用意する。
	//
	// `isRoot` は **host の選び方には使わない**（host の身元は session — `ensureTermHost`）。
	// 使うのは focus の優先だけ（`showLane` が「代表を優先して 1 枚に focus」する）。
	// ⚠️ root は **付け替えで動く**ので、既存 instance にも毎回**焼き直す** — 生成時の値を
	// 握り続けると、root 切替後もキーボード入力が旧 root に飛び続ける
	// （team-b 7 回目 2026-07-25。role を identity に混ぜない、の focus 側）。
	const ensureLane = (
		address: string,
		session: number,
		isRoot: boolean,
	): void => {
		const key = instKey(address, session);
		const existing = laneInstances.get(key);
		if (existing) {
			existing.isRootHost = !!isRoot;
			return;
		}
		const inst = createLaneInstance(address, session, !!isRoot);
		if (inst) {
			laneInstances.set(key, inst);
			// **表示中 lane の instance は生まれた時点で active**。`.active` を付けるのは元々
			// `showLane` だけで、それは lane 切替でしか呼ばれない — 表示中の lane に session を
			// 足すと instance は出来て出力も届くのに `display:none` のままで**黒い pane** になる
			// （doc 53 §6.5.0 ①、2026-07-26 実機）。
			//
			// active 化は「可視になった契機」なので **その場で同期する**。旧実装は
			// 「fit は ResizeObserver が拾う」に委ねていたが、RO は size **変化**でしか発火せず、
			// 生成後に誰もサイズを動かさなければ cols が PTY(120×48) とズレたまま残った。
			if (address === shownLane) {
				inst.container.classList.add("active");
				syncSize([inst]);
			}
			dbg(`[lane:${key}] ensured`);
		}
	};

	const showLane = (address: string | null, isChat: boolean): void => {
		shownLane = address;
		// empty placeholder は「Lane が選ばれていない」時だけ出す。
		//  tui (tui): 内容 = xterm instance。 未 ensure (Dead lane 等) なら placeholder。
		//  gui (chat): 内容 = ChatView。 xterm instance を持たないのが正常形なので、
		//   laneInstances 基準で判定すると placeholder が ChatView を覆い続ける
		//   (#lane-empty は position:absolute; inset:0)。 isChat で抑止する。
		//  doc 50 §4.6 A6: lane は N 枚の term instance を持ちうるので「1 枚でもあるか」で判定。
		let laneHasTerm = false;
		for (const [, info] of laneInstances) {
			if (info.address === address) {
				laneHasTerm = true;
				break;
			}
		}
		const hasContent = !!address && (isChat === true || laneHasTerm);
		document.getElementById("lane-empty")?.classList.toggle("active", !hasContent);
		// 当該 lane の全 term instance を active に（並列表示 = tiling 既定。位置決めは
		//  lane-panes.ts の resolved rect が担い、ここは lane の可視性だけを切替える）。
		const actives: LaneInstance[] = [];
		for (const [, info] of laneInstances) {
			const on = info.address === address;
			info.container.classList.toggle("active", on);
			if (on) actives.push(info);
		}
		// active 化直後の hidden→visible 遷移で fit / resize / focus（`syncSize` が SSOT）。
		syncSize(actives, true);
	};

	const removeLane = (address: string): void => {
		// doc 50 §4.6 A6: lane 消滅では **その lane の全 session** を畳む。
		//  session の停止は Rust 側 LanesLoaded reconcile が lane 消滅検知で行う (= map remove)。
		let removed = 0;
		for (const [key, info] of [...laneInstances]) {
			if (info.address !== address) continue;
			disposeInstance(key, info);
			removed++;
		}
		if (removed > 0) dbg(`[lane:${address}] removed (${removed} term)`);
	};

	/** doc 50 §4.6 A6: 1 session の term instance だけ畳む（mode 切替 tui→chat の後始末）。 */
	const removeLaneSession = (address: string, session: number): void => {
		const key = instKey(address, session);
		const info = laneInstances.get(key);
		if (!info) return;
		disposeInstance(key, info);
		dbg(`[lane:${key}] term removed`);
	};

	// terminal S4: Rust の per-lane terminal session が daemon canvas channel から受けた PTY 出力を
	//  `window.vpTerminal.handleOutput(address, session, base64)` で注入してくる
	//  (board-handler.ts と同じ wry-IPC edge)。
	//  doc 50 §4.6 A6: topic は lane 単位で共有されるので、**ここが session の振り分け点**。
	const handleOutput = (address: string, session: number, b64: string): void => {
		const info = laneInstances.get(instKey(address, session));
		if (!info) return;
		let bytes: Uint8Array;
		try {
			const bin = atob(b64);
			bytes = new Uint8Array(bin.length);
			for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
		} catch (_) {
			return;
		}
		info.writeOutput(bytes);
	};

	// Phase 4-paste-fix: Rust 側 arboard で読み取った OS clipboard 内容を active Lane の xterm に inject。
	// `terminal.rs::handle_ipc_message` の `paste:request` → `AppEvent::PasteText` → `app.rs` event loop
	// の `lane_js::deliver_paste` → `term:paste` envelope → dispatch.ts、の最終受け取り口。
	const deliverPaste = (text: string): void => {
		if (!text) return;
		// 宛先は **focus 中の 1 枚**。A6（session = Pane）で lane に active な pane が複数
		// 並ぶようになったので、「最初の active」では**意図しない pane に貼られる**
		// （それ以前は lane に active 1 枚だったので等価だった）。
		const actives = [...laneInstances.values()].filter((i) =>
			i.container.classList.contains("active"),
		);
		const target =
			actives.find((i) => i.term.textarea === document.activeElement) ||
			actives[0];
		if (!target) return; // active が無ければ noop
		try {
			target.term.paste(text);
		} catch (e) {
			console.error("deliverPaste error:", e);
		}
	};

	// 制御面（ensureLane / showLane / removeLane / removeLaneSession / deliverPaste）は
	// **window に生やさない** — `installTerm` の戻り値として dispatch.ts に渡し、Rust からは
	// 単一受け口 `window.vpDispatch` 経由の envelope で届く（`schema/vp-push.kdl`）。
	//
	// ⚠️ `vpTerminal.handleOutput` だけは window のまま。PTY 出力は高頻度 stream で、
	// envelope 化の際に「buffer するか」を制御面と別に決める必要がある（server 側 replay が
	// 取りこぼしを埋めるので、積むとメモリだけ食う）。移行は別 PR。
	Object.assign(window, {
		vpTerminal: { handleOutput },
		// DevTools console から laneInstances を手動検査できるよう露出
		__vpLanes: laneInstances,
		// `active-pane.ts` の `applyPaneSwitch` が kind=terminal で呼ぶ。TS 内の呼び出しなので
		// 本来は直 import が筋だが、`showLane` は `installTerm` の closure にあるため今は window
		// 経由のまま（両方 TS になった今、次段で直参照に畳める）。
		showLane,
	});

	// ========= VP-143: terminal Live Token 群の runtime 反映 (creo-ui-editor-host 連携) =========
	// creo-ui-editor-host (Ctrl+Shift+E で activate) が token slider/input 等で document.documentElement
	// の inline style を setProperty('--terminal-{font-size,line-height,letter-spacing,font-family,cursor-style}', ...)
	// で書き換えると、 MutationObserver が style 属性変更を検知して 5 token を全 xterm instance に伝播:
	//   - term.options setter で値反映 (xterm.js は init-time 受取 API だが setter も同等の runtime API)
	//   - fitAddon.fit() で grid 再計算 (font size / line height 変更で cell 寸法が変わる)
	//   - resize 通知で PTY 側にも cols/rows 伝達 (= SIGWINCH 相当)
	// → user は editor で値変更すると即時に全 lane terminal が追従。 5 token のうち diff があるものだけ
	// 反映する (= 不要な fitAddon.fit を避ける)。 cursorStyle は grid 寸法に影響しないので fit 不要だが、
	// 残り 4 token のいずれかが変わったら fit 必要 ─ 簡素化のため diff があれば fit する pattern で良い。
	let lastTokens = readTerminalTokens();
	const tokenObserver = new MutationObserver(() => {
		const current = readTerminalTokens();
		const fontSizeChanged = current.fontSize !== lastTokens.fontSize;
		const lineHeightChanged = current.lineHeight !== lastTokens.lineHeight;
		const letterSpacingChanged =
			current.letterSpacing !== lastTokens.letterSpacing;
		const fontFamilyChanged = current.fontFamily !== lastTokens.fontFamily;
		const cursorStyleChanged = current.cursorStyle !== lastTokens.cursorStyle;
		const anyChanged =
			fontSizeChanged ||
			lineHeightChanged ||
			letterSpacingChanged ||
			fontFamilyChanged ||
			cursorStyleChanged;
		if (!anyChanged) return;
		lastTokens = current;
		// grid 寸法に影響する 4 token のいずれか変更があれば fit 必要、 cursorStyle のみは fit 不要
		const needsFit =
			fontSizeChanged ||
			lineHeightChanged ||
			letterSpacingChanged ||
			fontFamilyChanged;
		for (const [, info] of laneInstances) {
			try {
				if (fontSizeChanged) info.term.options.fontSize = current.fontSize;
				if (lineHeightChanged) info.term.options.lineHeight = current.lineHeight;
				if (letterSpacingChanged)
					info.term.options.letterSpacing = current.letterSpacing;
				if (fontFamilyChanged) info.term.options.fontFamily = current.fontFamily;
				if (cursorStyleChanged)
					info.term.options.cursorStyle = current.cursorStyle;
				// 可視 lane のみ fit (hidden lane を fit すると 0 幅で潰れ PTY を狭める)。
				if (needsFit && info.container.clientWidth > 0) {
					info.fitAddon.fit();
					info.sendResize();
				}
			} catch (_) {
				/* noop on individual lane failure */
			}
		}
	});
	tokenObserver.observe(document.documentElement, {
		attributes: true,
		attributeFilter: ["style"],
	});

	window.addEventListener("resize", () => {
		// active かつ可視 (clientWidth>0) な instance を **全部** fit + resize 通知。
		// A6 以前は lane に active 1 枚だったので `break` で足りたが、tiling で複数並ぶ今は
		// 2 枚目以降が window resize で再フィットされず cols がずれたままになる。
		for (const [, info] of laneInstances) {
			if (
				info.container.classList.contains("active") &&
				info.container.clientWidth > 0
			) {
				try {
					info.fitAddon.fit();
					info.sendResize();
				} catch (_) {}
			}
		}
	});

	// 全体 Ctrl+Shift+C のフォールバック (active Lane の selection を copy)
	// Lane 個別の handler では取り切れないケース (focus が container 外にある等) の保険。
	window.addEventListener(
		"keydown",
		(e) => {
			if (!(e.ctrlKey && e.shiftKey && (e.key === "C" || e.key === "c"))) return;
			// active な Lane を探して selection 取得
			for (const [, info] of laneInstances) {
				if (!info.container.classList.contains("active")) continue;
				const sel = info.term.getSelection();
				if (sel) {
					e.preventDefault();
					e.stopPropagation();
					navigator.clipboard.writeText(sel).catch(() => {
						post({ t: "copy", d: sel });
					});
				}
				break;
			}
		},
		true,
	);

	// Rust からの押し込みはここを通る（dispatch.ts が envelope を解いて呼ぶ）。
	return { ensureLane, showLane, removeLane, removeLaneSession, deliverPaste };
}

// ---------------------------------------------------------------------------
// OSC notification capture（Slice 1: capture-only、UI は後続 PR）
// ---------------------------------------------------------------------------
//
// 3 codes 全部 cover ─ cc は terminal 検知して emit する code を切り替える可能性あり、
// defensive にすべて hook して dogfood 中に何が来るかを catalog 化する。
//
// - OSC 9  (iTerm2 / Windows Terminal style):
//     ESC ] 9 ; <message> BEL                ─ body only、 metadata 無し
//     ESC ] 9 ; <subcode> ; <args> BEL       ─ iTerm2 拡張 (9;2=notification 等)、 cwd reporting にも overload
// - OSC 99 (kitty notification protocol):
//     ESC ] 99 ; <metadata> ; <payload> ESC \
//   metadata は colon-separated key=value (i=ID:d=0|1:p=title|body|close|...:a=focus|report:u=0|1|2 等)
//   multi-chunk: 同 i=ID で `d=0` (cont) / `d=1` (final) を使い分け、 final で commit。
// - OSC 777 (rxvt-unicode、 Ghostty / foot 等が踏襲):
//     ESC ] 777 ; notify ; <TITLE> ; <BODY> BEL
//
// observed (2026-04-29 dogfood): cc は vp-app に対して OSC 99 multi-chunk を emit している。
//   例: i=211:d=0:p=title;Claude Code → i=211:p=body;Claude is waiting for your input → i=211:d=1:a=focus;

/** OSC 99 の `<metadata>;<value>` を key=value 対と value に割る。 */
export function parseOsc99(payload: string): {
	m: Record<string, string>;
	value: string;
} {
	const semi = payload.indexOf(";");
	const metaStr = semi >= 0 ? payload.substring(0, semi) : payload;
	const value = semi >= 0 ? payload.substring(semi + 1) : "";
	const m: Record<string, string> = {};
	for (const kv of metaStr.split(":")) {
		if (!kv) continue;
		const eq = kv.indexOf("=");
		if (eq > 0) m[kv.slice(0, eq)] = kv.slice(eq + 1);
		else m[kv] = "";
	}
	return { m, value };
}

export function fmtOsc99Keys(m: Record<string, string>): string {
	return Object.entries(m)
		.map(([k, v]) => (v === "" ? k : `${k}=${v}`))
		.join(" ");
}

/**
 * OSC 9 = `9;<msg>` (無印 iTerm2 notify) or iTerm2 拡張 `9;<subcode>;<args>` の混在。
 * 先頭 segment が pure 数字なら subcode 形式と判定する。
 *
 * 注意 (review F-233-1): `9;hello world` のような pure 数字始まり plain notify の case は
 * subcode="9" 扱いになる ambiguity がある。これは dogfood log のみへの影響で、cc は OSC 9 を
 * emit していない (PR #221 / #233 dogfood で観測ゼロ) ため実害なし。別 emitter が乗ってきた
 * 段階で iTerm2 既知 subcode (1 / 2 / 9 / 50 / 51 等) の whitelist に絞るかを再検討する。
 */
export function parseOsc9(payload: string): {
	subcode: string | null;
	rest: string;
} {
	const semi = payload.indexOf(";");
	if (semi < 0) return { subcode: null, rest: payload };
	const head = payload.substring(0, semi);
	if (/^\d+$/.test(head)) {
		return { subcode: head, rest: payload.substring(semi + 1) };
	}
	return { subcode: null, rest: payload };
}

/** OSC 777 = `notify;<title>;<body>` (urxvt / foot 流) — title/body を semicolon 区切りで取り出す。 */
export function parseOsc777(payload: string): {
	title: string | null;
	body: string;
} {
	const parts = payload.split(";");
	if (parts[0] === "notify" && parts.length >= 2) {
		return { title: parts[1] || "", body: parts.slice(2).join(";") };
	}
	return { title: null, body: payload };
}

function installOscHandlers(term: Terminal, address: string): void {
	try {
		term.parser.registerOscHandler(9, (data) => {
			const payload = String(data || "");
			console.log(`[OSC 9] lane=${address} payload=${JSON.stringify(payload)}`);
			dbg(`[osc9:${address}] ${payload}`);
			try {
				const p = parseOsc9(payload);
				if (p.subcode != null) {
					dbg(
						`[osc9-keys:${address}] subcode=${p.subcode} rest=${JSON.stringify(p.rest)}`,
					);
				} else {
					dbg(`[osc9-keys:${address}] (plain) msg=${JSON.stringify(p.rest)}`);
				}
			} catch (_) {}
			return true;
		});
		term.parser.registerOscHandler(99, (data) => {
			const payload = String(data || "");
			console.log(`[OSC 99] lane=${address} payload=${JSON.stringify(payload)}`);
			dbg(`[osc99:${address}] ${payload}`);
			try {
				const p = parseOsc99(payload);
				dbg(
					`[osc99-keys:${address}] {${fmtOsc99Keys(p.m)}} value=${JSON.stringify(p.value)}`,
				);
			} catch (_) {}
			// Phase 5-D Sprint C P2.1: final-chunk + focus action のみ「user attention 要求」と判定。
			//  metadata は最初の ; までの key=value list。d=1 (final) かつ a=focus を含む chunk が trigger。
			//  Rust 側で unread count を加算 → sidebar に push back → badge 表示。
			const semi = payload.indexOf(";");
			const meta = semi >= 0 ? payload.substring(0, semi) : payload;
			if (/\bd=1\b/.test(meta) && /\ba=focus\b/.test(meta)) {
				post({ t: "osc:notification", lane: address, code: 99 });
			}
			return true;
		});
		term.parser.registerOscHandler(777, (data) => {
			const payload = String(data || "");
			console.log(`[OSC 777] lane=${address} payload=${JSON.stringify(payload)}`);
			dbg(`[osc777:${address}] ${payload}`);
			try {
				const p = parseOsc777(payload);
				if (p.title !== null) {
					dbg(
						`[osc777-keys:${address}] title=${JSON.stringify(p.title)} body=${JSON.stringify(p.body)}`,
					);
				} else {
					dbg(
						`[osc777-keys:${address}] (non-notify form) raw=${JSON.stringify(p.body)}`,
					);
				}
			} catch (_) {}
			return true;
		});
	} catch (e) {
		console.warn(`[xterm:${address}] OSC handler registration failed:`, e);
	}

	// ===== window title (OSC 0 / 2) capture =====
	// xterm.js は OSC 0 (icon + title) と OSC 2 (title) を内部で parse して onTitleChange event を fire する。
	// dogfood 仮説: cc が `/rename` 後に session name を window title として emit していれば、
	//  この listener で renamed value が拾える。もし fire しなければ session JSONL file watch
	//  (~/.claude/repos/<encoded-cwd>/...) の fallback path 検討。
	try {
		term.onTitleChange((title) => {
			console.log(`[term-title] lane=${address} title=${JSON.stringify(title)}`);
			dbg(`[term-title:${address}] ${JSON.stringify(title)}`);
		});
	} catch (e) {
		console.warn(
			`[xterm:${address}] onTitleChange listener registration failed:`,
			e,
		);
	}
}
