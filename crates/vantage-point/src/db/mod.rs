//! SurrealDB 統合モジュール (embed mode)
//!
//! VP の状態管理を **プロセス内の SurrealDB** に統一する。
//! surrealkv backend を使い、外部 `surreal` バイナリは不要。
//!
//! ## 接続方式
//!
//! - 本番: `surrealkv://<data_dir>` で in-process embedded DB
//! - テスト: `kv-mem` で in-memory embedded DB
//!
//! 単一プロセスが DB を保持する single-writer モデル。
//! 複数 Process が同時に書くユースケースは現状ない (TheWorld が集約点)。
//!
//! ## テーブル設計
//!
//! - `processes`: プロセス状態（QUIC Registry + HTTP polling 代替）
//! - `pane_contents`: Canvas ペイン状態
//! - `stand_status`: Stand ステータス
//! - `prompts`: User Prompt
//! - `notifications`: CC 通知

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;

// LIVE SELECT の Action enum を caller に露出 (downstream が surrealdb 直接依存しなくて済むように)
pub use surrealdb::types::Action;

/// SurrealDB の名前空間
const NS: &str = "vp";

/// SurrealDB のデータベース名
const DB_NAME: &str = "vp";

/// SurrealDB データディレクトリの root (`vp_data_dir()/db`)
///
/// XDG restructure: DB は永続 data なので `vp_data_dir()` 配下 (= 全 OS で
/// `~/.local/share/vp/db/`、 `$XDG_DATA_HOME` 優先)。 macOS の Application
/// Support / Windows の %APPDATA% は撤去 (= roaming sync で DB 破損 risk 回避)。
fn db_root() -> PathBuf {
    crate::config::vp_data_dir().join("db")
}

/// World daemon (TheWorld) 専用の DB ディレクトリ (`vp_data_dir()/db/world`)
///
/// VP-182: surrealkv は OS レベル排他ロック (`try_lock_exclusive`) を持つため、
/// World と SP が同一ディレクトリを open すると LOCK 衝突で 2 番目が失敗する。
/// World は `processes` テーブルを保持する専用 DB を `db/world/` に分離。
/// (VP-188: 旧 `projects` テーブルは projects.kdl に移行済)
pub fn db_data_dir_for_world() -> PathBuf {
    db_root().join("world")
}

/// Project (SP / Star Platinum) 専用の DB ディレクトリ (`vp_data_dir()/db/sp_{slug}`)
///
/// VP-182: 各 SP は project slug 別の独立 DB ディレクトリを使う。 doc 19 §4.6 の
/// 「各 SP は自分の DB に write」 設計をディレクトリ分離で物理化し、 World との
/// surrealkv LOCK 衝突を構造的に解消する。 cross-process forward は受信側 SP の
/// DB に書く設計 (doc 19 §4.6) なので per-SP 分離と整合する。
pub fn db_data_dir_for_project(slug: &str) -> PathBuf {
    db_root().join(format!("sp_{}", slug))
}

/// VP のデータベースクライアント
///
/// `Surreal<Any>` を使うことで embedded (surrealkv) と kv-mem (テスト) の両方に対応。
pub struct VpDb {
    db: Surreal<Any>,
}

/// Arc でラップした VpDb（複数コンポーネントで共有するため）
pub type SharedVpDb = Arc<VpDb>;

impl VpDb {
    /// ローカルファイルシステム上の surrealkv DB を開いて接続する
    ///
    /// - `data_dir` が無ければ作成
    /// - 認証なし (in-process のみアクセス可能なのでパスワード不要)
    pub async fn connect_embedded(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| anyhow::anyhow!("DB data dir 作成失敗 ({}): {}", data_dir.display(), e))?;
        let endpoint = format!("surrealkv://{}", data_dir.display());
        let db = surrealdb::engine::any::connect(&endpoint)
            .await
            .map_err(|e| anyhow::anyhow!("SurrealDB embedded 接続失敗 ({}): {}", endpoint, e))?;
        db.use_ns(NS).use_db(DB_NAME).await?;
        tracing::info!("SurrealDB 接続成功 (embedded: {})", endpoint);
        Ok(Self { db })
    }

    /// kv-mem (in-memory) で接続（テスト用、 integration test からも利用可）
    ///
    /// VP-174 (Phase 3 PR-2) で `#[cfg(test)]` 除去、 integration test (= `tests/*.rs`) からも
    /// 呼べるように pub 化。 production code は `connect_embedded` を使う。
    pub async fn connect_mem() -> Result<Self> {
        let db = surrealdb::engine::any::connect("mem://").await?;
        db.use_ns(NS).use_db(DB_NAME).await?;
        Ok(Self { db })
    }

    /// スキーマを定義（全テーブル）
    ///
    /// 冪等: 既にテーブルが存在しても安全に実行できる。
    /// `.check()` で各ステートメントのエラーも検出する。
    pub async fn define_schema(&self) -> Result<()> {
        self.db
            .query(SCHEMA_SQL)
            .await
            .map_err(|e| anyhow::anyhow!("スキーマ定義失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("スキーマ定義エラー: {}", e))?;
        tracing::info!("SurrealDB スキーマ定義完了");
        Ok(())
    }

    /// ヘルスチェック（DB に接続できているか確認）
    pub async fn health(&self) -> bool {
        self.db.query("RETURN true").await.is_ok()
    }

    /// 内部の Surreal への参照を取得
    pub fn inner(&self) -> &Surreal<Any> {
        &self.db
    }

    // VP-188: Projects CRUD は撤去。 registered projects の SSOT は embedded DB から
    // `~/.config/vp/projects.kdl` に移行 (= VP-182 の「DB dir 変更で projects 消失」
    // regression を構造的に解消、 council 2026-05-16)。 projects 永続化は
    // `crate::projects_file::ProjectsFile` が担う。

    // =========================================================================
    // Processes CRUD
    // =========================================================================

    /// 稼働中プロセスを登録（UPSERT）
    pub async fn upsert_process(
        &self,
        project_path: &str,
        project_name: &str,
        port: u16,
        pid: u32,
        status: &str,
        tmux_session: Option<&str>,
    ) -> Result<()> {
        self.db
            .query(
                "INSERT INTO processes {
                    project_path: $project_path,
                    project_name: $project_name,
                    port: $port,
                    pid: $pid,
                    status: $status,
                    started_at: time::now(),
                    tmux_session: $tmux_session
                } ON DUPLICATE KEY UPDATE
                    project_name = $input.project_name,
                    port = $input.port,
                    pid = $input.pid,
                    status = $input.status,
                    tmux_session = $input.tmux_session",
            )
            .bind(("project_path", project_path.to_string()))
            .bind(("project_name", project_name.to_string()))
            .bind(("port", port as i64))
            .bind(("pid", pid as i64))
            .bind(("status", status.to_string()))
            .bind(("tmux_session", tmux_session.map(|s| s.to_string())))
            .await
            .map_err(|e| anyhow::anyhow!("process upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("process upsert エラー: {}", e))?;
        Ok(())
    }

    /// プロセスを登録解除（project_path で特定）
    pub async fn delete_process(&self, project_path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM processes WHERE project_path = $path")
            .bind(("path", project_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("process 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("process 削除エラー: {}", e))?;
        Ok(())
    }

    /// 稼働中プロセス一覧を取得
    pub async fn list_processes(&self) -> Result<Vec<serde_json::Value>> {
        let mut result = self
            .db
            .query("SELECT * FROM processes")
            .await
            .map_err(|e| anyhow::anyhow!("processes 取得失敗: {}", e))?;
        let records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records)
    }

    // =========================================================================
    // Active lane (presence、 Model Q): project ごとの選択中 lane を daemon-canonical に
    // =========================================================================

    /// active lane を upsert (project_path → lane_address)。
    pub async fn upsert_active_lane(&self, project_path: &str, lane_address: &str) -> Result<()> {
        self.db
            .query(
                "INSERT INTO active_lane {
                    project_path: $project_path,
                    lane_address: $lane_address,
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    lane_address = $input.lane_address,
                    updated_at = time::now()",
            )
            .bind(("project_path", project_path.to_string()))
            .bind(("lane_address", lane_address.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("active_lane upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("active_lane upsert エラー: {}", e))?;
        Ok(())
    }

    /// 全 active lane を (project_path, lane_address) で返す (boot 時の load 用)。
    pub async fn list_active_lanes(&self) -> Result<Vec<(String, String)>> {
        // list_processes と同じく serde_json::Value で受ける (surrealdb の SurrealValue 制約回避)。
        let mut result = self
            .db
            .query("SELECT project_path, lane_address FROM active_lane")
            .await
            .map_err(|e| anyhow::anyhow!("active_lane 取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        Ok(rows
            .into_iter()
            .filter_map(|v| {
                let path = v.get("project_path")?.as_str()?.to_string();
                let addr = v.get("lane_address")?.as_str()?.to_string();
                Some((path, addr))
            })
            .collect())
    }

    /// active lane を削除する (project remove 時の presence 回収、 §4.6 含有=所有=寿命)。
    pub async fn delete_active_lane(&self, project_path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM active_lane WHERE project_path = $path")
            .bind(("path", project_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("active_lane 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("active_lane 削除エラー: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Lane descriptor (doc 24 §10 Phase 2: LanePool authority 反転 SP→daemon)。
    // SP push の cache だった lane_registry を daemon-canonical な durable truth に。
    // SP disconnect では drop せず、 daemon 再起動は db から re-animate する (§3.3 / §4.1)。
    // =========================================================================

    /// 1 lane descriptor を upsert する (SP の Diff::Add / Diff::Update 反映)。
    ///
    /// (project_path, address) 複合 key で一意。 ON DUPLICATE の composite 挙動に依存せず、
    /// DELETE→CREATE を 1 query (= 中間状態を他読みに晒さない) で行う。 info は LaneInfo を
    /// 丸ごと JSON object 化して持つ (descriptor truth)。
    pub async fn upsert_lane(
        &self,
        project_path: &str,
        lane: &crate::process::lanes_state::LaneInfo,
    ) -> Result<()> {
        let address = lane.address.to_string();
        let descriptor = serde_json::to_value(lane)
            .map_err(|e| anyhow::anyhow!("lane descriptor serialize 失敗: {}", e))?;
        self.db
            .query(
                "DELETE lane WHERE project_path = $p AND address = $a;
                 CREATE lane CONTENT {
                    project_path: $p,
                    address: $a,
                    descriptor: $descriptor,
                    updated_at: time::now()
                 }",
            )
            .bind(("p", project_path.to_string()))
            .bind(("a", address))
            .bind(("descriptor", descriptor))
            .await
            .map_err(|e| anyhow::anyhow!("lane upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane upsert エラー: {}", e))?;
        Ok(())
    }

    /// 1 lane descriptor を削除する (SP の Diff::Remove 反映 / 単一 lane の destroy)。
    pub async fn delete_lane(&self, project_path: &str, address: &str) -> Result<()> {
        self.db
            .query("DELETE lane WHERE project_path = $p AND address = $a")
            .bind(("p", project_path.to_string()))
            .bind(("a", address.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("lane 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane 削除エラー: {}", e))?;
        Ok(())
    }

    /// 1 project の lane descriptor を全削除する (project remove 時の回収、 §4.6 含有=所有=寿命)。
    pub async fn delete_lanes_for_project(&self, project_path: &str) -> Result<()> {
        self.db
            .query("DELETE lane WHERE project_path = $p")
            .bind(("p", project_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("project lane 全削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("project lane 全削除エラー: {}", e))?;
        Ok(())
    }

    /// 1 project の lane descriptor を snapshot で全置換する (SP register snapshot 反映)。
    ///
    /// snapshot は「その時点の SP の全 lane」なので、 既存を消してから入れ直す project 単位
    /// replace 型 (active_lane の高頻度 1 行 upsert と違い、 lane は集合なので全置換が自然)。
    pub async fn replace_lanes_for_project(
        &self,
        project_path: &str,
        lanes: &[crate::process::lanes_state::LaneInfo],
    ) -> Result<()> {
        self.delete_lanes_for_project(project_path).await?;
        for lane in lanes {
            self.upsert_lane(project_path, lane).await?;
        }
        Ok(())
    }

    /// 全 lane descriptor を (project_path, LaneInfo) で返す (boot 時の load 用)。
    ///
    /// list_processes と同じく serde_json::Value で受け、 info object を LaneInfo に
    /// deserialize する。 壊れた行は warn して skip (boot を止めない、 §4.6 ゆるやか統治)。
    pub async fn list_lanes(&self) -> Result<Vec<(String, crate::process::lanes_state::LaneInfo)>> {
        let mut result = self
            .db
            .query("SELECT project_path, descriptor FROM lane")
            .await
            .map_err(|e| anyhow::anyhow!("lane 取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        let mut out = Vec::with_capacity(rows.len());
        for v in rows {
            let Some(path) = v.get("project_path").and_then(|x| x.as_str()) else {
                continue;
            };
            let Some(desc_val) = v.get("descriptor") else {
                continue;
            };
            match serde_json::from_value::<crate::process::lanes_state::LaneInfo>(desc_val.clone())
            {
                Ok(info) => out.push((path.to_string(), info)),
                Err(e) => tracing::warn!("lane descriptor deserialize 失敗 (skip): {}", e),
            }
        }
        Ok(out)
    }

    // =========================================================================
    // Lane lifecycle (doc 24 §4.6: durable lifecycle state machine、 軽量 WAL)。
    // descriptor (lane table) とは別 table — SP push に clobber されない daemon-internal。
    // =========================================================================

    /// lane の lifecycle を upsert する (provisioning / ready / dead)。
    ///
    /// team-b #2: active_lane が `INSERT ON DUPLICATE KEY UPDATE` (単一 key) なのに対し、 lane 系は
    /// **複合 key (project_path, address)** で ON DUPLICATE の発火が不確実なため DELETE+CREATE を使う
    /// (upsert_lane / lane table と同方針)。 2 statement は単一 `query()` = 1 transaction で
    /// atomic に走る (DELETE 後 CREATE 前に row が消える窓は無い)。
    pub async fn upsert_lane_lifecycle(
        &self,
        project_path: &str,
        address: &str,
        lifecycle: &str,
    ) -> Result<()> {
        self.db
            .query(
                "DELETE lane_lifecycle WHERE project_path = $p AND address = $a;
                 CREATE lane_lifecycle CONTENT {
                    project_path: $p,
                    address: $a,
                    lifecycle: $lc,
                    updated_at: time::now()
                 }",
            )
            .bind(("p", project_path.to_string()))
            .bind(("a", address.to_string()))
            .bind(("lc", lifecycle.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("lane_lifecycle upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane_lifecycle upsert エラー: {}", e))?;
        Ok(())
    }

    /// 全 lane lifecycle を (project_path, address, lifecycle) で返す (boot reconcile 用)。
    pub async fn list_lane_lifecycles(&self) -> Result<Vec<(String, String, String)>> {
        let mut result = self
            .db
            .query("SELECT project_path, address, lifecycle FROM lane_lifecycle")
            .await
            .map_err(|e| anyhow::anyhow!("lane_lifecycle 取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        Ok(rows
            .into_iter()
            .filter_map(|v| {
                let p = v.get("project_path")?.as_str()?.to_string();
                let a = v.get("address")?.as_str()?.to_string();
                let lc = v.get("lifecycle")?.as_str()?.to_string();
                Some((p, a, lc))
            })
            .collect())
    }

    /// 1 lane の lifecycle を削除 (lane destroy / lifecycle 回収)。
    pub async fn delete_lane_lifecycle(&self, project_path: &str, address: &str) -> Result<()> {
        self.db
            .query("DELETE lane_lifecycle WHERE project_path = $p AND address = $a")
            .bind(("p", project_path.to_string()))
            .bind(("a", address.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("lane_lifecycle 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane_lifecycle 削除エラー: {}", e))?;
        Ok(())
    }

    /// 1 project の lane lifecycle を全削除 (project remove 時の回収、 §4.6 含有=所有=寿命)。
    pub async fn delete_lane_lifecycles_for_project(&self, project_path: &str) -> Result<()> {
        self.db
            .query("DELETE lane_lifecycle WHERE project_path = $p")
            .bind(("p", project_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("project lane_lifecycle 全削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("project lane_lifecycle 全削除エラー: {}", e))?;
        Ok(())
    }

    /// 全プロセスを削除（TheWorld 再起動時のクリーンアップ用）
    pub async fn clear_all_processes(&self) -> Result<()> {
        self.db
            .query("DELETE FROM processes")
            .await
            .map_err(|e| anyhow::anyhow!("processes クリア失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("processes クリアエラー: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Projects CRUD（PoC: VP-188 revert、 db/world 真実源 + projects.kdl 一方向 export）
    // =========================================================================

    /// 登録 project を UPSERT（path で一意）。 ord = sidebar 並び順。
    pub async fn upsert_project(
        &self,
        path: &str,
        name: &str,
        enabled: Option<bool>,
        slot: Option<u16>,
        ord: i64,
    ) -> Result<()> {
        self.db
            .query(
                "INSERT INTO projects {
                    path: $path,
                    name: $name,
                    enabled: $enabled,
                    slot: $slot,
                    ord: $ord
                } ON DUPLICATE KEY UPDATE
                    name = $input.name,
                    enabled = $input.enabled,
                    slot = $input.slot,
                    ord = $input.ord",
            )
            .bind(("path", path.to_string()))
            .bind(("name", name.to_string()))
            .bind(("enabled", enabled))
            .bind(("slot", slot.map(|s| s as i64)))
            .bind(("ord", ord))
            .await
            .map_err(|e| anyhow::anyhow!("project upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("project upsert エラー: {}", e))?;
        Ok(())
    }

    /// 登録 project を削除（path で特定）。
    pub async fn delete_project(&self, path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM projects WHERE path = $path")
            .bind(("path", path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("project 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("project 削除エラー: {}", e))?;
        Ok(())
    }

    /// 登録 project 一覧を ord 昇順（= sidebar 並び順）で取得。
    pub async fn list_projects(&self) -> Result<Vec<serde_json::Value>> {
        let mut result = self
            .db
            .query("SELECT * FROM projects ORDER BY ord ASC")
            .await
            .map_err(|e| anyhow::anyhow!("projects 取得失敗: {}", e))?;
        let records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records)
    }

    /// DB の projects を ProjectEntry 列に export（ord 昇順、 PoC: 一方向 export）。
    pub async fn export_projects(&self) -> Result<Vec<crate::projects_file::ProjectEntry>> {
        let rows = self.list_projects().await?;
        Ok(rows
            .iter()
            .map(|v| crate::projects_file::ProjectEntry {
                name: v
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string(),
                path: v
                    .get("path")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string(),
                enabled: v.get("enabled").and_then(|x| x.as_bool()),
                slot: v.get("slot").and_then(|x| x.as_u64()).map(|n| n as u16),
            })
            .collect())
    }

    /// ProjectEntry 列を DB に import（出現順を ord に焼く、 PoC: 復旧用）。
    pub async fn import_projects(
        &self,
        entries: &[crate::projects_file::ProjectEntry],
    ) -> Result<()> {
        for (i, e) in entries.iter().enumerate() {
            self.upsert_project(&e.path, &e.name, e.enabled, e.slot, i as i64)
                .await?;
        }
        Ok(())
    }

    /// projects テーブルを `entries` で全置換する（DELETE → import、 ord = 出現順）。
    ///
    /// `persist_projects` の全置換セマンティクスを 1 メソッドに閉じる。 in-memory を真実源として
    /// DB を上書きするため、 in-memory から消えた project は DB からも消える (= upsert のみでは
    /// 残ってしまう削除分を確実に反映)。
    ///
    /// DELETE と import の間に空を読む窓が理論上あるが、 World は単一プロセスで reload/persist を
    /// 直列実行するため実害なし。 完全な単一トランザクション化は follow-up (epic memory のリスク表)。
    pub async fn replace_all_projects(
        &self,
        entries: &[crate::projects_file::ProjectEntry],
    ) -> Result<()> {
        self.db
            .query("DELETE FROM projects")
            .await
            .map_err(|e| anyhow::anyhow!("projects 全削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("projects 全削除エラー: {}", e))?;
        self.import_projects(entries).await
    }

    // =========================================================================
    // Pane Contents CRUD（Canvas ペイン状態の永続化）
    // =========================================================================

    /// ペイン状態を保存（UPSERT: project_path + pane_id で一意）
    pub async fn upsert_pane_content(
        &self,
        project_path: &str,
        pane_id: &str,
        content_type: &str,
        content: &str,
        title: Option<&str>,
    ) -> Result<()> {
        // lane_name='' (= conductor sentinel) の row として upsert。 新 schema (lane_name/stack/ui_state) は
        // ON DUPLICATE KEY UPDATE 句で **触らない** — 旧 caller (= 純粋な content / title 更新)
        // が PP Canvas Stack の stack / ui_state を巻き戻さないようにする。
        self.db
            .query(
                "INSERT INTO pane_contents {
                    project_path: $project_path,
                    pane_id: $pane_id,
                    lane_name: '',
                    content_type: $content_type,
                    content: $content,
                    title: $title,
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    content_type = $input.content_type,
                    content = $input.content,
                    title = $input.title,
                    updated_at = time::now()",
            )
            .bind(("project_path", project_path.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("content_type", content_type.to_string()))
            .bind(("content", content.to_string()))
            .bind(("title", title.map(|s| s.to_string())))
            .await
            .map_err(|e| anyhow::anyhow!("pane_content upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("pane_content upsert エラー: {}", e))?;
        Ok(())
    }

    /// PP Canvas Stack Model の lane scope な永続状態を upsert する (= doc 19 + pp-content-persist)。
    ///
    /// - `lane_name`: None なら conductor (= 内部で `''` sentinel)、 Some(name) なら performer。 UNIQUE INDEX は
    ///   (project_path, lane_name, pane_id) のため conductor/performer は別 record として独立。
    /// - `stack`: Canvas Stack (= items + cursor + capacity)。 None なら未保存。
    /// - `ui_state`: visibility/collapsed/サイズ等。 None なら未保存。
    /// - `content` / `content_type` / `title` は **現在 main pane で render 中の item の reflection**
    ///   (= 旧 caller 互換)。 stack が主、 content は seek 用 fallback。
    #[allow(clippy::too_many_arguments)] // pane_contents の field count に追従、 caller (route handler) も flat に展開する
    pub async fn upsert_pp_state(
        &self,
        project_path: &str,
        lane_name: Option<&str>,
        pane_id: &str,
        content_type: &str,
        content: &str,
        title: Option<&str>,
        stack: Option<&serde_json::Value>,
        ui_state: Option<&serde_json::Value>,
    ) -> Result<()> {
        // IPC contract 上は lane_name: Option<&str> を維持しつつ、 DB row では '' sentinel に変換。
        let lane_sentinel = lane_name.unwrap_or("");
        self.db
            .query(
                "INSERT INTO pane_contents {
                    project_path: $project_path,
                    pane_id: $pane_id,
                    lane_name: $lane_name,
                    content_type: $content_type,
                    content: $content,
                    title: $title,
                    stack: $stack,
                    ui_state: $ui_state,
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    content_type = $input.content_type,
                    content = $input.content,
                    title = $input.title,
                    stack = $input.stack,
                    ui_state = $input.ui_state,
                    updated_at = time::now()",
            )
            .bind(("project_path", project_path.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("lane_name", lane_sentinel.to_string()))
            .bind(("content_type", content_type.to_string()))
            .bind(("content", content.to_string()))
            .bind(("title", title.map(|s| s.to_string())))
            .bind(("stack", stack.cloned()))
            .bind(("ui_state", ui_state.cloned()))
            .await
            .map_err(|e| anyhow::anyhow!("pp_state upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("pp_state upsert エラー: {}", e))?;
        Ok(())
    }

    /// 特定 (project_path, lane_name, pane_id) の PP state を 1 件取得。 不在なら Ok(None)。
    ///
    /// 旧 record (= lane_name field なし) は schema DEFAULT '' で self-heal され、 conductor として読める。
    pub async fn load_pp_state(
        &self,
        project_path: &str,
        lane_name: Option<&str>,
        pane_id: &str,
    ) -> Result<Option<serde_json::Value>> {
        let lane_sentinel = lane_name.unwrap_or("");
        let mut result = self
            .db
            .query(
                "SELECT * FROM pane_contents
                 WHERE project_path = $path
                   AND pane_id = $pane_id
                   AND lane_name = $lane
                 LIMIT 1",
            )
            .bind(("path", project_path.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("lane", lane_sentinel.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("pp_state load 失敗: {}", e))?;
        let mut records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records.pop())
    }

    /// プロジェクトの全ペイン状態を取得
    pub async fn list_pane_contents(&self, project_path: &str) -> Result<Vec<serde_json::Value>> {
        let mut result = self
            .db
            .query("SELECT * FROM pane_contents WHERE project_path = $path")
            .bind(("path", project_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("pane_contents 取得失敗: {}", e))?;
        let records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records)
    }

    /// プロジェクトの全ペイン状態を削除
    pub async fn clear_pane_contents(&self, project_path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM pane_contents WHERE project_path = $path")
            .bind(("path", project_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("pane_contents 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("pane_contents 削除エラー: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Stand Status CRUD
    // =========================================================================

    /// Stand ステータスを更新（UPSERT）
    pub async fn upsert_stand_status(
        &self,
        project_path: &str,
        stand_key: &str,
        status: &str,
        detail: Option<&serde_json::Value>,
    ) -> Result<()> {
        self.db
            .query(
                "INSERT INTO stand_status {
                    project_path: $project_path,
                    stand_key: $stand_key,
                    status: $status,
                    detail: $detail,
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    status = $input.status,
                    detail = $input.detail,
                    updated_at = time::now()",
            )
            .bind(("project_path", project_path.to_string()))
            .bind(("stand_key", stand_key.to_string()))
            .bind(("status", status.to_string()))
            .bind(("detail", detail.cloned()))
            .await
            .map_err(|e| anyhow::anyhow!("stand_status upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("stand_status upsert エラー: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // LIVE SELECT（リアルタイム変更通知）
    // =========================================================================

    /// processes テーブルの LIVE SELECT を開始
    ///
    /// INSERT/UPDATE/DELETE のたびに `Notification<serde_json::Value>` を返すストリーム。
    /// TheWorld が購読して DistributedNotification に変換する。
    ///
    /// 返り値は `'static` ライフタイム（`Surreal<Any>` は内部 Arc なので clone が軽量）。
    pub async fn live_processes(
        &self,
    ) -> Result<surrealdb::method::Stream<Vec<serde_json::Value>>> {
        let stream = self
            .db
            .select("processes")
            .live()
            .await
            .map_err(|e| anyhow::anyhow!("LIVE SELECT processes 失敗: {}", e))?;
        Ok(stream)
    }

    /// プロジェクトの全 Stand ステータスを取得
    pub async fn list_stand_status(&self, project_path: &str) -> Result<Vec<serde_json::Value>> {
        let mut result = self
            .db
            .query("SELECT * FROM stand_status WHERE project_path = $path")
            .bind(("path", project_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("stand_status 取得失敗: {}", e))?;
        let records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records)
    }
}

// =============================================================================
// スキーマ定義 SQL
// =============================================================================

/// 全テーブルのスキーマ定義（冪等）
const SCHEMA_SQL: &str = r#"
-- =========================================================================
-- グローバルテーブル
-- =========================================================================

-- プロセス状態（QUIC Registry + HTTP polling 代替）
DEFINE TABLE IF NOT EXISTS processes SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS project_path ON processes TYPE string;
DEFINE FIELD IF NOT EXISTS project_name ON processes TYPE string;
DEFINE FIELD IF NOT EXISTS port ON processes TYPE int;
DEFINE FIELD IF NOT EXISTS pid ON processes TYPE int;
DEFINE FIELD IF NOT EXISTS status ON processes TYPE string;
DEFINE FIELD IF NOT EXISTS started_at ON processes TYPE datetime;
DEFINE FIELD IF NOT EXISTS stands ON processes TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS tmux_session ON processes TYPE option<string>;
DEFINE INDEX IF NOT EXISTS idx_processes_path ON processes COLUMNS project_path UNIQUE;

-- registered projects (PoC: VP-188 を revert し DB 真実源へ戻す)。
-- 当時 council (2026-05-16) が file に逃した理由は VP-182 (surrealkv の OS 排他
-- ロックで DB dir を分離 → DB dir 変更で projects 消失)。 本 PoC の仮説:
--   ① projects を **World 専用 DB (db/world) に限定** すれば SP は触らず LOCK 衝突なし
--   ② DB 消失耐性 + 人間可読性は projects.kdl への **一方向 export** で担保
-- ord = sidebar 並び順 (projects.kdl の node 出現順を保持)。
DEFINE TABLE IF NOT EXISTS projects SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS path ON projects TYPE string;
DEFINE FIELD IF NOT EXISTS name ON projects TYPE string;
DEFINE FIELD IF NOT EXISTS enabled ON projects TYPE option<bool>;
DEFINE FIELD IF NOT EXISTS slot ON projects TYPE option<int>;
DEFINE FIELD IF NOT EXISTS ord ON projects TYPE int DEFAULT 0;
DEFINE INDEX IF NOT EXISTS idx_projects_path ON projects COLUMNS path UNIQUE;

-- active lane (presence、 Model Q): project ごとの選択中 lane。 daemon-canonical。
-- presence なので projects とは別テーブル (projects.kdl export に混ぜず、 click ごとの
-- 高頻度 upsert を 1 行に閉じる)。 §4.6 durability tier: presence は tail-loss 許容。
DEFINE TABLE IF NOT EXISTS active_lane SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS project_path ON active_lane TYPE string;
DEFINE FIELD IF NOT EXISTS lane_address ON active_lane TYPE string;
DEFINE FIELD IF NOT EXISTS updated_at ON active_lane TYPE datetime;
DEFINE INDEX IF NOT EXISTS idx_active_lane_path ON active_lane COLUMNS project_path UNIQUE;

-- lane descriptor (doc 24 §10 Phase 2: LanePool authority を SP→daemon に反転)。
-- 旧来 lane_registry は「SP push の in-memory cache、 SP disconnect で全 drop」だったが、
-- これを daemon-canonical な **durable truth** にする。 SP が落ちても descriptor は残り
-- (§4.1 app quit = 喪失ゼロ)、 daemon 再起動は db から re-animate する (§3.3)。
--   descriptor = LaneInfo を丸ごと持つ FLEXIBLE object (descriptor truth、 pane_contents.stack 前例)。
--     (列名 `info` は SurrealQL 予約語 `INFO` と衝突するため `descriptor` を使う)
--   key       = (project_path, address) 複合 UNIQUE (1 project 内で lane address は一意)。
-- §4.6 durability tier: descriptor は堅く durable / live 値 (pid/state) は projection なので
-- boot-load 値が stale でも SP reconnect の snapshot が上書きする (= 正直な tier 分け)。
DEFINE TABLE IF NOT EXISTS lane SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS project_path ON lane TYPE string;
DEFINE FIELD IF NOT EXISTS address ON lane TYPE string;
DEFINE FIELD IF NOT EXISTS descriptor ON lane TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS updated_at ON lane TYPE datetime;
DEFINE INDEX IF NOT EXISTS idx_lane_addr ON lane COLUMNS project_path, address UNIQUE;

-- lane lifecycle (doc 24 §4.6: daemon 堅牢化の durable lifecycle state machine = 軽量 WAL)。
-- provisioning / ready / dead を **descriptor (lane table) とは別テーブル** に持つ。 分離理由:
-- descriptor は SP が push で round-trip するため、 SP snapshot (lifecycle 未知=default) が
-- daemon の `provisioning` intent を clobber してしまう。 lifecycle は daemon-internal な
-- crash-recovery state なので、 active_lane (presence) と同じく独立 table にする。
-- process liveness (LaneInfo.state) とも別軸 (= ground の lifecycle、 PtySlot の生死ではない)。
-- intent-first bracket: create は descriptor+provisioning を先に書く → worktree provision →
-- ready。 crash で provisioning が残れば boot reconcile が ground 存在で heal する。
DEFINE TABLE IF NOT EXISTS lane_lifecycle SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS project_path ON lane_lifecycle TYPE string;
DEFINE FIELD IF NOT EXISTS address ON lane_lifecycle TYPE string;
DEFINE FIELD IF NOT EXISTS lifecycle ON lane_lifecycle TYPE string;
DEFINE FIELD IF NOT EXISTS updated_at ON lane_lifecycle TYPE datetime;
DEFINE INDEX IF NOT EXISTS idx_lane_lifecycle_addr ON lane_lifecycle COLUMNS project_path, address UNIQUE;

-- wiremsg R6: 旧 msgbox table (VP-169 以前の cross-process メッセージング) は撤去。
-- agent 間通信は wiremsg (下記 wire_messages table) に一本化済。
-- R5-3 で VP-169 msgs table、 R6 で本 table を撤去し msgbox 系が完全消滅した。

-- =========================================================================
-- SP 固有テーブル（project_path でフィルタ — D11 準拠）
-- =========================================================================

-- Canvas ペイン状態（PP Canvas Stack Model 永続化、 doc 19）
--
-- 2026-05-28 [pp-content-persist]:
--   lane scope 対応 — 旧 idx_pane (project_path, pane_id) を
--   (project_path, lane_name, pane_id) UNIQUE に置換。 lane_name="" が conductor、
--   "<name>" が performer。 同一 project の conductor と performer は **独立した PP state** を持つ。
--   追加 field:
--     - lane_name: string DEFAULT ''       — lane scope key (空文字=conductor / 非空=performer 名)
--     - stack:     option<object> FLEXIBLE — Canvas Stack { items: [], cursor: id, capacity: 10 }
--     - ui_state:  option<object> FLEXIBLE — { visible, collapsed, width, height }
--   注: lane_name を **option ではなく DEFAULT 空文字** にしたのは、 SurrealDB の UNIQUE INDEX が
--   NULL 同士を不一致扱いし、 (path, NONE, pane_id) の UNIQUE 制約が成立せず ON DUPLICATE が
--   発火しないため。 IPC contract 上は lane: string|null を保ち、 Rust 側で null↔'' を変換。
--   旧 record (lane_name 不在) は schema DEFAULT '' で self-heal してそのまま conductor 扱いになる。
REMOVE INDEX IF EXISTS idx_pane ON pane_contents;
DEFINE TABLE IF NOT EXISTS pane_contents SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS project_path ON pane_contents TYPE string;
DEFINE FIELD IF NOT EXISTS pane_id ON pane_contents TYPE string;
DEFINE FIELD IF NOT EXISTS content_type ON pane_contents TYPE string;
DEFINE FIELD IF NOT EXISTS content ON pane_contents TYPE string;
DEFINE FIELD IF NOT EXISTS title ON pane_contents TYPE option<string>;
DEFINE FIELD IF NOT EXISTS lane_name ON pane_contents TYPE string DEFAULT '';
DEFINE FIELD IF NOT EXISTS stack ON pane_contents TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS ui_state ON pane_contents TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS updated_at ON pane_contents TYPE datetime DEFAULT time::now();
DEFINE INDEX IF NOT EXISTS idx_pane_lane ON pane_contents COLUMNS project_path, lane_name, pane_id UNIQUE;

-- Stand ステータス
DEFINE TABLE IF NOT EXISTS stand_status SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS project_path ON stand_status TYPE string;
DEFINE FIELD IF NOT EXISTS stand_key ON stand_status TYPE string;
DEFINE FIELD IF NOT EXISTS status ON stand_status TYPE string;
DEFINE FIELD IF NOT EXISTS detail ON stand_status TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS updated_at ON stand_status TYPE datetime DEFAULT time::now();
DEFINE INDEX IF NOT EXISTS idx_stand ON stand_status COLUMNS project_path, stand_key UNIQUE;

-- User Prompt（2秒ポーリング廃止）
DEFINE TABLE IF NOT EXISTS prompts SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS project_path ON prompts TYPE string;
DEFINE FIELD IF NOT EXISTS request_id ON prompts TYPE string;
DEFINE FIELD IF NOT EXISTS prompt_type ON prompts TYPE string;
DEFINE FIELD IF NOT EXISTS title ON prompts TYPE string;
DEFINE FIELD IF NOT EXISTS description ON prompts TYPE option<string>;
DEFINE FIELD IF NOT EXISTS options ON prompts TYPE option<array>;
DEFINE FIELD IF NOT EXISTS timeout_seconds ON prompts TYPE int;
DEFINE FIELD IF NOT EXISTS response ON prompts TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS created_at ON prompts TYPE datetime DEFAULT time::now();
DEFINE INDEX IF NOT EXISTS idx_request ON prompts COLUMNS request_id UNIQUE;

-- CC 通知（DistributedNotification 代替）
DEFINE TABLE IF NOT EXISTS notifications SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS project_path ON notifications TYPE string;
DEFINE FIELD IF NOT EXISTS project_name ON notifications TYPE string;
DEFINE FIELD IF NOT EXISTS message ON notifications TYPE string;
DEFINE FIELD IF NOT EXISTS read ON notifications TYPE bool DEFAULT false;
DEFINE FIELD IF NOT EXISTS created_at ON notifications TYPE datetime DEFAULT time::now();

-- =========================================================================
-- wiremsg R5-3: 旧 VP-169 msgs table (Whitesnake-primary msgbox) は撤去。
-- msg messaging は下記 wiremsg threaded inbox (messages table) に一本化済。
-- =========================================================================

-- =========================================================================
-- wiremsg threaded inbox (Phase A ① / R1、 設計 memory mem_1CbDLrECNZiNEZqjySLfSB)
-- =========================================================================
-- 既存 msgs table (= Mailbox の claim-based inbox) と並走する threading 対応 inbox。
-- `wire_send` / `wire_recv` が直接 long-poll する store。 TopicRouter は介さない。
--
-- 設計判断: `prev` は record link ではなく plain string (= message の local id) で
-- 保持する。 理由:
--   1. 既存 msgs table の `id` / `reply_to` も plain string で、 同型を踏襲
--   2. record-link traversal を query で使うと migration / 部分適用で壊れやすい
--      (creo-memories mem: 「migration の data-UPDATE 句は record-link traversal を避ける」)
-- `created_at` も datetime ではなく epoch ms (number) で保持
-- (= msgs.ts と同じ表現、 thread 内表示順の比較を素直な数値比較にする)。
--
-- R1 (決定 thread_id 全廃 / cursor local-seq 化):
--   - `thread_id` field を全廃。 thread 構造は `prev` (parent-pointer forest) 一本。
--     thread の識別子が要る場面では root message の id (`prev` を辿った先) を使う。
--   - `local_seq` を追加。 ローカル accumulation の厳密単調 ingestion 順序 (number)。
--     各 SP は自分の accumulation の唯一の writer なので厳密単調。 cursor 比較は
--     この `local_seq` で行う (`created_at` は同一 ms 衝突や clock skew で取りこぼす)。
-- 既存 DB の旧 schema 残骸を除去 (thread_id field / wire_thread_idx index)。
-- wiremsg は Phase A 新設で deployed data はごく僅か。
REMOVE INDEX IF EXISTS wire_thread_idx ON wire_messages;
REMOVE FIELD IF EXISTS thread_id ON wire_messages;
DEFINE TABLE IF NOT EXISTS wire_messages SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS id ON wire_messages TYPE string;
DEFINE FIELD IF NOT EXISTS prev ON wire_messages TYPE option<string>;
DEFINE FIELD IF NOT EXISTS from_addr ON wire_messages TYPE string;
DEFINE FIELD IF NOT EXISTS to_addrs ON wire_messages TYPE array<string>;
DEFINE FIELD IF NOT EXISTS body ON wire_messages TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS created_at ON wire_messages TYPE number;
-- ローカル accumulation の厳密単調 ingestion 順序。 cursor 比較の基準。
DEFINE FIELD IF NOT EXISTS local_seq ON wire_messages TYPE number;
-- 主 query path index: 「agent 宛 message を cursor 超過で引く」 (to ベース配送)。
-- 旧 wire_thread_idx (thread_id, created_at) の置き換え (moody #4)。
DEFINE INDEX IF NOT EXISTS wire_to_seq_idx ON wire_messages FIELDS to_addrs, local_seq;

-- per-agent 単一 cursor (決定 III)。 1 agent 1 行 = O(agents)。
-- `last_read` = 最後に読んだ message の local_seq。 NONE = 全 message 未読。
-- 配送は wire_messages.to_addrs から創発する (= to ベース配送)。
DEFINE TABLE IF NOT EXISTS agent_cursor SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS agent ON agent_cursor TYPE string;
DEFINE FIELD IF NOT EXISTS last_read ON agent_cursor TYPE option<number>;
DEFINE FIELD IF NOT EXISTS updated_at ON agent_cursor TYPE number;
DEFINE INDEX IF NOT EXISTS agent_cursor_uniq ON agent_cursor FIELDS agent UNIQUE;

-- thread 参加の sparse 例外表 (決定 III)。 status ∈ {muted, left} の行のみ持つ。
-- default (active) は行を持たない — active 参加は wire_messages.to_addrs から創発。
-- 行数 = O(mute・leave 回数)。
-- R1: `thread` field は thread の root message id (`prev` を辿った先)。
-- thread_id 全廃のため denormalize copy ではなく root id そのものを使う。
DEFINE TABLE IF NOT EXISTS thread_participant SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS thread ON thread_participant TYPE string;
DEFINE FIELD IF NOT EXISTS agent ON thread_participant TYPE string;
DEFINE FIELD IF NOT EXISTS status ON thread_participant TYPE string DEFAULT 'active';
DEFINE FIELD IF NOT EXISTS updated_at ON thread_participant TYPE number;
-- (thread, agent) で一意
DEFINE INDEX IF NOT EXISTS thread_participant_uniq ON thread_participant FIELDS thread, agent UNIQUE;
DEFINE INDEX IF NOT EXISTS thread_participant_agent_idx ON thread_participant FIELDS agent, status;

-- wiremsg R2-a (設計 mem_1CbvcJj4ppU3QKH9d7xMpT 決定 D3): per-message ack 台帳。
-- command category の「読まれた」確認用。cursor (agent_cursor) とは独立で、
-- recv で cursor が進んでも wire_ack されるまで delivery loop (R2-b) の再掲示対象。
DEFINE TABLE IF NOT EXISTS wire_acks SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS message_id ON wire_acks TYPE string;
DEFINE FIELD IF NOT EXISTS agent ON wire_acks TYPE string;
DEFINE FIELD IF NOT EXISTS acked_at ON wire_acks TYPE number;
DEFINE INDEX IF NOT EXISTS wire_acks_uniq ON wire_acks FIELDS message_id, agent UNIQUE;

-- agent 委譲 (delegation、 doc 28 §4 / §6): durable cross-agent future の World 中央 store。
-- wire と同じく TheWorld の SurrealDB に持つ (= SP 再起動を跨いで生存、 World reconcile の駆動源)。
-- requester / doer は論理 wire address。 state ∈ {pending, active, awaiting_response, done, failed}。
-- outcome = {kind, result|reason|question} (= Outcome の serde 形)。 created_at/updated_at は ms
-- (B reconcile の timeout 判定用)。 delivered = 直近 wake が target に届いたか (B/C の取りこぼし検出用)。
DEFINE TABLE IF NOT EXISTS delegations SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS id ON delegations TYPE string;
DEFINE FIELD IF NOT EXISTS requester ON delegations TYPE string;
DEFINE FIELD IF NOT EXISTS doer ON delegations TYPE string;
DEFINE FIELD IF NOT EXISTS task ON delegations TYPE string;
DEFINE FIELD IF NOT EXISTS state ON delegations TYPE string;
DEFINE FIELD IF NOT EXISTS outcome ON delegations TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS created_at ON delegations TYPE number;
DEFINE FIELD IF NOT EXISTS updated_at ON delegations TYPE number;
DEFINE FIELD IF NOT EXISTS delivered ON delegations TYPE bool DEFAULT false;
DEFINE INDEX IF NOT EXISTS delegations_id_idx ON delegations FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS delegations_state_idx ON delegations FIELDS state;
"#;

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用ヘルパー: kv-mem VpDb をスキーマ付きで作成
    async fn make_test_db() -> VpDb {
        let db = VpDb::connect_mem().await.unwrap();
        db.define_schema().await.unwrap();
        db
    }

    #[test]
    fn test_db_data_dir_world_and_project_separated() {
        let world = db_data_dir_for_world();
        let proj = db_data_dir_for_project("vantage-point");

        // VP-192: 両方とも vp_data_dir()/db 配下
        assert!(
            world.to_string_lossy().contains("db"),
            "World DB dir に 'db' が含まれていない: {}",
            world.display()
        );
        assert!(
            world.starts_with(crate::config::vp_data_dir()),
            "World DB dir は vp_data_dir() 配下であるべき: {}",
            world.display()
        );
        // VP-182: World と SP の DB ディレクトリは別であること (= surrealkv LOCK 衝突回避)
        assert_ne!(
            world, proj,
            "World と project の DB ディレクトリは分離されているべき"
        );
        assert!(
            world.ends_with("world"),
            "World DB dir は 'world' で終わるべき: {}",
            world.display()
        );
        assert!(
            proj.ends_with("sp_vantage-point"),
            "project DB dir は 'sp_<slug>' 形式であるべき: {}",
            proj.display()
        );
    }

    #[test]
    fn test_constants() {
        assert_eq!(NS, "vp");
        assert_eq!(DB_NAME, "vp");
    }

    #[tokio::test]
    async fn test_define_schema_mem() {
        let db = make_test_db().await;
        assert!(db.health().await, "ヘルスチェック失敗");
    }

    #[tokio::test]
    async fn test_active_lane_upsert_and_list() {
        // Model Q: active lane (presence) の daemon-canonical round-trip。
        let db = make_test_db().await;

        // 初期は空
        assert!(db.list_active_lanes().await.unwrap().is_empty());

        // project ごとに upsert
        db.upsert_active_lane("/repos/vp", "vp/conductor")
            .await
            .unwrap();
        db.upsert_active_lane("/repos/nexus", "nexus/performer/foo")
            .await
            .unwrap();

        let mut rows = db.list_active_lanes().await.unwrap();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                (
                    "/repos/nexus".to_string(),
                    "nexus/performer/foo".to_string()
                ),
                ("/repos/vp".to_string(), "vp/conductor".to_string()),
            ]
        );

        // 同 project の upsert は置換 (UNIQUE index、 per-project に 1 つ)
        db.upsert_active_lane("/repos/vp", "vp/performer/bar")
            .await
            .unwrap();
        let rows = db.list_active_lanes().await.unwrap();
        assert_eq!(rows.len(), 2, "同 project は置換、 件数は増えない");
        assert!(rows.contains(&("/repos/vp".to_string(), "vp/performer/bar".to_string())));

        // §4.6 含有=所有=寿命: project remove 時の presence 回収 (delete_active_lane)。
        db.delete_active_lane("/repos/vp").await.unwrap();
        let rows = db.list_active_lanes().await.unwrap();
        assert_eq!(rows.len(), 1, "削除した project の active_lane は消える");
        assert_eq!(rows[0].0, "/repos/nexus", "他 project は残る");
        // 不在 project の削除は no-op (冪等)
        db.delete_active_lane("/repos/absent").await.unwrap();
        assert_eq!(db.list_active_lanes().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_lane_upsert_list_and_delete() {
        use crate::process::lanes_state::{LaneAddress, LaneInfo, LaneKind, LaneState};
        // doc 24 §10 Phase 2: lane descriptor の daemon-canonical durable round-trip。
        let db = make_test_db().await;

        // 初期は空
        assert!(db.list_lanes().await.unwrap().is_empty());

        // テスト用 LaneInfo builder (live 値 pid は埋めるが、 検証は descriptor 中心)。
        let mk = |project: &str, kind: LaneKind, name: Option<&str>| {
            let address = match kind {
                LaneKind::Conductor => LaneAddress::conductor(project),
                LaneKind::Performer => LaneAddress::performer(project, name.unwrap()),
            };
            LaneInfo {
                id: Default::default(),
                address,
                kind,
                name: name.map(|s| s.to_string()),
                state: LaneState::Running,
                stand: "echoes".to_string(),
                created_at: "2026-06-20T00:00:00Z".to_string(),
                pid: Some(1234),
                cwd: "/tmp".to_string(),
                performer_status: None,
                cc_session_id: None,
                tmux: Vec::new(),
            }
        };

        // 2 project に lane を入れる
        db.upsert_lane("/repos/vp", &mk("vp", LaneKind::Conductor, None))
            .await
            .unwrap();
        db.upsert_lane("/repos/vp", &mk("vp", LaneKind::Performer, Some("foo")))
            .await
            .unwrap();
        db.upsert_lane("/repos/nexus", &mk("nexus", LaneKind::Conductor, None))
            .await
            .unwrap();

        let rows = db.list_lanes().await.unwrap();
        assert_eq!(rows.len(), 3, "3 lane descriptor が round-trip する");

        // descriptor が round-trip する (address / stand)
        let vp_conductor = rows
            .iter()
            .find(|(p, l)| p == "/repos/vp" && l.kind == LaneKind::Conductor)
            .expect("vp conductor が読める");
        assert_eq!(vp_conductor.1.address.to_string(), "vp/conductor");
        assert_eq!(vp_conductor.1.stand, "echoes");

        // 同 address の upsert は置換 (複合 UNIQUE、 件数は増えない)
        db.upsert_lane("/repos/vp", &mk("vp", LaneKind::Conductor, None))
            .await
            .unwrap();
        assert_eq!(
            db.list_lanes().await.unwrap().len(),
            3,
            "同 address の upsert は置換"
        );

        // 単一 lane の削除 (Diff::Remove)
        db.delete_lane("/repos/vp", "vp/performer/foo")
            .await
            .unwrap();
        let rows = db.list_lanes().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            !rows
                .iter()
                .any(|(_, l)| l.address.to_string() == "vp/performer/foo"),
            "削除した lane は消える"
        );

        // snapshot 全置換 (register snapshot): /repos/vp を performer 2 つに置換
        db.replace_lanes_for_project(
            "/repos/vp",
            &[
                mk("vp", LaneKind::Performer, Some("a")),
                mk("vp", LaneKind::Performer, Some("b")),
            ],
        )
        .await
        .unwrap();
        let vp_lanes: Vec<_> = db
            .list_lanes()
            .await
            .unwrap()
            .into_iter()
            .filter(|(p, _)| p == "/repos/vp")
            .collect();
        assert_eq!(
            vp_lanes.len(),
            2,
            "snapshot で /repos/vp は 2 lane に全置換"
        );
        assert!(
            vp_lanes.iter().all(|(_, l)| l.kind == LaneKind::Performer),
            "snapshot 後は conductor が消え performer のみ"
        );

        // §4.6 含有=所有=寿命: project remove 時の回収 (delete_lanes_for_project)。
        db.delete_lanes_for_project("/repos/vp").await.unwrap();
        let rows = db.list_lanes().await.unwrap();
        assert_eq!(rows.len(), 1, "削除した project の lane は消える");
        assert_eq!(rows[0].0, "/repos/nexus", "他 project は残る");
    }

    #[tokio::test]
    async fn test_lane_lifecycle_upsert_list_delete() {
        // doc 24 §4.6: lane lifecycle (別 table) の round-trip。
        let db = make_test_db().await;
        assert!(db.list_lane_lifecycles().await.unwrap().is_empty());

        db.upsert_lane_lifecycle("/repos/vp", "vp/performer/foo", "provisioning")
            .await
            .unwrap();
        db.upsert_lane_lifecycle("/repos/vp", "vp/performer/bar", "ready")
            .await
            .unwrap();
        db.upsert_lane_lifecycle("/repos/nexus", "nexus/performer/x", "ready")
            .await
            .unwrap();
        assert_eq!(db.list_lane_lifecycles().await.unwrap().len(), 3);

        // 同 (project, address) の upsert は置換 (複合 UNIQUE)。
        db.upsert_lane_lifecycle("/repos/vp", "vp/performer/foo", "ready")
            .await
            .unwrap();
        let rows = db.list_lane_lifecycles().await.unwrap();
        assert_eq!(rows.len(), 3, "同 address は置換、 件数は増えない");
        assert!(
            rows.iter()
                .any(|(p, a, lc)| p == "/repos/vp" && a == "vp/performer/foo" && lc == "ready"),
            "provisioning → ready に置換される"
        );

        // 単一削除。
        db.delete_lane_lifecycle("/repos/vp", "vp/performer/foo")
            .await
            .unwrap();
        assert_eq!(db.list_lane_lifecycles().await.unwrap().len(), 2);

        // project 単位削除 (§4.6 含有=所有=寿命)。
        db.delete_lane_lifecycles_for_project("/repos/vp")
            .await
            .unwrap();
        let rows = db.list_lane_lifecycles().await.unwrap();
        assert_eq!(rows.len(), 1, "削除した project の lifecycle は消える");
        assert_eq!(rows[0].0, "/repos/nexus", "他 project は残る");
    }

    // VP-188: Projects CRUD テストは撤去 (= projects は projects.kdl に移行、
    // crate::projects_file 側の round-trip test でカバー)。

    // =========================================================================
    // Processes CRUD テスト
    // =========================================================================

    #[tokio::test]
    async fn test_processes_crud() {
        let db = make_test_db().await;

        // 登録
        db.upsert_process("/repos/vp", "vp", 33000, 1234, "running", None)
            .await
            .unwrap();

        // 一覧
        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0]["project_name"], "vp");
        assert_eq!(procs[0]["port"], 33000);

        // 更新（同じ path で upsert）
        db.upsert_process("/repos/vp", "vp", 33001, 5678, "running", Some("vp-vp"))
            .await
            .unwrap();
        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0]["port"], 33001);
        assert_eq!(procs[0]["tmux_session"], "vp-vp");

        // 削除
        db.delete_process("/repos/vp").await.unwrap();
        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 0);
    }

    #[tokio::test]
    async fn test_processes_clear_all() {
        let db = make_test_db().await;

        db.upsert_process("/a", "a", 33000, 1, "running", None)
            .await
            .unwrap();
        db.upsert_process("/b", "b", 33001, 2, "running", None)
            .await
            .unwrap();

        db.clear_all_processes().await.unwrap();
        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 0);
    }

    // =========================================================================
    // define_schema 冪等性テスト
    // =========================================================================

    /// define_schema を2回呼び出しても失敗しない（IF NOT EXISTS を検証）
    #[tokio::test]
    async fn test_define_schema_idempotent() {
        let db = VpDb::connect_mem().await.unwrap();
        // 1回目
        db.define_schema().await.unwrap();
        // 2回目: DEFINE TABLE IF NOT EXISTS / DEFINE FIELD IF NOT EXISTS があるため再定義でもエラーにならない
        db.define_schema()
            .await
            .expect("2回目の define_schema が失敗してはいけない");
        assert!(
            db.health().await,
            "2回目の define_schema 後もヘルスチェックが通る"
        );
    }

    // =========================================================================
    // Processes エッジケーステスト
    // =========================================================================

    /// tmux_session=None を保存 → NULL として保存される
    #[tokio::test]
    async fn test_processes_tmux_session_none() {
        let db = make_test_db().await;

        db.upsert_process("/repos/vp", "vp", 33000, 1234, "running", None)
            .await
            .unwrap();

        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 1);
        // NULL は serde_json::Value::Null として取得される
        assert!(
            procs[0]["tmux_session"].is_null(),
            "tmux_session が NULL でない: {:?}",
            procs[0]["tmux_session"]
        );
    }

    /// 存在しない project_path を delete_process してもエラーにならない
    #[tokio::test]
    async fn test_processes_delete_nonexistent() {
        let db = make_test_db().await;

        // 何も INSERT せずに DELETE → エラーなし（空操作）
        db.delete_process("/repos/nonexistent")
            .await
            .expect("存在しないレコードの削除はエラーにならない");

        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 0);
    }

    // =========================================================================
    // Pane Contents CRUD テスト
    // =========================================================================

    /// 基本的な INSERT → SELECT フロー
    #[tokio::test]
    async fn test_pane_contents_basic_crud() {
        let db = make_test_db().await;

        db.upsert_pane_content(
            "/repos/vp",
            "pane-1",
            "markdown",
            r##"{"Markdown":"# Hello"}"##,
            Some("テストペイン"),
        )
        .await
        .unwrap();

        let panes = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0]["pane_id"], "pane-1");
        assert_eq!(panes[0]["content_type"], "markdown");
        assert_eq!(panes[0]["content"], r##"{"Markdown":"# Hello"}"##);
        assert_eq!(panes[0]["title"], "テストペイン");
    }

    /// 同一 (project_path, pane_id) で再度 upsert → content が更新される（UPSERT 冪等性）
    #[tokio::test]
    async fn test_pane_contents_upsert_updates_content() {
        let db = make_test_db().await;

        db.upsert_pane_content(
            "/repos/vp",
            "pane-1",
            "markdown",
            r#"{"Markdown":"初回内容"}"#,
            Some("初回タイトル"),
        )
        .await
        .unwrap();

        // 同じ pane_id で異なる content
        db.upsert_pane_content(
            "/repos/vp",
            "pane-1",
            "html",
            r#"{"Html":"<h1>更新後</h1>"}"#,
            Some("更新後タイトル"),
        )
        .await
        .unwrap();

        let panes = db.list_pane_contents("/repos/vp").await.unwrap();
        // レコード数は1のまま（UPSERT）
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0]["content_type"], "html");
        assert_eq!(panes[0]["content"], r#"{"Html":"<h1>更新後</h1>"}"#);
        assert_eq!(panes[0]["title"], "更新後タイトル");
    }

    /// 異なる project_path のペインは list_pane_contents で見えない（プロジェクト分離）
    #[tokio::test]
    async fn test_pane_contents_project_isolation() {
        let db = make_test_db().await;

        db.upsert_pane_content(
            "/repos/vp",
            "pane-1",
            "markdown",
            r#"{"Markdown":"VP の内容"}"#,
            None,
        )
        .await
        .unwrap();

        db.upsert_pane_content(
            "/repos/creo",
            "pane-1",
            "markdown",
            r#"{"Markdown":"Creo の内容"}"#,
            None,
        )
        .await
        .unwrap();

        // VP のペイン → VP の内容だけ見える
        let vp_panes = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(vp_panes.len(), 1);
        assert_eq!(vp_panes[0]["content"], r#"{"Markdown":"VP の内容"}"#);

        // Creo のペイン → Creo の内容だけ見える
        let creo_panes = db.list_pane_contents("/repos/creo").await.unwrap();
        assert_eq!(creo_panes.len(), 1);
        assert_eq!(creo_panes[0]["content"], r#"{"Markdown":"Creo の内容"}"#);
    }

    /// clear_pane_contents は対象 project_path のみ削除（他プロジェクトに影響なし）
    #[tokio::test]
    async fn test_pane_contents_clear_isolates_projects() {
        let db = make_test_db().await;

        db.upsert_pane_content("/repos/vp", "pane-1", "log", r#"{"Log":[]}"#, None)
            .await
            .unwrap();
        db.upsert_pane_content("/repos/creo", "pane-2", "log", r#"{"Log":[]}"#, None)
            .await
            .unwrap();

        // VP のみクリア
        db.clear_pane_contents("/repos/vp").await.unwrap();

        let vp_panes = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(vp_panes.len(), 0, "VP のペインはクリアされている");

        let creo_panes = db.list_pane_contents("/repos/creo").await.unwrap();
        assert_eq!(creo_panes.len(), 1, "Creo のペインは残っている");
    }

    // =========================================================================
    // PP Canvas Stack Model (lane scope) — pp-content-persist
    // =========================================================================

    /// 新 API: lane_name=None (conductor) と Some(name) (performer) が独立 record として共存できる
    #[tokio::test]
    async fn test_pp_state_conductor_and_performer_independent() {
        let db = make_test_db().await;

        let conductor_stack = serde_json::json!({
            "items": [{"id":"i1","content":"# conductor\n","contentType":"markdown","createdAt":"2026-05-28T00:00:00Z"}],
            "cursor": "i1",
            "capacity": 10
        });
        let performer_stack = serde_json::json!({
            "items": [{"id":"i2","content":"# performer\n","contentType":"markdown","createdAt":"2026-05-28T00:00:01Z"}],
            "cursor": "i2",
            "capacity": 10
        });
        let ui =
            serde_json::json!({"visible": true, "collapsed": false, "width": 480, "height": 720});

        db.upsert_pp_state(
            "/repos/vp",
            None,
            "paisley-park",
            "markdown",
            "# conductor\n",
            None,
            Some(&conductor_stack),
            Some(&ui),
        )
        .await
        .unwrap();
        db.upsert_pp_state(
            "/repos/vp",
            Some("foo"),
            "paisley-park",
            "markdown",
            "# performer\n",
            None,
            Some(&performer_stack),
            Some(&ui),
        )
        .await
        .unwrap();

        // conductor 読み込み
        let conductor = db
            .load_pp_state("/repos/vp", None, "paisley-park")
            .await
            .unwrap()
            .expect("conductor record 不在");
        assert_eq!(
            conductor["lane_name"], "",
            "conductor は lane_name='' sentinel (= None)"
        );
        assert_eq!(conductor["stack"]["cursor"], "i1");

        // performer 読み込み — conductor と独立した record
        let performer = db
            .load_pp_state("/repos/vp", Some("foo"), "paisley-park")
            .await
            .unwrap()
            .expect("performer record 不在");
        assert_eq!(performer["lane_name"], "foo");
        assert_eq!(performer["stack"]["cursor"], "i2");

        // list_pane_contents は両方見える (project scope)
        let all = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(all.len(), 2, "conductor + performer で 2 record");
    }

    /// upsert_pp_state は同 (project_path, lane_name, pane_id) で stack を上書きする (= roundtrip)
    #[tokio::test]
    async fn test_pp_state_upsert_roundtrip() {
        let db = make_test_db().await;
        let stack_v1 = serde_json::json!({"items": [], "cursor": null, "capacity": 10});
        let stack_v2 = serde_json::json!({
            "items": [{"id":"a","content":"x","contentType":"markdown","createdAt":"2026-05-28T00:00:00Z"}],
            "cursor": "a",
            "capacity": 10
        });

        db.upsert_pp_state(
            "/repos/vp",
            None,
            "paisley-park",
            "markdown",
            "",
            None,
            Some(&stack_v1),
            None,
        )
        .await
        .unwrap();
        db.upsert_pp_state(
            "/repos/vp",
            None,
            "paisley-park",
            "markdown",
            "x",
            None,
            Some(&stack_v2),
            None,
        )
        .await
        .unwrap();

        let rec = db
            .load_pp_state("/repos/vp", None, "paisley-park")
            .await
            .unwrap()
            .expect("record 不在");
        assert_eq!(rec["stack"]["cursor"], "a");
        assert_eq!(rec["stack"]["items"][0]["id"], "a");

        // 1 record だけ (UPSERT 冪等)
        let all = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(all.len(), 1);
    }

    /// 旧 caller (upsert_pane_content) は lane_name=None で row を作る。 stack/ui_state を巻き戻さない。
    #[tokio::test]
    async fn test_pp_state_legacy_upsert_keeps_stack() {
        let db = make_test_db().await;
        let stack = serde_json::json!({
            "items": [{"id":"keep","content":"keep","contentType":"markdown","createdAt":"2026-05-28T00:00:00Z"}],
            "cursor": "keep",
            "capacity": 10
        });

        // 新 API で stack を先に保存
        db.upsert_pp_state(
            "/repos/vp",
            None,
            "paisley-park",
            "markdown",
            "keep",
            Some("t"),
            Some(&stack),
            None,
        )
        .await
        .unwrap();

        // 旧 API (content / title だけ更新)。 stack は触らないことを期待
        db.upsert_pane_content(
            "/repos/vp",
            "paisley-park",
            "markdown",
            "updated",
            Some("t2"),
        )
        .await
        .unwrap();

        let rec = db
            .load_pp_state("/repos/vp", None, "paisley-park")
            .await
            .unwrap()
            .expect("record 不在");
        assert_eq!(rec["content"], "updated", "content は旧 API で更新される");
        assert_eq!(rec["title"], "t2");
        assert_eq!(
            rec["stack"]["cursor"], "keep",
            "stack は旧 API で巻き戻されてはいけない"
        );
    }

    /// load_pp_state: 不在の (project_path, lane_name, pane_id) は Ok(None)
    #[tokio::test]
    async fn test_pp_state_load_missing_returns_none() {
        let db = make_test_db().await;
        let v = db
            .load_pp_state("/repos/vp", None, "missing")
            .await
            .unwrap();
        assert!(v.is_none());
    }

    /// title が None → NULL で保存・復元できる
    #[tokio::test]
    async fn test_pane_contents_title_null() {
        let db = make_test_db().await;

        db.upsert_pane_content(
            "/repos/vp",
            "pane-notitle",
            "url",
            r#"{"Url":"https://example.com"}"#,
            None,
        )
        .await
        .unwrap();

        let panes = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(panes.len(), 1);
        assert!(
            panes[0]["title"].is_null(),
            "title が NULL でない: {:?}",
            panes[0]["title"]
        );
    }

    // =========================================================================
    // Stand Status CRUD テスト
    // =========================================================================

    /// 基本的な INSERT → SELECT フロー
    #[tokio::test]
    async fn test_stand_status_basic_crud() {
        let db = make_test_db().await;

        db.upsert_stand_status("/repos/vp", "heaven-door", "running", None)
            .await
            .unwrap();

        let statuses = db.list_stand_status("/repos/vp").await.unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["stand_key"], "heaven-door");
        assert_eq!(statuses[0]["status"], "running");
    }

    /// 同一 (project_path, stand_key) で再度 upsert → status が更新される
    #[tokio::test]
    async fn test_stand_status_upsert_updates_status() {
        let db = make_test_db().await;

        db.upsert_stand_status("/repos/vp", "heaven-door", "running", None)
            .await
            .unwrap();

        db.upsert_stand_status("/repos/vp", "heaven-door", "stopped", None)
            .await
            .unwrap();

        let statuses = db.list_stand_status("/repos/vp").await.unwrap();
        // レコード数は1のまま（UPSERT）
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["status"], "stopped");
    }

    /// detail が None → NULL で保存できる
    #[tokio::test]
    async fn test_stand_status_detail_null() {
        let db = make_test_db().await;

        db.upsert_stand_status("/repos/vp", "paisley-park", "idle", None)
            .await
            .unwrap();

        let statuses = db.list_stand_status("/repos/vp").await.unwrap();
        assert_eq!(statuses.len(), 1);
        assert!(
            statuses[0]["detail"].is_null(),
            "detail が NULL でない: {:?}",
            statuses[0]["detail"]
        );
    }

    /// detail に JSON オブジェクト → 保存・復元できる
    #[tokio::test]
    async fn test_stand_status_detail_with_json() {
        let db = make_test_db().await;

        let detail = serde_json::json!({
            "canvas_open": true,
            "pane_count": 3
        });

        db.upsert_stand_status("/repos/vp", "paisley-park", "running", Some(&detail))
            .await
            .unwrap();

        let statuses = db.list_stand_status("/repos/vp").await.unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["detail"]["canvas_open"], true);
        assert_eq!(statuses[0]["detail"]["pane_count"], 3);
    }

    /// 異なる project_path の stand_status は分離される
    #[tokio::test]
    async fn test_stand_status_project_isolation() {
        let db = make_test_db().await;

        db.upsert_stand_status("/repos/vp", "heaven-door", "running", None)
            .await
            .unwrap();
        db.upsert_stand_status("/repos/creo", "heaven-door", "stopped", None)
            .await
            .unwrap();

        let vp_statuses = db.list_stand_status("/repos/vp").await.unwrap();
        assert_eq!(vp_statuses.len(), 1);
        assert_eq!(vp_statuses[0]["status"], "running");

        let creo_statuses = db.list_stand_status("/repos/creo").await.unwrap();
        assert_eq!(creo_statuses.len(), 1);
        assert_eq!(creo_statuses[0]["status"], "stopped");
    }

    // =========================================================================
    // LIVE SELECT テスト
    // =========================================================================

    /// kv-mem で live_processes を開始してストリームが取得できる（接続確認）
    #[tokio::test]
    async fn test_live_processes_stream_connects() {
        let db = make_test_db().await;

        // ストリーム開始がエラーにならないことを確認
        let _stream = db
            .live_processes()
            .await
            .expect("live_processes ストリームの開始が失敗してはいけない");
    }
}
