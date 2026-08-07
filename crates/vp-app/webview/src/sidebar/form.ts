/**
 * 左 sidebar の view 形（フル ⇄ スリム帯）— sidebar view modes（2026-08-01）。
 *
 * `Cmd+[`（directive `[`）で行き来する 2 態。スリム = 右端の edge rail と対になる
 * icon 幅の帯（repo badge 列 + daemon ドット、描画は Shell.tsx の SlimRail）。
 *
 * - **data**: `sidebarForm` signal（この module が SSOT）
 * - **actions**: `toggleSidebarForm` / `expandSidebar`（signal と `#sidebar-root` の
 *   幅 class を常に同時に書く — 片方だけ動く状態を作らない）
 *
 * 幅そのもの（280px / 44px）は main_area.rs の `#sidebar-root` CSS が司る。
 * 形の**永続**は `shell-layout.ts`（main bundle）が持つ — ここは形を変えたことを
 * `vp:sidebar-form` で伝え、復元は `vp:shell-restore` / 保留箱で受け取る側。
 */
import { createSignal } from "solid-js";

export type SidebarForm = "full" | "slim";

const [sidebarForm, setSidebarForm] = createSignal<SidebarForm>("full");

/** Shell.tsx が読む reactive な現在形。 */
export { sidebarForm };

/** 形の反映: signal（Solid 側の描画分岐）と root class（CSS 幅）の 2 点セット。 */
function applyForm(next: SidebarForm): void {
	setSidebarForm(next);
	document
		.getElementById("sidebar-root")
		?.classList.toggle("slim", next === "slim");
	// shell layout（main bundle の shell-layout.ts）へ「形が変わった」を伝える。
	// あちらが取っ手の出し入れ（slim では掴めない）と永続化を持つ。bundle が別なので
	// import ではなく document の CustomEvent bus を使う（`vp:board-view` と同じ流儀）。
	document.dispatchEvent(
		new CustomEvent("vp:sidebar-form", { detail: { form: next } }),
	);
}

/** 復元 detail を形へ。未知の値は無視（既定のまま）。 */
function adoptRestore(d: { form?: string } | null | undefined): void {
	if (d?.form === "slim" || d?.form === "full") applyForm(d.form);
}

/**
 * 復元（shell-layout.ts の `vp:shell-restore`）を受けて形を戻す。boot で 1 回。
 *
 * ⚠️ **event だけに頼らない**。この module は sidebar bundle に居て、復元を撃つ
 * shell-layout.ts は main bundle — **あちらが先に評価される**ので、`addEventListener` が
 * 生える前に撃たれる窓が実在する（2026-08-06 実機で確認: 幅は戻るのに slim だけ戻らない）。
 * CustomEvent は保持されないため、その窓に落ちた復元は二度と来ない。
 *
 * そこで shell-layout.ts は撃つと同時に `window.__vpShellRestore` に**保持**する。
 * ここでは「listener を張る」と「既に置かれていたら引き取る」の**両方**をやる。
 * 順序がどちらでも 1 回だけ当たる（同じ値なので二重適用も無害）。
 */
export function installSidebarFormRestore(): void {
	document.addEventListener("vp:shell-restore", (e) => {
		adoptRestore((e as CustomEvent<{ form?: string }>).detail);
	});
	// 取りこぼし回収 — install より前に撃たれていた場合はここで当たる。
	adoptRestore(
		(globalThis as unknown as { __vpShellRestore?: { form?: string } })
			.__vpShellRestore,
	);
}

/** `[` directive — フル ⇄ スリムを toggle。 */
export function toggleSidebarForm(): void {
	applyForm(sidebarForm() === "full" ? "slim" : "full");
}

/** スリム中の badge click 等「フルに戻って続きを操作する」動線。フル中は no-op。 */
export function expandSidebar(): void {
	if (sidebarForm() === "slim") applyForm("full");
}
