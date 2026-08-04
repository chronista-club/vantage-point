/**
 * Devices 🧲 — main area の Devices pane に接続中 device 一覧を render する API。
 *
 * board-render.ts (Board body render) と同じ純 action layer (DOM 操作のみ、 state なし)。
 * Rust が device event 時に `window.vpDevices.renderDevices(devices)` を呼ぶ
 * (= EventBus → daemon-device channel → AppEvent::DeviceEvent → main_view push)。
 *
 * ## 計器盤として何を出すか
 *
 * 「機材が挿さっている」と「**VP が掴んでいる**」は別の事実なので、別々に出す。
 * 潰すと、`vp midi off` 中に「機材が無い」と読めてしまい、ケーブルを疑いに行かせる。
 *
 * | 表示 | 意味 |
 * |---|---|
 * | ● 掴んでいる | listener / ROTO 常駐が生きている |
 * | ◌ 譲渡中 | `vp midi off` — user が他アプリ（ladyland 等）へ渡している |
 * | ○ 対応外 | VP に parser が無い機材 = **最初から取り合っていない** |
 *
 * ⚠️ 「最後に触られた時刻」は**掴んでいる間しか観測できない**（listener が無ければ入力が
 * 届かない）。譲渡中に「触っていない」と書くと嘘になるので、その間は伏せる。
 *
 * 公開 API (entry.tsx で window.vpDevices に attach):
 * - `renderDevices(devices)`: Devices pane に接続中 device 一覧を完全置換 render
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

/** 掴んでいる状態の表示（dot + ラベル + CSS 用の state token）。 */
export function holdBadge(d: DeviceSnapshot): {
	state: string;
	dot: string;
	label: string;
} {
	if (d.held) {
		// ROTO は listener ではなく専用常駐が持つ — 出所を出しておくと切り分けが早い
		const via = d.hold_reason === "roto" ? "掴んでいる（常駐）" : "掴んでいる";
		return { state: "held", dot: "●", label: via };
	}
	switch (d.hold_reason) {
		case "released":
			return { state: "released", dot: "◌", label: "譲渡中" };
		case "unsupported":
			return { state: "unsupported", dot: "○", label: "対応外" };
		default:
			return { state: "idle", dot: "○", label: "未接続" };
	}
}

/**
 * 「最後に触られた」の表示。**掴んでいない間は伏せる**（観測していないので言えない）。
 *
 * `now` を引数に取るのは純関数にして test で固定するため。
 */
export function lastTouchLabel(d: DeviceSnapshot, now: Date): string {
	if (!d.held) return "—";
	if (!d.last_input_at) return "まだ触られていない";
	const at = new Date(d.last_input_at);
	const sec = Math.max(0, Math.floor((now.getTime() - at.getTime()) / 1000));
	if (sec < 2) return "いま";
	if (sec < 60) return `${sec} 秒前`;
	if (sec < 3600) return `${Math.floor(sec / 60)} 分前`;
	return `${Math.floor(sec / 3600)} 時間前`;
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
	const now = new Date();
	target.innerHTML = devices
		.map((d) => {
			const io = [d.has_input ? "IN" : "", d.has_output ? "OUT" : ""]
				.filter(Boolean)
				.join(" · ");
			const hold = holdBadge(d);
			return `<div class="devices-device" data-hold="${hold.state}"><span class="devices-device-name">${escapeHtml(
				d.port_name,
			)}</span><span class="devices-device-io">${io}</span><span class="devices-device-hold"><span class="devices-device-dot">${
				hold.dot
			}</span>${escapeHtml(hold.label)}</span><span class="devices-device-touch">${escapeHtml(
				lastTouchLabel(d, now),
			)}</span></div>`;
		})
		.join("");
}
