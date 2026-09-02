# doc 59 — 設定ページ: 「好み」に置き場所を与える

**Status**: **設計確定（2026-08-31、mako × Claude の一問一答）。実装未着手。**
発端 = mako「ユーザ設定ページを作りたい」。進め方も mako 指定で
「まずは調査と、項目の洗い出しとか、見せ方とかを議論して、実装イメージを詳細化していこう」
「**見せ方の前に、項目の精査からやりたい**」「私が設定ページでいじりたいものを決めないと、
見せ方の対象が決まらない」— 全項目を board に掲示して mako が選ぶ形で確定した。
**Owners**: vantage-point（`settings.kdl` / daemon 所有）+ vp-app（設定 overlay）
**Related**: [56-edge-rail.md](./56-edge-rail.md)（**級 = 住所**。app 級 = サイドバー下部・
daemon status の直上）/ [58-sidebar-roster.md](./58-sidebar-roster.md)（名簿 = 設定の隣人）/
[54-lane-worker-model.md](./54-lane-worker-model.md)（既定 agent × model の意味論）

> 各決定に mako の決め言葉を引用してある（facts-over-narrative）。

---

## 1. 原理: 設定は「何に属するか」で 3 層に割る

> mako「ここは過去の決定に引きずられないで、**新しくこうあるべきから考えて、それを仕様に据えよう**」

従来の分け方は「**誰が書くか**」だった（config.kdl = 人が書く / vp-app.toml = GUI が書く）。
これが捻れの源で、`claude-cli-path`（このマシンで claude がどこにあるか）と
「既定は claude × Opus 5」（mako の好み）が同じ config.kdl に同居していた。**前者はマシンを
移ると無意味になり、後者は人に付いていくべき**もので、寿命も持ち運び先も違う。

そこで軸を「**何に属するか**」へ変える:

| 層 | 何に属するか | 例 | 置き場所 | 書き手 | 持ち運べるか |
|---|---|---|---|---|---|
| **環境** | このマシンの事実 | `claude-cli-path` / `hub-addr` | `config.kdl` | **人だけ**（VP は触らない） | ✗ マシン固有 |
| **好み** | **user 本人** | 既定 agent × model / theme / アイドル時間 / ログ詳細度 | **`settings.kdl`（新設）** | **daemon**（+ 人の手編集も可） | ○ 将来 Creo ID 同期の余地 |
| **作業** | 今なにを開いているか | 登録 repo / lane / session | `repos.kdl` / DB | VP | ✗ マシン固有 |

### 傍証: 分類が mako の選択と一致した

全項目メニュー（A1〜E1）から mako が**選ばなかった**のは `claude-cli-path` と `hub-addr` の 2 つ。
どちらも「環境」層である。選ばれた 9 項目はすべて「好み」か「操作」だった。
**設定ページに出したいものと config.kdl に残すべきものが、この線でそのまま割れる。**

---

## 2. 実態 — 守られていた原則の中身は 3 行だった

「VP は config zone に書き戻さない」という従来の方針は、**既に事実ではなかった**。

| ファイル | 誰が書くか | 実測（2026-08-31） |
|---|---|---|
| `config.kdl` | 人（read-only 方針） | **3 行**。`hub-addr` 1 キーのみ。7月3日から不変 |
| `repos.kdl` | **VP** | 同日 00:32 更新・12 repo |
| `vp-app.toml` | GUI（menu 経由） | 22 バイト（ほぼ空） |

しかも `repos.kdl` が VP に書かれているのは事故ではなく、**council 2026-05-16 の裁定**である
（[`repos_file.rs`](../../crates/vantage-point/src/repos_file.rs) 冒頭）:

> repos は唯一の永続データであり、ephemeral な embedded DB に置いたのが設計ミス。
> → **人間可読 file を SSOT に**

VP には既に「**VP が読み書きする人間可読 KDL file**」という確立したパターンがあり、それは
DB から移ってきた勝ちパターンだった。read-only 方針が守っていたのは、実質「hub-addr 1 行に
コメントを添えて手で書ける」ことだけである。**引きずられる理由がない。**

---

## 3. `settings.kdl` — daemon が所有し、GUI は頼む

```
設定ページ（GUI）──wire──▶ daemon ──書く──▶ settings.kdl
  vp config set ──────────────▶   ▲
                          人が手で編集も可（reload で反映）
```

**daemon を唯一の書き手にする。** `RepoManagerCapability::add_repo` が `repos.kdl` に対して
やっている形をそのまま踏襲する:

- 実装パターンを流用できる（新しい書き込み規律を発明しない）
- GUI と CLI が同時に書いて壊す競合が**構造的に**起きない
- daemon 側が読む設定（アイドル時間 / ログ詳細度）が素直に届く。GUI が書いて daemon が
  読むための伝達経路を別途作らずに済む

### 優先順位

env > `settings.kdl` > 組み込み既定。`config.kdl` とはキーを**重複させない**
（同じ設定が 2 箇所にある状態を作らない = どちらが勝つかを覚えなくてよい）。

既存の `VP_DEVELOPER_MODE` / `VP_LOG` / `VANTAGE_DEBUG` は env として最優先のまま残す
（`initial_developer_mode` の 1Password 風の優先順を壊さない）。

---

## 4. 住所

doc 56 の「**動詞の級がそのまま住所を決める**」に従う。設定は **app 級**なので rail ではなく
サイドバー、位置は**下部・daemon status の直上**（ACTIONS の下）。これは doc 56 §7 で
mako 裁定済みで、本 doc はその予約席に入るだけである。

> ⚠️ doc 56 §7 は「最初の設定項目 = rail の形態トグル（B ⇄ A）」と書いているが、**今回 mako は
> これを選ばなかった**。A 形（浮遊）が未実装でトグルの片側が存在しないため。rail トグルは
> 設定基盤が立った後に足す（doc 56 の位置づけ自体は不変）。

---

## 5. 項目 — 確定した 9 つと扱い

| | 項目 | 層 | 裁定 |
|---|---|---|---|
| A1+A2 | 既定 agent × model | 好み | **組で登録**。対象は claude と codex のみ。**第一弾は codex の model 欄を出さない** |
| A4 | Add Repo 初期フォルダ | 好み | そのまま |
| B2 | theme | 好み | **terminal は触らない。sidebar は追随させる** |
| B3+B4 | アイドル時間 | 好み | **1 つの設定に統合** |
| B6 | Developer Mode | 好み | **設定ページへ移設**（View メニューから外す） |
| C2 | Creo / hub login | 操作 | **両方に置く**（設定ページと sidebar Hub 行） |
| E1 | ログ詳細度 | 好み | **daemon 再起動で反映**（即時反映は作らない） |
| — | daemon 再起動ボタン | 操作 | 確認ダイアログ必須 |

### 5.1 既定 agent × model を「組」にする理由

現状の `default-agent` と `default-lane-model` は**独立した 2 キー**で、既定を解決する
[`routes/lanes.rs`](../../crates/vantage-point/src/repo/routes/lanes.rs) は
**agent を見ずに** model を返してから agent と組にして registry へ書く:

```rust
if let Some(model) = engine_model::resolve_default(req.model, config.default_lane_model()) {
    session_registry::set_model(&addr.repo, lane_label, &agent, 1, Some(&model));
    //                                                  ^^^^^^ agent はここで初めて登場
}
```

つまり `default-agent "codex"` + `default-lane-model "claude-opus-5"` という**意味のない
組み合わせが表現できてしまう**。設定を組にすれば、この穴が設定の形そのもので塞がる。

> ⚠️ **codex は現在 model を受け取れない** — `EngineKind::model_choices` が空で、
> `codex_command` も `--model` を注入していない。ただし codex CLI 自体には `-m, --model` が
> 実在する（2026-08-31 実測）ので技術的な壁ではない。第一弾では codex 行の model 欄を
> 「codex 側で選択」と表示して**選択肢を出さない**（「押しても効かない選択肢を並べない」
> 原則）。catalog 新設は別タスク。

### 5.2 アイドル時間を 1 つにする理由

[`lanes_state.rs`](../../crates/vantage-point/src/repo/lanes_state.rs) の
`IDLE_TEARDOWN_AFTER_MS` には「**now-line の quiet 閾値と同値**」と明記されている。
2 つのスライダーにすると、この意図的な同値関係が黙って壊れる（片方だけ動かせてしまう）。

統合すると **client 判定（quiet 表示）と daemon 判定（engine teardown）の両方に
同じ値を届ける**必要が出る。settings.kdl を daemon が所有する形なら、daemon は直接読み、
GUI へは roster と同じ経路で運べる。

### 5.3 daemon 再起動ボタンの警告義務

daemon を止めると **全 repo が落ちる = 全 lane の claude が落ちる**（doc 44 P1 fold-in 以降、
repo は daemon プロセス内の `Arc<AppState>`）。GUI から気軽に押せる位置に置く以上、
確認ダイアログで「**何が落ちて、何が戻るか**」を明示する:

- 落ちる: すべての lane のプロセス
- 戻る: 会話は `cc_session` の `--resume` で次回 spawn 時に継がれる

---

## 6. 実装フェーズ

**分割線**: A4 / B6 / C2 / daemon 再起動の 4 つは **`vp-app.toml` と既存 UI で足りる**ため、
設定基盤が無くても出せる。器を先に立て、中身を後から増やす。

| Phase | 内容 | 依存 |
|---|---|---|
| **P1** | 設定 overlay の器（住所 = サイドバー下部）+ A4 / B6 / C2 / daemon 再起動 | なし |
| **P2** | 設定基盤: `settings.kdl` + daemon 所有 + wire + `vp config set` + **原則 doc の修正（§7）** | なし |
| **P3** | 基盤に乗る設定: E1 / アイドル時間（B3+B4 統合） | P2 |
| **P4** | 既定 agent × model の組（A1+A2） | P2 |
| **P5** | sidebar の theme 追随（B2） | P1 |

P5 が最後なのは、sidebar が creo の theme token を使わず **"Light Grid" 独自体系を生 hex で
直書き**しているため（`Shell.tsx`）。theme 切り替えを効かせるには色体系の載せ替えが要る。
terminal の ANSI 16 色は `:root[data-theme="contrast-dark"]` の中にだけ定義され外に
fallback が無いが、**terminal は触らない**ので現状のセレクタを維持する。

---

## 7. 本 doc が上書きする記述

「config zone = 人が編集」という記述は、`repos.kdl`（VP が書く）と `settings.kdl`（新設）を
含まないため不正確。以下を §1 の 3 層モデルに合わせて直す:

| 場所 | 直す方向 |
|---|---|
| `CLAUDE.md` §設定・ポート | 3 層モデルを明記。人が書く file と VP が書く file を分けて記述 |
| `crates/vp-paths/src/lib.rs` zone 表 | 同上（2026-08-30 に `vp-app.toml` だけ例外として追記したが**不完全だった** — `repos.kdl` も VP が書く側） |
| `docs/guide/setup.md` zone 表 | 同上 |
| `crates/vantage-point/src/config.rs` module doc | `config.kdl` = **環境層**に限定する旨へ |

---

## 8. 見送った項目（記録）

| 項目 | 理由 |
|---|---|
| `claude-cli-path`（A3） | 環境層 — config.kdl の手編集で足りる |
| spawn 同時数（A5） | 選ばれず |
| rail A/B トグル（B1） | A 形が未実装でトグルの片側が無い（§4） |
| 画像上限（B5） | 選ばれず |
| `hub-addr`（C1） | 環境層。daemon 再起動が要るため設定ページ向きでもない |
| repo 管理の一覧形（D1） | 既存 UI が完備 |
