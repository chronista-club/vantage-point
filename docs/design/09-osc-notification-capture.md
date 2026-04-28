# 09: OSC notification capture pipeline (vp-app)

> **Status**: in-progress (S1 done @ PR #221, S2 / S3 active in this worker lane)
> **Worker lane**: `vantage-point-vp-osc-pipeline`
> **Branch**: `mako/osc-pipeline-s2`
> **Related memory** (main session local): `~/.claude/projects/-Users-makoto-repos-vantage-point/memory/osc_notification_capture.md`

---

## 0. 背景・経緯

vp-app PR #221 (Slice 1 capture-only) で **cc が OSC 99 を natively emit している** ことを dogfood で発見。 観測 payload (vp-app の log より):

```
[osc99:vantage-point/lead] i=211:d=0:p=title;Claude Code
[osc99:vantage-point/lead] i=211:p=body;Claude is waiting for your input
[osc99:vantage-point/lead] i=211:d=1:a=focus;
```

xterm.js layer に OSC 9 / 99 / 777 handler を 3 つとも置いて capture (PR #221 で main merge 済、 commit `f2b6e3d`)。 これを sidebar status UI に活かす。

## 1. このスライス (S2 + S3) の責務

| slice | 内容 |
|------|------|
| **S2** | id-based accumulator + `d=1` commit + IPC `lane:notify` push + Rust 側 `LaneNotification` store |
| **S3** | sidebar Lane row tint UI + click clear + tooltip (title/body) |

S4 以降 (DistributedNotification fan-out / VP self-emit / Hub federation) は backlog。

## 2. Architecture (S2 + S3)

```
PTY (cc emit OSC 99 multi-chunk)
   ↓
ws_terminal → xterm.js
   ↓ (registerOscHandler 99/9/777、 PR #221 で実装済)
JS accumulator (per-lane, per-id chunk store)
   ↓ d=1 で commit
window.ipc.postMessage({t:'lane:notify', address, id, title, body, action, urgency, ts})
   ↓
Rust app.rs IPC dispatch
   ↓
SidebarState.notifications_by_lane に insert (id replace)
   ↓
sidebar push (既存 path)
   ↓
sidebar JS が Lane row に vp-lane-pinged class 付与、 CSS で tint
   ↓
user click Lane row → lane:select IPC
   ↓
Rust が notifications_by_lane から該当 entry remove (read = clear)
```

## 3. S2 詳細

### 3.1 JS accumulator (main_area.rs 内 xterm.js layer)

```javascript
// per Lane / per id の chunk 累積。 d=1 で commit、 timeout で stale flush。
const pendingByLane = new Map(); // address → Map<id, PendingNotif>

function ensurePending(address) {
  if (!pendingByLane.has(address)) pendingByLane.set(address, new Map());
  return pendingByLane.get(address);
}

function parseOsc99Metadata(data) {
  // 'i=211:d=0:p=title;Claude Code' → {meta: {i:'211', d:'0', p:'title'}, payload: 'Claude Code'}
  const semi = data.indexOf(';');
  const metaStr = semi >= 0 ? data.slice(0, semi) : data;
  const payload = semi >= 0 ? data.slice(semi + 1) : '';
  const meta = {};
  for (const kv of metaStr.split(':')) {
    const eq = kv.indexOf('=');
    if (eq > 0) meta[kv.slice(0, eq)] = kv.slice(eq + 1);
  }
  return { meta, payload };
}

const CHUNK_TIMEOUT_MS = 5000;

term.parser.registerOscHandler(99, (data) => {
  const { meta, payload } = parseOsc99Metadata(String(data || ''));
  const id = meta.i;
  if (!id) return true; // i 必須

  // p=close: dismiss
  if (meta.p === 'close') {
    pendingByLane.get(address)?.delete(id);
    window.ipc.postMessage(JSON.stringify({t:'lane:notify:close', address, id}));
    return true;
  }

  const pending = ensurePending(address);
  const entry = pending.get(id) || {
    title: '', body: '', action: '', urgency: '',
    ts_first: Date.now(), ts_last: Date.now()
  };
  entry.ts_last = Date.now();

  // payload 種別ごとに concat (multi-chunk: 同 p=type で複数 chunk あり得る)
  const ptype = meta.p || 'title'; // default は title
  if (ptype === 'title') entry.title += payload;
  else if (ptype === 'body') entry.body += payload;
  // icon / buttons は S2 では skip

  if (meta.a) entry.action = meta.a;
  if (meta.u) entry.urgency = meta.u;

  pending.set(id, entry);

  // d=1 で commit (default は 1)
  const done = (meta.d ?? '1') === '1';
  if (done) {
    pending.delete(id);
    window.ipc.postMessage(JSON.stringify({
      t: 'lane:notify',
      address, id,
      title: entry.title, body: entry.body,
      action: entry.action, urgency: entry.urgency,
      ts: entry.ts_last
    }));
  } else {
    // 5s で stale flush (orphan chunk 防止)
    setTimeout(() => {
      const cur = pendingByLane.get(address)?.get(id);
      if (cur && cur.ts_last <= entry.ts_last && cur.ts_first === entry.ts_first) {
        pendingByLane.get(address)?.delete(id);
        // partial でも commit するか drop するかは要判断。 当面 drop。
        console.warn('[osc99] chunk timeout, drop id=' + id + ' lane=' + address);
      }
    }, CHUNK_TIMEOUT_MS);
  }
  return true;
});

// OSC 9 / 777 は当面 dispatch 対象にしない (cc は 99 を vp-app に出してる、
// 9/777 は別 emitter 用)。 capture log は維持して将来用に観察。
```

### 3.2 Rust 側 (pane.rs / app.rs / terminal.rs)

#### pane.rs

```rust
/// Lane 1 件の notification 状態 (latest 1 件保持、 id replace)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneNotification {
    pub id: String,
    pub title: String,
    pub body: String,
    pub action: Option<String>,   // "focus" 等
    pub urgency: Option<String>,  // "0" / "1" / "2"
    pub ts: i64, // millis
}

// SidebarState に追加
pub notifications_by_lane: HashMap<String, LaneNotification>,
```

`#[serde(default, skip_serializing_if = "HashMap::is_empty")]` で empty 時はワイヤから省略。

#### app.rs IPC dispatch (sidebar IPC handler 内)

```rust
"lane:notify" => {
    let id = parsed.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if id.is_empty() { return out; }
    let title = parsed.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let body  = parsed.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let action = parsed.get("action").and_then(|v| v.as_str()).map(String::from);
    let urgency = parsed.get("urgency").and_then(|v| v.as_str()).map(String::from);
    let ts = parsed.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);

    let notif = LaneNotification { id, title, body, action, urgency, ts };
    state.notifications_by_lane.insert(address.to_string(), notif);
    out.changed = true;
}
"lane:notify:close" => {
    state.notifications_by_lane.remove(address);
    out.changed = true;
}
```

#### lane:select 時の clear

既存 `lane:select` handler 内で `state.notifications_by_lane.remove(&address);` を追加。 read = clear semantics。

### 3.3 Edge case 対応

| 問題 | 対策 |
|------|------|
| **scrollback replay で再 fire** | 既存 `subscribe_with_scrollback` (ws_terminal.rs:141) で initial bytes が再 emit されると OSC handler も再 fire する。 → JS 側 accumulator に **replay session marker** を持たせ、 connect 後 N ms (例: 1500ms) は IPC を suppress (=過去 chunk として扱う)。 |
| **multi-instance** | secondary vp-app で同 PTY attach、 同 OSC が両方の xterm.js で fire → 両方 IPC。 → Rust 側で `(lane, id, ts)` で idempotent check (重複 insert は ignore)。 |
| **`e=1` Base64 payload** | S2 では skip、 unknown ptype として無視。 future。 |
| **chunk timeout** | 5s (defined in S2 JS code)。 partial は drop。 |
| **forward compat** | 未知 metadata key は無視、 必須は `i` のみ |

## 4. S3 詳細 (sidebar UI)

### 4.1 sidebar JS (main_area.rs / app.rs の sidebar section)

```javascript
// renderProjectAccordion / Lane row 描画時、 notifications_by_lane[address] あり?
const notif = (state.notifications_by_lane || {})[laneAddress];
if (notif) {
  laneRow.classList.add('vp-lane-pinged');
  laneRow.title = notif.title + (notif.body ? '\n' + notif.body : '');
  // urgency に応じた modifier (cc は今 emit してないので一旦 default)
  if (notif.urgency === '2') laneRow.classList.add('vp-lane-pinged--critical');
  else if (notif.urgency === '0') laneRow.classList.add('vp-lane-pinged--low');
}
```

### 4.2 CSS (xterm.css 隣接 / sidebar 既存 style block 内)

```css
/* 周辺視野で気づく程度の subtle tint */
.vp-lane-pinged {
  border-left: 3px solid var(--accent-cyan, #5ec5ff);
  background-image: linear-gradient(
    to right,
    color-mix(in oklch, var(--accent-cyan) 8%, transparent) 0%,
    transparent 40%
  );
}
.vp-lane-pinged--critical { border-left-color: var(--accent-coral, #ff6b6b); }
.vp-lane-pinged--low { border-left-color: var(--text-muted, #888); }
```

色 token は creo-ui 既存 OKLCH palette に合わせる。

### 4.3 Click semantics

- Lane row click は既存 `lane:select` IPC を発火 → Rust 側で `notifications_by_lane.remove(address)` → 次 sidebar push で `vp-lane-pinged` class が外れる
- 自動 clear なので user は明示的な dismiss 不要

## 5. 実装ファイル touch list

| file | 変更 |
|------|------|
| `crates/vp-app/src/main_area.rs` | JS accumulator 追加、 既存 OSC handler を refactor、 sidebar JS の Lane row rendering に tint class 追加 |
| `crates/vp-app/src/pane.rs` | `LaneNotification` struct + `SidebarState.notifications_by_lane` field |
| `crates/vp-app/src/app.rs` | `lane:notify` / `lane:notify:close` IPC handler、 `lane:select` 時 clear |
| `crates/vp-app/src/terminal.rs` (該当なら) | IPC dispatch に新 type 追加 (sidebar IPC に集約なら不要) |

## 6. test plan

### unit / integration

- [ ] `cargo check -p vp-app` pass
- [ ] `cargo clippy --workspace --all-targets` 警告なし
- [ ] `cargo fmt --all -- --check` pass
- [ ] (option) JS accumulator の parse fn を Rust 側にも mirror して unit test (forward-compat regression)

### dogfood

- [ ] cc の input 待ち emit (`i=N:d=0:p=title;Claude Code` → `p=body;...` → `d=1:a=focus;`) で sidebar Lane row tint
- [ ] tooltip に title + body 表示
- [ ] Lane row click で tint 消える
- [ ] vp-app 再起動 / WS reconnect で過去 OSC が re-fire しても sidebar tint は **flash しない** (replay suppression が効く)
- [ ] secondary vp-app instance を上げて同 PTY attach、 重複 IPC で sidebar が flicker しない

### vocabulary catalog (継続観察)

`docs/design/09-osc-notification-capture.md` の最下部 table を dogfood で更新:

| trigger | observed payload |
|---------|------------------|
| input 待ち | `i=N:d=0:p=title;Claude Code` → `p=body;Claude is waiting for your input` → `d=1:a=focus;` (PR #221 dogfood で観測) |
| turn 完了 (Stop) | (TBD) |
| permission prompt | (TBD) |
| error | (TBD) |
| subagent invoke | (TBD) |

## 7. 後続 (S4+ backlog)

- OSC 99 → DistributedNotification fan-out (vp-app 非 focus 時の OS notification)
- VP 自身が OSC 99 emit (worker spawn / build done 等を任意 terminal で観察可能に)
- Hub federation (Lane notification を chronista hub へ push、 team activity feed)
- type 別 color mapping (cc が `t=` / `u=` を emit するようになったら、 または body text 推論で)

## 8. Rollback path

S2/S3 で問題出たら:
- 個別 commit を revert
- もしくは PR を revert

S1 (PR #221) は capture-only で副作用無し。 S2/S3 が問題でも S1 は残せる。

## 9. References

- [Kitty Desktop Notifications spec](https://sw.kovidgoyal.net/kitty/desktop-notifications/)
- [xterm.js IParser](https://xtermjs.org/docs/api/terminal/interfaces/iparser/)
- [Claude Code terminal config (official)](https://code.claude.com/docs/en/terminal-config)
- PR #221 (S1) — main merged @ `f2b6e3d`
- main session memory: `~/.claude/projects/-Users-makoto-repos-vantage-point/memory/osc_notification_capture.md`
