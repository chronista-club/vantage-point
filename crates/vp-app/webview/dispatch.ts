/**
 * Rust → webview 押し込みの **単一の受け口**（`window.vpDispatch`）。
 *
 * SSOT は `crates/vp-app/schema/vp-push.kdl`。型は codegen が両側に出す
 * （Rust `PushEventEnvelope` / TS `PushEventEnvelope`、再生成は
 * `cargo test -p vp-app --test push_codegen`）。
 *
 * ## なぜ窓口を 1 つにするか
 *
 * 旧来 Rust は `window.ensureLane(...)` のように**名前で関数を呼んで**いた（面は ~24 個）。
 * この形の負債は 2 つあり、**実際にバグを生んできたのは 2 つ目**だった:
 *
 * 1. **型が無い** — 引数の数や順序が食い違っても Rust も TS も黙る
 * 2. **押し込みが黙って落ちる** — `window.X && window.X.y(...)` は bundle 準備前なら
 *    **no-op で「成功」する**。VP はこの穴を feature ごとの pull で埋めてきた
 *    （旧 `lanes:ensure-all` / `bastet:devices_fetch` / `board:demand` — 3 本とも退役済）
 *
 * 窓口が 1 つになると、**そこに buffer を 1 個置くだけで全部の取りこぼしが消える**。
 * ~24 個の窓口それぞれに buffer は置けなかった。
 *
 * ## 何を buffer するか
 *
 * この channel は**制御面だけ**（取りこぼすと state が食い違う）なので全部積む。
 * PTY 出力のような stream は**積まない**方針で、別 event として足すときに改めて決める
 * （server 側 replay が取りこぼしを埋めるので、積むとメモリだけ食う）。
 *
 * ⚠️ buffer が救えるのは「bundle は評価済みだが受け手がまだ install されていない」窓まで。
 * bundle 評価**前**に Rust が撃った分は `window.vpDispatch` 自体が居ないので届かない
 * （Rust 側も `window.vpDispatch &&` で guard している）。その窓の救済は **`ready` の
 * replay**（`entry.tsx` の `t:"ready"` → Rust `AppEvent::WebviewReady`）— 受け口が揃った
 * ことを Rust に伝え、現在の状態を丸ごと撃ち直させる。つまり
 * **保留箱 = install 前の窓 / `ready` = bundle 評価前の窓** で役割が分かれている。
 */
import type { PushEventEnvelope } from "./src/generated/Push";

/** 受け手が揃うまでの保留箱。install 時に順序どおり流す。 */
let pending: PushEventEnvelope[] | null = [];

/** wire 名 → 実処理。`installDispatch` が受け取る。 */
export interface PushHandlers {
	ensureLane(lane: string, session: number, isRoot: boolean): void;
	showLane(lane: string | null, isChat: boolean): void;
	removeLane(lane: string): void;
	removeLaneSession(lane: string, session: number): void;
	deliverPaste(text: string): void;
	renderDevices(devices: unknown[]): void;
	handleBoardMessage(message: unknown): void;
	consoleSessionList(lane: string, payload: unknown): void;
	consoleEvent(lane: string, event: unknown, session: number): void;
	consoleModeApplied(lane: string, session: number, mode: string): void;
	consoleStands(lane: string, payload: unknown, req: string | null): void;
	inkSnapshot(path: string): void;
	inkSnapshotError(message: string): void;
}

/** `ink.ts` が持ち分として返す arm（mount target 不在なら null を返す = 受け手不在）。 */
export type InkPushHandlers = Pick<
	PushHandlers,
	"inkSnapshot" | "inkSnapshotError"
>;

/**
 * `term.ts` が持ち分として返す arm。
 *
 * 面が増えるほど `PushHandlers` は太るが、**実処理の持ち主は module ごとに違う**ので、
 * 各 module は自分の担当だけ返して entry.tsx が 1 つに束ねる。`Pick` にしてあるのは、
 * 引数の形を二重に書かないため（schema → codegen → PushHandlers が唯一の出どころ）。
 */
export type TermPushHandlers = Pick<
	PushHandlers,
	| "ensureLane"
	| "showLane"
	| "removeLane"
	| "removeLaneSession"
	| "deliverPaste"
>;

let handlers: PushHandlers | null = null;

function apply(msg: PushEventEnvelope): void {
	if (!handlers) return;
	// ⚠️ `switch` の網羅性は TS が見る — schema に event を足して codegen を回すと、
	// ここに arm を足すまで型が通らない。**これが「境界に型が無い」の解消そのもの**。
	switch (msg.t) {
		case "term:ensure_lane":
			handlers.ensureLane(msg.lane, msg.session, msg.is_root);
			break;
		case "term:show_lane":
			// `lane` は schema で optional — 「lane 未選択」が型に載る（旧: JS の null 直書き）。
			handlers.showLane(msg.lane ?? null, msg.is_chat);
			break;
		case "term:remove_lane":
			handlers.removeLane(msg.lane);
			break;
		case "term:remove_session":
			handlers.removeLaneSession(msg.lane, msg.session);
			break;
		case "term:paste":
			handlers.deliverPaste(msg.text);
			break;
		case "devices:render":
			handlers.renderDevices(msg.devices);
			break;
		case "board:message":
			handlers.handleBoardMessage(msg.message);
			break;
		case "ink:snapshot":
			handlers.inkSnapshot(msg.path);
			break;
		case "ink:snapshot_error":
			handlers.inkSnapshotError(msg.message);
			break;
		case "console:session_list":
			handlers.consoleSessionList(msg.lane, msg.payload);
			break;
		case "console:event":
			handlers.consoleEvent(msg.lane, msg.event, msg.session);
			break;
		case "console:mode_applied":
			handlers.consoleModeApplied(msg.lane, msg.session, msg.mode);
			break;
		case "console:agents":
			// `req` は schema で optional — 「応答を誰も拾わない」が型に載る。
			handlers.consoleStands(msg.lane, msg.payload, msg.req ?? null);
			break;
		default: {
			// 網羅していれば `never`。Rust 側が新しい event を撃ってきた（= 版ズレ）ときだけ来る。
			const unknown: never = msg;
			console.warn("[vp-dispatch] 未知の push", unknown);
		}
	}
}

/**
 * 受け口を window に生やす。**module 評価時に呼ぶ**こと — 早く載るほど、Rust の押し込みを
 * 取りこぼす窓が狭い。実処理の install（[`installDispatch`]）はその後でよく、間に届いた分は
 * 保留箱が預かる。
 */
export function openDispatch(): void {
	(
		window as unknown as { vpDispatch: (m: PushEventEnvelope) => void }
	).vpDispatch = (msg) => {
		if (pending) {
			pending.push(msg);
			return;
		}
		apply(msg);
	};
}

/** 実処理を繋ぎ、保留分を**届いた順に**流す。 */
export function installDispatch(h: PushHandlers): void {
	handlers = h;
	const queued = pending ?? [];
	pending = null; // 以降は直通
	for (const msg of queued) apply(msg);
	if (queued.length > 0) {
		console.info(`[vp-dispatch] 保留 ${queued.length} 件を流した`);
	}
}
