# 78. アプリ共通 Editor とテーマ保存

> **Status**: Draft
> **Related**: design 48、`mem_1CfjZnPVNMbQrVauwF6W4C`
> **対象**: `crates/vp-app/webview/editor/`, `crates/vp-app/webview/entry.tsx`, `crates/vp-app/webview/src/sidebar/ResponsePoint.tsx`

## 目的と範囲

Ctrl+Shift+E で開く Editor を THEME / SIDEBAR / COMPONENT の縦並びに整理し、初回は THEME を開く。
部品を選ぶ必要がある操作と、アプリに直接適用する操作を分け、調整を名前付きテーマとして再利用できるようにする。

- COMPONENT は「画面から選ぶ」を入口にする。選択位置を枠で示し、同じ種類の全インスタンスへの変更であることを説明する。DOM の部品ツリーは折りたたんだ補助導線。
- SIDEBAR は選択不要。文字、背景、選択行、状態ポイントを扱う。現在表示されない旧コネクターの演出ノブは登録しない。現行 CSS で使うアクセントは残す。
- THEME はプリセット選択と、旧 GLOBAL / SURFACE・Chat・Resizer の有効な調整をまとめる。
- 各テーマでの調整は自動保存し、名前を付けてカスタムテーマを追加できる。カスタムには保存済みの値と編集中の値がある。
- daemon、端末テーマ、共有ライブラリの配布、実アプリの入替は対象外。

## 構成

公開済み `@chronista-club/creo-ui-editor-host` 0.8.1 の API で VP のパネルを構成する。
`EditorHostProvider` / `EditorHost` の field・selection・MCP 契約を利用し、非公開 FieldEditor や配布済み JS は変更しない。

- `VpEditor`: テーマ選択と Provider の寿命。テーマを切り替えると Editor だけを再生成する。Sidebar / Chat / pane はこの Provider の外にあり、再マウントしない。
- `EditorSession`: field 復元、変更購読、`window.vpEditorHost` の公開、遅延 component の復元。
- `app-fields`: VP の調整項目。既存 Chat / Resizer / Lane 選択 field ID を維持する。
- `EditorPanel`: 範囲別の入力、明示選択、枠、補助ツリー、JSON 書き出し。
- `theme-store`: version 付き保存データの検証と旧 field 保存の読込み。

Provider 0.8.1 は初期テーマの色相・彩度の基準を生成時に保持する。したがって `data-theme` だけの切替は行わない。
切替前に Editor が扱う CSS 変数を解除し、新しいテーマを設定してから Provider を生成する。
初期の `contrast-dark` は VP の既存配色を保ち、他のプリセットでは VP の面と文字を Creo のテーマ変数に接続する。

状態色は `ACTIVITY_COLOR` の CSS 変数参照を通じて、ポイントと状態文字に同時に適用する。状態判定・鮮度・点滅のロジックは変更しない。

## 保存形式

`localStorage['vp:editor:themes:v1']`:

```typescript
{
  version: 1,
  selected: string, // 組込み ThemeId または custom:<UUID>
  drafts: Record<string, Record<string, string | number | boolean>>,
  custom: Array<{ id: string; name: string; base: ThemeId; values: Record<string, string | number | boolean> }>
}
```

保存する値は field の初期値との差分。背景の色相・彩度と個別色は同じ CSS 変数を操作するため、背景の連動調整では実効色も保持する。初期化を先に済ませ、全体の色相・彩度、個別色の順で復元する。テーマ A → B → A では A の調整に戻り、B の上書きは残さない。
カスタム名は空白除去後 1〜80 文字。同名を自動上書きせず、選択中のカスタムへは「変更を保存」で更新する。
保存先が利用できなければ画面内の値を維持し、書き出しを促す。

旧 namespace の field 保存は VP 保存が存在しない初回に読み込む。旧キーは削除・上書きしない。
Provider 内蔵の保存は `vp:editor:runtime` を互換キャッシュとして分離し、生成前にこの prefix の field キーだけを解除する。
復元の正本は VP 保存。field の型・数値範囲に合わない値は適用しない。
未知の field ID は保持し、部品が後から現れた場合は公開 resolver で登録して適用する。

## 選択とキーボード

通常は Provider の `selectionRoot` が null。「画面から選ぶ」の間だけ document.body を対象とし、選択で通常操作へ戻る。
補助ツリーと保存復元には別の公開 resolver を使い、選択モード外でも DOM の部品を列挙できるようにする。
パネルは `data-editor-layer` で選択対象から除外する。選択枠の位置だけが更新されたときは入力欄を保持し、編集中の値とフォーカスを失わない。

Esc は選択操作中なら取り消し、選択済みなら解除、選択なしなら閉じる。Ctrl+Shift+E は開閉。
タブは上下キー / Home / End で切り替える。閉じると元のフォーカスへ戻す。

## やってはいけない

- 新テーマの Provider 生成前に、前テーマの inline CSS を残さない。初期色キャッシュに混ざる。
- 初回の field 登録前に Editor の CSS を CSSOM に接続する。JSX の style 挿入を待つと、保存済みライトテーマの初期値が旧配色になる。
- 旧保存キーを削除しない。読み取り可能な移行元として残す。
- 復元を最初に存在した field だけで完了扱いにしない。component field は遅延登録される。
- Editor のテーマ切替のために Sidebar / Chat / daemon を再起動しない。
- happy-dom の `CSS.supports` を色の妥当性検証の証拠にしない。実ブラウザの描画・入力で確認する。

## 検証

`editor-theme.test.ts` で区分・保存/復元・テーマ分離・旧保存・壊れた保存・遅延 component・明示選択と Esc を検証する。
`sidebar-hierarchy.test.ts` で状態ポイントと状態文字の共通 token を検証する。
実ブラウザ用の独立プレビューは [guide](../guide/editor-theme.md) を参照。
WKWebView 上の見た目・操作は、実アプリ入替を別途行うまで未確認。

## Status log

- 2026-10-06: VP 所有パネル、テーマごとの調整と名前付き保存、明示選択を実装。公開 0.8.1 を維持。
