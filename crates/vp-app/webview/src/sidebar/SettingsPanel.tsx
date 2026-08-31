/**
 * 設定 overlay（doc 59 P1）— app 級の設定を 1 箇所に集める。
 *
 * `window.vpSettings.open()`（sidebar 下部の ⚙ 行 click）で開き、`settings:fetch` IPC で
 * 現在値を取りに行く。応答は `settings:result` → `window.vpSettings.handleResult` で
 * push back される（`WirePanel` と同じ流儀 — 常時 push はしない）。
 *
 * ## P1 に載っているものの性格
 *
 * 4 項目のうち **3 つは既に動いていた**（Add Repo のフォールバック解決 / Developer Mode の
 * menu / login の OAuth フロー）。つまりこの面の最初の価値は新機能ではなく、
 * **menu・sidebar・toml に散らばっていた操作を 1 箇所に集める**ことにある。
 *
 * ## 楽観更新をしない
 *
 * toggle / 入力の保存はすべて `settings:save` を撃ち、**Rust が書いた確定値**を
 * `settings:result` で受けて表示に反映する。client 側で先に state を進めないので、
 * 保存失敗時の巻き戻しを持たなくてよい（唯一の真実は vp-app.toml）。
 */
import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { CreoIcon } from "@chronista-club/creo-ui-icons-web";
import { sendIpc } from "./ipc";
import { useActivity } from "./DaemonWidget";

/** `settings:result` が運ぶ確定値（schema `vp-sidebar.kdl` の event 定義と 1:1）。 */
export type SettingsSnapshot = {
	developerMode: boolean;
	/** `VP_DEVELOPER_MODE` で固定されている = 設定ページで変えても効かない。 */
	developerModeLocked: boolean;
	/** vp-app.toml の明示値（未設定なら空文字）。 */
	defaultRepoRoot: string;
	/** 明示値が無いとき実際に使われるパス（placeholder 用）。 */
	resolvedRepoRoot: string;
	/** daemon に届いたか。false = settings.kdl 側は編集できない（doc 59 P3）。 */
	daemonReachable: boolean;
	/** ログ詳細度（settings.kdl 由来、未設定は空文字）。 */
	logLevel: string;
	/** アイドル判定の分数（settings.kdl 由来、0 = 未設定 = 既定 5 分）。 */
	idleTimeoutMinutes: number;
};

declare global {
	interface Window {
		vpSettings?: {
			open: () => void;
			handleResult: (s: SettingsSnapshot) => void;
		};
	}
}

// module スコープに置き、onMount で window.vpSettings に attach する（WirePanel と同じ）。
const [visible, setVisible] = createSignal(false);
const [loaded, setLoaded] = createSignal(false);
const [devMode, setDevMode] = createSignal(false);
const [devLocked, setDevLocked] = createSignal(false);
const [repoRoot, setRepoRoot] = createSignal("");
const [resolvedRoot, setResolvedRoot] = createSignal("");
const [daemonReachable, setDaemonReachable] = createSignal(false);
const [logLevel, setLogLevel] = createSignal("");
const [idleMinutes, setIdleMinutes] = createSignal(0);

function open(): void {
	setVisible(true);
	// 開くたびに引き直す（手で vp-app.toml を編集した後でも現実に追いつく）。
	sendIpc({ t: "settings:fetch" });
}

function dismiss(): void {
	setVisible(false);
}

function handleResult(s: SettingsSnapshot): void {
	setDevMode(s.developerMode);
	setDevLocked(s.developerModeLocked);
	setRepoRoot(s.defaultRepoRoot);
	setResolvedRoot(s.resolvedRepoRoot);
	setDaemonReachable(s.daemonReachable);
	setLogLevel(s.logLevel);
	setIdleMinutes(s.idleTimeoutMinutes);
	setLoaded(true);
}

/** Developer Mode の toggle。env で固定されている間は撃たない（押しても効かないため）。 */
function toggleDevMode(): void {
	if (devLocked()) return;
	sendIpc({ t: "settings:save", developer_mode: !devMode() });
}

/**
 * 初期フォルダの保存。**空文字は「未設定に戻す」**を意味する（推定へフォールバックする）。
 * 入力欄を空にして blur すればリセットできる = 消し方を別 UI にしない。
 */
function saveRepoRoot(value: string): void {
	sendIpc({ t: "settings:save", default_repo_root: value.trim() });
}

/**
 * daemon 側（settings.kdl）の保存。**vp-app は書かない** — daemon に中継するだけで、
 * 書き手は daemon 唯一（doc 59 §3）。確定値は `settings:result` で戻る。
 */
function saveLogLevel(value: string): void {
	sendIpc({ t: "settings:save", log_level: value });
}

/**
 * アイドル判定の保存。空 / 0 は「未設定に戻す」= 既定に倒す。
 * ⚠️ 1 つの値が **now-line の「⏸N分」と engine 停止の両方**を決める（doc 59 §5.2）。
 */
function saveIdleMinutes(raw: string): void {
	const n = Number.parseInt(raw.trim(), 10);
	// ⚠️ 空 / 数値でない / 0 以下は **何も送らない**（= 現状維持）。
	// schema 上この field は int なので「消す」を表現できず、0 は daemon 側でエラーになる
	// （0 に「無効化」の意味を持たせない方針 — doc 59）。既定に戻したい人は 5 を入れる。
	if (!Number.isFinite(n) || n <= 0) return;
	sendIpc({ t: "settings:save", idle_timeout_minutes: n });
}

/** ログ詳細度の選択肢。空 = 未設定（VP の組み込み既定に従う）。 */
const LOG_LEVELS = ["", "trace", "debug", "info", "warn", "error"];

/** アイドル判定の既定（分）。daemon が 0 を返した時の表示用。 */
const DEFAULT_IDLE_MINUTES = 5;

export function SettingsPanel() {
	const v = useActivity();

	onMount(() => {
		window.vpSettings = { open, handleResult };
		// Esc で閉じる（WirePanel と同じく document listener、visible 時のみ反応）。
		const onKeyDown = (e: KeyboardEvent) => {
			if (visible() && e.key === "Escape") {
				e.preventDefault();
				dismiss();
			}
		};
		document.addEventListener("keydown", onKeyDown);
		onCleanup(() => {
			document.removeEventListener("keydown", onKeyDown);
			if (window.vpSettings?.handleResult === handleResult) {
				window.vpSettings = undefined;
			}
		});
	});

	return (
		<Show when={visible()}>
			<div class="vp-settings-backdrop" onClick={dismiss}>
				<div class="vp-settings-panel" onClick={(e) => e.stopPropagation()}>
					<header class="vp-settings-header">
						<CreoIcon name="ph:gear-six" size={14} />
						<span class="vp-settings-title">設定</span>
						<span class="vp-settings-spacer" />
						<button
							type="button"
							class="vp-settings-iconbtn"
							title="閉じる (Esc)"
							onClick={dismiss}
						>
							<CreoIcon name="ph:x" size={13} />
						</button>
					</header>

					<div class="vp-settings-body">
						<Show
							when={loaded()}
							fallback={<div class="vp-settings-empty">読み込み中…</div>}
						>
							{/* ── repo ─────────────────────────────────────────── */}
							<section class="vp-settings-section">
								<h3 class="vp-settings-h">repo</h3>
								<label class="vp-settings-row" for="vp-set-reporoot">
									<span class="vp-settings-label">Add Repo の初期フォルダ</span>
									<span class="vp-settings-hint">
										repo を追加するときに picker が最初に開く場所。空にすると
										既存 repo の位置から推定します。
									</span>
									<div class="vp-settings-inputrow">
										<input
											id="vp-set-reporoot"
											type="text"
											class="vp-settings-input"
											value={repoRoot()}
											placeholder={resolvedRoot() || "~/repos"}
											onChange={(e) => saveRepoRoot(e.currentTarget.value)}
										/>
										<button
											type="button"
											class="vp-settings-btn"
											title="フォルダを選ぶ"
											onClick={() => sendIpc({ t: "settings:pick_repo_root" })}
										>
											参照…
										</button>
									</div>
									<Show when={!repoRoot() && resolvedRoot()}>
										<span class="vp-settings-hint">
											現在は <code>{resolvedRoot()}</code> が使われています（推定）。
										</span>
									</Show>
								</label>
							</section>

							{/* ── アカウント ───────────────────────────────────── */}
							<section class="vp-settings-section">
								<h3 class="vp-settings-h">アカウント</h3>
								<div class="vp-settings-row">
									<span class="vp-settings-label">Creo ID</span>
									<span class="vp-settings-hint">
										identity は 1 つですが、token は宛先ごとに 1 本ずつ要ります。
									</span>
									<div class="vp-settings-authrow">
										<span class="vp-settings-authname">hub</span>
										<span class="vp-settings-authstate">
											{v.authState("hub")}
										</span>
										<button
											type="button"
											class="vp-settings-btn"
											onClick={() =>
												sendIpc(
													v.authState("hub") === "valid"
														? { t: "auth:logout", target: "hub" }
														: { t: "auth:login", target: "hub" },
												)
											}
										>
											{v.authState("hub") === "valid" ? "ログアウト" : "ログイン"}
										</button>
									</div>
									<div class="vp-settings-authrow">
										<span class="vp-settings-authname">creo</span>
										<span class="vp-settings-authstate">
											{v.authState("creo")}
										</span>
										<button
											type="button"
											class="vp-settings-btn"
											onClick={() =>
												sendIpc(
													v.creoValid()
														? { t: "auth:logout", target: "creo" }
														: { t: "auth:login", target: "creo" },
												)
											}
										>
											{v.creoValid() ? "ログアウト" : "ログイン"}
										</button>
									</div>
								</div>
							</section>

							{/* ── 動作（daemon 側 = settings.kdl）──────────────── */}
							<section class="vp-settings-section">
								<h3 class="vp-settings-h">動作</h3>
								<Show
									when={daemonReachable()}
									fallback={
										<div class="vp-settings-hint">
											daemon に接続すると編集できます。
										</div>
									}
								>
									<label class="vp-settings-row" for="vp-set-idle">
										<span class="vp-settings-label">アイドルとみなす時間</span>
										<span class="vp-settings-hint">
											この時間だけ動きが無い session は now-line が「⏸N分」に沈み、
											開いていない lane は engine を停止してメモリを返します。
										</span>
										<div class="vp-settings-inputrow">
											<input
												id="vp-set-idle"
												type="number"
												min="1"
												class="vp-settings-input vp-settings-num"
												value={idleMinutes() || ""}
												placeholder={String(DEFAULT_IDLE_MINUTES)}
												onChange={(e) => saveIdleMinutes(e.currentTarget.value)}
											/>
											<span class="vp-settings-unit">分</span>
										</div>
									</label>

									<label class="vp-settings-row" for="vp-set-loglevel">
										<span class="vp-settings-label">ログの詳細度</span>
										<span class="vp-settings-hint">
											不具合を追うときだけ上げます。⚠️ 反映には daemon の再起動が要ります。
										</span>
										<select
											id="vp-set-loglevel"
											class="vp-settings-input"
											value={logLevel()}
											onChange={(e) => saveLogLevel(e.currentTarget.value)}
										>
											{LOG_LEVELS.map((lv) => (
												<option value={lv}>{lv || "（既定）"}</option>
											))}
										</select>
									</label>
								</Show>
							</section>

							{/* ── 開発者 ──────────────────────────────────────── */}
							<section class="vp-settings-section">
								<h3 class="vp-settings-h">開発者</h3>
								<div class="vp-settings-row">
									<button
										type="button"
										class="vp-settings-toggle"
										classList={{ on: devMode(), locked: devLocked() }}
										disabled={devLocked()}
										onClick={toggleDevMode}
									>
										<span class="vp-settings-knob" />
									</button>
									<span class="vp-settings-label">Developer Mode</span>
									<span class="vp-settings-hint">
										View → Open Developer Tools と Reload WebView を有効にします。
									</span>
									<Show when={devLocked()}>
										<span class="vp-settings-locked">
											環境変数 <code>VP_DEVELOPER_MODE</code> で固定されているため、
											ここからは変更できません。
										</span>
									</Show>
								</div>
							</section>

							{/* ── メンテナンス ─────────────────────────────────── */}
							<section class="vp-settings-section">
								<h3 class="vp-settings-h">メンテナンス</h3>
								<div class="vp-settings-row">
									<span class="vp-settings-label">アップデート</span>
									<Show
										when={v.updateAvailable()}
										fallback={
											<span class="vp-settings-hint">
												最新版です。新しい版が出ると、ここに更新ボタンが現れます。
											</span>
										}
									>
										<span class="vp-settings-hint">
											新しいバージョン <strong>v{v.latestVersion() ?? "?"}</strong> が
											利用できます。更新すると VP が再起動します。
										</span>
										<button
											type="button"
											class="vp-settings-btn primary"
											onClick={() => {
												const ver = v.latestVersion();
												if (ver) sendIpc({ t: "update:apply", version: ver });
											}}
										>
											v{v.latestVersion() ?? "?"} に更新…
										</button>
									</Show>
								</div>
								<div class="vp-settings-row">
									<span class="vp-settings-label">daemon の再起動</span>
									<span class="vp-settings-hint">
										⚠️ <strong>すべての lane のプロセスが落ちます</strong>。
										会話は次に開いたときに復帰します。
									</span>
									<button
										type="button"
										class="vp-settings-btn danger"
										onClick={() => sendIpc({ t: "daemon:restart" })}
									>
										daemon を再起動…
									</button>
								</div>
							</section>
						</Show>
					</div>
				</div>
			</div>
		</Show>
	);
}

/** Shell.tsx の <style> に連結する CSS（WIRE_PANEL_CSS と同じ流儀）。 */
export const SETTINGS_PANEL_CSS = `
.vp-settings-backdrop{position:absolute;inset:0;background:rgba(10,12,16,.55);z-index:9000;display:flex;align-items:stretch;}
.vp-settings-panel{margin:24px 8px;flex:1;display:flex;flex-direction:column;background:var(--vp-bg,#14171d);border:1px solid rgba(255,255,255,.09);border-radius:10px;overflow:hidden;min-height:0;}
.vp-settings-header{display:flex;align-items:center;gap:6px;padding:8px 10px;border-bottom:1px solid rgba(255,255,255,.08);font-size:12px;}
.vp-settings-title{font-weight:600;}
.vp-settings-spacer{flex:1;}
.vp-settings-iconbtn{background:none;border:none;color:inherit;opacity:.7;cursor:pointer;padding:2px;display:flex;}
.vp-settings-iconbtn:hover{opacity:1;}
.vp-settings-body{flex:1;overflow-y:auto;padding:4px 10px 12px;min-height:0;}
.vp-settings-section{padding:10px 0;border-bottom:1px solid rgba(255,255,255,.06);}
.vp-settings-section:last-child{border-bottom:none;}
.vp-settings-h{margin:0 0 8px;font-size:10px;font-weight:600;letter-spacing:.08em;text-transform:uppercase;color:var(--lg-mute,#5C7A85);}
.vp-settings-row{display:flex;flex-direction:column;gap:4px;font-size:11.5px;}
.vp-settings-label{font-weight:600;color:rgba(255,255,255,.9);}
.vp-settings-hint{font-size:10.5px;line-height:1.5;color:rgba(255,255,255,.55);}
.vp-settings-hint code{font-size:10px;padding:0 3px;border-radius:3px;background:rgba(255,255,255,.08);}
.vp-settings-inputrow{display:flex;gap:6px;margin-top:2px;}
.vp-settings-input{flex:1;min-width:0;font:inherit;font-size:11px;padding:4px 7px;border-radius:6px;border:1px solid rgba(255,255,255,.12);background:rgba(255,255,255,.04);color:inherit;}
.vp-settings-input:focus{outline:none;border-color:var(--sb-conn-auto,#FFF76B);}
.vp-settings-btn{flex:0 0 auto;font:inherit;font-size:10.5px;padding:4px 10px;border-radius:6px;border:1px solid rgba(255,255,255,.14);background:rgba(255,255,255,.05);color:inherit;cursor:pointer;}
.vp-settings-btn:hover{background:rgba(255,255,255,.11);}
.vp-settings-btn.primary{align-self:flex-start;margin-top:4px;border-color:color-mix(in srgb,var(--sb-conn-auto,#FFF76B),transparent 55%);background:color-mix(in srgb,var(--sb-conn-auto,#FFF76B),transparent 88%);}
.vp-settings-btn.primary:hover{background:color-mix(in srgb,var(--sb-conn-auto,#FFF76B),transparent 78%);}
.vp-settings-num{max-width:88px;}
.vp-settings-unit{align-self:center;font-size:10.5px;opacity:.6;}
select.vp-settings-input{cursor:pointer;}
.vp-settings-btn.danger{align-self:flex-start;margin-top:4px;border-color:rgba(255,74,45,.45);background:rgba(255,74,45,.1);color:#ff8b73;}
.vp-settings-btn.danger:hover{background:rgba(255,74,45,.2);}
.vp-settings-authrow{display:flex;align-items:center;gap:8px;margin-top:4px;}
.vp-settings-authname{flex:0 0 34px;font-size:11px;color:rgba(255,255,255,.85);}
.vp-settings-authstate{flex:1;font-size:10.5px;opacity:.6;}
/* toggle は label の左に置くので row を横並びに上書きする */
.vp-settings-row:has(> .vp-settings-toggle){display:grid;grid-template-columns:auto 1fr;gap:2px 8px;align-items:center;}
.vp-settings-row > .vp-settings-toggle{grid-row:1 / span 1;}
.vp-settings-row:has(> .vp-settings-toggle) > .vp-settings-hint,
.vp-settings-row:has(> .vp-settings-toggle) > .vp-settings-locked{grid-column:2;}
.vp-settings-toggle{width:30px;height:17px;flex:0 0 auto;padding:0;border-radius:9px;border:1px solid rgba(255,255,255,.18);background:rgba(255,255,255,.07);cursor:pointer;position:relative;transition:background .12s;}
.vp-settings-toggle.on{background:color-mix(in srgb,var(--sb-conn-auto,#FFF76B),transparent 62%);border-color:color-mix(in srgb,var(--sb-conn-auto,#FFF76B),transparent 45%);}
.vp-settings-toggle.locked{opacity:.45;cursor:not-allowed;}
.vp-settings-knob{position:absolute;top:2px;left:2px;width:11px;height:11px;border-radius:50%;background:rgba(255,255,255,.75);transition:transform .12s;}
.vp-settings-toggle.on .vp-settings-knob{transform:translateX(13px);}
.vp-settings-locked{font-size:10px;color:#ffb74d;line-height:1.5;}
.vp-settings-empty{padding:16px;text-align:center;font-size:11px;opacity:.5;}
/* sidebar 下部の ⚙ 入口（daemon status の直上 — doc 56 §7 / doc 59 §4） */
.vp-settings-entry{display:flex;align-items:center;gap:8px;padding:3px var(--spacing-sm,8px);cursor:pointer;font-size:var(--sb-text-meta,11px);color:var(--lg-mute-2,#38525b);background:none;border:none;width:100%;text-align:left;font-family:inherit;}
.vp-settings-entry:hover{color:var(--lg-mute,#5C7A85);}
`;
