# The World 要件定義

> REQ-WORLD-*: The World（常駐コアプロセス）の要件

## 概要

The Worldは、Vantage Pointシステムの中核となる常駐プロセス。
システム全体のライフサイクル管理、外部連携、UI提供を担う。

> 命名由来: JoJoの奇妙な冒険「ザ・ワールド」より。
> DIOのスタンド。「世界」を統括し、配下のPaisley Parkたちを従える絶対的存在。

## 要件一覧

### REQ-WORLD-001: システムライフサイクル管理

**概要**: VPシステム全体の起動・終了を管理

**起動トリガー**:
- `vp up` コマンド
- VantagePoint.app 起動

**終了トリガー**:
- `vp down` コマンド
- VantagePoint.app 終了

**受け入れ条件**:
- [ ] `vp up` でThe Worldが起動する
- [ ] `vp down` でThe World及び全Paisley Parkが終了する
- [ ] VantagePoint.appの起動/終了と連動する

---

### REQ-WORLD-002: Conductor機能

**概要**: Paisley Park Standの管理・監視

**機能**:
- Paisley Parkの起動/停止
- Paisley Parkの状態監視
- 動的ポート割り当て管理

**受け入れ条件**:
- [ ] Paisley Parkの起動をPoint経由で行える
- [ ] Paisley Parkの停止をPoint経由で行える
- [ ] 稼働中Paisley Parkの一覧を取得できる
- [ ] 各Paisley Parkにユニークなポートを動的割り当てできる

---

### REQ-WORLD-003: 固定ポート

**概要**: The Worldは固定ポート33000で待ち受け

**受け入れ条件**:
- [ ] The Worldはポート33000で起動する
- [ ] ポート競合時はエラーを報告する

---

### REQ-WORLD-004: HTTP MCP Server

**概要**: AI AgentとのインターフェースをHTTP MCPで提供

**機能**:
- MCP Tools の提供（Paisley Park操作、View操作等）
- ローカルファイルアクセス権限
- stdio方式は提供しない（HTTP Only）

**受け入れ条件**:
- [ ] HTTP経由でMCPツールを呼び出せる
- [ ] ローカルファイルの読み書きが可能
- [ ] Paisley Park操作ツールを提供する
- [ ] View操作ツールを提供する

---

### REQ-WORLD-005: macOS統合

**概要**: macOSシステムとの統合

**機能**:
- メニューバーアイコン（システムトレイ）
- macOS通知センター連携
- OS監視（オプション）

**受け入れ条件**:
- [ ] メニューバーにVPアイコンが表示される
- [ ] メニューから基本操作ができる
- [ ] エラー時にmacOS通知が表示される

---

### REQ-WORLD-006: エラーリカバリ

**概要**: 異常終了時の自動復旧

**挙動**:
1. クラッシュ時、最大3回まで自動再起動
2. 3回超過でmacOS通知を送信して停止
3. Paisley Park異常時も同様のポリシー

**受け入れ条件**:
- [ ] クラッシュ後に自動再起動する
- [ ] 3回再起動失敗でmacOS通知が表示される
- [ ] 3回超過後は手動対応が必要になる

---

### REQ-WORLD-007: Hot Reload通知

**概要**: 再起動時にViewPointへ通知

**機能**:
- ViewPoint/Paisley Park再起動をWebSocket経由で通知
- ViewPointが通知を受けて自動更新

**受け入れ条件**:
- [ ] 再起動時にWebSocketで`restart`通知が送信される
- [ ] ViewPointが通知を受けて再接続する
- [ ] 必要に応じてビュー状態が復元される

---

### REQ-WORLD-008: Vantage DB接続

**概要**: SurrealDB（ローカル+クラウド同期）への接続

**機能**:
- ローカルSurrealDB Embeddedの起動・管理
- クラウドSurrealDBとの同期（user_status等）
- 将来的なCRDT対応の基盤

**受け入れ条件**:
- [ ] ローカルSurrealDBが起動する
- [ ] ユーザー状態がDBに永続化される
- [ ] クラウドDBとの同期が動作する（設定時）

---

### REQ-WORLD-009: 設定ファイル読み込み

**概要**: Paisley Parkオンデマンド設定等の読み込み

**設定項目**:
- プロジェクト一覧
- Paisley Parkオンデマンド設定
- 常駐Paisley Park数（最後にアクティブだったN個）
- MIDIマッピング

**受け入れ条件**:
- [ ] 設定ファイルからプロジェクト一覧を読み込む
- [ ] オンデマンド設定に基づきPaisley Parkを管理する
- [ ] 設定変更時にホットリロードする

---

## 関連設計

- [design/07-point-stand-architecture.md](../design/07-point-stand-architecture.md)（予定）
- [design/08-unison-protocol.md](../design/08-unison-protocol.md)（予定）
