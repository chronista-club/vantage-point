/**
 * ShellRail — 左 edge の Rail 列 + peek（doc 87 §2 / §8 slice 1）。creo-ui の `CURail`（0.31.0〜）を
 * VP に繋ぐ薄い wrapper。
 *
 * ## 経緯
 *
 * creo-ui 0.30.0 の `CURail` は VP の左 edge を表せなかった（peek 幅 240px 固定 / slim を表せない /
 * 列に registry の icon しか置けない）ので、slice 1 では同じ DOM 契約の仮実装を置いた（doc 87 §8.1）。
 * 足りなかった口が 0.31.0 に入ったので本物に差し替えた。契約は `ShellRail.dom.test.ts` が固定している。
 *
 * ## この wrapper が VP 側で持つもの
 *
 * - **click → rail id**: `CURail` の `onNavigate` は rail の route を渡す。VP の rail click は「選択中の
 *   rail を押したら peek を畳む」toggle を含むので、route から rail id を引いて `onSelect` に渡し、
 *   遷移そのものは呼び手（`route-store.ts` の `clickRail`）が決める
 * - **peek を全 rail 分 mount したまま**（`keepPeeksMounted`）: rail を行き来しても名簿の AddSub form の
 *   開閉と入力・`n` directive の登録・PAUSED の開閉・一覧の scroll 位置が消えない
 * - **peek の幅** = VP の保存値（`--vp-peek-w`、`shell-layout.ts` が書く）を `peekWidth` に渡す。
 *   `#sidebar-root` は中身（列 + peek）の幅になるので、幅の動き（slim ⇄ full の transition）は
 *   creo-ui の peek の 1 か所だけが持つ
 * - **drag 中**（`body.shell-resizing`、main bundle の `shell-layout.ts` が付ける）は `resizing` で
 *   peek の transition を止める — 止めないと境界の drag に幅が遅れて付いてくる
 */
import { createSignal, onCleanup, onMount, type Component, type JSX } from "solid-js";
import { CURail, type RailDef } from "@chronista-club/creo-ui/shells";

interface ShellRailProps {
	/** app 供給の registry（PL-3 — primitive は rail を hardcode しない）。 */
	registry: readonly RailDef[];
	/** 今の pseudo-route（PL-4: 選択の唯一の SSOT）。 */
	pathname: string;
	/** peek を開いているか（full = true / slim = false）。 */
	peekOpen: boolean;
	/** icon click。選択を変えるのは呼び手（route の持ち主）— ここは rail id を渡すだけ。 */
	onSelect: (railId: string) => void;
	/** 列の中央 slot（slim の repo badge 列）。 */
	columnCenter?: JSX.Element;
	/** 列の下端 slot（下起点の rail の直上 — slim の daemon dot）。 */
	columnBottom?: JSX.Element;
	/** peek の下端 slot（どの rail を選んでも同じ位置に残る帯 — machine 帯）。 */
	peekFooter?: JSX.Element;
}

/**
 * 境界を drag している間か。drag の状態は別 bundle（main bundle の `shell-layout.ts`）が持ち、
 * body の class（`shell-resizing`）でしか見えないので、class の変化を見張って signal にする。
 */
function useShellResizing(): () => boolean {
	const [resizing, setResizing] = createSignal(false);
	onMount(() => {
		const sync = () => setResizing(document.body.classList.contains("shell-resizing"));
		sync();
		const observer = new MutationObserver(sync);
		observer.observe(document.body, { attributes: true, attributeFilter: ["class"] });
		onCleanup(() => observer.disconnect());
	});
	return resizing;
}

export const ShellRail: Component<ShellRailProps> = (props) => {
	const resizing = useShellResizing();
	const railIdOf = (route: string) => props.registry.find((r) => r.route === route)?.id;
	return (
		<CURail
			registry={props.registry}
			pathname={props.pathname}
			onNavigate={(route) => {
				const id = railIdOf(route);
				if (id) props.onSelect(id);
			}}
			peekOpen={props.peekOpen}
			keepPeeksMounted
			peekWidth="var(--vp-peek-w, 280px)"
			resizing={resizing()}
			columnCenter={props.columnCenter}
			columnBottom={props.columnBottom}
			peekFooter={props.peekFooter}
		/>
	);
};

/**
 * Rail の VP 上書き（`SHELL_CSS` に連結される。creo-ui の shells の CSS は `sidebar.tsx` が先に入れる）。
 *
 * creo-ui の class は CSS module の hash 名なので、**属性セレクタ**（`[data-region]` /
 * `nav[aria-label="Rail"]` / `button[data-rail]` / `[data-expanded]`）で当て、`#sidebar-root` で
 * 詳細度を上げて上書きする。色は Light Grid（`--lg-*` / `--sb-*`）。
 */
export const SHELL_RAIL_CSS = `
/* .vp-sidebar-shell（flex 列）の中で残りの高さを埋める（上にエラー帯が出ても溢れない）。
   creo-ui の既定は height:100% で、エラー帯の分だけ下に溢れる。 */
#sidebar-root [data-region="leftEdge"]{height:auto;flex:1 1 0;min-height:0;}
/* 列 = 起点の帯。幅 48px は creo-ui の既定のまま（border を含めて 48px にする）。 */
#sidebar-root nav[aria-label="Rail"]{box-sizing:border-box;padding:8px 0;background:transparent;
  border-right-color:var(--lg-hairline,#12222b);}
#sidebar-root nav[aria-label="Rail"] > div{gap:6px;}
#sidebar-root nav[aria-label="Rail"] > .anchor-center{padding:4px 0;}
#sidebar-root button[data-rail]{position:relative;width:36px;height:36px;flex:none;
  border:1px solid transparent;border-radius:8px;background:transparent;color:var(--lg-mute,#5C7A85);
  transition:background .12s ease,color .12s ease,border-color .12s ease;}
#sidebar-root button[data-rail]:hover{color:var(--lg-hot,#EAFBFF);background:#ffffff08;}
/* 選択中の rail は選択行（横タブ）と同じ面の色で示す — 光らせない（光 = 注意は needs-you の専有）。 */
#sidebar-root button[data-rail][aria-current="page"]{color:var(--lg-hot,#EAFBFF);
  background:var(--sb-selection-bg);border-color:var(--sb-selection-border);}
#sidebar-root button[data-rail]:focus-visible{outline:1px solid var(--sb-selection-border);outline-offset:1px;}
/* rail icon の用事 dot（Lanes = input 待ちの lane が在る）— slim badge の awaiting と同じ語彙。 */
.vp-rail-flag{position:absolute;top:3px;right:3px;width:7px;height:7px;border-radius:50%;
  background:var(--sb-conn-hitl,#FF4A2D);}
/* peek の面は Light Grid の地（void）のまま。右の境界線は .vp-sidebar-shell の inset が引くので、
   creo-ui の border は外す（二重線にしない）。 */
#sidebar-root [data-region="leftEdge"] > [data-expanded]{background:transparent;}
#sidebar-root [data-region="leftEdge"] > [data-expanded="true"]{border-right:none;}
`;
