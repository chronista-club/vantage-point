#!/usr/bin/env bash
# daily-journal-open.sh
# 朝 session 開始時に docs/journals/YYYY-MM-DD.md を template から生成する。
# L1 Steward の "Daily Journal" 手動運用を automation に寄せた最小実装。
#
# Usage:
#   scripts/daily-journal-open.sh              # 今日の日付
#   scripts/daily-journal-open.sh 2026-04-22   # 指定日

set -euo pipefail

DATE="${1:-$(date +%Y-%m-%d)}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${REPO_ROOT}/docs/journals/${DATE}.md"

if [[ -e "${OUT}" ]]; then
    echo "⚠️  Already exists: ${OUT}"
    echo "    既存のファイルを上書きしない。追記するなら手動で編集。"
    exit 0
fi

START_TIME="$(date +%H:%M)"
BRANCH="$(git -C "${REPO_ROOT}" branch --show-current 2>/dev/null || echo '(detached)')"
OPEN_PRS="$(gh pr list --state open --limit 20 --json number,title 2>/dev/null \
    | jq -r '.[] | "- #\(.number) \(.title)"' 2>/dev/null || echo '- (gh pr list failed)')"

mkdir -p "$(dirname "${OUT}")"

cat > "${OUT}" <<EOF
# Daily Journal — ${DATE}

**Started**: ${START_TIME}
**Project**: vantage-point
**Branch**: ${BRANCH}

---

## Open PRs (at open)
${OPEN_PRS}

---

## Today's goals
- [ ]

## Progress
-

## Findings / Insights
-

## Running workers (if any)
- (name) — (task)

---

**Session end**: (footer を daily-journal-close.sh で追記)
EOF

echo "✅ Created: ${OUT}"
echo ""
echo "次のステップ:"
echo "  - このファイルを編集して goals / progress を記録"
echo "  - 日暮れに \`scripts/daily-journal-close.sh\` で footer 追加"
echo "  - 明日の session 開始時に CC で creo-memories にも save"
