# 90. 実物の diff による出荷時の段

> **Status**: Draft
> **Related**: `mem_1ChSjZ8945Y4MsvU4F2qEf`、[dev-flow guide](../guide/dev-flow-primitives.md)
> **対象**: `crates/vantage-point/src/flow.rs`, `crates/vantage-point/src/lane/commands.rs`, `crates/vantage-point/src/commands/flow.rs`

## 目的と境界

handoff の段は着手時の予想。出荷時は committed / staged / unstaged / untracked を含む実物の変更で
A / B / C を決め、上げ下げの両方を許す。判定結果はレビュー・PR・merge 権限の案内であり、
wire の送信、レビュー実行、承認、merge を自動実行しない。分類はパスと変更量に基づくヒューリスティックで、
変更内容の意味を理解するモデルではない。既知の危険なパスの保守と週末の dogfooding を継続する。

## 収集と純粋判定

- `collect_diff_stats(dir, base_ref)` は merge-base から作業ツリーまでの `git diff --numstat -z --no-renames` と
  `git ls-files --others --exclude-standard -z` を収集する。git subprocess は収集本体で 3 回。
- CLI はカレントディレクトリまたは `--path` から worktree root を解決する。基準 ref は共通 repo の
  `sub-files.kdl` の `base-ref`、ローカルの `origin/HEAD` の順で解決する。ネットワーク取得は行わない。
  設定 ref は `origin/<base-ref>` を優先し、ローカル ref も受理する。基準不明・git 失敗はエラー。
- `classify_diff(&[DiffFileStat])` は I/O を持たない。空なら `None`。
- C は daemon、protocol/wire、vendor、root Cargo.toml、swap/release/CI、AGENTS.md/CLAUDE.md、
  session registry / settings、実機書き込みの境界のいずれか。1 行でも該当すれば理由付き C。
- C 以外で docs / テスト専用のみ、または実装 1 ファイル以下かつ実装の追加削除合計が 80 行以下なら A。
  その他は B。行数不明の binary 実装は B とし、0 行の局所変更扱いを避ける。
- A は review `none` / PR `ready` / merge `lead`、B は `moody-blues` / `ready` / `lead`、
  C は `santa` / `draft` / `user`。

## 投影と互換性

`SubStatus` に省略可能な `diff_tier` と `tier_reasons` を追加し、CLI / MCP の flow progress は
既存の `sub_status` JSON に載せる。テーブルは `TIER` 列を表示する。取得エラー時は段なし・理由にエラー、
空 diff は段なし・理由なしとして区別する。旧 producer からの欠落フィールドは default で読める。
vp-app の wire 型と生成 TS 型も追随し、往復でフィールドを落とさない。

## やってはいけない

- 公開 default（VP の main）を開発 trunk（nightly）の代わりに使わない。lane の base-ref を優先する。
- rename 先だけで判定しない。危険な旧パスの削除を保持するため rename 検出を無効にする。
  このため移動は 2 ファイルとして数える。
- numstat を行やタブだけで path 分割しない。NUL 区切りで日本語・タブ・改行入り path を保つ。
- git の失敗や読み込めない untracked を無変更に変換しない。
- root / base の解決と収集で Git の環境を分けない。全プローブから repo-local な `GIT_*` を外す。
  呼び出し元の `GIT_DIR` だけが基準解決へ漏れると、対象と別 repo の設定を使い C を空 diff に落とし得る。

## Status log

- 2026-11-27: 実物 diff の分類、CLI / progress / wire 投影、出荷時の段の上げ下げを実装。
- 2026-11-27: Santa の独立再現で見つかった Git 環境の混在を、全プローブの共通境界と CLI 回帰テストで修正。
