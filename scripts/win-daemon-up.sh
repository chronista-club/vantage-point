#!/usr/bin/env bash
# VP-93 Step 2a: WSL 側で TheWorld daemon を起動し :32000 が ready になるまで待つ
# vp-app 側の launch は `mise run win` が行う（env 経由で VP_TERMINAL_MODE=daemon を伝達）
# 副作用: /tmp/vp-world-url に `http://<wsl-ip>:32000` を書き出し、win task が読む
set -euo pipefail

# WSL IP を特定 (Windows 側 vp-app から到達可能な vEthernet IP)
#   `ip route get` で外部宛ての送信元 IP を引き、default route が通る NIC の
#   グローバル IP (eth0 など) を取得する。loopback (10.255.x.y) は除外。
WSL_IP=$(ip route get 1.1.1.1 2>/dev/null | awk '{for(i=1;i<=NF;i++) if ($i=="src") print $(i+1); exit}' || true)
if [ -z "${WSL_IP:-}" ]; then
  # fallback: default route の dev の IP
  IFACE=$(ip -4 route show default 2>/dev/null | awk '{for(i=1;i<=NF;i++) if ($i=="dev") print $(i+1); exit}' || true)
  if [ -n "${IFACE:-}" ]; then
    WSL_IP=$(ip -4 -o addr show dev "$IFACE" 2>/dev/null | awk '{print $4}' | cut -d/ -f1 | head -1 || true)
  fi
fi
if [ -z "${WSL_IP:-}" ]; then
  echo "❌ WSL IP を検出できません (ip コマンド不在?)" >&2
  exit 1
fi

WORLD_URL="http://${WSL_IP}:32000"
echo "$WORLD_URL" > /tmp/vp-world-url
echo "🔗 WSL IP: $WSL_IP  (VP_WORLD_URL=$WORLD_URL)"

# 既存 daemon 停止 (port 競合回避)
pkill -f 'vantage-point.*world' 2>/dev/null || true
pkill -f 'vp world' 2>/dev/null || true
sleep 0.3

# daemon を background で起動 (log は /tmp/vp-world.log)
# 注: SurrealDB が未インストールなら ~10s retry してから DB なしで続行するため
# daemon が ready になるまで 15s 程度待つ。
echo "🌍 TheWorld daemon を WSL で起動 (log: /tmp/vp-world.log, up まで ~10-15s)..."
setsid vp world </dev/null >/tmp/vp-world.log 2>&1 &
WORLD_PID=$!
disown "$WORLD_PID" 2>/dev/null || true

# :32000 が up するまで待つ (max 20s)
for i in $(seq 1 40); do
  if curl -sfm 1 http://127.0.0.1:32000/api/health >/dev/null 2>&1; then
    echo "✅ TheWorld daemon up (pid=$WORLD_PID, $(( i * 500 ))ms 以内)"
    exit 0
  fi
  sleep 0.5
done

echo "❌ TheWorld daemon 起動失敗 (20s timeout):" >&2
tail -30 /tmp/vp-world.log >&2 || true
exit 1
