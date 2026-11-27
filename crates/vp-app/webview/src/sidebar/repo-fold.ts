/**
 * lead 行（RepoAccordion）の折りたたみが「何を畳んでいるか」の計算（mem_1ChNtdQKHrSB78Ppek2etg）。
 *
 * 畳む場所 = `.vp-proj-content`。そこに入る物の数で、開閉 caret の濃さ（0 = 控えめ）と、
 * 畳んでいる間に lane 数の chip が語る内訳を決める。DOM も store も読まない純関数。
 */

/** `.vp-proj-content` に入っている物の内訳。 */
export interface FoldContent {
	/** hint（repo stopped / loading lanes など）を出している。この間は lane 行を描かない */
	hint: boolean;
	/** sub lane の本数 */
	subs: number;
	/** 追加セッション（root / sub の非 root session）の数 */
	sessions: number;
	/** lane 作成フォーム（AddSub）が開いている */
	form: boolean;
}

/** 畳むと隠れる物の数。0 = 畳む中身が無い（lead 1 本だけ）。 */
export function foldItemCount(c: FoldContent): number {
	// hint の間は lane 行を描かない（RepoAccordion の Show fallback）ので、hint の 1 行だけが中身
	if (c.hint) return 1;
	return c.subs + c.sessions + (c.form ? 1 : 0);
}

/**
 * 畳んでいる間に lane 数の chip が語る内訳。隠れている lane / セッション / フォームが
 * 無ければ null（hint だけの時も null — 畳んで隠れる lane は無い）。
 */
export function foldedSummary(c: FoldContent): string | null {
	if (c.hint) return null;
	const parts: string[] = [];
	if (c.subs > 0) parts.push(`sub lane ${c.subs} 本`);
	if (c.sessions > 0) parts.push(`追加セッション ${c.sessions} 個`);
	if (c.form) parts.push("lane 作成フォーム");
	return parts.length > 0 ? `${parts.join("・")}を畳んでいます` : null;
}
