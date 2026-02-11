# ターミナル表示崩れ修正 (2026-02-11)

## 問題
`vp start --browser` でWebUI上のghostty-webターミナルの表示が崩れる。
出力が増えるにつれてテキストが重なり、ANSIエスケープシーケンスが正しく反映されない。

## 根本原因と修正

### 1. Broadcastチャネル容量不足（主因）
- `hub.rs`: `broadcast::channel(100)` → `broadcast::channel(10000)`
- PTY出力は高速（4096バイト/回×base64）でバッファオーバーフロー → ANSIシーケンス断片化

### 2. PTY初期サイズのレースコンディション
- `server.rs`: PTY起動を`Ready`から`TerminalResize`受信時に遅延
- ブラウザの実サイズ確定後にPTYを起動することで初期出力のサイズ不整合を防止

### 3. エラー握り潰し修正
- `pty.rs`: `let _ = tx.send(...)` → matchでログ出力
- `server.rs`: WebSocket送信側で`Lagged`エラーをログ出力して継続

## 学び
- Tokio broadcast channelのLagged: ターミナルのようにシーケンス順序が重要なプロトコルでは致命的
- PTYサイズの同期: PTY起動タイミングはブラウザの実サイズ確定後にすべき
- `let _ =` パターン: リアルタイムストリーミングでのエラー握り潰しは問題発見を遅らせる
