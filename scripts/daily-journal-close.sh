#!/usr/bin/env bash
# daily-journal-close.sh
# session 終了時に daily journal に footer (commit 数 / branch 動き / PR 状態) を追記する。
# L1 Steward の "daily summary" を minimal な shell で実装。
#
# Usage:
#   scripts/daily-journal-close.sh              # 今日の日付
#   scripts/daily-journal-close.sh 2026-04-22   # 指定日

set -euo pipefail

DATE="${1:-$(date +%Y-%m-%d)}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${REPO_ROOT}/docs/journals/${DATE}.md"

if [[ ! -e "${OUT}" ]]; then
    echo "⚠️  No journal for ${DATE}: ${OUT}"
    echo "    先に daily-journal-open.sh を実行してください。"
    exit 1
fi

END_TIME="$(date +%H:%M)"

# 日付の開始 / 終了 (UTC ではなく local midnight ベース)
SINCE="${DATE}T00:00"
UNTIL="${DATE}T23:59"

COMMITS_TODAY="$(git -C "${REPO_ROOT}" log --since="${SINCE}" --until="${UNTIL}" --oneline 2>/dev/null | wc -l | tr -d ' ')"
BRANCHES_TOUCHED="$(git -C "${REPO_ROOT}" log --since="${SINCE}" --until="${UNTIL}" --format='%D' --all 2>/dev/null \
    | tr ',' '\n' | grep -v '^$' | grep -v '^HEAD' | sed 's/^ *//' | sort -u | tr '\n' ' ')"
OPEN_PRS_NOW="$(gh pr list --state open --limit 20 --json number,title,mergeable 2>/dev/null \
    | jq -r '.[] | "- #\(.number) \(.title) [\(.mergeable)]"' 2>/dev/null || echo '- (gh pr list failed)')"

cat >> "${OUT}" <<EOF

## Footer (auto, ${END_TIME})

- **Commits today**: ${COMMITS_TODAY}
- **Branches touched**: ${BRANCHES_TOUCHED:-(none)}
- **Open PRs (at close)**:
${OPEN_PRS_NOW}

---

**Session ended**: ${END_TIME}

次のステップ: CC session で以下を実行して creo-memories に永続化:
\`\`\`
remember に現行 Journal の内容を所感込みで save (feedback_dev_journal.md 運用)
\`\`\`
EOF

echo "✅ Appended footer to: ${OUT}"
