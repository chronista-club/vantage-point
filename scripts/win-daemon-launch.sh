#!/usr/bin/env bash
# VP-93 Step 2a: win:daemon task — daemon 起動後に vp-app を daemon mode で launch
# `scripts/win-daemon-up.sh` が先に走っていて /tmp/vp-world-url が書かれている前提
set -euo pipefail

if [ ! -s /tmp/vp-world-url ]; then
  echo "❌ /tmp/vp-world-url が空 — daemon 未起動?" >&2
  echo "   先に scripts/win-daemon-up.sh を実行してください" >&2
  exit 1
fi

VP_WORLD_URL=$(cat /tmp/vp-world-url)
export VP_TERMINAL_MODE=daemon
export VP_WORLD_URL
export VP_DAEMON_SHELL=bash

# WSL daemon に繋ぐので Windows 側 vp.exe の cross-build はスキップ (~3分節約)
export VP_SKIP_DAEMON_BUILD=1

echo "▶ VP_TERMINAL_MODE=$VP_TERMINAL_MODE VP_WORLD_URL=$VP_WORLD_URL VP_SKIP_DAEMON_BUILD=$VP_SKIP_DAEMON_BUILD"

# 既存 win task を call — vp-app を Windows cross-build + launch
exec mise run win
