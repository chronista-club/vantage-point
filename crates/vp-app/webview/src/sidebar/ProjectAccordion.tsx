/**
 * Project (= Runtime Process) 1 件を accordion で描画する component。
 *
 * v1.0 柱 2。 旧 SIDEBAR_HTML の `renderProjectAccordion` を SolidJS に port。
 * native `<details>` で expand/collapse、 開閉時に `process:toggle` IPC を送って
 * Rust 側 state に永続化する。 展開時の内容は SP state に応じた hint、 または Lane 行。
 *
 * PR-3: active project (= 現在 active な Lane を含む project) の summary 右上に
 * 「+」アイコンを出し、 click で Add Performer フォームを開閉する。
 */
import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { CreoIcon } from "creoui-icons-web";
import type { ProjectPaneState } from "../generated/ProjectPaneState";
import { sidebar } from "./store";
import { sendIpc } from "./ipc";
import { openContextMenu, type ContextMenuItem } from "./ContextMenu";
import { isRunningProcess } from "./classify";
import { laneAddressKey, isLaneAlive, isPerformerLane } from "./lane";
import type { LaneInfo } from "../generated/LaneInfo";
import { LaneRow } from "./LaneRow";
import { AddPerformer } from "./AddPerformer";
import { registerAddPerformerOpenSetter } from "./directive-state";
import {
	clearDrag,
	commitProjectReorder,
	dragPath,
	dropMark,
	setDragPath,
	setDropMark,
	type DropPos,
} from "./dnd";

/**
 * Lane の tree connector の状態 class を導出する (2026-05-30 glyph 版 → 2026-07 CSS 描画版)。
 *
 * 脱 TUI hybrid: 描画は Shell.tsx の `.vp-lane-connector` (CSS pseudo-element) が担い、
 * ここは「線種 = control surrender FSM」の意味論だけを class で返す。
 * ツリー構造 (途中 ├ / 末尾 └ 相当) は LaneRow の `connectorLast` prop で伝える。
 *
 * - conductor: solid (= 幹、 固定)
 * - performer inactive (idle/dead, pid null): dotted (= 休眠、 dim)
 * - performer awaiting (hitl_pending): solid warn 色 (= 人を待つ、 control 握る)
 * - performer 自走 (working/autonomous = active & not awaiting): 流れる破線 info 色
 *   (= control 手放した = self-running、 BPM 82.7 同期アニメ)
 */
function laneConnector(lane: LaneInfo): string {
	if (!isPerformerLane(lane)) {
		return "conn-conductor"; // conductor は幹 = solid
	}
	const addr = laneAddressKey(lane);
	// 生死は isLaneAlive に一本化 (pid 直参照だと chat performer を dotted = 休眠に描いてしまう)。
	if (!isLaneAlive(lane)) return "conn-dead"; // 休眠 = dotted
	if (sidebar.awaiting_input[addr]) return "conn-hitl"; // HITL = solid (色で warn)
	return "conn-auto"; // 自走 = 流れる破線
}

/**
 * SP の state に応じた hint 文字列。 `null` を返したら Lane 行を描画する。
 * 旧 SIDEBAR_HTML のロジックを踏襲 — SP 未起動/過渡/error は spinner で永久ロード
 * 表示にならないよう、 state 別に明示的な hint を返す。
 */
function hintFor(
	proc: ProjectPaneState,
	laneCount: number,
	subState: string | undefined,
): string | null {
	const s = proc.state;
	if (!s || s === "stopped") {
		return proc.expanded ? "⏳ SP starting…" : "💤 SP stopped — open to spawn";
	}
	if (s === "starting") return "⏳ SP starting…";
	if (s === "stopping") return "⏳ SP stopping…";
	if (s === "error") return "⚠️ SP error — restart で復帰";
	// lane 供給 (World "lanes" channel) の可用性を SP state とは別軸で見る (doc 30 §5-3)。
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

export function ProjectAccordion(props: { proc: ProjectPaneState }) {
	const lanes = () => sidebar.lanes_by_project[props.proc.path] ?? [];
	const hint = () =>
		hintFor(
			props.proc,
			lanes().length,
			sidebar.lane_sub_state?.[props.proc.path],
		);
	// L1 lifecycle: SP の presence（daemon-canonical、`/api/health` の processes[] 由来）。
	// entry 不在（旧 daemon / 未取得）は "unregistered" 扱いで ○（dim）。
	const presence = () =>
		sidebar.activity.presence?.[props.proc.path] ?? "unregistered";
	// active project = 現在 active な Lane を含む project。 Add Performer の「+」はこの時だけ出す。
	const isActiveProject = () => {
		const a = sidebar.active_lane_address;
		return a != null && lanes().some((l) => laneAddressKey(l) === a);
	};

	const [addPerformerOpen, setAddPerformerOpen] = createSignal(false);
	// PR 445 `n` directive: keyboard で AddPerformer form を open するため、 ProjectAccordion 内 local
	// signal を **module-scope registry** に export する。 directive 発火時に registry から
	// setter を引いて open する経路 (= directive-state.ts::openAddPerformerFor)。
	onMount(() => {
		const unreg = registerAddPerformerOpenSetter(props.proc.path, (open) =>
			setAddPerformerOpen(open),
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

	// 停止中 = SP の Process が無い state (`isRunningProcess` の否定)。 Start ボタン (▶) と
	// context menu の出し分けに使う (タブ分割は撤去済、 project は 1 リストに留まる)。
	const isPaused = () => !isRunningProcess(props.proc);

	// Add Performer「+」を出す条件。 isActiveProject だけだと、 一度 active にした project を
	// Stop した後も active_lane_address / lanes_by_project が残る (SP 停止で自動クリア
	// されない) ため、 停止中でも true になりうる。 稼働中であることを明示的に AND して、
	// 停止中 project に「+」と Start「▶」が同居する / 失敗する Add Performer を開けるのを防ぐ。
	const showAddPerformer = () => isActiveProject() && !isPaused();

	// 📁 project ヘッダの右クリック → project context menu。
	//   - 一時停止中: Start project (restart_process は dead な project も起こす)
	//   - 稼働中: Restart project + Stop project (SP が listen 中 = port あり の時のみ)
	//   - Delete project: 常時。 破壊的 (projects.kdl から unregister) なので 2-click 確認。
	const onSummaryContextMenu = (e: MouseEvent) => {
		const proc = props.proc;
		const items: ContextMenuItem[] = [];
		if (isPaused()) {
			// Start も Restart も IPC は同じ `process:restart` — restart_process が
			// 「stop が失敗しても start を試みる」 ので停止中 project の起動を兼ねる。
			items.push({
				label: "Start project",
				icon: "ph:play",
				onSelect: () => sendIpc({ t: "process:restart", path: proc.path }),
			});
		} else {
			items.push({
				label: "Restart project",
				icon: "ph:arrow-clockwise",
				onSelect: () => sendIpc({ t: "process:restart", path: proc.path }),
			});
			// Stop project: SP が実際に listen 中 (port あり) の時のみ。 停止しても
			// project は registered のまま同じリストに残り、 起動 ▶ affordance が出る。
			if (proc.port != null) {
				items.push({
					label: "Stop project",
					icon: "ph:stop",
					onSelect: () => sendIpc({ t: "process:stop", path: proc.path }),
				});
			}
		}
		items.push({
			label: "Delete project",
			icon: "ph:trash",
			danger: true,
			confirm: { label: "もう一度クリックで削除", icon: "ph:check" },
			onSelect: () => sendIpc({ t: "process:delete", path: proc.path }),
		});
		openContextMenu(proc.name, items, e.clientX, e.clientY);
	};

	// ── Project D&D 並べ替え (#124) ──────────────────────────────
	// draggable は `<details>` 要素に付ける。 `<summary>` を draggable にすると WebKit
	// (WKWebView) では disclosure トグルの活性化機構が mousedown を消費して drag が
	// 開始しない (旧 SIDEBAR_HTML も `<details>` 側に付けて動いていた)。 掴んだ後は
	// summary を他 Project の手前 / 後ろへ落とすと `process:reorder` を送る。
	// この Project を掴んでいるか (= 半透明表示)。
	const isDragging = () => dragPath() === props.proc.path;
	// この Project の手前 / 後ろに drop インジケータ線を出すか。
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
	// 記録し、 summary 由来のドラッグだけを通す。 これで展開中の Lane 行から project の
	// 並べ替えが誤発火しない。
	let summaryGrabbed = false;
	const onMouseDown = (e: MouseEvent) => {
		const t = e.target as HTMLElement | null;
		summaryGrabbed = t != null && t.closest(".vp-proj-summary") != null;
	};

	const onDragStart = (e: DragEvent) => {
		if (!summaryGrabbed) {
			// summary 以外 (Lane 行など) から掴んだ — project 並べ替えにはしない。
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
		// 行の上半分 = 手前、 下半分 = 後ろ。 末尾 Project の下半分にも落とせるので
		// 「末尾 Project が D&D で動かせない」 (#124) が解消する。
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
			commitProjectReorder(dragged, props.proc.path, mark.pos);
		}
		clearDrag();
	};

	return (
		<details
			class="vp-proj"
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
			<summary class="vp-proj-summary" onContextMenu={onSummaryContextMenu}>
				<span
					class="vp-proj-presence-dot"
					classList={{
						connected: presence() === "connected",
						connecting: presence() === "connecting",
						disconnected: presence() === "disconnected",
						unregistered: presence() === "unregistered",
					}}
					title={`SP presence: ${presence()}`}
				/>
				<CreoIcon
					name={props.proc.expanded ? "ph:folder-open" : "ph:folder"}
					size={14}
				/>
				<span class="vp-proj-name">{props.proc.name}</span>
				<Show when={showAddPerformer()}>
					<button
						class="vp-proj-addperformer"
						classList={{ open: addPerformerOpen() }}
						title="Add Performer"
						onClick={(e) => {
							// summary click は <details> を toggle するので止める。
							e.preventDefault();
							e.stopPropagation();
							setAddPerformerOpen((v) => !v);
						}}
					>
						<CreoIcon name="ph:plus" size={12} />
					</button>
				</Show>
				{/* 停止中 project の起動 affordance。 「+」(showAddPerformer) は稼働中限定
            なので、 停止中のこの「▶」とは同居しない。 */}
				<Show when={isPaused()}>
					<button
						class="vp-proj-start"
						title="Start project"
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
			<div class="vp-proj-content">
				<Show
					when={hint()}
					fallback={
						<>
							<For each={lanes()}>
								{(lane, i) => (
									<LaneRow
										lane={lane}
										projectPath={props.proc.path}
										connectorClass={laneConnector(lane)}
										connectorLast={i() === lanes().length - 1}
									/>
								)}
							</For>
							<Show when={addPerformerOpen()}>
								<AddPerformer
									projectPath={props.proc.path}
									onClose={() => setAddPerformerOpen(false)}
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
