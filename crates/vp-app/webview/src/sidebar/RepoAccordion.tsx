/**
 * Repo (= Runtime Process) 1 件を accordion で描画する component。
 *
 * v1.0 柱 2。 旧 SIDEBAR_HTML の `renderRepoAccordion` を SolidJS に port。
 * native `<details>` で expand/collapse、 開閉時に `process:toggle` IPC を送って
 * Rust 側 state に永続化する。 展開時の内容は repo state に応じた hint、 または Lane 行。
 *
 * Add Sub form は名簿内に ephemeral に出る（開く入口 = edge rail の + New menu /
 * `n` directive、doc 58 ④。常設「+」は撤去済み）。
 */
import {
	For,
	Show,
	createSignal,
	onCleanup,
	onMount,
} from "solid-js";
import { CreoIcon } from "@chronista-club/creo-ui-icons-web";
import type { RepoPaneState } from "../generated/RepoPaneState";
import { sidebar } from "./store";
import { sendIpc } from "./ipc";
import { openContextMenu, type ContextMenuItem } from "./ContextMenu";
import { isRunningProcess } from "./classify";
import { laneAddressKey, laneConnector } from "./lane";
import type { LaneInfo } from "../generated/LaneInfo";
import { LaneRow, SessionRow } from "./LaneRow";
import { AddSub } from "./AddSub";
import { registerAddSubOpenSetter } from "./directive-state";
import {
	clearDrag,
	commitRepoReorder,
	dragPath,
	dropMark,
	setDragPath,
	setDropMark,
	type DropPos,
} from "./dnd";

/**
 * Lane の tree connector class (FSM 投影 2026-07-11: 実体は lane.ts の純関数 `laneConnector`)。
 *
 * 一次 source = server 側 flow_state (daemon が wire store から derive)、 fallback = pid
 * heuristic。 OSC 99 の awaiting_input (store 依存) だけここで束ねて渡す。
 */
function connectorFor(lane: LaneInfo): string {
	return laneConnector(lane, !!sidebar.awaiting_input[laneAddressKey(lane)]);
}

/**
 * 相部屋の非 root session（doc 58 ②-b — 行 = session の展開元、純関数的導出）。
 * registry 欠落（旧 wire / boot 窓）= 空 = root 1 行のみ（従来と同じ見え方）。
 */
function extraSessionsOf(lane: LaneInfo) {
	const reg = lane.sessions;
	if (!reg) return [];
	return reg.sessions.filter((s) => s.key !== reg.root);
}

/**
 * repo の state に応じた hint 文字列。 `null` を返したら Lane 行を描画する。
 * 旧 SIDEBAR_HTML のロジックを踏襲 — repo 未起動/過渡/error は spinner で永久ロード
 * 表示にならないよう、 state 別に明示的な hint を返す。
 */
function hintFor(
	proc: RepoPaneState,
	laneCount: number,
	subState: string | undefined,
): string | null {
	const s = proc.state;
	if (!s || s === "stopped") {
		// 旧実装は expanded なら「⏳ repo starting…」と言った —「accordion を開く = spawn」の
		// 旧仮定で、stop / disable された repo にも「起動中」と嘘をつく（2026-07-24 実機:
		// 全 repo 停止時に全行が repo starting… のまま）。事実だけを言う。
		return "💤 repo stopped — ▷ で起動";
	}
	if (s === "starting") return "⏳ repo starting…";
	if (s === "stopping") return "⏳ repo stopping…";
	if (s === "error") return "⚠️ repo error — restart で復帰";
	// lane 供給 (Daemon "lanes" channel) の可用性を repo state とは別軸で見る (doc 30 §5-3)。
	// QUIC 購読が停滞 (open/subscribe/snapshot timeout or QUIC 未接続) したら `loading lanes` に
	// 潰さず、 daemon restart で復帰できると surface する。 snapshot 受信で "ready" に解消。
	if (laneCount === 0) {
		if (subState === "stalled")
			return "⚠️ lane 接続が停滞 — daemon restart で復帰";
		if (subState === "ready") return "📡 lane なし";
		return "📡 loading lanes…"; // 初期 (購読開始〜初回 snapshot 待ち)
	}
	return null;
}

export function RepoAccordion(props: { proc: RepoPaneState }) {
	const lanes = () => sidebar.lanes_by_repo[props.proc.path] ?? [];
	const hint = () =>
		hintFor(
			props.proc,
			lanes().length,
			sidebar.lane_sub_state?.[props.proc.path],
		);
	// L1 lifecycle: repo の presence（daemon-canonical、`/api/health` の processes[] 由来）。
	// entry 不在（旧 daemon / 未取得）は "unregistered" 扱いで ○（dim）。
	const presence = () =>
		sidebar.activity.presence?.[props.proc.path] ?? "unregistered";
	// photon one-shot（spine を走る光）は doc 58 台帳で spine ごと撤去。
	const [addSubOpen, setAddSubOpen] = createSignal(false);
	// PR 445 `n` directive: keyboard で AddSub form を open するため、 RepoAccordion 内 local
	// signal を **module-scope registry** に export する。 directive 発火時に registry から
	// setter を引いて open する経路 (= directive-state.ts::openAddSubFor)。
	onMount(() => {
		const unreg = registerAddSubOpenSetter(props.proc.path, (open) =>
			setAddSubOpen(open),
		);
		onCleanup(unreg);
	});

	// native toggle → process:toggle IPC。 store 由来の open 反映で発火した場合は
	// 値が一致するので IPC を送らない (echo loop 防止)。
	const onToggle = (e: Event & { currentTarget: HTMLDetailsElement }) => {
		const open = e.currentTarget.open;
		if (open !== props.proc.expanded) {
			sendIpc({ t: "process:toggle", path: props.proc.path, expanded: open });
		}
	};

	// 停止中 = repo の Process が無い state (`isRunningProcess` の否定)。 Start ボタン (▶) と
	// context menu の出し分けに使う (タブ分割は撤去済、 repo は 1 リストに留まる)。
	const isPaused = () => !isRunningProcess(props.proc);

	// 📁 repo ヘッダの右クリック → repo context menu。
	//   - 一時停止中: Start repo (restart_process は dead な repo も起こす)
	//   - 稼働中: Restart repo + Stop repo (repo が listen 中 = port あり の時のみ)
	//   - Delete repo: 常時。 破壊的 (repos.kdl から unregister) なので 2-click 確認。
	const onSummaryContextMenu = (e: MouseEvent) => {
		const proc = props.proc;
		const items: ContextMenuItem[] = [];
		if (isPaused()) {
			// Start も Restart も IPC は同じ `process:restart` — restart_process が
			// 「stop が失敗しても start を試みる」 ので停止中 repo の起動を兼ねる。
			items.push({
				label: "Start repo",
				icon: "ph:play",
				onSelect: () => sendIpc({ t: "process:restart", path: proc.path }),
			});
		} else {
			items.push({
				label: "Restart repo",
				icon: "ph:arrow-clockwise",
				onSelect: () => sendIpc({ t: "process:restart", path: proc.path }),
			});
			// Stop repo: repo が実際に listen 中 (port あり) の時のみ。 停止しても
			// repo は registered のまま同じリストに残り、 起動 ▶ affordance が出る。
			if (proc.port != null) {
				items.push({
					label: "Stop repo",
					icon: "ph:stop",
					onSelect: () => sendIpc({ t: "process:stop", path: proc.path }),
				});
			}
		}
		items.push({
			label: "Delete repo",
			icon: "ph:trash",
			danger: true,
			confirm: { label: "もう一度クリックで削除", icon: "ph:check" },
			onSelect: () => sendIpc({ t: "repo:delete", path: proc.path }),
		});
		openContextMenu(proc.name, items, e.clientX, e.clientY);
	};

	// ── Repo D&D 並べ替え (#124) ──────────────────────────────
	// draggable は `<details>` 要素に付ける。 `<summary>` を draggable にすると WebKit
	// (WKWebView) では disclosure トグルの活性化機構が mousedown を消費して drag が
	// 開始しない (旧 SIDEBAR_HTML も `<details>` 側に付けて動いていた)。 掴んだ後は
	// summary を他 Repo の手前 / 後ろへ落とすと `process:reorder` を送る。
	// この Repo を掴んでいるか (= 半透明表示)。
	const isDragging = () => dragPath() === props.proc.path;
	// この Repo の手前 / 後ろに drop インジケータ線を出すか。
	const dropBefore = () => {
		const m = dropMark();
		return m != null && m.path === props.proc.path && m.pos === "before";
	};
	const dropAfter = () => {
		const m = dropMark();
		return m != null && m.path === props.proc.path && m.pos === "after";
	};

	// `dragstart` の `target` は draggable 要素 (= `<details>`) に固定で、「実際に掴んだ
	// 子要素」を示さない (HTML 仕様: source node)。 そこで直前の `mousedown` の実 target を
	// 記録し、 summary 由来のドラッグだけを通す。 これで展開中の Lane 行から repo の
	// 並べ替えが誤発火しない。
	let summaryGrabbed = false;
	const onMouseDown = (e: MouseEvent) => {
		const t = e.target as HTMLElement | null;
		summaryGrabbed = t != null && t.closest(".vp-proj-summary") != null;
	};

	const onDragStart = (e: DragEvent) => {
		if (!summaryGrabbed) {
			// summary 以外 (Lane 行など) から掴んだ — repo 並べ替えにはしない。
			e.preventDefault();
			return;
		}
		setDragPath(props.proc.path);
		if (e.dataTransfer) {
			e.dataTransfer.effectAllowed = "move";
			// Firefox は dataTransfer に何か入れないと drag が始まらない。
			e.dataTransfer.setData("text/plain", props.proc.path);
		}
	};

	const onDragOver = (e: DragEvent & { currentTarget: HTMLElement }) => {
		const dragged = dragPath();
		// 自分自身の上 / ドラッグ中でない時は drop を許可しない (preventDefault しない)。
		if (dragged == null || dragged === props.proc.path) return;
		e.preventDefault();
		if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
		// 行の上半分 = 手前、 下半分 = 後ろ。 末尾 Repo の下半分にも落とせるので
		// 「末尾 Repo が D&D で動かせない」 (#124) が解消する。
		const rect = e.currentTarget.getBoundingClientRect();
		const pos: DropPos =
			e.clientY < rect.top + rect.height / 2 ? "before" : "after";
		const cur = dropMark();
		if (cur == null || cur.path !== props.proc.path || cur.pos !== pos) {
			setDropMark({ path: props.proc.path, pos });
		}
	};

	const onDrop = (e: DragEvent) => {
		e.preventDefault();
		const dragged = dragPath();
		const mark = dropMark();
		if (dragged != null && mark != null && dragged !== props.proc.path) {
			commitRepoReorder(dragged, props.proc.path, mark.pos);
		}
		clearDrag();
	};

	return (
		<details
			class="vp-proj creo-sidenav-group"
			data-path={props.proc.path}
			classList={{
				dragging: isDragging(),
				"drop-before": dropBefore(),
				"drop-after": dropAfter(),
			}}
			draggable="true"
			open={props.proc.expanded}
			onToggle={onToggle}
			onMouseDown={onMouseDown}
			onDragStart={onDragStart}
			onDragOver={onDragOver}
			onDrop={onDrop}
			onDragEnd={clearDrag}
		>
			<summary
				class="vp-proj-summary creo-sidenav-title"
				onContextMenu={onSummaryContextMenu}
			>
				<span
					class="vp-proj-presence-dot"
					classList={{
						connected: presence() === "connected",
						connecting: presence() === "connecting",
						disconnected: presence() === "disconnected",
						unregistered: presence() === "unregistered",
					}}
					title={`repo presence: ${presence()}`}
				/>
				{/* Light Grid course-correction: ラベルは地の目印なので icon も 11px に縮小。 */}
				<CreoIcon
					name={props.proc.expanded ? "ph:folder-open" : "ph:folder"}
					size={11}
				/>
				<span class="vp-proj-name">{props.proc.name}</span>
				{/* Add Sub の常設「+」は doc 58 ④ で edge rail の + New menu へ移設。
				    form 本体と registry（openAddSubFor）は残る — 開く経路が rail と
				    `n` directive になっただけ。 */}
				{/* 停止中 repo の起動 affordance。 Add Sub form の入口は稼働中限定
            なので、 停止中のこの「▶」とは同居しない。 */}
				<Show when={isPaused()}>
					<button
						class="vp-proj-start"
						title="Start repo"
						onClick={(e) => {
							// summary click の <details> toggle を止めて起動だけ行う。
							e.preventDefault();
							e.stopPropagation();
							sendIpc({ t: "process:restart", path: props.proc.path });
						}}
					>
						<CreoIcon name="ph:play" size={12} />
					</button>
				</Show>
			</summary>
			<div class="vp-proj-content creo-sidenav-list">
				<Show
					when={hint()}
					fallback={
						<>
							<For each={lanes()}>
								{(lane) => (
									<>
										<LaneRow
											lane={lane}
											repoPath={props.proc.path}
											connectorClass={connectorFor(lane)}
										/>
										{/* doc 58 ②-b: 相部屋（非 root session）は場所ラベル省略の
										    session 行として root 行の直下に並ぶ。 */}
										<For each={extraSessionsOf(lane)}>
											{(sess) => (
												<SessionRow
													lane={lane}
													repoPath={props.proc.path}
													session={sess}
												/>
											)}
										</For>
									</>
								)}
							</For>
							<Show when={addSubOpen()}>
								<AddSub
									repoPath={props.proc.path}
									onClose={() => setAddSubOpen(false)}
								/>
							</Show>
						</>
					}
				>
					<div class="vp-proj-hint">{hint()}</div>
				</Show>
			</div>
		</details>
	);
}
