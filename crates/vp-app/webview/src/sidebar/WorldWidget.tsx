/**
 * sidebar 最下部の World widget。
 *
 * v1.0 柱 2 PR-2。 旧 SIDEBAR_HTML の World widget + `renderActivity` を port。
 * collapsed = 1 行サマリ (状態 dot + version + P/R count)、 expanded = 詳細 stats。
 * `ActivitySnapshot` (Rust が 5s 周期で push) を消費する。
 */
import { Show, createSignal, onCleanup } from "solid-js";
import { CreoIcon } from "creoui-icons-web";
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

export function WorldWidget() {
	const a = () => sidebar.activity;
	const online = () => a().world_online;
	const summary = () =>
		online()
			? `TheWorld v${a().world_version ?? "?"} — P${a().project_count} R${a().running_process_count}`
			: "TheWorld offline";

	// uptime 表示を 30s 周期で tick させる (started_at は不変なので時計側を signal 化)。
	const [now, setNow] = createSignal(Date.now());
	const timer = setInterval(() => setNow(Date.now()), 30_000);
	onCleanup(() => clearInterval(timer));

	// Bastet 🧲 — World scope の物理 device。 device 一覧は main area の Bastet pane が render、
	// ここ (World レベルの Devices) は pane を開く入口 + 接続 device 数 badge。
	const devices = () => sidebar.bastet_devices ?? [];
	const bastetActive = () => sidebar.active_stand?.kind === "bastet";

	return (
		<>
			<details class="vp-world">
				<summary class="vp-world-summary">
					<span class="vp-world-dot" classList={{ offline: !online() }} />
					<span class="vp-world-line">{summary()}</span>
				</summary>
				<div class="vp-world-detail">
					<div class="vp-world-stat">
						<span class="k">version</span>
						<span class="v">{a().world_version ?? "—"}</span>
					</div>
					<div class="vp-world-stat">
						<span class="k">uptime</span>
						<span class="v">
							<Show when={online()} fallback="—">
								{formatUptime(a().world_started_at, now())}
							</Show>
						</span>
					</div>
					<div class="vp-world-stat">
						<span class="k">projects</span>
						<span class="v">{a().project_count}</span>
					</div>
					<div class="vp-world-stat">
						<span class="k">running</span>
						<span class="v">{a().running_process_count}</span>
					</div>
				</div>
			</details>
			<div class="vp-devices">
				<div
					class="vp-stand-row"
					classList={{ active: bastetActive() }}
					onClick={() =>
						sendIpc({ t: "stand:select", path: "", kind: "bastet" })
					}
				>
					<span class="vp-stand-icon">
						<CreoIcon
							name={standIcon("bastet", bastetActive()) ?? "ph:magnet"}
							size={14}
						/>
					</span>
					<span class="vp-stand-title">{standDisplayName("bastet")}</span>
					<Show when={devices().length > 0}>
						<span class="vp-stand-badge">{devices().length}</span>
					</Show>
				</div>
			</div>
		</>
	);
}
