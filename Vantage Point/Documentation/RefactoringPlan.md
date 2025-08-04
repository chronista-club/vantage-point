# Vantage macOSアプリ リファクタリング計画

## 現状の課題

### ChatViewModelの責務過多
- 419行の巨大なクラス
- 8つ以上の異なる責務を持つ
- テストが困難
- 変更による影響範囲が大きい

## アーキテクチャ設計

### レイヤー構造

```
┌─────────────────────────────────────┐
│         UI Layer (Views)            │
├─────────────────────────────────────┤
│    Presentation Layer (ViewModels)  │
├─────────────────────────────────────┤
│     Domain Layer (UseCases)         │
├─────────────────────────────────────┤
│   Infrastructure Layer (Services)   │
└─────────────────────────────────────┘
```

### 新しいクラス構造

#### 1. Services Layer

**MessageService**
- メッセージの送受信
- ストリーミング処理
- メッセージ履歴管理

**ClaudeAPIService**
- ClaudeClientのラッパー
- API通信の詳細を隠蔽
- エラーハンドリング

**SessionService**
- セッションの作成・更新・削除
- セッションの永続化
- 現在のセッション管理

**LoggingService**
- ログの記録
- ログレベル管理
- ログのフィルタリング

**KeychainService**
- APIキーの保存・読み込み
- セキュアな情報管理

#### 2. Repositories Layer

**MessageRepository**
- メッセージの永続化
- メッセージの取得

**SessionRepository**
- セッションの永続化
- セッションの検索

#### 3. Domain Layer

**SendMessageUseCase**
- メッセージ送信のビジネスロジック
- リトライ処理
- トークン計算

**SessionManagementUseCase**
- セッション管理のビジネスロジック
- セッション切り替え

#### 4. ViewModels

**ChatViewModel** (リファクタリング後)
- UI状態管理のみ
- UseCase呼び出し
- View向けのデータ変換

**ConsoleViewModel**
- コンソールログ表示
- ログフィルタリング

## 実装手順

### Phase 1: Service層の実装
1. ClaudeAPIServiceの作成
2. MessageServiceの作成
3. SessionServiceの作成
4. LoggingServiceの作成

### Phase 2: Repository層の実装
1. MessageRepositoryの作成
2. SessionRepositoryの作成

### Phase 3: UseCase層の実装
1. SendMessageUseCaseの作成
2. SessionManagementUseCaseの作成

### Phase 4: ViewModelのリファクタリング
1. 依存性注入の実装
2. ChatViewModelの責務削減
3. ConsoleViewModelの分離

### Phase 5: テストの実装
1. 各Serviceの単体テスト
2. UseCaseの単体テスト
3. ViewModelの単体テスト
4. 統合テスト

## 期待される効果

1. **保守性の向上**
   - 各クラスの責務が明確
   - 変更の影響範囲が限定的

2. **テスタビリティの向上**
   - 各層を独立してテスト可能
   - Mockの作成が容易

3. **拡張性の向上**
   - 新機能追加時の影響が最小限
   - プロトコル指向で柔軟な実装

4. **可読性の向上**
   - 各クラスが小さく理解しやすい
   - 責務が明確で追跡しやすい