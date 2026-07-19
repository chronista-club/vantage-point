/**
 * Lane (Conductor / Performer) 1 行の描画 component。
 *
 * v1.0 柱 2。 旧 SIDEBAR_HTML の `.vp-lane-row` 構築ロジックを SolidJS に port。
 * 描画 (PR-2): stand icon / label / performer git meta / awaiting dot / mailbox icon /
 * session title (2 行目)。 click 選択 (PR-3): row click → `lane:select` IPC で
 * main area を当該 Lane に切り替え。 右クリック操作 (restart / delete) は
 * ContextMenu に集約 (VP-204 PR-1)。
 */
import { Show } from "solid-js";
import { CreoIcon } from "@chronista-club/creo-ui-icons-web";
import type { LaneInfo } from "../generated/LaneInfo";
import type { PerformerStatusWire } from "../generated/PerformerStatusWire";
import { sidebar } from "./store";
import { sendIpc } from "./ipc";
import { openContextMenu, type ContextMenuItem } from "./ContextMenu";
import {
	isLaneAlive,
	isPerformerLane,
	laneAddressKey,
	laneCwdLabel,
	laneLabel,
	standDisplayName,
	standIcon,
} from "./lane";

/** Performer Lane の git 状態を右端に小さく表示 (= dirty / ahead-behind の signal のみ、
 *  branch 名 / merged ラベルは noise なので omit)。 ミニマム表示 (2026-05-30)。 */
function PerformerMeta(props: { ws: PerformerStatusWire }) {
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

/**
 * connector class (= control surrender FSM の投影) から state 文字を導出する。
 * conn-auto/run = working、 conn-hitl = needs you。
 * idle (conn-dead) は quiet pass (mako 019f5100) で文字を出さない — 「idle はほぼ消える」。
 * conn-conductor (root) も state を持たない (spine の頭) ので null。
 */
function stateLabel(connectorClass: string | undefined): string | null {
	switch (connectorClass) {
		case "conn-auto":
		case "conn-run":
			return "working";
		case "conn-hitl":
			return "needs you";
		default:
			return null;
	}
}

export function LaneRow(props: {
	lane: LaneInfo;
	projectPath: string;
	/** connector の線種 class (conn-*)。 未指定なら connector 自体を描かない。 */
	connectorClass?: string;
	/** lane list 内の最終行 (= tree corner を └ 相当にする)。 */
	connectorLast?: boolean;
}) {
	const addr = () => laneAddressKey(props.lane);
	const isActive = () => sidebar.active_lane_address === addr();
	// F.8 B Convergent: Pane (Echoes) 不在 = pid:null は Dead Lane (spawn 失敗)、 dim 表示。
	const isInactive = () => !isLaneAlive(props.lane);
	const isPerformer = () => isPerformerLane(props.lane);
	const icon = () => standIcon(props.lane.stand, isActive());
	// mailbox inbox: entry がある Lane のみ icon 表示 (mailbox infra が active)。
	const inbox = () => sidebar.lane_inboxes?.[addr()];
	// OSC 99 由来の入力待ち。 active lane は即読扱いで dot を出さない。 inactive も除外。
	const isAwaiting = () =>
		!isInactive() && !isActive() && !!sidebar.awaiting_input[addr()];
	// Canvas (PP) 着信 badge (bug: canvas 可観測性 D): 現在 active でない lane に show が
	// 届くと点灯。 awaiting(magenta = 用事)とは別語彙の「絵が届いた」 info 信号で、
	// active 化 (行 click) で reset される。
	// isInactive は見ない: canvas 着信は「絵が届いた」事実で、lane の claude の生死とは無関係
	// (dead lane に届いた content も気付かせる。awaiting=入力待ちが alive 前提なのとは意味論が違う)。
	const canvasUnread = () =>
		!isActive() && (sidebar.canvas_unread?.[addr()] ?? 0) > 0;
	// cc `/rename` の custom-title (2 行目)。 未設定 lane は dimmed "—"。
	const sessionTitle = () => sidebar.session_titles?.[addr()];
	// 地 (ground): cwd を project root 起点の差分に畳む。 絶対 path は project が持つので
	// lane は offset だけを名乗る。 conductor は差分ゼロ = "" → 行ごと出さない。
	const cwdLabel = () => laneCwdLabel(props.lane.cwd, props.projectPath);

	// row click → main area を当該 Lane に切り替え。 Dead Lane (pid:null) も select を通す:
	// activate_lane 側の maybe_respawn_dead_lane が on-demand で respawn し、 PtySlot 生成後に
	// main area が追随する。 旧 early-return は「pid:null を select すると WS 1006 → reconnect
	// loop」を防ぐガードだったが、 その後 demand-driven pump (PtySlot 無なら graceful no_lane) と
	// on-demand respawn が入り、 ガードが respawn 経路そのものを握り潰す本末転倒になっていたため撤廃 (BUG#2)。
	const onSelect = () => {
		sendIpc({ t: "lane:select", path: props.projectPath, address: addr() });
	};

	// 右クリック → context menu。 Lane 操作は ContextMenu に一本化 (VP-204 PR-1 で
	// inline hover ボタンを撤去)。 操作対象が無い Lane (inactive Conductor — project 削除は
	// PR-2) は items 空 → openContextMenu が no-op。
	const onContextMenu = (e: MouseEvent) => {
		const lane = props.lane;
		const performer = isPerformerLane(lane);
		// dim 表示 (isInactive) と同じ述語を使う — 生死判定を 2 箇所に散らさない。
		const active = isLaneAlive(lane);
		const items: ContextMenuItem[] = [];
		if (active) {
			items.push({
				label: `Restart ${performer ? "Performer" : "Conductor"} Session`,
				icon: "ph:arrow-clockwise",
				onSelect: () =>
					sendIpc({
						t: "lane:restart",
						path: props.projectPath,
						address: addr(),
					}),
			});
			// doc 39 §1: Reset Lane (fresh=true) — 全 session store + registry 破棄の破壊的動詞。
			// 旧 "New Conductor Session"。日常の「新しい会話を始める」はヘッダの ✨ New（非破壊 =
			// Act I は root 張り替え / Act II は新 Draft タブ）に移り、こちらは「lane を素に戻す」
			// 最終手段として sidebar の奥 + 2-click 確認に退避した。
			items.push({
				label: "Reset Lane",
				icon: "ph:trash",
				danger: true,
				confirm: { label: "もう一度クリックで全会話破棄", icon: "ph:check" },
				onSelect: () =>
					sendIpc({
						t: "lane:restart",
						path: props.projectPath,
						address: addr(),
						fresh: true,
					}),
			});
		} else {
			// Dead Lane (pid:null): 明示 respawn。 左 click の on-demand respawn と同じ lane:restart を
			// menu からも撃てるようにする (会話を継ぐ = fresh 無し)。
			items.push({
				label: `Respawn ${performer ? "Performer" : "Conductor"} Session`,
				icon: "ph:arrow-clockwise",
				onSelect: () =>
					sendIpc({
						t: "lane:restart",
						path: props.projectPath,
						address: addr(),
					}),
			});
		}
		if (performer) {
			// delete は破壊的 (PTY kill + tmux kill + workspace dir 削除) なので 2-click 確認。
			items.push({
				label: "Delete Performer",
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
			classList={{
				active: isActive(),
				inactive: isInactive(),
				performer: isPerformer(),
			}}
			onClick={onSelect}
			onContextMenu={onContextMenu}
		>
			{/* ⓪ tree connector (CSS 描画、 線種で control surrender FSM を表現。
			    脱 TUI hybrid 2026-07: glyph → pseudo-element、 描画は SHELL_CSS 参照) */}
			<Show when={props.connectorClass}>
				<span
					class={`vp-lane-connector ${props.connectorClass}`}
					classList={{ last: props.connectorLast }}
				/>
			</Show>
			{/* ① stand icon */}
			<Show when={icon()}>
				<span class="vp-lane-icon" title={standDisplayName(props.lane.stand)}>
					<CreoIcon name={icon()!} size={14} />
				</span>
			</Show>
			{/* session title を stand icon の右へ (= 旧 2 段目を 1 行目に昇格)。
			    label (④) は tree 段下げで performer 視認可なので omit。
			    fallback: session title 未設定なら performer は name、 conductor は project 名を
			    dimmed で出す (= 旧 "—" placeholder の代替、 空行回避)。 */}
			<span
				class="vp-lane-title"
				classList={{ "is-fallback": !sessionTitle() }}
				title={sessionTitle() ?? laneLabel(props.lane)}
			>
				{sessionTitle() ??
					(isPerformer() ? laneLabel(props.lane) : props.lane.address.project)}
			</span>
			{/* 右端ブロック: ⑦ state 文字 → ⑤ git meta (dirty/↑↓ のみ) → ⑥ awaiting dot → ② files → ③ mailbox */}
			<span class="vp-lane-right">
				{/* Light Grid state 言語の文字面 (working / idle / needs you)。 FSM の SSOT は
				    connectorClass (laneConnector 導出) — 二重導出しない。 root (conductor) は出さない。 */}
				<Show when={stateLabel(props.connectorClass)}>
					<span class="vp-lane-state">{stateLabel(props.connectorClass)}</span>
				</Show>
				<Show when={isPerformer() && props.lane.performer_status}>
					<PerformerMeta ws={props.lane.performer_status!} />
				</Show>
				<Show when={isAwaiting()}>
					<span class="vp-lane-awaiting" title="Claude is waiting for input" />
				</Show>
				<Show when={canvasUnread()}>
					<span
						class="vp-lane-canvas"
						title="Canvas に新しい内容が届きました"
					/>
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
					{/* mailbox badge は Wire Inbox panel (doc 34 §4 V1) の起動ボタンを兼ねる。 */}
					<span
						class="vp-lane-msg"
						classList={{ unread: (inbox()!.unread_count | 0) > 0 }}
						title={`wire inbox を開く: ${addr()}`}
						onClick={(e) => {
							e.stopPropagation();
							window.vpWire?.open(addr());
						}}
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
			{/* ⑧ 地 (ground): project root 起点の cwd 差分。 1 行目が図 (title / state / git meta)、
			    ここが地。 mute-2 / micro / mono = git meta と同じ最も引っ込んだ層に置き、 光らせない
			    (光 = 注意は needs-you の専有)。 CSS の flex:0 0 100% で 2 行目へ折り返す。
			    差分ゼロ (conductor = project root) は **行ごと出さない** — 語ることが無い行は黙る。
			    tooltip には常に完全な絶対 path を出すので情報は落ちない。 */}
			<Show when={cwdLabel()}>
				<span class="vp-lane-cwd" title={props.lane.cwd}>
					{cwdLabel()}
				</span>
			</Show>
		</div>
	);
}
