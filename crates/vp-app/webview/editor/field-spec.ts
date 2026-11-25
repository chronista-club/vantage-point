/**
 * Editor field の宣言（pure data）と導出（doc 87 §5 / §8.3）。
 *
 * この module の宣言（`AppFieldSpec`、`:root` に書く field）は 2 か所から来る:
 *
 * - **app-fields.ts の基本の宣言**（`sb.*` / `chat.*` / `shell.center.*` / `shell.resizer.*`）— 配色・文字・名札の高さ。
 *   多くは region に属し、境界（`shell.resizer.*`）のように属さないものもある
 * - **3 つの registry の行が同梱する field**（rail = 左 edge / edge resident = 右 edge / pane kind = center）—
 *   行が `fields` を持てば、collector がそれを Editor に出す。行を足す人は Editor 側を触らない（open-closed）
 *
 * 左右の幅はここの宣言ではない（`:root` に書かず shell-layout の書き口を通る — app-fields.ts の `SHELL_LAYOUT_FIELDS`）。
 *
 * ## 層の分離
 *
 * この module は **型と純関数だけ**（DOM も editor-host の runtime も持たない）。registry の module
 * （`src/sidebar/shell-rails.ts` / `edge-residents.ts` / `app-pane-registry.ts`）は `fields` の型だけを
 * ここから借りる。EditorField（apply / computed の初期値）への組み立ては app-fields.ts。
 */
import type { EditorField, EditorSemantic } from "@chronista-club/creo-ui-editor-host";
import type { Region } from "@chronista-club/creo-ui/shells";
import type { EditorSection } from "./EditorPanel";

export interface FieldConstraints {
	readonly min?: number;
	readonly max?: number;
	readonly step?: number;
	readonly unit?: string;
}

/**
 * registry の行が同梱する Editor field（D-13 の knob に当たる、pure data）。
 *
 * ⚠️ `cssVar` は **`:root` に既定値の宣言がある token** に限る。Editor は値を `:root` の inline style に
 * 書くので、要素の側で同じ変数を宣言していると（`#sidebar-root{--vp-peek-w:…}` など）子孫には届かない。
 * そういう値は field を同梱せず、持ち主の module に書き口を作る（peek / R の幅 = shell-layout.ts）。
 */
export interface BundledFieldSpec {
	/** 行の中での名前。field id は collector が `<registry>.<行 id>.<key>` に組み立てる。 */
	readonly key: string;
	readonly label: string;
	/** Editor の中の見出し（同じ group の field を 1 つの折りたたみに並べる）。 */
	readonly group: string;
	readonly cssVar: string;
	/** stylesheet の既定値と同じ値。number = slider、string = 色（登録時に computed の値で上書きされる）。 */
	readonly initial: number | string;
	readonly constraints?: FieldConstraints;
}

/** VP の app field 1 本の宣言。region 無し = 全体（テーマ・境界）。 */
export interface AppFieldSpec {
	readonly id: string;
	readonly label: string;
	readonly group: string;
	readonly region?: Region;
	readonly cssVar: string;
	readonly initial: number | string;
	readonly constraints?: FieldConstraints;
}

/** collector に渡す registry 1 枚（行は `id` と任意の `fields` だけ読む）。 */
export interface FieldRegistrySource {
	/** field id の頭（`rail` / `resident` / `pane`）。 */
	readonly prefix: string;
	/** この registry の行が住む region。 */
	readonly region: Region;
	readonly entries: readonly { readonly id: string; readonly fields?: readonly BundledFieldSpec[] }[];
}

/** registry の行が同梱した field を、表の順・行の中の宣言順で集める。 */
export function collectBundledFields(sources: readonly FieldRegistrySource[]): AppFieldSpec[] {
	const out: AppFieldSpec[] = [];
	for (const source of sources) {
		for (const entry of source.entries) {
			for (const f of entry.fields ?? []) {
				out.push({
					id: `${source.prefix}.${entry.id}.${f.key}`,
					label: f.label,
					group: f.group,
					region: source.region,
					cssVar: f.cssVar,
					initial: f.initial,
					...(f.constraints ? { constraints: f.constraints } : {}),
				});
			}
		}
	}
	return out;
}

/**
 * region → Editor Mode の semantic（creo-ui editor-mode.md D-2: TOP = global / LEFT = source /
 * RIGHT = tool / BOTTOM = utility）。center と region 無しは global。
 */
export function semanticOfRegion(region: Region | undefined): EditorSemantic {
	switch (region) {
		case "leftEdge":
			return "source";
		case "rightEdge":
			return "tool";
		case "downEdge":
			return "utility";
		default:
			return "global";
	}
}

/**
 * 保存済みテーマの互換のためだけに登録している field（値は読むが、どのタブにも出さない）。
 * 通常の VP 画面に適用先が無いもの — SIDEBAR の旧「本文」と、creo-ui の汎用 XS / S / M / XL・部品の間隔
 * （doc 78 の 2026-10-06 の裁定。Board の本文 = size.l だけは出す）。
 */
const HIDDEN_FIELD_IDS: ReadonlySet<string> = new Set([
	"sb.text.base",
	"typography.size.xs",
	"typography.size.s",
	"typography.size.m",
	"typography.size.xl",
	"layout.gap.sibling",
]);

/**
 * field が Editor のどのタブに住むか。`region` は VP の宣言が持つ region（VP の field でなければ `undefined`）。
 * 左右の edge は SIDEBAR、`chat.*` は CHAT、残り（全体・center・境界、creo-ui の token）は THEME。component の
 * knob は COMPONENT（選択した時だけ出る）。互換のためだけの field は `undefined`（どこにも出さない）。
 *
 * ⚠️ semantic からは決めない — creo-ui の token の自動発見（console の `creoEditor.autoDiscover()`）は semantic が
 * 既定で `tool` なので、semantic で振ると VP の左右の edge に混ざる。semantic は region を外（MCP / Link hub）へ
 * 見せる写しで、タブは VP の data（region）から引く。
 */
export function editorSectionOf(
	field: Pick<EditorField, "id" | "scope">,
	region: Region | undefined,
): Exclude<EditorSection, "MIDI"> | undefined {
	if (field.scope === "component") return "COMPONENT";
	if (HIDDEN_FIELD_IDS.has(field.id)) return undefined;
	if (field.id.startsWith("chat.")) return "CHAT";
	if (region === "leftEdge" || region === "rightEdge") return "SIDEBAR";
	return "THEME";
}

/** 宣言が `:root` に書く変数の集合（テーマを切り替える時に inline style から消す対象）。 */
export function managedVarsOf(specs: readonly AppFieldSpec[]): ReadonlySet<string> {
	return new Set(specs.map((s) => s.cssVar));
}
