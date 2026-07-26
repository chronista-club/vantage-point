-- Vantage Point DB Schema v1
-- stand_events, settings, kv_store の3テーブル

-- stand_events: 起動/停止イベント、セッションログ、ヘルスチェック結果
CREATE TABLE IF NOT EXISTS stand_events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    port       INTEGER NOT NULL,
    event_type TEXT    NOT NULL,
    repo    TEXT,
    details    TEXT,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_stand_events_port ON stand_events(port);
CREATE INDEX IF NOT EXISTS idx_stand_events_type ON stand_events(event_type);
CREATE INDEX IF NOT EXISTS idx_stand_events_time ON stand_events(created_at);

-- settings: ユーザー設定・repo固有設定
-- repo='' はグローバル設定（NULLだとUNIQUEが効かない）
CREATE TABLE IF NOT EXISTS settings (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    repo    TEXT    NOT NULL DEFAULT '',
    key        TEXT    NOT NULL,
    value      TEXT    NOT NULL,
    updated_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE(repo, key)
);

-- kv_store: 汎用KVストア（Capabilityごとにnamespaceで分離）
CREATE TABLE IF NOT EXISTS kv_store (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    namespace  TEXT    NOT NULL,
    key        TEXT    NOT NULL,
    value      TEXT    NOT NULL,
    updated_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE(namespace, key)
);
