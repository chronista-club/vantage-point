/**
 * sidebar 下部の 2 段（doc 58 ③ — 分け方 = アーキテクチャの scope 境界、mako 2026-08-19）:
 *
 * - [`CreoIdRow`] … **creo 段**の住人（cloud scope）。Shell が ACTIONS（BucketList）と
 *   同じ `.vp-creo-zone` に並べる。offline は段ごと dim = creo 依存を名札付きで隔離。
 * - [`MachineStrip`] … **machine 帯**（最下、machine scope = daemon ⚙️ + hub + devices 🧲）。
 *   健康なら 1 行サマリだけ、click で詳細（stats / hub nodes / devices 入口）が展開。
 *   非健康 signal（offline / update あり / 機材譲渡中）はサマリ行にも出す —
 *   「畳んであるから気づけない」を作らない。
 *
 * `ActivitySnapshot`（Rust が 5s 周期で push）を消費する。旧 DaemonWidget
 * （11 行 ≈ 340px 常時表示）はこの 2 export に分解されて退役。
 */
import { For, Show, createSignal, onCleanup } from "solid-js";
import { CreoIcon } from "@chronista-club/creo-ui-icons-web";
import { sidebar } from "./store";
import { sendIpc } from "./ipc";
import { agentDisplayName, agentIcon } from "./lane";

/** ISO 8601 の `started_at` から相対 uptime 文字列を作る。 */
function formatUptime(iso: string | null | undefined, nowMs: number): string {
	if (!iso) return "—";
	const t = Date.parse(iso);
	if (Number.isNaN(t)) return iso;
	const sec = Math.max(0, Math.floor((nowMs - t) / 1000));
	if (sec < 60) return `${sec}s ago`;
	const m = Math.floor(sec / 60);
	if (m < 60) return `${m}m ago`;
	const h = Math.floor(m / 60);
	return `${h}h ${m % 60}m ago`;
}

/** activity snapshot の共通導出（MachineStrip / CreoIdRow が共用）。 */
function useActivity() {
	const a = () => sidebar.activity;
	const online = () => a().node_online;
	const summary = () =>
		online()
			? `daemon v${a().daemon_version ?? "?"} — P${a().repo_count} R${a().running_repo_count}`
			: "daemon offline";

	// chronista-hub federation 接続状態（Daemon 行の下に常時表示）。`/api/health` の `hub`:
	// "connected" / "connecting" / "disconnected" / "disabled"、未取得 or 旧 daemon は空文字。
	// connected のみ緑 dot、それ以外は .offline（赤）。disabled / 空 / daemon offline では非表示
	// （federation を使っていない daemon にノイズを出さない）。
	const hub = () => a().hub ?? "";
	const hubConnected = () => hub() === "connected";
	// hub の向こうの available nodes（daemon が 45s 周期 discover で更新、切断で空に戻る）。
	// Hub 行の label に「· N nodes」を足し、行の直下に handle を常時リスト表示する。
	const hubDaemons = () => a().hub_nodes ?? [];
	const hubLabel = () => {
		switch (hub()) {
			case "connected": {
				const n = hubDaemons().length;
				return n > 0
					? `Hub — connected · ${n} daemon${n > 1 ? "s" : ""}`
					: "Hub — connected";
			}
			case "connecting":
				return "Hub — connecting…";
			case "disconnected":
				return "Hub — disconnected";
			default:
				return `Hub — ${hub()}`;
		}
	};
	const showHub = () => online() && hub() !== "" && hub() !== "disabled";
	// hub 接続の auth 状態（`/api/health` の `hub_auth`）: "credentialed" / "anonymous" / 空。
	// 表示の SSOT は file でなく「接続がどう成立したか」— login 済み file があっても接続が
	// 匿名なら Login を出す（再ログイン + 再接続が正しい復旧手順のため）。
	const hubCredentialed = () => (a().hub_auth ?? "") === "credentialed";

	// 宛先ごとの credential 状態（`/api/health` の `auth_targets`、local file の判定）。
	// ⚠️ `hub_auth` とは別物 — あちらは hub 接続の副産物なので **hub を切ると読めない**。
	// こちらは hub と無関係に「creo にログイン済みか」が言えるので、Creo ID 行の根拠になる。
	const authState = (target: string) => a().auth_targets?.[target] ?? "none";
	const creoValid = () => authState("creo") === "valid";
	/** どれか 1 つでも token を持っていれば Creo ID にはログイン済み（identity は共通）。 */
	const signedIn = () => authState("hub") !== "none" || authState("creo") !== "none";


	// Devices 🧲 — machine scope の物理 device。 device 一覧は main area の Devices pane が render、
	// ここ (Daemon レベルの Devices) は pane を開く入口 + 接続 device 数 badge。
	const devices = () => sidebar.devices ?? [];
	const devicesActive = () => sidebar.active_component?.kind === "devices";
	// 艦隊スイッチ（`vp midi off`）で機材を他アプリへ譲っているか。
	//
	// ⚠️ `/api/health` の `services` ではなく **device 一覧そのもの**から導く。sidebar は
	// `services` を一切読んでいないので、そちらに出しても読み手ゼロになる。device 一覧は
	// daemon-device channel で既にリアルタイムに来ているので、新しい配管が要らない。
	const released = () => devices().some((d) => d.hold_reason === "released");
	// VP が実際に掴んでいる台数。「7 台見えているが掴んでいるのは 2 台」を言い分ける
	// （残りは parser 対応外 = 最初から他アプリと取り合っていない）。
	const heldCount = () => devices().filter((d) => d.held).length;

	// in-app update: daemon の定期チェック (起動時 + 24h) が検知した新 release。
	// update_available 時のみ Daemon widget 直下に「更新する」CTA を出す。latest_version は
	// ボタン label + `update:apply` IPC の payload (Rust 側の native ダイアログ文言に使う)。
	const updateAvailable = () => a().update_available;
	// 適用フロー実行中 (AppEvent::UpdateFlowPhase 由来の GUI local 状態、health 由来ではない)。
	const updateApplying = () => a().update_applying;
	const latestVersion = () => a().latest_version ?? undefined;

	return {
		a,
		online,
		summary,
		hub,
		hubConnected,
		hubDaemons,
		hubLabel,
		showHub,
		hubCredentialed,
		authState,
		creoValid,
		signedIn,
		devices,
		devicesActive,
		released,
		heldCount,
		updateAvailable,
		updateApplying,
		latestVersion,
	};
}

/**
 * Creo ID 行（doc 57 Phase 2 → doc 58 ③ で creo 段へ）— **identity の席**。
 * hub とは独立（identity は 1 つ、token は宛先ごと、service はそれを使う側）。
 */
export function CreoIdRow() {
	const v = useActivity();
	return (
		<Show when={v.online()}>
			<div
				class="vp-daemon-summary"
				title="Creo ID — VP の identity。token は宛先ごとに 1 本ずつ持つ"
			>
				<span
					class="vp-daemon-dot"
					classList={{ offline: !v.signedIn() }}
					title={`hub: ${v.authState("hub")} / creo: ${v.authState("creo")}`}
				/>
				<span class="vp-daemon-line">
					Creo ID{v.signedIn() ? "" : " — signed out"}
				</span>
				{/* creo（ACTIONS の同期先）の席。hub と独立に張り替えられる。 */}
				<button
					type="button"
					class="vp-hub-auth-btn"
					title={
						v.creoValid()
							? "creo-memories の認証を解除する（ACTIONS の同期が止まる）"
							: "creo-memories にログインする（Creo ID の session があれば素通りする）"
					}
					onClick={(e) => {
						e.stopPropagation();
						sendIpc(
							v.creoValid()
								? { t: "auth:logout", target: "creo" }
								: { t: "auth:login", target: "creo" },
						);
					}}
				>
					{v.creoValid() ? "creo ✓" : "creo"}
				</button>
			</div>
		</Show>
	);
}

/**
 * machine 帯（doc 58 ③）— daemon ⚙️ + hub + devices 🧲 の 1 行サマリ + click 展開。
 */
export function MachineStrip() {
	const v = useActivity();
	// uptime 表示を 30s 周期で tick させる (started_at は不変なので時計側を signal 化)。
	// ⚠️ useActivity ではなくここに置く — now() の読み手は本 component の uptime 行だけで、
	// CreoIdRow にまで timer を張るのは無駄 (review 2026-08-19)。
	const [now, setNow] = createSignal(Date.now());
	const timer = setInterval(() => setNow(Date.now()), 30_000);
	onCleanup(() => clearInterval(timer));
	return (
		<details class="vp-daemon vp-machine-strip">
			<summary class="vp-daemon-summary">
				<span class="vp-daemon-dot" classList={{ offline: !v.online() }} />
				<span class="vp-daemon-line">{v.summary()}</span>
				{/* 非健康 signal は畳んでいても見える（サマリ行の右端に集約） */}
				<Show when={v.showHub() && !v.hubConnected()}>
					<span class="vp-machine-flag" title={v.hubLabel()}>hub!</span>
				</Show>
				<Show when={v.released()}>
					<span class="vp-machine-flag" title="vp midi off で機材を他アプリへ譲っています">譲渡中</span>
				</Show>
				<Show when={v.online() && v.updateAvailable()}>
					<span class="vp-machine-flag update" title={`v${v.latestVersion() ?? "?"} が利用可能`}>
						<CreoIcon name="ph:arrow-circle-up" size={12} />
					</span>
				</Show>
			</summary>

			<div class="vp-daemon-detail">
				<div class="vp-daemon-stat">
					<span class="k">version</span>
					<span class="v">{v.a().daemon_version ?? "—"}</span>
				</div>
				<div class="vp-daemon-stat">
					<span class="k">uptime</span>
					<span class="v">
						<Show when={v.online()} fallback="—">
							{formatUptime(v.a().daemon_started_at, now())}
						</Show>
					</span>
				</div>
				<div class="vp-daemon-stat">
					<span class="k">repos</span>
					<span class="v">{v.a().repo_count}</span>
				</div>
				<div class="vp-daemon-stat">
					<span class="k">running</span>
					<span class="v">{v.a().running_repo_count}</span>
				</div>
			</div>

			<Show when={v.online() && v.updateAvailable()}>
				<div
					class="vp-daemon-update"
					classList={{ applying: v.updateApplying() }}
					title={
						v.updateApplying()
							? "更新を適用しています…"
							: `v${v.latestVersion() ?? "?"} が利用可能です。クリックで更新します`
					}
					onClick={() => {
						if (v.updateApplying()) return;
						const ver = v.latestVersion();
						if (ver) sendIpc({ t: "update:apply", version: ver });
					}}
				>
					<CreoIcon name="ph:arrow-circle-up" size={14} />
					<span class="vp-daemon-update-label">
						{v.updateApplying() ? "更新中…" : "更新する"}
					</span>
					<Show when={v.latestVersion()}>
						<span class="vp-daemon-update-ver">v{v.latestVersion()}</span>
					</Show>
				</div>
			</Show>

			<Show when={v.showHub()}>
				<div
					class="vp-daemon-summary"
					title={`chronista-hub federation: ${v.hub()}`}
				>
					<span class="vp-daemon-dot" classList={{ offline: !v.hubConnected() }} />
					<span class="vp-daemon-line">{v.hubLabel()}</span>
					<button
						type="button"
						class="vp-hub-auth-btn"
						title={
							v.hubCredentialed()
								? "Creo ID からログアウトする（hub 接続は匿名に落ちる）"
								: "Creo ID にログインする（browser で認証 → hub 接続に即反映）"
						}
						onClick={(e) => {
							e.stopPropagation();
							if (v.hubCredentialed()) {
								sendIpc({ t: "auth:logout", target: "hub" });
							} else {
								sendIpc({ t: "auth:login", target: "hub" });
							}
						}}
					>
						{v.hubCredentialed() ? "Logout" : "Login"}
					</button>
				</div>
				<Show when={v.hubConnected() && v.hubDaemons().length > 0}>
					<div class="vp-hub-nodes">
						<For each={v.hubDaemons()}>
							{(w) => (
								<div
									class="vp-hub-daemon"
									title={`${w.node_id ? `${w.handle} (${w.node_id})` : w.handle} — ${w.connected ? "connected" : "offline (stale registry entry)"}`}
								>
									<span
										class="vp-hub-daemon-dot"
										classList={{ offline: !w.connected }}
									/>
									<span class="k">{w.handle}</span>
									<Show when={w.endpoints_count > 0}>
										<span class="v">{w.endpoints_count} ep</span>
									</Show>
								</div>
							)}
						</For>
					</div>
				</Show>
			</Show>

			<div class="vp-devices">
				<div
					class="vp-agent-row"
					classList={{ active: v.devicesActive() }}
					onClick={() =>
						sendIpc({ t: "stand:select", path: "", kind: "devices" })
					}
				>
					<span class="vp-agent-icon">
						<CreoIcon
							name={agentIcon("devices", v.devicesActive()) ?? "ph:magnet"}
							size={14}
						/>
					</span>
					<span class="vp-agent-title">{agentDisplayName("devices")}</span>
					<Show when={v.released()}>
						<span
							class="vp-agent-badge released"
							title="vp midi off で機材を他アプリへ譲っています（`vp midi on` で戻す）"
						>
							譲渡中
						</span>
					</Show>
					<Show when={v.devices().length > 0}>
						<span
							class="vp-agent-badge"
							title={`${v.heldCount()} 台を掴んでいます（見えている ${v.devices().length} 台のうち）`}
						>
							{v.heldCount()}/{v.devices().length}
						</span>
					</Show>
				</div>
			</div>
		</details>
	);
}
