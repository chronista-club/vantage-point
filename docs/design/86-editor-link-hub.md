# 86. Editor Link hub — daemon が面を受け、AI と机の機材に写す

> **Status**: Implemented（口 / registry / 調停 / MCP 全面切替 / vp-app を面に）。機材への写し（§4）は未着手。実機確認待ち
> **Related**: creo-ui `docs/design/editor-link.md`（wire の SSOT、D-16）/ doc 48 D-C（AI の口 = MCP、daemon が唯一の listener）/ doc 45 §2（HTTP は鈍い外殻）/ doc 49 LE-19（機材 → 面の対応は consumer 供給）/ doc 84（ROTO MIDI setup）
> **Task**: creo-memories thread `editor-link`（`mem_1CgXANG7gC7dcDz5PHcwcK`）、VP todo `mem_1CgX1nFWLctfgFavRJZtxa`
> **対象**: `crates/vantage-point/src/daemon/link.rs`（hub）/ `repo/http/link.rs`（口）/ `daemon/server.rs`（"link" channel）/ `mcp/editor.rs` / `crates/vp-app/src/webview/editor_bridge.rs`（`link_init_script`）/ `crates/vp-app/webview/editor/link-config.ts`

## 1. 何を決めたか

Editor Mode の selection が机の上のノブになる（mako 裁定 2026-10-30、thread `editor-link`）。
その配線を **VP daemon が hub** になって受ける。creo-ui の editor-host は Editor protocol
（fields / selection / values / mode）をそのまま WebSocket で流すだけで MIDI を知らず、
「何に写すか」は全部こちらが持つ。

- **URL は固定** `ws://127.0.0.1:32000/link`。面の側は設定なしで繋がる。token は当面なし
  （Link に乗るのは Editor の操作だけ。lease もファイル書き込みも乗らない）
- **面（surface）** = editor-host を載せた 1 画面。vp-app の webview も、Chrome で開いた creo-ui
  site（dev で `link` 有効）も、同じ契約で同じ口に来る。vp-app は特別扱いしない
- **AI の口は MCP `editor_*` のまま**（doc 48 D-C）。届け先が「vp-app の webview」から「active な面」
  （または `surface` で指名した面）に広がった。旧経路（repo "process" channel → `EditorCommand`
  broadcast → vp-app が JS 評価）は面が 1 つという前提の上に在ったので撤去し、`layout_*` だけが残る
- **1 本の PR** で口・registry・MCP 切替・vp-app の面化まで入れた。creo-ui 側の release（editor-host
  0.10.0）を先に切ったので、「vp-app が面になるまで旧経路に fallback」という中間状態を作らなかった

## 2. 構成

```
Chrome: creo-ui site (dev, link 有効)  ──┐
vp-app webview (config.link)            ──┤ ws://127.0.0.1:32000/link   ← HTTP 外殻、upgrade 前に 403
                                          ▼
                 daemon  repo/http/link.rs（口）── hello → daemon/link.rs LinkHub（registry / 調停 / clamp）
                                                                    ▲ "link" channel（Unison）
                            MCP editor_fields / editor_values / editor_set ─┘   （lane の Claude から）
                                                                    ▲
                            /api/health services.link ──────────────┘（面の数 / active の名札）
```

### 2.1 口 — `/link`（`repo/http/link.rs`）

daemon の HTTP listener に route を 1 つ足す。**新しい listener は作らない**（doc 48 D-C）。
doc 45 §2 が HTTP に残した `/api/health` `/api/shutdown` は「鈍い外殻」だが、`/link` が HTTP に乗る
理由は別で、**consumer がブラウザ = Unison QUIC を話せない**こと。この 2 つ目の例外は口を 1 つに保つ。

守りは **upgrade の前**に判定して 403 で断る（`daemon::link::admit`、純 calculation）:

| 条件 | 判定 | 理由 |
|---|---|---|
| peer が loopback 以外 | 403 | listener が dual-stack で LAN からも届く。`127/8` `::1` と v4-mapped `::ffff:127.x` の 3 形を見る（最後を忘れると Chrome からの接続が全部落ちる） |
| `Origin` 無し | 通す | ブラウザは必ず付け、非ブラウザは付けない。非ブラウザは loopback 判定だけで `vp` CLI と同じ信頼 |
| `Origin` が `vp-asset://app` | 通す | vp-app webview（`boot.rs` の統合 origin fix で与えた実 origin） |
| `Origin` が `http(s)://{localhost,127.0.0.1,[::1]}[:port]` | 通す | creo-ui site の dev server 等。port は問わない |
| それ以外（`null` を含む） | 403 | cross-site WebSocket hijacking の防ぎ。`null`（opaque origin）は sandbox iframe 等から誰でも名乗れるので allowlist に入れない |

peer address は `serve_with_cleanup` が `into_make_service_with_connect_info::<SocketAddr>()` で
全 route に載せる。拒否は warn log に理由を残す（沈黙だと「繋ぎに来ていない」と「弾いている」が
区別できない）。

### 2.2 hub — `LinkHub`（`daemon/link.rs`）

`DaemonState::link` として daemon に 1 つ。面ごとに `hello` の名札（`app` / `surface`）、mode、
fields 全量、selection、values を cache する。

- **接続は `hello` が先頭**（10s 以内に来なければ閉じる）。`v` が `LINK_VERSION`（1）と違えば 1002 で閉じる。
  版違いは upgrade の前には分からない（hello は upgrade 後）ので、ここだけは接続後の拒否になる
- **同じ `surface` id の再接続は置き換え**（tab の reload、vp-app の再起動）。接続ごとの `conn_id` で
  区別し、古い接続の後始末が新しい登録を消さない。古い socket は hub 側が送り口を落とすことで閉じる
- **cache の更新**: 面からの `set` / `values` / `fields` / `selection` / `mode` を写す。hub から出した
  `set` は面が echo しない契約なので、**hub が自分で cache に書く**

### 2.3 調停 — active な面

MCP が `surface` を省略したときの届け先。順位は **(今 Editor ON か, 最後に ON になった時刻, 繋いだ時刻)**
の降順。つまり「最後に Editor ON した面」、どれも ON していなければ「最後に繋いだ面」。
面が離れれば次へ。`surface` の指名は面の id（厳密一致）か名札（`app`、大文字小文字無視。同名が複数
なら同じ順位で選ぶ）。Touch 中の切替保留（手が主）は機材が入る §4 で足す。

### 2.4 読みと書き

- **`editor_set`**: number field は値を数に直し（MCP client が union 型の引数を `"99"` のように文字列で
  送る実例が実機で出た。そのまま流すと doc 48 の「`"1.75"` が焼き付く」の再演）、`constraints.min / max`
  に収めてから送る。color / select / bool はそのまま。知らない id は面が黙って捨てる契約なので **hub が先に言う**（error）。応答は
  `{ ok, surface, id, value（適用した値）, clamped }`
- **`editor_fields` / `editor_values`**: `get` を面に送り、面が `values` を返した瞬間の cache を返す
  （= authoritative、3s で timeout）。面は `get` に `mode → fields → selection → values` の順で答える
  ので、`values` の到着を待てば fields も揃っている。cache を直接返さないのは、hub からの `set` を
  面が echo しない契約のため、host 側の正規化（丸め等）を cache が知らないことがあるから
- **`/api/health` `services.link`**: `status` = `connected` / `idle`、`detail` = 面の一覧と active の名札。
  midi feature 無しの build でも `services` が出るようになった（従来は `None`）

### 2.5 AI の口（MCP、`mcp/editor.rs`）

3 tool とも任意の `surface` 引数を得た（省略 = active）。MCP は repo-proxy ではなく daemon 直結の
"link" channel（`VantageMcp::link_call`）を叩く — editor_* は repo ではなく面に届くもので、
その MCP がどの repo の lane に居るかと無関係。

### 2.6 vp-app を面にする

- Rust（`app/boot.rs`）が bundle 読み込み前に `window.__VP_LINK__ = { url, surface, app }` を置く
  （`link_init_script`）。url の port は **自分が使う daemon の port**（brew 32000 / dev 32100）なので
  VP_PROFILE の分離（#643）がそのまま効く。面の id は `vp-app#<instance>`、名札は `Vantage Point`
- webview（`editor/link-config.ts`）がそれを `LinkOptions` に落とし、`EditorHostProvider` の
  `config.link` に渡す。無ければ Link を張らない（preview / test / 旧 Rust）
- editor-host は `^0.11.0`（Link は 0.10.0 で、`useEditorPicking` の export は 0.11.0 で乗った。creo-ui #178 /
  #184）。0.9.0 で click 捕捉の主語が provider 内部の picking に移ったので、vp-app の「画面から選ぶ」は
  `useEditorPicking` の toggle / cancel で provider を動かす。旧 `window.vpEditorHost` を叩く JS builder の
  editor 系 arm は撤去（`window.vpEditorHost` 自体は DevTools / test の手掛かりとして残る）

## 3. 切ったこと

- creo-ui に MIDI の語彙（knob / ControlEvent / ParamSpec）を持ち込まない。写すのはこちら
- host 側で値を clamp / 変換しない（契約）。clamp は hub
- `null` origin を通さない。token / 認証も当面持たない（Editor の操作しか乗らないため）
- 旧経路との fallback（面が無ければ repo 経路へ）を作らない — creo-ui を先に release して不要にした

## 4. 後続（別 PR）

1. **ROTO への写し**: knob（14bit）と touch の入力配線（`roto_midi.rs` は今 button だけ、profile 側の
   parser は未使用）、field ↔ ParamSpec の mapping（label → name、`(value−min)/(max−min)` → value、
   select → steps）、selection → page 切替、Touch 調停（手が主、release = settle）、LCD / ring への出力
   （midistage 側の出力口と同時）。**ノブの所有**（Editor fields か、roto-knobs todo の session 選びか。
   案 = Editor ON で fields / OFF で session 選び）は着手前に裁定
2. **配管の名**: `EditorCommand` / `editor_result` / `repo/editor_bridge.rs` は今 layout_* しか乗らない。
   wire と vp-app を跨ぐ rename なので、ここでは据え置いた
3. creo-ui site の agent-bridge（vite HMR ws）を Link に吸収する（creo-ui 側）
4. `vp link` CLI（面の一覧 / set）は要るときに。今は `/api/health` と MCP で足りる

## 5. 検証

- unit: `daemon::link`（admit の表 / wire 型 / registry と調停 / clamp / `get` 往復と timeout / channel
  dispatch / health）、`repo::http::link`（実 TCP + tokio-tungstenite: 403 / 一周 / 版違い close /
  再接続の置き換え）、`repo::server`（`/link` route が居て守りが先に働く）、`mcp::editor`
  （`surface` が required に入らない）、vp-app `link_init_script`、webview `link-config.test.ts`
- 実機（app:swap、daemon 再起動込み）: `/api/health` に `services.link` の面 `vp-app#0` が出る →
  lane の Claude の `editor_fields` が vp-app の field を列挙 → `editor_set` で画面が変わり、応答の
  `value` が clamp 後 → Chrome で creo-ui nightly を `bun dev` で開くと面が 2 つになり、Editor ON
  した方が active になる

## Status log

- 2026-10-30: 実機確認 — swap 後に `services.link` に vp-app の面 2 つ（window ごと）、MCP の fields / values / set が hub 経由で通った。set の値が文字列で届く欠陥を見つけて hub 側で数に直す形に。editor-host 0.11.0 で `useEditorPicking` が export されたので自前 picker を撤去。
- 2026-10-30: 起草と実装（1 PR）。mako 裁定: VP daemon を hub に / URL 固定 / token 当面なし /
  creo-ui を先に release して VP は 1 PR に。path 名 `/link` 確定（creo-ui 側の変更不要）。
