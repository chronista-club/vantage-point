# Paisley Park 要件定義

> REQ-PAISLEY-*: Paisley Park（プロジェクト単位Agent）の要件

## 概要

Paisley Parkは、プロジェクトごとに生成されるAI Agentプロセス。
The Worldの統括下で、各プロジェクトのタスク実行・開発支援を担う。

> 命名由来: JoJoの奇妙な冒険 Part8「ペイズリー・パーク」より。
> 広瀬康穂のスタンド。電子機器を通じてユーザーを最適解へ導く。
> AI Agentとしての「導き」の役割を象徴。

## 要件一覧

### REQ-PAISLEY-001: プロジェクト単位の生成

**概要**: 各プロジェクトに対して1つのPaisley Parkインスタンスが生成される

**受け入れ条件**:
- [ ] プロジェクトごとに独立したPaisley Parkプロセスが起動する
- [ ] 複数のPaisley Parkが同時に稼働できる
- [ ] 各Paisley Parkは担当プロジェクトのコンテキストを保持する

---

### REQ-PAISLEY-002: ライフサイクル管理

**概要**: Paisley Parkの起動・停止はThe World経由で管理

**起動方法**:
1. 手動: `vp park start <project>`
2. オンデマンド: 設定ファイルに基づき自動起動
3. スマート常駐: 最後にアクティブだったN個は自動常駐

**停止方法**:
1. 手動: `vp park stop <project>`
2. アイドルタイムアウト: 設定時間無操作で自動停止
3. The World終了: 全Paisley Parkが終了

**受け入れ条件**:
- [ ] `vp park start <project>` でPaisleyが起動する
- [ ] `vp park stop <project>` でPaisleyが停止する
- [ ] 設定に基づくオンデマンド起動が動作する
- [ ] The World終了時に全Paisley Parkが終了する

---

### REQ-PAISLEY-003: 動的ポート割り当て

**概要**: Paisley ParkのポートはThe Worldが動的に割り当て

**受け入れ条件**:
- [ ] The WorldがPaisley Park起動時にポートを割り当てる
- [ ] 割り当てられたポートがVantage DBに記録される
- [ ] ポート情報がThe World経由で取得できる

---

### REQ-PAISLEY-004: Claude CLI統合

**概要**: Claude CLIをバックエンドとしたAI Agent機能

**実行モード**:
- OneShot: 単発プロンプト実行
- Interactive: 持続セッション（Stream-JSON I/O）
- PTY: 真の対話モード（Multiplexer対応）

**受け入れ条件**:
- [ ] Claude CLIを起動してプロンプトを実行できる
- [ ] Interactiveモードで複数ターン会話ができる
- [ ] PTYモードでMultiplexer連携ができる

---

### REQ-PAISLEY-005: The World通信（Unison Protocol）

**概要**: The Worldとの通信はUnison Protocol（QUIC + KDL）で行う

**通信内容**:
- 状態報告（稼働中、アイドル、エラー等）
- タスク受信・結果送信
- イベント通知（MIDI、外部イベント等）

**受け入れ条件**:
- [ ] Unison Protocol経由でThe Worldと通信できる
- [ ] 状態変更がリアルタイムでThe Worldに通知される
- [ ] The Worldからのイベントを受信できる

---

### REQ-PAISLEY-006: ViewPoint操作

**概要**: The Worldが管理するViewPointを操作

**操作内容**:
- コンテンツ表示（Markdown、HTML、ログ）
- ペイン操作（分割、切り替え、トグル）
- チャットメッセージ送信

**受け入れ条件**:
- [ ] ViewにMarkdownコンテンツを表示できる
- [ ] ペインの分割・切り替えができる
- [ ] チャットUIにメッセージを送信できる

---

### REQ-PAISLEY-007: Terminal管理

**概要**: 複数のTerminalを自由に管理

**機能**:
- Terminal生成・破棄
- コマンド実行
- 出力のView表示

**受け入れ条件**:
- [ ] 複数のTerminalを生成できる
- [ ] 各Terminalでコマンドを実行できる
- [ ] Terminal出力をViewに表示できる

---

### REQ-PAISLEY-008: MIDIイベント受信

**概要**: The World経由でMIDIイベントを受信

**受け入れ条件**:
- [ ] The WorldからMIDIイベントを受信できる
- [ ] イベントに応じたアクションを実行できる
- [ ] マッピング設定に基づいて動作する

---

### REQ-PAISLEY-009: エラーリカバリ

**概要**: 異常終了時の自動復旧

**挙動**:
1. クラッシュ時、最大3回まで自動再起動
2. 3回超過でThe Worldに通知
3. The WorldがmacOS通知を表示

**受け入れ条件**:
- [ ] クラッシュ後に自動再起動する
- [ ] 3回超過でThe Worldに通知される
- [ ] ユーザーにmacOS通知が表示される

---

### REQ-PAISLEY-010: Hot Reload対応

**概要**: Paisley Park再起動時にViewが自動更新

**受け入れ条件**:
- [ ] 再起動時にThe Worldへ通知が送信される
- [ ] Viewが再接続して状態を復元する

---

## 関連設計

- [design/08-paisley-architecture.md](../design/08-paisley-architecture.md)（予定）
- [design/09-unison-protocol.md](../design/09-unison-protocol.md)（予定）
- [spec/07-vpworld.md](./07-point-stand.md)
