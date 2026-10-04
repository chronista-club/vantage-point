/** Instance-local session.json payload. Unknown versions / malformed lanes are ignored. */
import type { Layout } from "@chronista-club/creo-ui-layout";

export type PaneStowState = {
	version: 1;
	layout: Layout;
	shares: Record<string, number>;
};

/** Read only known pane ids and finite weights; never retain aliases to the input. */
export function readPaneStowState(value: unknown): PaneStowState | null {
	if (!value || typeof value !== "object") return null;
	const s = value as PaneStowState;
	if (
		s.version !== 1 || !Array.isArray(s.layout?.structure?.columns) ||
		!s.layout.attention || !s.shares || typeof s.shares !== "object"
	) return null;
	const ids: string[] = [];
	for (const c of s.layout.structure.columns) {
		if (!c || !Array.isArray(c.panes) || !c.panes.length) return null;
		for (const id of c.panes) {
			if (
				typeof id !== "string" ||
				!/^(chat-session-[1-9]\d*|term-session-[1-9]\d*|lane-board|lane-code)$/.test(id) ||
				ids.includes(id)
			) return null;
			const weight = s.layout.attention[id];
			if (typeof weight !== "number" || !Number.isFinite(weight) || weight < 0) return null;
			ids.push(id);
		}
	}
	for (const [id, share] of Object.entries(s.shares)) {
		if (
			!ids.includes(id) || s.layout.attention[id] !== 0 ||
			typeof share !== "number" || !Number.isFinite(share) || share <= 0
		) return null;
	}
	return {
		version: 1,
		layout: {
			structure: { columns: s.layout.structure.columns.map(c => ({ panes: [...c.panes] })) },
			attention: Object.fromEntries(ids.map(id => [id, s.layout.attention[id]])),
		},
		shares: { ...s.shares },
	};
}
