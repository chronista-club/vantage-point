import { mountMidiUse } from "./midi-use";
/**
 * Devices pane: Midistage の機材単位の使用設定を主表示にする。
 * OS の入出力ポート一覧は診断用の折り畳みへ置き、使用状態を二重表示しない。
 * Rust は device event 時に window.vpDevices.renderDevices を呼ぶ。
 */

/** Rust `SidebarState.devices` の 1 entry (generated/DeviceSnapshot.ts と同形)。 */
export interface DeviceSnapshot {
	port_name: string;
	has_input: boolean;
	has_output: boolean;
	/** VP が今この port を掴んでいるか。旧 daemon は field 不在 → undefined。 */
	held?: boolean;
	/** 掴んでいない理由（`released` / `unsupported` / `idle`）。 */
	hold_reason?: string;
	/** 最後に触られた時刻（ISO 8601 秒精度）。掴んでいる間だけ更新される。 */
	last_input_at?: string | null;
}

/** Devices pane body の DOM target. main_area.rs HTML 側で `id="device-list"` を保証. */
const TARGET_SELECTOR = "#device-list";

/** textContent 経由で HTML escape (port_name は OS 由来なので念のため)。 */
function escapeHtml(s: string): string {
	const span = document.createElement("span");
	span.textContent = s;
	return span.innerHTML;
}

/** Devices pane に device 一覧を render (完全置換)。 0 件は placeholder。 */
export function renderDevices(devices: DeviceSnapshot[]): void {
	const target = document.querySelector<HTMLElement>(TARGET_SELECTOR);
	if (!target) {
		console.warn(
			"[vpDevices] renderDevices: target not found:",
			TARGET_SELECTOR,
		);
		return;
	}
	mountMidiUse(target);
	const expanded = target.querySelector<HTMLDetailsElement>("details")?.open ?? false;
	const details = document.createElement("details");
	details.className = "devices-port-details";
	details.open = expanded;
	const summary = document.createElement("summary");
	summary.textContent = `接続ポート詳細 · ${devices.length} ポート`;
	const ports = document.createElement("div");
	details.append(summary, ports);
	target.replaceChildren(details);
	if (devices.length === 0) {
		ports.innerHTML = '<p class="devices-empty">接続ポートはありません</p>';
		return;
	}
	ports.innerHTML = devices
		.map((d) => {
			const io = [d.has_input ? "IN" : "", d.has_output ? "OUT" : ""]
				.filter(Boolean)
				.join(" · ");
			return `<div class="devices-device"><span class="devices-device-name">${escapeHtml(
				d.port_name,
			)}</span><span class="devices-device-io">${io}</span></div>`;
		})
		.join("");
}
