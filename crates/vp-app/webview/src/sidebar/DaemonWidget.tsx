/**
 * sidebar 最下部の Daemon widget。
 *
 * v1.0 柱 2 PR-2。 旧 SIDEBAR_HTML の Daemon widget + `renderActivity` を port。
 * collapsed = 1 行サマリ (状態 dot + version + P/R count)、 expanded = 詳細 stats。
 * `ActivitySnapshot` (Rust が 5s 周期で push) を消費する。
 */
import { For, Show, createSignal, onCleanup } from "solid-js";
import { CreoIcon } from "@chronista-club/creo-ui-icons-web";
import { sidebar } from "./store";
import { sendIpc } from "./ipc";
import { standDisplayName, standIcon } from "./lane";

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

export function DaemonWidget() {
	const a = () => sidebar.activity;
	const online = () => a().node_online;
	const summary = () =>
		online()
			? `daemon v${a().daemon_version ?? "?"} — P${a().project_count} R${a().running_process_count}`
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

	// uptime 表示を 30s 周期で tick させる (started_at は不変なので時計側を signal 化)。
	const [now, setNow] = createSignal(Date.now());
	const timer = setInterval(() => setNow(Date.now()), 30_000);
	onCleanup(() => clearInterval(timer));

	// Devices 🧲 — machine scope の物理 device。 device 一覧は main area の Devices pane が render、
	// ここ (Daemon レベルの Devices) は pane を開く入口 + 接続 device 数 badge。
	const devices = () => sidebar.devices ?? [];
	const devicesActive = () => sidebar.active_stand?.kind === "devices";

	// in-app update: daemon の定期チェック (起動時 + 24h) が検知した新 release。
	// update_available 時のみ Daemon widget 直下に「更新する」CTA を出す。latest_version は
	// ボタン label + `update:apply` IPC の payload (Rust 側の native ダイアログ文言に使う)。
	const updateAvailable = () => a().update_available;
	const latestVersion = () => a().latest_version ?? undefined;

	return (
		<>
			<details class="vp-daemon">
				<summary class="vp-daemon-summary">
					<span class="vp-daemon-dot" classList={{ offline: !online() }} />
					<span class="vp-daemon-line">{summary()}</span>
				</summary>
				<div class="vp-daemon-detail">
					<div class="vp-daemon-stat">
						<span class="k">version</span>
						<span class="v">{a().daemon_version ?? "—"}</span>
					</div>
					<div class="vp-daemon-stat">
						<span class="k">uptime</span>
						<span class="v">
							<Show when={online()} fallback="—">
								{formatUptime(a().daemon_started_at, now())}
							</Show>
						</span>
					</div>
					<div class="vp-daemon-stat">
						<span class="k">projects</span>
						<span class="v">{a().project_count}</span>
					</div>
					<div class="vp-daemon-stat">
						<span class="k">running</span>
						<span class="v">{a().running_process_count}</span>
					</div>
				</div>
			</details>
			<Show when={online() && updateAvailable()}>
				<div
					class="vp-daemon-update"
					title={`v${latestVersion() ?? "?"} が利用可能です。クリックで更新します`}
					onClick={() => {
						const v = latestVersion();
						// version が無い場合は IPC を送らない (Rust arm も空 version を無視)。
						if (v) sendIpc({ t: "update:apply", version: v });
					}}
				>
					<CreoIcon name="ph:arrow-circle-up" size={14} />
					<span class="vp-daemon-update-label">更新する</span>
					<Show when={latestVersion()}>
						<span class="vp-daemon-update-ver">v{latestVersion()}</span>
					</Show>
				</div>
			</Show>
			<Show when={showHub()}>
				<div
					class="vp-daemon-summary"
					title={`chronista-hub federation: ${hub()}`}
				>
					<span class="vp-daemon-dot" classList={{ offline: !hubConnected() }} />
					<span class="vp-daemon-line">{hubLabel()}</span>
				</div>
				<Show when={hubConnected() && hubDaemons().length > 0}>
					<div class="vp-hub-nodes">
						<For each={hubDaemons()}>
							{(w) => (
								<div
									class="vp-hub-daemon"
									title={`${w.wld_id ? `${w.handle} (${w.wld_id})` : w.handle} — ${w.connected ? "connected" : "offline (stale registry entry)"}`}
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
					class="vp-stand-row"
					classList={{ active: devicesActive() }}
					onClick={() =>
						sendIpc({ t: "stand:select", path: "", kind: "devices" })
					}
				>
					<span class="vp-stand-icon">
						<CreoIcon
							name={standIcon("devices", devicesActive()) ?? "ph:magnet"}
							size={14}
						/>
					</span>
					<span class="vp-stand-title">{standDisplayName("devices")}</span>
					<Show when={devices().length > 0}>
						<span class="vp-stand-badge">{devices().length}</span>
					</Show>
				</div>
			</div>
		</>
	);
}
