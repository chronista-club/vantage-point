/**
 * Wire Inbox overlay panel (doc 34 §4 V1) — 選択 lane の wire 履歴表示 + ack。
 *
 * FileExplorer と同型の singleton overlay。 LaneRow の mailbox badge click で
 * `window.vpWire.open(address)` が呼ばれ、 Rust へ `wire:fetch` IPC → World "wire"
 * channel の read-only request (wire/history + wire/unread-count、 **cursor 不触り** =
 * GUI が覗いても lane の claude の未読は減らない) → 結果が
 * `window.vpWire.handleResult(payload)` で push back される。
 *
 * 更新ループ: wire 活動 (send/ack) のたびに World が lanes snapshot を再 push する
 * (既存機構) ため、 panel 表示中に sidebar の lanes が更新されたら再 fetch する —
 * 専用の push 購読なしで「wire が動くと panel が追従する」。
 */
import {
	For,
	Show,
	createEffect,
	createSignal,
	on,
	onCleanup,
	onMount,
} from "solid-js";
import { CreoIcon } from "@chronista-club/creo-ui-icons-web";
import { sidebar } from "./store";
import { sendIpc } from "./ipc";
import { laneAddressKey } from "./lane";

/** wire/history の 1 message (routes/wire.rs `wire_history_store` の出力形) */
interface WireMsg {
	id: string;
	prev: string | null;
	from: string;
	to: string[];
	body: Record<string, unknown>;
	created_at: number;
	local_seq: number;
	inbound: boolean;
	acked: boolean;
}

/** Rust `wire_fetch_payload` が push back する payload 形 */
interface WireResult {
	address: string;
	agent?: string;
	error?: string;
	history?: { messages?: WireMsg[]; error?: string };
	unread?: { total?: number; error?: string };
}

declare global {
	interface Window {
		/** Wire Inbox panel の起動 (LaneRow mailbox badge) と Rust からの結果受け口。 */
		vpWire?: {
			open(address: string): void;
			handleResult(result: WireResult): void;
		};
	}
}

// Panel は singleton (sidebar 全体に 1 つの overlay)。 FileExplorer と同じく state は
// module スコープに置き、 onMount で window.vpWire に attach する。
const [visible, setVisible] = createSignal(false);
const [targetAddress, setTargetAddress] = createSignal("");
const [loading, setLoading] = createSignal(false);
const [agent, setAgent] = createSignal("");
const [error, setError] = createSignal<string | null>(null);
const [unread, setUnread] = createSignal(0);
const [messages, setMessages] = createSignal<WireMsg[]>([]);
const [expanded, setExpanded] = createSignal<Set<string>>(new Set());

/** address → project path の逆引き (FileExplorer の resolveProjectPath と同型)。 */
function resolveProjectPath(address: string): string | undefined {
	const map = sidebar.lanes_by_project ?? {};
	for (const [projectPath, lanes] of Object.entries(map)) {
		if (!Array.isArray(lanes)) continue;
		if (lanes.some((l) => laneAddressKey(l) === address)) {
			return projectPath;
		}
	}
	return undefined;
}

function fetchInbox(address: string): void {
	const projectPath = resolveProjectPath(address);
	if (!projectPath) {
		console.warn("[WirePanel] address の project が見つかりません:", address);
		return;
	}
	setLoading(true);
	sendIpc({ t: "wire:fetch", path: projectPath, address });
}

function open(address: string): void {
	setTargetAddress(address);
	setMessages([]);
	setError(null);
	setUnread(0);
	setAgent("");
	setExpanded(new Set<string>());
	setVisible(true);
	fetchInbox(address);
}

function dismiss(): void {
	setVisible(false);
	setLoading(false);
}

function handleResult(result: WireResult): void {
	// dismiss 後 / 他 lane の遅延到着結果は捨てる (FileExplorer と同じ二重防御)。
	if (!visible()) return;
	if (result.address !== targetAddress()) return;
	setLoading(false);
	if (result.error) {
		setError(result.error);
		return;
	}
	const innerErr = result.history?.error ?? result.unread?.error;
	if (innerErr) {
		setError(String(innerErr));
		return;
	}
	setError(null);
	setAgent(result.agent ?? "");
	setMessages(result.history?.messages ?? []);
	setUnread(Number(result.unread?.total ?? 0));
}

function ackMessage(id: string): void {
	const address = targetAddress();
	const projectPath = resolveProjectPath(address);
	if (!projectPath) return;
	setLoading(true);
	// Rust 側が「ack → 再 fetch」を 1 往復に畳むので、 結果は handleResult に戻ってくる。
	sendIpc({ t: "wire:ack", path: projectPath, address, message_id: id });
}

function toggleExpand(id: string): void {
	const next = new Set(expanded());
	if (next.has(id)) {
		next.delete(id);
	} else {
		next.add(id);
	}
	setExpanded(next);
}

/** ack ボタンを出す条件: 受信 command で未 ack (= delivery loop の再掲示対象)。 */
function needsAck(m: WireMsg): boolean {
	return m.inbound && !m.acked && String(m.body?.category ?? "") === "command";
}

/** body から人間向け preview を選ぶ (wire body は自由 JSON — 頻出 field を優先)。 */
function preview(m: WireMsg): string {
	const b = (m.body ?? {}) as Record<string, unknown>;
	for (const key of ["summary", "task_spec", "text", "message", "subject"]) {
		const v = b[key];
		if (typeof v === "string" && v.trim()) return v;
	}
	try {
		return JSON.stringify(b);
	} catch {
		return "";
	}
}

function chipOf(m: WireMsg, key: "kind" | "category"): string {
	const v = (m.body ?? ({} as Record<string, unknown>))[key];
	return typeof v === "string" ? v : "";
}

function fmtTime(ms: number): string {
	const d = new Date(ms);
	const p = (n: number) => String(n).padStart(2, "0");
	return `${p(d.getMonth() + 1)}/${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

function shortAddr(a: string): string {
	return a.replace(/^agent@/, "");
}

export function WirePanel() {
	onMount(() => {
		window.vpWire = { open, handleResult };
		// Esc で閉じる (他 overlay は検索 input の onKeyDown だが、 本 panel は input を
		// 持たないため document listener を張る。 visible 時のみ反応)。
		const onKeyDown = (e: KeyboardEvent) => {
			if (visible() && e.key === "Escape") {
				e.preventDefault();
				dismiss();
			}
		};
		document.addEventListener("keydown", onKeyDown);
		onCleanup(() => {
			document.removeEventListener("keydown", onKeyDown);
			if (window.vpWire?.handleResult === handleResult) {
				window.vpWire = undefined;
			}
		});
	});

	// wire 活動 → World が lanes snapshot を再 push (既存) → 表示中なら追従 fetch。
	createEffect(
		on(
			() => sidebar.lanes_by_project,
			() => {
				if (visible() && targetAddress()) fetchInbox(targetAddress());
			},
			{ defer: true },
		),
	);

	return (
		<Show when={visible()}>
			<div class="vp-wire-backdrop" onClick={dismiss}>
				<div class="vp-wire-panel" onClick={(e) => e.stopPropagation()}>
					<header class="vp-wire-header">
						<CreoIcon name="ph:envelope" size={14} />
						<span class="vp-wire-agent" title={agent()}>
							{shortAddr(agent()) || targetAddress()}
						</span>
						<Show when={unread() > 0}>
							<span class="vp-wire-unread">{unread()} 未読</span>
						</Show>
						<span class="vp-wire-spacer" />
						<button
							type="button"
							class="vp-wire-iconbtn"
							title="再読み込み"
							onClick={() => fetchInbox(targetAddress())}
						>
							<CreoIcon name="ph:arrows-clockwise" size={13} />
						</button>
						<button
							type="button"
							class="vp-wire-iconbtn"
							title="閉じる (Esc)"
							onClick={dismiss}
						>
							<CreoIcon name="ph:x" size={13} />
						</button>
					</header>
					<Show when={error()}>
						<div class="vp-wire-error">{error()}</div>
					</Show>
					<div class="vp-wire-list">
						<For each={messages()}>
							{(m) => (
								<div
									class="vp-wire-card"
									classList={{ inbound: m.inbound, unacked: needsAck(m) }}
								>
									<div class="vp-wire-meta">
										<span class="vp-wire-dir" title={m.inbound ? "受信" : "送信"}>
											{m.inbound ? "⬇" : "⬆"}
										</span>
										<span class="vp-wire-from" title={`${m.from} → ${m.to.join(", ")}`}>
											{shortAddr(m.inbound ? m.from : m.to.join(","))}
										</span>
										<Show when={chipOf(m, "kind")}>
											<span class="vp-wire-chip kind">{chipOf(m, "kind")}</span>
										</Show>
										<Show when={chipOf(m, "category")}>
											<span class="vp-wire-chip cat">{chipOf(m, "category")}</span>
										</Show>
										<span class="vp-wire-spacer" />
										<Show when={m.inbound && m.acked}>
											<span class="vp-wire-acked" title="ack 済み">
												✓
											</span>
										</Show>
										<span class="vp-wire-time">{fmtTime(m.created_at)}</span>
										<Show when={needsAck(m)}>
											<button
												type="button"
												class="vp-wire-ack-btn"
												title="処理済みとして ack する (再掲示が止まる)"
												onClick={() => ackMessage(m.id)}
											>
												ack
											</button>
										</Show>
									</div>
									<div
										class="vp-wire-body"
										classList={{ open: expanded().has(m.id) }}
										onClick={() => toggleExpand(m.id)}
									>
										{preview(m)}
									</div>
								</div>
							)}
						</For>
						<Show when={!loading() && messages().length === 0 && !error()}>
							<div class="vp-wire-empty">wire 履歴なし</div>
						</Show>
						<Show when={loading()}>
							<div class="vp-wire-empty">読み込み中…</div>
						</Show>
					</div>
				</div>
			</div>
		</Show>
	);
}

/** Shell.tsx の <style> に連結する CSS (FILE_EXPLORER_CSS と同じ流儀)。 */
export const WIRE_PANEL_CSS = `
/* LaneRow の mailbox badge は WirePanel の起動ボタンを兼ねる */
.vp-lane-msg{cursor:pointer;}
.vp-wire-backdrop{position:absolute;inset:0;background:rgba(10,12,16,.55);z-index:9000;display:flex;align-items:stretch;}
.vp-wire-panel{margin:24px 8px;flex:1;display:flex;flex-direction:column;background:var(--vp-bg,#14171d);border:1px solid rgba(255,255,255,.09);border-radius:10px;overflow:hidden;min-height:0;}
.vp-wire-header{display:flex;align-items:center;gap:6px;padding:8px 10px;border-bottom:1px solid rgba(255,255,255,.08);font-size:12px;}
.vp-wire-agent{font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-wire-unread{font-size:10px;padding:1px 7px;border-radius:9px;background:rgba(240,98,146,.18);color:#f06292;}
.vp-wire-spacer{flex:1;}
.vp-wire-iconbtn{background:none;border:none;color:inherit;opacity:.7;cursor:pointer;padding:2px;display:flex;}
.vp-wire-iconbtn:hover{opacity:1;}
.vp-wire-error{padding:8px 10px;font-size:11px;color:#ffb74d;border-bottom:1px solid rgba(255,255,255,.06);}
.vp-wire-list{flex:1;overflow-y:auto;padding:6px;display:flex;flex-direction:column;gap:6px;min-height:0;}
.vp-wire-card{border:1px solid rgba(255,255,255,.07);border-radius:8px;padding:6px 8px;background:rgba(255,255,255,.02);}
.vp-wire-card.inbound{border-left:2px solid rgba(77,208,225,.55);}
.vp-wire-card.unacked{border-left:2px solid #f06292;}
.vp-wire-meta{display:flex;align-items:center;gap:5px;font-size:10.5px;color:rgba(255,255,255,.75);}
.vp-wire-dir{opacity:.8;}
.vp-wire-from{font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:36%;}
.vp-wire-chip{font-size:9px;padding:0 6px;border-radius:7px;background:rgba(255,255,255,.08);}
.vp-wire-chip.cat{background:rgba(129,199,132,.15);color:#81c784;}
.vp-wire-chip.kind{background:rgba(77,208,225,.14);color:#4dd0e1;}
.vp-wire-acked{color:#81c784;font-size:11px;}
.vp-wire-time{font-size:9.5px;opacity:.55;}
.vp-wire-ack-btn{font-size:9.5px;padding:1px 8px;border-radius:7px;border:1px solid rgba(240,98,146,.5);background:rgba(240,98,146,.12);color:#f06292;cursor:pointer;}
.vp-wire-ack-btn:hover{background:rgba(240,98,146,.28);}
.vp-wire-body{margin-top:4px;font-size:11px;line-height:1.5;color:rgba(255,255,255,.88);white-space:pre-wrap;word-break:break-word;overflow:hidden;display:-webkit-box;-webkit-line-clamp:3;-webkit-box-orient:vertical;cursor:pointer;}
.vp-wire-body.open{display:block;-webkit-line-clamp:unset;}
.vp-wire-empty{padding:16px;text-align:center;font-size:11px;opacity:.5;}
`;
