/**
 * Lane (Lead / Wing) 1 行の描画 component。
 *
 * v1.0 柱 2。 旧 SIDEBAR_HTML の `.vp-lane-row` 構築ロジックを SolidJS に port。
 * 描画 (PR-2): stand icon / label / wing git meta / awaiting dot / mailbox icon /
 * session title (2 行目)。 click 選択 (PR-3): row click → `lane:select` IPC で
 * main area を当該 Lane に切り替え。 右クリック操作 (restart / delete) は
 * ContextMenu に集約 (VP-204 PR-1)。
 */
import { Show } from "solid-js";
import { CreoIcon } from "creoui-icons-web";
import type { LaneInfo } from "../generated/LaneInfo";
import type { WingStatusWire } from "../generated/WingStatusWire";
import { sidebar } from "./store";
import { sendIpc } from "./ipc";
import { openContextMenu, type ContextMenuItem } from "./ContextMenu";
import {
	isWingLane,
	laneAddressKey,
	laneLabel,
	standDisplayName,
	standIcon,
} from "./lane";

/** Wing Lane の git 状態を右端に小さく表示 (= dirty / ahead-behind の signal のみ、
 *  branch 名 / merged ラベルは noise なので omit)。 ミニマム表示 (2026-05-30)。 */
function WingMeta(props: { ws: WingStatusWire }) {
	const ahead = () => props.ws.ahead | 0;
	const behind = () => props.ws.behind | 0;
	const dirty = () => props.ws.dirty_count | 0;
	return (
		<span class="vp-lane-meta">
			<Show when={ahead() > 0}>
				<span class="ahead">↑{ahead()}</span>
			</Show>
			<Show when={behind() > 0}>
				<span class="behind">↓{behind()}</span>
			</Show>
			<Show when={dirty() > 0}>
				<span class="dirty">{dirty()}M</span>
			</Show>
		</span>
	);
}

export function LaneRow(props: {
	lane: LaneInfo;
	projectPath: string;
	connector?: string;
	connectorClass?: string;
}) {
	const addr = () => laneAddressKey(props.lane);
	const isActive = () => sidebar.active_lane_address === addr();
	// F.8 B Convergent: Pane (Echoes) 不在 = pid:null は Dead Lane (spawn 失敗)、 dim 表示。
	const isInactive = () => props.lane.pid == null;
	const isWing = () => isWingLane(props.lane);
	const icon = () => standIcon(props.lane.stand, isActive());
	// mailbox inbox: entry がある Lane のみ icon 表示 (mailbox infra が active)。
	const inbox = () => sidebar.lane_inboxes?.[addr()];
	// OSC 99 由来の入力待ち。 active lane は即読扱いで dot を出さない。 inactive も除外。
	const isAwaiting = () =>
		!isInactive() && !isActive() && !!sidebar.awaiting_input[addr()];
	// cc `/rename` の custom-title (2 行目)。 未設定 lane は dimmed "—"。
	const sessionTitle = () => sidebar.session_titles?.[addr()];

	// row click → main area を当該 Lane に切り替え。 Inactive Lane (pid:null) は SP に
	// PtySlot が無く、 select すると WS 1006 切断 → reconnect loop に入るためガード
	// (旧 SIDEBAR_HTML と同じ挙動)。
	const onSelect = () => {
		if (isInactive()) return;
		sendIpc({ t: "lane:select", path: props.projectPath, address: addr() });
	};

	// 右クリック → context menu。 Lane 操作は ContextMenu に一本化 (VP-204 PR-1 で
	// inline hover ボタンを撤去)。 操作対象が無い Lane (inactive Lead — project 削除は
	// PR-2) は items 空 → openContextMenu が no-op。
	const onContextMenu = (e: MouseEvent) => {
		const lane = props.lane;
		const wing = isWingLane(lane);
		const active = lane.pid != null;
		const items: ContextMenuItem[] = [];
		if (active) {
			items.push({
				label: `Restart ${wing ? "Wing" : "Lead"} Stand`,
				icon: "ph:arrow-clockwise",
				onSelect: () =>
					sendIpc({
						t: "lane:restart",
						path: props.projectPath,
						address: addr(),
					}),
			});
		}
		if (wing) {
			// delete は破壊的 (PTY kill + tmux kill + workspace dir 削除) なので 2-click 確認。
			items.push({
				label: "Delete Wing",
				icon: "ph:trash",
				danger: true,
				confirm: { label: "もう一度クリックで削除", icon: "ph:check" },
				onSelect: () =>
					sendIpc({
						t: "lane:delete",
						path: props.projectPath,
						address: addr(),
					}),
			});
		}
		openContextMenu(laneLabel(lane), items, e.clientX, e.clientY);
	};

	return (
		<div
			class="vp-lane-row"
			classList={{ active: isActive(), inactive: isInactive(), wing: isWing() }}
			onClick={onSelect}
			onContextMenu={onContextMenu}
		>
			{/* ⓪ tree connector (box-drawing、 線種で control surrender FSM を表現) */}
			<Show when={props.connector}>
				<span class={`vp-lane-connector ${props.connectorClass ?? ""}`}>
					{props.connector}
				</span>
			</Show>
			{/* ① stand icon */}
			<Show when={icon()}>
				<span class="vp-lane-icon" title={standDisplayName(props.lane.stand)}>
					<CreoIcon name={icon()!} size={14} />
				</span>
			</Show>
			{/* session title を stand icon の右へ (= 旧 2 段目を 1 行目に昇格)。
			    label (④) は tree 段下げで wing 視認可なので omit。
			    fallback: session title 未設定なら wing は name、 lead は project 名を
			    dimmed で出す (= 旧 "—" placeholder の代替、 空行回避)。 */}
			<span
				class="vp-lane-title"
				classList={{ "is-fallback": !sessionTitle() }}
				title={sessionTitle() ?? laneLabel(props.lane)}
			>
				{sessionTitle() ??
					(isWing() ? laneLabel(props.lane) : props.lane.address.project)}
			</span>
			{/* 右端ブロック: ⑤ git meta (dirty/↑↓ のみ) → ⑥ awaiting dot → ② files → ③ mailbox */}
			<span class="vp-lane-right">
				<Show when={isWing() && props.lane.wing_status}>
					<WingMeta ws={props.lane.wing_status!} />
				</Show>
				<Show when={isAwaiting()}>
					<span class="vp-lane-awaiting" title="Claude is waiting for input" />
				</Show>
				<button
					class="vp-lane-files-btn"
					type="button"
					title="ファイルを開く (Cmd+F)"
					onClick={(e) => {
						e.stopPropagation();
						window.vpFilePicker?.open(addr());
					}}
				>
					<CreoIcon name="ph:folder-open" size={12} />
				</button>
				<Show when={inbox()}>
					<span
						class="vp-lane-msg"
						classList={{ unread: (inbox()!.unread_count | 0) > 0 }}
						title={`mailbox: ${addr()}`}
					>
						<CreoIcon
							name={
								(inbox()!.unread_count | 0) > 0
									? "ph:envelope-fill"
									: "ph:envelope"
							}
							size={13}
						/>
					</span>
				</Show>
			</span>
		</div>
	);
}
