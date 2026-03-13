# VantagePoint macOS メニューバーアプリ要件定義

## 概要

メニューバーからプロジェクトを管理し、Process およびUIを起動するmacOSネイティブアプリケーション。

## アーキテクチャ

```
┌─────────────────────────────────────────────────────────────┐
│            VantagePoint.app (Swift/macOS)                    │
│                   メニューバーUI                             │
│  ┌─────────────────────────────────────────────────────┐   │
│  │  - メニューバーアイコン表示                          │   │
│  │  - プロジェクト一覧UI                               │   │
│  │  - ユーザーアクション受付                           │   │
│  └─────────────────────────────────────────────────────┘   │
└──────────────────────┬──────────────────────────────────────┘
                       │ REST API呼び出し
                       ▼
┌─────────────────────────────────────────────────────────────┐
│          TheWorld (Rust/vp world)                            │
│                    常駐プロセス                              │
│  ┌─────────────────────────────────────────────────────┐   │
│  │  ProcessManagerCapability                            │   │
│  │  - プロジェクト Process のライフサイクル管理          │   │
│  │  - Bonjour発見・監視                                 │   │
│  │  - REST API提供 (/api/world/*)                       │   │
│  └─────────────────────────────────────────────────────┘   │
└──────────────────────┬──────────────────────────────────────┘
                       │ 管理・指揮
         ┌─────────────┼─────────────┐
         ▼             ▼             ▼
   ┌──────────┐  ┌──────────┐  ┌──────────┐
   │ Process  │  │ Process  │  │ Process  │
   │    A     │  │    B     │  │    C     │
   │ :33000   │  │ :33001   │  │ :33002   │
   └──────────┘  └──────────┘  └──────────┘
```

### VantagePoint.app (Swift)

macOSネイティブのメニューバーアプリ。UIを担当。

**役割**:
- メニューバーアイコンの表示
- プロジェクト一覧・ステータス表示
- ユーザーアクションの受付
- TheWorld へのAPI呼び出し

### TheWorld (Rust)

常駐プロセスとして動作し、複数の Process を指揮・管理する。
**ProcessManagerCapability**として実装。

**役割**:
- Process の起動・停止・監視
- Bonjour経由での Process 発見
- REST API提供（Swiftアプリから呼び出し）
- 設定管理
- セルフアップデート

### Process (Rust)

各プロジェクトで動作するvpプロセス。TheWorld の管理下で動作する。

## 要件一覧

### REQ-MENU-001: プロジェクト一覧表示

メニューバーアイコンをクリックすると、登録済みプロジェクトの一覧とステータスを表示する。

**受け入れ条件**:
- [ ] `~/.config/vp/config.toml` から登録プロジェクトを読み込む
- [ ] 各プロジェクトの Process 稼働状態を表示（稼働中/停止中）
- [ ] 稼働中の場合、ポート番号を表示

### REQ-MENU-002: プロジェクトステータス表示

各プロジェクトの現在のステータス（Process 状態）を視覚的に確認できる。

**受け入れ条件**:
- [ ] 稼働中: 緑のインジケータ + ポート番号
- [ ] 停止中: グレーのインジケータ
- [ ] Bonjour発見されたものは「ネットワーク」アイコン付き

### REQ-MENU-003: PointView表示アクション（主アクション）

プロジェクトを選択して「PointView」を開く。Process から提供されるネイティブWebViewウィンドウ。

**受け入れ条件**:
- [ ] プロジェクト選択 → Process に「PointViewを開く」リクエスト
- [ ] Process が停止中の場合、自動起動してからPointViewを開く
- [ ] PointViewは Process が管理するwry WebViewウィンドウ
- [ ] 既に開いている場合はウィンドウをフォーカス

**フロー**:
```
プロジェクト選択
    ↓
Process 稼働中？ ─No→ Process 自動起動（REQ-MENU-005）
    ↓ Yes              ↓
    └──────────────────┘
    ↓
Process にPointView表示リクエスト
    ↓
PointViewウィンドウ表示
```

### REQ-MENU-004: WebView表示アクション（代替アクション）

プロジェクトを選択してシステムブラウザで表示する。

**受け入れ条件**:
- [ ] プロジェクト選択 → デフォルトブラウザでURLを開く
- [ ] Process が停止中の場合、自動起動してから開く
- [ ] URL: `http://localhost:{port}`

### REQ-MENU-005: Process 自動起動

PointView/WebViewを開く際、Process が起動していない場合は自動的に起動する。

**受け入れ条件**:
- [ ] プロジェクトディレクトリで `vp start` を実行
- [ ] 起動完了を待ってからUIを開く（ヘルスチェック）
- [ ] 起動した Process がそのプロジェクトのルート Process となる
- [ ] 起動失敗時はエラーダイアログを表示
- [ ] ユーザーは明示的に Process を起動する必要がない（透過的）

### REQ-MENU-006: プロジェクト設定読み込み

`~/.config/vp/config.toml` からプロジェクト設定を読み込む。

**受け入れ条件**:
- [ ] 設定ファイルパス: `~/.config/vp/config.toml`
- [ ] プロジェクト名とパスのマッピング
- [ ] 設定変更時の自動リロード（オプション）

### REQ-MENU-007: ツールステータス表示

利用可能なツール（MIDIコントローラー等）の接続状態を表示する。

**受け入れ条件**:
- [ ] 接続中のMIDIデバイス一覧を表示（例: LPD8）
- [ ] 各デバイスの接続状態（接続中/未接続）
- [ ] Process との紐付け状態（どの Process で使用中か）
- [ ] デバイス名とポート情報を表示

### REQ-MENU-008: Capability状態表示

各 Process で有効なCapability（機能）の状態を表示する。

**受け入れ条件**:
- [ ] MidiCapability: 接続デバイス、マッピング状態
- [ ] AgentCapability: Claude CLI接続状態
- [ ] BonjourCapability: ネットワーク広告状態
- [ ] 各Capabilityの稼働状態（Idle/Active/Error）

### REQ-MENU-009: 設定UI

設定画面への遷移メニューを提供する。

**受け入れ条件**:
- [ ] 「設定...」メニュー項目（⌘,）
- [ ] 設定ウィンドウを開く
- [ ] プロジェクト管理、MIDI設定、一般設定等

### REQ-MENU-010: アプリ終了

アプリケーションを終了するメニューを提供する。

**受け入れ条件**:
- [ ] 「Quit Vantage Point」メニュー項目（⌘Q）
- [ ] 終了時に稼働中 Process をどうするか確認（オプション）

### REQ-MENU-011: セルフアップデート

アプリケーション自体のアップデート機能を提供する。

**受け入れ条件**:
- [ ] 「アップデートを確認...」メニュー項目
- [ ] GitHubリリースから最新バージョンを確認
- [ ] 新バージョンがある場合、ダウンロード・インストール
- [ ] VantagePoint.app（macOSアプリ）のアップデート
- [ ] vp バイナリのアップデート（オプション）

### REQ-MENU-012: 全PointViewリロード

開いている全てのPointViewウィンドウをリロードする。

**受け入れ条件**:
- [ ] 「全PointViewをリロード」メニュー項目（⌘R）
- [ ] 稼働中の全 Process に対してPointViewリロードリクエストを送信
- [ ] 各PointViewがページをリフレッシュ（F5相当）
- [ ] フロントエンド更新時に便利

### REQ-WORLD-001: ProcessManagerCapability（Rust側実装）

TheWorld のプロセス管理能力を定義する。Capability Traitに準拠。
**実装先: vantage-point (Rust)**

**受け入れ条件**:
- [ ] Capability Traitを実装（info, state, initialize, shutdown）
- [ ] Process のライフサイクル管理
- [ ] Bonjour経由での Process 発見・監視
- [ ] Process 間の通信仲介（将来拡張）
- [ ] イベント購読: `process.*`, `project.*`
- [ ] macOSアプリ（Swift）からAPI経由で操作可能

**Capability情報**:
- name: `process-manager-capability`
- description: "TheWorld - 複数の Process を指揮・管理"
- type: Orchestration（オーケストレーション型）

**実装場所**:
- `crates/vantage-point/src/capability/process_manager.rs`

### REQ-WORLD-002: Process 登録・管理（Rust側実装）

TheWorld が Process を登録・管理する。
**実装先: vantage-point (Rust)**

**受け入れ条件**:
- [ ] config.tomlから登録プロジェクトを読み込み
- [ ] 各 Process の状態を監視（稼働中/停止中）
- [ ] Process の起動・停止コマンド送信
- [ ] Process の死活監視（ヘルスチェック）
- [ ] Process 消失時の自動検知
- [ ] REST API `/api/world/*` でSwiftから操作

### REQ-WORLD-003: macOSアプリ連携

macOSアプリ（Swift）が ProcessManagerCapability と連携する。
**実装先: vantage-point-mac (Swift)**

**受け入れ条件**:
- [ ] TheWorld（Rust）のAPIを呼び出し
- [ ] メニューバーUIはSwiftで実装
- [ ] Rust側 ProcessManagerCapability の状態をUIに反映
- [ ] ユーザーアクションをRust側に送信

---

## 確定事項

1. **PointViewとWebView**
   - PointView = Process が提供するwry WebViewウィンドウ
   - WebView = システムブラウザ（Safari等）で開く

2. **アーキテクチャ**
   - Swift: メニューバーUI（VantagePoint.app）
   - Rust: ProcessManagerCapability + Process（vp）
   - Swift → Rust: REST API経由で連携

3. **構成**
   - TheWorld: 常駐、Process を管理
   - Process: 各プロジェクトで動作

