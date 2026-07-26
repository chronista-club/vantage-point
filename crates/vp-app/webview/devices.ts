/**
 * Devices 🧲 — main area の Devices pane に接続中 device 一覧を render する API。
 *
 * board-render.ts (Board body render) と同じ純 action layer (DOM 操作のみ、 state なし)。
 * Rust が device event 時に `window.vpDevices.renderDevices(devices)` を呼ぶ
 * (= EventBus → world-device channel → AppEvent::DeviceEvent → main_view push)。
 *
 * 公開 API (entry.tsx で window.vpDevices に attach):
 * - `renderDevices(devices)`: Devices pane に接続中 device 一覧を完全置換 render
 */

/** Rust `SidebarState.devices` の 1 entry (generated/DeviceSnapshot.ts と同形)。 */
export interface DeviceSnapshot {
	port_name: string;
	has_input: boolean;
	has_output: boolean;
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
	if (devices.length === 0) {
		target.innerHTML = '<p class="devices-empty">No devices connected</p>';
		return;
	}
	target.innerHTML = devices
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
