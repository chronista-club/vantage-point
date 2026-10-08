/** Per-device use settings. The daemon owns the client and all MIDI I/O. */
interface Device {
	device_id: string;
	profile_id: string;
	name: string;
	present: boolean;
	phase: string;
	assignment: { client_id?: string | null; revision: number };
	lease?: { session_id: string } | null;
	error?: string | null;
}
export interface MidiUseStatus {
	enabled?: boolean;
	connected?: boolean;
	error?: string | null;
	request_error?: string;
	snapshot?: { sequence: number; session_id: string; devices: Device[] } | null;
}
type Send = (payload: Record<string, unknown>) => void;
let root: HTMLElement | null = null;
let send: Send = () => {};
let status: MidiUseStatus = {};
let statusSignature = "";
let confirmation: Device | null = null;
let failure = "";
let pending = false;
let sentAt = 0;
let timer: ReturnType<typeof setInterval> | undefined;
const supported = new Set(["roto", "lpd8", "xtouch", "nanokontrol"]);
function request(payload: Record<string, unknown>) {
	pending = true;
	sentAt = Date.now();
	send(payload);
}
export function mountMidiUse(
	before: Element,
	sender: Send = (payload) => {
		window.ipc?.postMessage(JSON.stringify({ t: "midi:use", payload }));
	},
): void {
	if (root?.isConnected) return;
	if (timer) clearInterval(timer);
	root = document.createElement("section");
	root.className = "midi-use-settings";
	before.before(root);
	send = sender;
	status = {};
	statusSignature = "";
	confirmation = null;
	failure = "";
	pending = false;
	draw();
	request({});
	timer = setInterval(() => {
		if (!root?.isConnected) {
			clearInterval(timer);
			timer = undefined;
			return;
		}
		if (!pending || Date.now() - sentAt > 12000) request({});
	}, 1000);
}
export function renderMidiUse(next: MidiUseStatus): void {
	pending = false;
	if (next.request_error) failure = next.request_error;
	else {
		const comparable = (value: MidiUseStatus) =>
			JSON.stringify({
				...value,
				snapshot: value.snapshot
					? { ...value.snapshot, sequence: 0 }
					: value.snapshot,
			});
		const signature = comparable(next);
		if (statusSignature === signature) return;
		statusSignature = signature;
		status = next;
	}
	draw();
}
function element<K extends keyof HTMLElementTagNameMap>(
	tag: K,
	text = "",
): HTMLElementTagNameMap[K] {
	const node = document.createElement(tag);
	node.textContent = text;
	return node;
}
function stateLabel(device: Device): string {
	if (device.error) return `接続エラー: ${device.error}`;
	if (device.phase === "releasing") return "切り替え中…";
	if (!device.present)
		return device.assignment.client_id === "vp"
			? "使用 ON・未接続（接続すると再開）"
			: "未接続";
	if (!supported.has(device.profile_id)) return "VP の操作割り当て未対応";
	if (!status.enabled && device.assignment.client_id === "vp")
		return "全体 OFF・使用設定は保持";
	if (
		device.phase === "active" &&
		device.lease?.session_id === status.snapshot?.session_id
	)
		return "VP で使用中";
	return device.assignment.client_id
		? `${device.assignment.client_id} に割り当て済み`
		: "使用 OFF";
}
function change(device: Device, enabled: boolean, takeover = false) {
	confirmation = null;
	failure = "";
	request({
		set: {
			device_id: device.device_id,
			enabled,
			expected_revision: device.assignment.revision,
			takeover,
		},
	});
	draw();
}
function draw() {
	if (!root) return;
	root.replaceChildren();
	const css = element(
		"style",
		`.midi-use-settings{padding:16px;margin-bottom:18px;border:1px solid var(--border-subtle,#454545);border-radius:8px}.midi-use-settings h3{margin:0 0 12px;font:inherit;font-weight:600}.midi-use-settings p{font-size:12px;opacity:.8;margin:8px 0}.midi-use-master{display:flex;align-items:center;gap:12px}.midi-use-master button{font:inherit;color:inherit;padding:8px 14px;border:1px solid #809bbd;border-radius:6px;background:#45648b55;cursor:pointer}.midi-use-master button:disabled{opacity:.5;cursor:wait}.midi-use-row{display:flex;gap:16px;align-items:center;padding:12px 0;border-bottom:1px solid #ffffff18}.midi-use-row label{margin-left:auto;display:flex;gap:8px;align-items:center;min-height:36px;cursor:pointer}.midi-use-row input{width:18px;height:18px;accent-color:#80b9e8}.midi-use-row label:has(input:disabled){opacity:.4;cursor:not-allowed}.midi-use-caption{display:block;font-size:12px;opacity:.7;margin-top:4px}.midi-use-confirm{padding:12px;background:#8a66152b;border-radius:6px;margin:12px 0}.midi-use-confirm button{margin-right:8px}.midi-use-error{color:#eda47c}.devices-port-details summary{cursor:pointer;padding:8px 0;opacity:.7}`,
	);
	root.append(css, element("h3", "このアプリで使う MIDI 機材"));
	const master = element("div");
	master.className = "midi-use-master";
	const toggle = element("button", status.enabled ? "MIDI を停止する" : "MIDI を有効にする");
	toggle.type = "button";
	toggle.disabled = status.enabled === undefined;
	toggle.dataset.midiMaster = "";
	toggle.onclick = () => {
		failure = "";
		request({ master_enabled: !status.enabled });
	};
	master.append(
		toggle,
		document.createTextNode(
			status.enabled ? "全体 ON" : "全体 OFF",
		),
	);
	root.append(master);
	root.append(
		element(
			"p",
			status.enabled
				? "使う機材の「使用」を ON にしてください。別アプリから切り替えるときは確認します。"
				: "機材の「使用」を操作するには、先に MIDI を有効にしてください。",
		),
	);
	if (!status.connected)
		root.append(
			element("p", status.error || "MIDI サービスに接続していません"),
		);
	if (failure) {
		const error = element("p", failure);
		error.className = "midi-use-error";
		root.append(error);
	}
	if (confirmation) {
		const d = confirmation;
		const box = element("div");
		box.className = "midi-use-confirm";
		box.append(
			element(
				"p",
				`${d.name} は ${d.assignment.client_id} に割り当て済みです。VP に切り替えますか？`,
			),
		);
		const confirm = element("button", "VP に切り替える");
		confirm.dataset.midiConfirm = "";
		confirm.onclick = () => change(d, true, true);
		const cancel = element("button", "キャンセル");
		cancel.onclick = () => {
			confirmation = null;
			draw();
		};
		box.append(confirm, cancel);
		root.append(box);
	}
	for (const d of status.snapshot?.devices ?? []) {
		const row = element("div");
		row.className = "midi-use-row";
		const name = element("div", d.name);
		const detail = element("span", stateLabel(d));
		if (d.profile_id === "nanokontrol") detail.append(" · 標準 CC (ch1)、LED 表示は未対応");
		detail.className = "midi-use-caption";
		name.append(detail);
		const label = element("label");
		const input = element("input");
		input.type = "checkbox";
		input.checked = d.assignment.client_id === "vp";
		input.dataset.midiDevice = d.device_id;
		input.setAttribute("aria-label", `${d.name} を VP で使用`);
		input.disabled =
			!status.enabled ||
			!status.connected ||
			!supported.has(d.profile_id) ||
			d.phase === "releasing";
		input.onchange = () => {
			if (
				input.checked &&
				d.assignment.client_id &&
				d.assignment.client_id !== "vp"
			) {
				confirmation = d;
				draw();
			} else change(d, input.checked);
		};
		label.append(input, document.createTextNode("使用"));
		row.append(name, label);
		root.append(row);
	}
}
