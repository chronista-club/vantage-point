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
 * 形は in-memory（app 再起動でフルに戻る）— layout 永続は doc 50 P5 の領分。
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

/** 復元（shell-layout.ts の `vp:shell-restore`）を受けて形を戻す。boot で 1 回。 */
export function installSidebarFormRestore(): void {
	document.addEventListener("vp:shell-restore", (e) => {
		const d = (e as CustomEvent<{ form?: string }>).detail;
		if (d?.form === "slim" || d?.form === "full") applyForm(d.form);
	});
}

/** `[` directive — フル ⇄ スリムを toggle。 */
export function toggleSidebarForm(): void {
	applyForm(sidebarForm() === "full" ? "slim" : "full");
}

/** スリム中の badge click 等「フルに戻って続きを操作する」動線。フル中は no-op。 */
export function expandSidebar(): void {
	if (sidebarForm() === "slim") applyForm("full");
}
