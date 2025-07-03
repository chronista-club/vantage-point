# Vision Pro コンソール機能実装ガイド

## 概要
Vision Pro上でコンソール（文字列の表示）機能を実装しました。このコンソールは、アプリケーションのログメッセージをリアルタイムで表示し、デバッグ情報を確認できます。

## 実装済み機能

### 1. ConsoleViewModel
- ログレベル管理（Debug, Info, Warning, Error）
- カテゴリ別ログ管理
- タイムスタンプ付きメッセージ
- 最大メッセージ数制限（メモリ管理）
- フィルタリング機能

### 2. ConsoleView
- スクロール可能なログ表示
- 自動スクロール機能
- ログレベル別の色分け
- カテゴリフィルター
- クリア機能

### 3. 統合済みコンポーネント
- ContentView: コンソール表示/非表示トグル
- Renderer: レンダリングプロセスのログ出力
- ARKit: セッション状態のログ出力

## 使用方法

### 基本的な使い方
```swift
// グローバルコンソールインスタンスを使用
globalConsole.info("情報メッセージ", category: "MyCategory")
globalConsole.warning("警告メッセージ", category: "MyCategory")
globalConsole.error("エラーメッセージ", category: "MyCategory")
globalConsole.debug("デバッグメッセージ", category: "MyCategory")
```

### ContentViewでの表示
1. "Show Console"ボタンをタップしてコンソールを表示
2. ログテストボタンでメッセージを追加
3. フィルターアイコンでログレベルやカテゴリでフィルタリング

## 実機テストの準備

### 1. 開発者アカウントの設定
1. Xcodeでプロジェクトを開く
2. プロジェクト設定 > Signing & Capabilities
3. Team: 開発者アカウントを選択
4. Bundle Identifier: 一意のIDを設定（例: com.yourcompany.vantage）

### 2. Vision Proデバイスの準備
1. Vision Proを開発者モードに設定
   - 設定 > プライバシーとセキュリティ > デベロッパモード
2. MacとVision Proを同じネットワークに接続
3. Xcodeで Window > Devices and Simulators
4. Vision Proをペアリング

### 3. ビルドと実行
```bash
# 実機向けビルド
xcodebuild -scheme Vantage -configuration Debug -destination 'platform=visionOS,name=Your Vision Pro' build

# または Xcode GUI から
# 1. スキーム選択で実機を選択
# 2. Run ボタンをクリック
```

### 4. プロビジョニングプロファイル
- Xcodeが自動的に作成（Automatically manage signing を有効化）
- または Apple Developer Portalで手動作成

## 次のステップ

### ImmersiveSpace内でのコンソール表示
現在、コンソールは通常のウィンドウ内に表示されています。今後の実装：

1. **RealityKitでの3D表示**
   - AttachmentComponentを使用してSwiftUIビューを3D空間に配置
   - ユーザーの視線に追従する配置

2. **コマンド入力機能**
   - テキストフィールドの追加
   - 簡単なコマンド処理（clear, filter等）

3. **パフォーマンス最適化**
   - 大量のログでのスクロールパフォーマンス
   - メモリ使用量の最適化

## トラブルシューティング

### ビルドエラーが発生する場合
1. Xcode 16.0以上を使用していることを確認
2. visionOS 2.0 SDKがインストールされていることを確認
3. Swift 6.0言語モードが有効になっていることを確認

### 実機で動作しない場合
1. デベロッパモードが有効になっているか確認
2. プロビジョニングプロファイルが正しく設定されているか確認
3. Bundle IDが一意であることを確認

## 参考リンク
- [Apple Vision Pro Developer Documentation](https://developer.apple.com/visionos/)
- [RealityKit Documentation](https://developer.apple.com/documentation/realitykit/)
- [SwiftUI for visionOS](https://developer.apple.com/documentation/swiftui/)