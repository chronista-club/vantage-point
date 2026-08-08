/**
 * chat 入力の slash command 補完 — **判断だけ**（DOM も Solid も触らない）。
 *
 * ## 何を信じるか
 *
 * 候補の源は `system/init` の `slash_commands[]` **だけ**。公式（Agent SDK / slash-commands）は
 * こう書いている:
 *
 * > Only commands that work without an interactive terminal are dispatchable through the SDK;
 * > the `system/init` message lists the ones available in your session.
 *
 * ⚠️ つまり **CLI 側で「この経路で打てるもの」に絞り込み済み**。VP が doc の表を写して
 * 除外リストを持ってはいけない（外部 contract の二重管理 = 向こうが変わった日に無音でずれる）。
 *
 * ⚠️ **例外が 1 つある**: `/help` `/login` のような terminal 専用の組み込みは「名前は予約
 * されているが実行できない」状態で載りうる（skills doc に明記）。実測でも `/help` は
 * 「isn't available in this environment」で弾かれた。**弾かれても壊れない**（エラーではなく
 * 素っ気ない応答が返るだけ）ので、学習も除外もせず素直に出す。
 *
 * ## 送り方は普通のテキストと同じ
 *
 * > Send slash commands by including them in your prompt string, just like regular text.
 *
 * なので送信側（`conversation:submit`）に手を入れる必要は無い。要るのは候補を出す UI だけ。
 */

/**
 * いま slash 補完を出すべきか。出すなら**絞り込みの語**を返す（`/` は含まない）。
 *
 * ⚠️ **行頭でしか効かない** — 公式に "A command is only recognized at the start of your
 * message." とある。文中の `/`（path や URL）で palette を出すと、ただの邪魔になる。
 *
 * ⚠️ 空白が来たら閉じる。そこから先は**引数**（`/fix-issue 123 high` の `$0` `$1`）で、
 * コマンド名の絞り込みではない。
 */
export function slashQuery(text: string): string | null {
	if (!text.startsWith("/")) return null;
	const rest = text.slice(1);
	// 改行も空白扱い（複数行の 1 行目だけがコマンド行）。
	if (/\s/.test(rest)) return null;
	return rest;
}

/**
 * 候補を絞る。近い順に並べる。
 *
 * ⚠️ `plugin:skill` 形式（`chronista-style:codeflow`）が混じるので、**`:` の後ろ**でも
 * 前方一致を見る。user は `codeflow` と打つほうが自然で、plugin 名を覚えていない。
 */
export function filterSlashCommands(
	all: readonly string[],
	query: string,
): string[] {
	if (query === "") return [...all];
	const q = query.toLowerCase();
	// 小さいほど近い。-1 = 不一致。
	const rank = (name: string): number => {
		const n = name.toLowerCase();
		const at = n.indexOf(":");
		const tail = at >= 0 ? n.slice(at + 1) : n;
		if (n.startsWith(q)) return 0;
		if (tail.startsWith(q)) return 1;
		if (n.includes(q)) return 2;
		return -1;
	};
	return (
		all
			.map((name) => ({ name, r: rank(name) }))
			.filter((x) => x.r >= 0)
			// ⚠️ 同順位は**短い順**。組み込みや素の skill は短く、`plugin:skill` は長いので、
			// 辞書順だけだと `c` の 1 打で `chronista-style:codeflow` が `clear` を追い越す。
			.sort(
				(a, b) =>
					a.r - b.r ||
					a.name.length - b.name.length ||
					a.name.localeCompare(b.name),
			)
			.map((x) => x.name)
	);
}

/** 候補の中で選択位置を動かす。端は**巻く**（一覧は短く、行き止まりを作らない）。 */
export function moveSelection(at: number, dir: -1 | 1, len: number): number {
	if (len <= 0) return 0;
	return (at + dir + len) % len;
}

/**
 * 候補を確定したときの入力欄の中身。
 *
 * ⚠️ **末尾に空白を足す**。足さないと次の打鍵がコマンド名の続きとして絞り込みに食われ、
 * 引数を打ち始められない（`slashQuery` が空白で閉じる設計と対になっている）。
 */
export function applyCompletion(name: string): string {
	return `/${name} `;
}
