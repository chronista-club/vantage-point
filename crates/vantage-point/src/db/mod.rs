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

/// VP 唯一の DB ディレクトリ (`vp_data_dir()/db/world`)
///
/// doc 44 P1 PR4 (DB 統合): 旧構成では World (`db/world/`) と project (`db/sp_{slug}/`) が
/// **別ディレクトリ**だった。理由は VP-182 — surrealkv は OS レベル排他ロック
/// (`try_lock_exclusive`) を持つため、別プロセスの World と SP が同一ディレクトリを
/// open すると LOCK 衝突で 2 番目が失敗する。
///
/// fold-in で SP プロセスが消え、World が全 project を同一プロセス内に抱えるようになった
/// 時点でこの分離理由は消滅した（同一プロセスからの open は handle 共有で足りる）。
/// project 次元は**ディレクトリではなく table の `project_path` 列**が持つ
/// （SP 固有 table も元から全て `project_path` を持っており、クエリも全てそれで絞る）。
///
/// 名前が `world` のままなのは、既存の `db/world/` を移行なしでそのまま使い続けるため。
pub fn db_data_dir_for_world() -> PathBuf {
    db_root().join("world")
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

        // surrealkv は OS レベル排他ロック (try_lock_exclusive) を持つ。 unclean shutdown
        // 直後や起動レース時に、 直前の holder が release し切る前だと connect が一時的に
        // 「Database at .../LOCK is already locked by another process」で失敗しうる。
        // ここで諦めて caller が「DB なしで継続」してしまうと wire store 等が無効のまま
        // 走り続ける (= 静かな degrade、 wire_send が "store not initialized" で恒久失敗)。
        // → lock 衝突に限り backoff retry。 さらに、 unclean shutdown / crash で取り残された
        //   stale LOCK (= live holder 不在) は `clear_stale_lock` で削除して即 retry し、
        //   手動 rm / reboot なしで self-heal する (一時的な race は backoff retry で待つ)。
        const MAX_ATTEMPTS: u32 = 8;
        let mut last_err = None;
        for attempt in 1..=MAX_ATTEMPTS {
            match surrealdb::engine::any::connect(&endpoint).await {
                Ok(db) => {
                    db.use_ns(NS).use_db(DB_NAME).await?;
                    if attempt > 1 {
                        tracing::info!(
                            "SurrealDB 接続成功 (embedded: {}, lock 取得まで {} 回試行)",
                            endpoint,
                            attempt
                        );
                    } else {
                        tracing::info!("SurrealDB 接続成功 (embedded: {})", endpoint);
                    }
                    return Ok(Self { db });
                }
                Err(e) => {
                    let is_lock = {
                        let m = e.to_string();
                        m.contains("locked") || m.contains("LOCK")
                    };
                    if !is_lock {
                        // lock 以外の失敗は retry しても無駄なので即座に返す
                        return Err(anyhow::anyhow!(
                            "SurrealDB embedded 接続失敗 ({}): {}",
                            endpoint,
                            e
                        ));
                    }
                    last_err = Some(e);
                    if attempt < MAX_ATTEMPTS {
                        // unclean shutdown で取り残された stale LOCK (= live holder 不在) なら
                        // 削除して即 retry。 これで手動 rm / reboot なしで self-heal する。
                        if Self::clear_stale_lock(data_dir) {
                            tracing::warn!(
                                "stale LOCK を削除 (live holder 不在) → 即 retry: {}",
                                endpoint
                            );
                            continue;
                        }
                        let wait = std::time::Duration::from_millis(250 * attempt as u64);
                        tracing::warn!(
                            "SurrealDB lock 衝突 ({}/{} 回目)、 {:?} 後に retry: {}",
                            attempt,
                            MAX_ATTEMPTS,
                            wait,
                            endpoint
                        );
                        tokio::time::sleep(wait).await;
                    }
                }
            }
        }
        // ここまで来た = 全 attempt で LOCK 衝突が続き、 stale 判定 (clear_stale_lock) も
        // 毎回 false (= holder 生存 = 別プロセスの VP が同じ db を開いている)。
        //
        // doc 44 P1 PR4 以前は、これを typed marker (`DbLockHeldByLiveHolder`) で返し SP 起動路が
        // downcast して「重複 spawn」と判定していた。db が単一化された今、この db を開くのは
        // World だけで、World の単一性は :32000 の port bind (`bind_dual_stack` は SO_REUSEADDR
        // のみで SO_REUSEPORT を使わない = 二重 listen 不可) が bind 時点で保証する。
        // よって本エラーに到達したら異常事態であり、caller が分岐に使う marker は不要になった。
        // (`daemon.pid` は bind 成功後に書く bookkeeping で、起動排他には関与しない。)
        Err(anyhow::anyhow!(
            "SurrealDB embedded 接続失敗 ({}): lock 衝突が {} 回 retry 後も解消せず (holder 生存): {}",
            endpoint,
            MAX_ATTEMPTS,
            last_err.map(|e| e.to_string()).unwrap_or_default()
        ))
    }

    /// LOCK ファイルに live holder が居ない (= 自分で非ブロッキング flock を取得できる) なら
    /// stale とみなして削除し、 true を返す。 unclean shutdown / crash 後に surrealkv が
    /// 取り残す LOCK を self-heal するための判定。 holder 生存時は触らず false（= 正常な排他）。
    #[cfg(unix)]
    fn clear_stale_lock(data_dir: &Path) -> bool {
        use std::os::unix::io::AsRawFd;
        let lock_path = data_dir.join("LOCK");
        let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
        else {
            return false; // LOCK が無い / open 不可 → 触らない
        };
        let fd = file.as_raw_fd();
        // SAFETY: fd は直上で open した有効な fd。 非ブロッキング排他 flock を試す。
        //   取得成功 = 他プロセスが握っていない = stale。
        let acquired = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if !acquired {
            return false; // live holder が居る → 削除しない
        }
        // flock を**保持したまま** remove_file して TOCTOU を排除する。
        //   先に LOCK_UN すると「UN → 他プロセスが open+flock 取得 → 我々が remove」の隙が生じ、
        //   他プロセスが削除済み inode の flock を握ったまま接続続行 → 我々の retry が新 inode の
        //   LOCK を作って接続成功 → 二重 holder → DB 破損、という race になる。
        //   保持中に unlink すれば、消す対象＝自分が握る inode なので他者侵入の窓が無い。
        //   明示的 LOCK_UN は不要（drop(file) の fd close で flock は自動解放される）。
        let removed = std::fs::remove_file(&lock_path).is_ok();
        drop(file); // ここで fd close → flock 自動解放
        removed
    }

    /// `clear_stale_lock` の非 unix 版 (no-op)。 flock が無い環境では stale 判定をせず、
    /// backoff retry のみで対処する。
    #[cfg(not(unix))]
    fn clear_stale_lock(_data_dir: &Path) -> bool {
        false
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
        self.normalize_legacy_lane_addresses().await;
        Ok(())
    }

    /// doc 44 P2: `lane` / `lane_lifecycle` の **address 文字列列**を新形へ正規化する（冪等）。
    ///
    /// フラット化で address の表示形が `<project>/performer/<name>` → `<project>/<name>` に
    /// 変わった。descriptor（object 列）は `LaneAddress` の serde default が吸収するが、
    /// **address を文字列 key として持つ列は吸収できない** — 旧形の行が残ると
    /// upsert（DELETE+CREATE の WHERE が新形で当たらない）が重複行を作り、
    /// lifecycle は照合できず孤児になる。
    ///
    /// 失敗しても起動は続ける（best-effort）。正規化できなかった行は旧形のまま残るだけで、
    /// 次回起動で再試行される。
    async fn normalize_legacy_lane_addresses(&self) {
        for table in ["lane", "lane_lifecycle"] {
            match self.normalize_lane_addresses_in(table).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("doc 44 P2: {} の旧形 address を {} 件正規化", table, n),
                Err(e) => {
                    tracing::warn!("{} の address 正規化に失敗（旧形のまま継続）: {}", table, e)
                }
            }
        }
    }

    /// 1 テーブル分の address 正規化。戻り値は書き換えた行数。
    async fn normalize_lane_addresses_in(&self, table: &str) -> Result<usize> {
        let mut result = self
            .db
            .query(format!("SELECT meta::id(id) AS rid, address FROM {table}"))
            .await?;
        let rows: Vec<serde_json::Value> = result.take(0)?;

        let mut fixed = 0;
        for row in rows {
            let (Some(rid), Some(old)) = (
                row.get("rid").and_then(|v| v.as_str()),
                row.get("address").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            // parse_address は旧 3 分節形を受理して新形に正規化する。
            let Some(new) = crate::process::lanes_state::LanePool::parse_address(old)
                .map(|a| a.to_string())
                .filter(|new| new != old)
            else {
                continue;
            };
            // 1 行の失敗で他行を巻き込まない。典型的な失敗は
            // `(project_path, address)` の UNIQUE 衝突（旧形と新形が同じ lane を指して
            // 両方残っているケース）で、これは当該行だけの問題。`?` で抜けると同じ
            // SELECT に載った**残り全行**の正規化が飛び、次回起動でも衝突源が在る限り
            // 毎回巻き添えになる（= 恒久的に旧形が残る）。
            let updated = self
                .db
                .query(format!(
                    "UPDATE type::record('{table}', $rid) SET address = $addr"
                ))
                .bind(("rid", rid.to_string()))
                .bind(("addr", new.clone()))
                .await
                .and_then(|mut r| r.take::<Vec<serde_json::Value>>(0));
            match updated {
                Ok(_) => fixed += 1,
                Err(e) => tracing::warn!(
                    "{}:{} の address 正規化に失敗（この行のみ旧形のまま継続、{} → {}）: {}",
                    table,
                    rid,
                    old,
                    new,
                    e
                ),
            }
        }
        Ok(fixed)
    }

    /// ヘルスチェック（DB に接続できているか確認）
    pub async fn health(&self) -> bool {
        self.db.query("RETURN true").await.is_ok()
    }

    /// 内部の Surreal への参照を取得
    pub fn inner(&self) -> &Surreal<Any> {
        &self.db
    }

    // =========================================================================
    // World identity (federation L2、 ADR-020 D2): home-World の位置独立 安定 id `wld_xxx`。
    // db/world の singleton row (固定 record id world_identity:self)。daemon が初回起動で
    // 1 度だけ発行し永続、 以降の再起動は復元する。[`crate::lane::lane_id::load_or_create`] の
    // db 版 — lane は (project,lane) ごと file 永続、 World は daemon に 1 つなので db singleton。
    // =========================================================================

    /// home-World の wld_id を取得する (無ければ生成して永続)。
    ///
    /// - 既存 singleton row があり非空なら **それを復元** (= 再起動を越えて安定)。
    /// - 無い / 空なら **新規生成して永続** し、 その id を返す。
    ///
    /// World daemon は single-writer (db comment 参照) かつ boot で 1 度だけ呼ぶため race は無い。
    /// 書き込みは DELETE+CREATE を単一 query (= 1 transaction、 [`Self::upsert_lane`] と同方針) で
    /// atomic に行う (空 row が残っていた場合も確実に上書き)。
    pub async fn load_or_create_world_id(&self) -> Result<crate::world::WorldId> {
        // 既存 singleton row の wld_id を読む (存在しなければ空配列)。
        let mut result = self
            .db
            .query("SELECT VALUE wld_id FROM world_identity:self")
            .await
            .map_err(|e| anyhow::anyhow!("world_id 取得失敗: {}", e))?;
        let existing: Vec<String> = result.take(0)?;
        if let Some(id) = existing.into_iter().find(|s| !s.trim().is_empty()) {
            return Ok(crate::world::WorldId::from(id));
        }

        // 無ければ新規発行して永続する。
        let id = crate::world::WorldId::generate();
        self.db
            .query(
                "DELETE world_identity:self;
                 CREATE world_identity:self CONTENT {
                    wld_id: $wld_id,
                    created_at: time::now()
                 }",
            )
            .bind(("wld_id", id.as_str().to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("world_id 永続失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("world_id 永続エラー: {}", e))?;
        tracing::info!("home-World identity 発行: wld_id={}", id);
        Ok(id)
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
    ) -> Result<()> {
        self.db
            .query(
                "INSERT INTO processes {
                    project_path: $project_path,
                    project_name: $project_name,
                    port: $port,
                    pid: $pid,
                    status: $status,
                    started_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    project_name = $input.project_name,
                    port = $input.port,
                    pid = $input.pid,
                    status = $input.status",
            )
            .bind(("project_path", project_path.to_string()))
            .bind(("project_name", project_name.to_string()))
            .bind(("port", port as i64))
            .bind(("pid", pid as i64))
            .bind(("status", status.to_string()))
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
    // Project Host 帳簿①: 開発起点ポインタ (doc 44 D4)。
    //
    // active_lane (注視) と形は同じ 1-project-1-row だが意味が違う (D5 が分けた
    // 「注視の切替」と「起点の再指定」)。値が **lane_id** なのは rename 耐性のため。
    // =========================================================================

    /// 開発起点ポインタを upsert する (project_path → lane_id)。
    pub async fn upsert_host_origin(&self, project_path: &str, lane_id: &str) -> Result<()> {
        self.db
            .query(
                "INSERT INTO host_origin {
                    project_path: $project_path,
                    lane_id: $lane_id,
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    lane_id = $input.lane_id,
                    updated_at = time::now()",
            )
            .bind(("project_path", project_path.to_string()))
            .bind(("lane_id", lane_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_origin upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("host_origin upsert エラー: {}", e))?;
        Ok(())
    }

    /// 開発起点ポインタを引く。
    ///
    /// `None` は「未指定」= 予約名フォールバック（[`crate::host::ledger::resolve_origin_name`]）。
    /// 指す lane が既に消えている場合も呼び出し側の解決で予約名に落ちるので、ここでは
    /// 実在検証をしない（DB は lane の生死を知らない）。
    pub async fn get_host_origin(&self, project_path: &str) -> Result<Option<String>> {
        let mut result = self
            .db
            .query("SELECT lane_id FROM host_origin WHERE project_path = $path")
            .bind(("path", project_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_origin 取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        Ok(rows
            .first()
            .and_then(|v| v.get("lane_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()))
    }

    /// 開発起点ポインタを削除する (project remove 時の回収、`delete_active_lane` と対)。
    pub async fn delete_host_origin(&self, project_path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM host_origin WHERE project_path = $path")
            .bind(("path", project_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_origin 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("host_origin 削除エラー: {}", e))?;
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

    // =========================================================================
    // board モデル (2026-07-15): scope 別 Canvas board の CRUD
    //
    // board = PP Canvas に show した item の scope 別永続リスト（SP が唯一の truth を持つ）。
    // stack = { items: [...新→古], cursor: <id|NONE> } を pane_contents.stack に保存する。
    // キーは (project_path, scope, lane_name, pane_id)。 lane board は lane_name で lane ごとに
    // 分離、 proj board は lane_name='' (project 共有)。
    // =========================================================================

    /// board に item を atomic に head-push する（= mcp__show 着信 1 件）。
    ///
    /// - item を items の先頭に追加し（新→古）、 `capacity` を超えた末尾（最古）を切り、
    ///   cursor を新 item に更新する。
    /// - RMW を避け ON DUPLICATE KEY UPDATE 内の array 関数で atomic に行う
    ///   （人/agent が連続 show した際の read-modify-write race を排除）。
    /// - `item` は webview の CanvasItem 形（camelCase: id/content/contentType/title/createdAt）。
    ///   top-level content/content_type/title は「現在 main で見せる item の reflection」(seek fallback)。
    pub async fn append_board_item(
        &self,
        project_path: &str,
        scope: &str,
        lane_name: &str,
        pane_id: &str,
        item: &serde_json::Value,
        capacity: usize,
    ) -> Result<()> {
        let item_id = item
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let content = item
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let content_type = item
            .get("contentType")
            .and_then(|v| v.as_str())
            .unwrap_or("markdown")
            .to_string();
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        self.db
            .query(
                "INSERT INTO pane_contents {
                    project_path: $project_path,
                    scope: $scope,
                    lane_name: $lane_name,
                    pane_id: $pane_id,
                    content_type: $content_type,
                    content: $content,
                    title: $title,
                    stack: { items: [$item], cursor: $item_id },
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    stack = {
                        items: array::slice(array::prepend(stack.items ?? [], $item), 0, $cap),
                        cursor: $item_id
                    },
                    content_type = $input.content_type,
                    content = $input.content,
                    title = $input.title,
                    updated_at = time::now()",
            )
            .bind(("project_path", project_path.to_string()))
            .bind(("scope", scope.to_string()))
            .bind(("lane_name", lane_name.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("content_type", content_type))
            .bind(("content", content))
            .bind(("title", title))
            .bind(("item", item.clone()))
            .bind(("item_id", item_id))
            .bind(("cap", capacity as i64))
            .await
            .map_err(|e| anyhow::anyhow!("board append 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("board append エラー: {}", e))?;
        Ok(())
    }

    /// board から item を 1 件削除する（= thumbnail ✕）。 cursor が削除対象を指していたら
    /// 削除後の先頭（最新）に fallback、 空なら NONE。
    pub async fn delete_board_item(
        &self,
        project_path: &str,
        scope: &str,
        lane_name: &str,
        pane_id: &str,
        item_id: &str,
    ) -> Result<()> {
        // SET 内の右辺は「更新前の stack」で評価されるため、 cursor 判定と items 更新は整合する。
        self.db
            .query(
                "UPDATE pane_contents SET
                    stack.cursor = IF stack.cursor = $item_id
                        THEN array::filter(stack.items ?? [], |$it| $it.id != $item_id)[0].id
                        ELSE stack.cursor END,
                    stack.items = array::filter(stack.items ?? [], |$it| $it.id != $item_id),
                    updated_at = time::now()
                 WHERE project_path = $path AND scope = $scope
                   AND lane_name = $lane AND pane_id = $pane_id",
            )
            .bind(("path", project_path.to_string()))
            .bind(("scope", scope.to_string()))
            .bind(("lane", lane_name.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("item_id", item_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("board delete 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("board delete エラー: {}", e))?;
        Ok(())
    }

    /// board を空にする（= mcp__clear / Clear ボタン）。
    pub async fn clear_board(
        &self,
        project_path: &str,
        scope: &str,
        lane_name: &str,
        pane_id: &str,
    ) -> Result<()> {
        self.db
            .query(
                "UPDATE pane_contents SET
                    stack = { items: [], cursor: NONE },
                    content = '', title = NONE,
                    updated_at = time::now()
                 WHERE project_path = $path AND scope = $scope
                   AND lane_name = $lane AND pane_id = $pane_id",
            )
            .bind(("path", project_path.to_string()))
            .bind(("scope", scope.to_string()))
            .bind(("lane", lane_name.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("board clear 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("board clear エラー: {}", e))?;
        Ok(())
    }

    /// 特定 (project_path, scope, lane_name, pane_id) の board を 1 件取得。 不在なら Ok(None)。
    pub async fn load_board(
        &self,
        project_path: &str,
        scope: &str,
        lane_name: &str,
        pane_id: &str,
    ) -> Result<Option<serde_json::Value>> {
        let mut result = self
            .db
            .query(
                "SELECT * FROM pane_contents
                 WHERE project_path = $path AND scope = $scope
                   AND lane_name = $lane AND pane_id = $pane_id
                 LIMIT 1",
            )
            .bind(("path", project_path.to_string()))
            .bind(("scope", scope.to_string()))
            .bind(("lane", lane_name.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("board load 失敗: {}", e))?;
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
DEFINE INDEX IF NOT EXISTS idx_processes_path ON processes COLUMNS project_path UNIQUE;

-- home-World identity (federation L2、 ADR-020 D2): 位置独立な安定 id `wld_xxx`。
-- daemon が初回起動で 1 度だけ発行し db/world に永続する singleton (固定 record id
-- world_identity:self、 index 不要)。machine/hostname/endpoint から独立で、 hub の routing
-- key になる。書き手は daemon 起動路のみ (doc 44 P1 PR4 で db は単一化されたが、 本 table を
-- 触るのは World bootstrap だけなので daemon-canonical な truth であることは変わらない)。
DEFINE TABLE IF NOT EXISTS world_identity SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS wld_id ON world_identity TYPE string;
DEFINE FIELD IF NOT EXISTS created_at ON world_identity TYPE datetime DEFAULT time::now();

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

-- Project Host の帳簿①: 開発起点ポインタ (doc 44 D4 / §8)。
-- 「この project の開発の起点はどの lane か」を Host が 1 本だけ持つ。
--
-- ⚠️ active_lane (注視) とは別物 — D5 が明示的に分けている:
--   active_lane = 今どの lane を見ているか (presence、click ごとに動く)
--   host_origin = 開発の起点はどこか       (intent、明示的に指定した時だけ動く)
--
-- key が address 文字列ではなく **lane_id (UUID)** なのは、将来 lane 名を変えられるように
-- するため。名前は表示のための自然キーで、rename で動く。ポインタが指すのは lane そのもの
-- なので surrogate key で持つ (doc 44 §8.2)。行が無い / 指す lane が実在しない場合は
-- 予約名 `conductor` にフォールバックする (= 従来挙動、`ledger::resolve_origin_name`)。
DEFINE TABLE IF NOT EXISTS host_origin SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS project_path ON host_origin TYPE string;
DEFINE FIELD IF NOT EXISTS lane_id ON host_origin TYPE string;
DEFINE FIELD IF NOT EXISTS updated_at ON host_origin TYPE datetime;
DEFINE INDEX IF NOT EXISTS idx_host_origin_path ON host_origin COLUMNS project_path UNIQUE;

-- wiremsg R6: 旧 msgbox table (VP-169 以前の cross-process メッセージング) は撤去。
-- agent 間通信は wiremsg (下記 wire_messages table) に一本化済。
-- R5-3 で VP-169 msgs table、 R6 で本 table を撤去し msgbox 系が完全消滅した。

-- =========================================================================
-- project 固有テーブル（project_path でフィルタ — D11 準拠）
--
-- doc 44 P1 PR4 (DB 統合): 旧称「SP 固有テーブル」。SP プロセス時代は per-SP DB
-- (`db/sp_{slug}/`) に置かれ、1 DB = 1 project だったため project_path 列は事実上
-- 冗長だった。db 単一化で、この列が唯一の project 次元になる（= 全クエリが
-- `WHERE project_path = $path` で絞る前提。これを欠くと他 project の行を掴む）。
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
-- board モデル (2026-07-15): scope 軸を追加し (project_path, scope, lane_name, pane_id) で board を
--   分離する。 scope='lane' が lane board (lane_name で lane ごとに独立)、 'proj' が project 共有 board
--   (lane_name='')。 旧 record (scope 不在) は DEFAULT 'lane' で self-heal され、 既存 lane/conductor
--   board を現挙動のまま保存する。 現状の scope は lane/proj の 2 つ。
--   (doc 44 P1 PR4 まで「将来の 'vp'(全体 board) は別 DB 行き」と書かれていたが、 db 単一化で
--    その制約は消えた — 全体 board を足すなら project_path を跨ぐ scope 値を 1 つ増やすだけで済む。)
DEFINE FIELD IF NOT EXISTS scope ON pane_contents TYPE string DEFAULT 'lane';
DEFINE FIELD IF NOT EXISTS stack ON pane_contents TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS ui_state ON pane_contents TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS updated_at ON pane_contents TYPE datetime DEFAULT time::now();
-- 旧 UNIQUE (project_path, lane_name, pane_id) を破棄し scope を含む新 index に置換。
REMOVE INDEX IF EXISTS idx_pane_lane ON pane_contents;
DEFINE INDEX IF NOT EXISTS idx_pane_scope ON pane_contents COLUMNS project_path, scope, lane_name, pane_id UNIQUE;

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

    /// doc 44 P2: 旧形の address 文字列（`<project>/performer/<name>`）が起動時に新形へ
    /// 正規化されること。
    ///
    /// これを怠ると実害が出る: `lane` は upsert（DELETE+CREATE）の WHERE が新形で当たらず
    /// **旧形の行が残って重複**し、`lane_lifecycle` は照合できず**孤児**になる。
    /// descriptor（object 列）は `LaneAddress` の serde default が吸収するが、
    /// address を文字列 key として持つ列はそれでは救えない。
    #[tokio::test]
    async fn legacy_address_strings_are_normalized_on_schema_define() {
        let db = VpDb::connect_mem().await.unwrap();
        db.define_schema().await.unwrap();

        // 旧形の行を直に流し込む（P2 以前の永続状態を再現）
        db.inner()
            .query(
                "CREATE lane_lifecycle CONTENT {
                     project_path: '/repos/vp', address: 'vp/performer/foo',
                     lifecycle: 'ready', updated_at: time::now()
                 };
                 CREATE lane_lifecycle CONTENT {
                     project_path: '/repos/vp', address: 'vp/conductor',
                     lifecycle: 'ready', updated_at: time::now()
                 };",
            )
            .await
            .unwrap()
            .check()
            .unwrap();

        // define_schema が正規化を走らせる（冪等なので 2 度目も安全）
        db.define_schema().await.unwrap();

        let rows = db.list_lane_lifecycles().await.unwrap();
        let addrs: Vec<&str> = rows.iter().map(|(_, a, _)| a.as_str()).collect();
        assert!(
            addrs.contains(&"vp/foo"),
            "旧形 vp/performer/foo は vp/foo に正規化されるべき: {addrs:?}"
        );
        assert!(
            !addrs.contains(&"vp/performer/foo"),
            "旧形が残ってはならない（孤児化する）: {addrs:?}"
        );
        assert!(
            addrs.contains(&"vp/conductor"),
            "元から新形と一致する行は触られない: {addrs:?}"
        );
    }

    /// doc 44 P1 PR4: DB ディレクトリは `vp_data_dir()/db/world` の**単一**であること。
    ///
    /// 旧テストは「World と SP の dir が分離されていること」を固定していた（VP-182 の
    /// LOCK 衝突回避）。fold-in で project がプロセスでなくなり handle 共有になったため、
    /// 固定すべき性質が「分離」から「単一」に反転した。
    #[test]
    fn test_db_data_dir_is_single_world_dir() {
        let world = db_data_dir_for_world();

        // VP-192: vp_data_dir()/db 配下
        assert!(
            world.starts_with(crate::config::vp_data_dir()),
            "DB dir は vp_data_dir() 配下であるべき: {}",
            world.display()
        );
        assert!(
            world.parent().is_some_and(|p| p.ends_with("db")),
            "DB dir の親は 'db' であるべき: {}",
            world.display()
        );
        assert!(
            world.ends_with("world"),
            "DB dir は 'world' で終わるべき: {}",
            world.display()
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
    async fn test_world_id_load_or_create_is_stable() {
        // federation L2: wld_id singleton の発行 → 復元 round-trip。
        let db = make_test_db().await;

        // 初回は生成して永続 (EntId 形式 wld_1.. )。
        let first = db.load_or_create_world_id().await.unwrap();
        assert!(
            first.as_str().starts_with("wld_1"),
            "EntId 形式 wld_1.. のはず: {first}"
        );

        // 2 回目以降は同じ id を復元する (= singleton、 再起動越え安定の核)。
        let second = db.load_or_create_world_id().await.unwrap();
        assert_eq!(first, second, "wld_id は singleton で安定して復元される");
    }

    /// doc 44 D4: 開発起点ポインタの round-trip（upsert → get → 上書き → 削除）。
    ///
    /// **削除まで見る**のは、`remove_project` が project namespace を倒す時にこの行を
    /// 回収する契約だから（§4.6 含有=所有=寿命）。残ると同 path で再登録した時に旧 lane の
    /// UUID を指す孤児ポインタが復活し、起点が `Dangling` に落ちる。
    #[tokio::test]
    async fn test_host_origin_round_trip() {
        let db = make_test_db().await;

        // 未設定は None = 予約名フォールバック（`ledger::resolve_origin_name` が受ける形）
        assert!(db.get_host_origin("/repos/vp").await.unwrap().is_none());

        db.upsert_host_origin("/repos/vp", "id-alpha")
            .await
            .unwrap();
        assert_eq!(
            db.get_host_origin("/repos/vp").await.unwrap().as_deref(),
            Some("id-alpha")
        );

        // project ごとに独立（1 project 1 ポインタ）
        db.upsert_host_origin("/repos/nexus", "id-beta")
            .await
            .unwrap();
        assert_eq!(
            db.get_host_origin("/repos/vp").await.unwrap().as_deref(),
            Some("id-alpha"),
            "他 project の指定に引きずられない"
        );

        // 起点の移動は行の上書き（増えない）
        db.upsert_host_origin("/repos/vp", "id-gamma")
            .await
            .unwrap();
        assert_eq!(
            db.get_host_origin("/repos/vp").await.unwrap().as_deref(),
            Some("id-gamma"),
            "UNIQUE index により上書きされる"
        );

        // project 回収でポインタも消える
        db.delete_host_origin("/repos/vp").await.unwrap();
        assert!(db.get_host_origin("/repos/vp").await.unwrap().is_none());
        assert_eq!(
            db.get_host_origin("/repos/nexus").await.unwrap().as_deref(),
            Some("id-beta"),
            "削除は project scope に閉じる"
        );
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
        use crate::process::lanes_state::{LaneAddress, LaneInfo, LaneState};
        // doc 24 §10 Phase 2: lane descriptor の daemon-canonical durable round-trip。
        let db = make_test_db().await;

        // 初期は空
        assert!(db.list_lanes().await.unwrap().is_empty());

        // テスト用 LaneInfo builder (live 値 pid は埋めるが、 検証は descriptor 中心)。
        let mk = |project: &str, name: &str| LaneInfo {
            console_mode: Default::default(),
            id: Default::default(),
            address: LaneAddress::new(project, name),
            state: LaneState::Running,
            stand: "echoes".to_string(),
            created_at: "2026-06-20T00:00:00Z".to_string(),
            pid: Some(1234),
            cwd: "/tmp".to_string(),
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            engine_stand: None,
            flow_state: None,
        };

        // 2 project に lane を入れる
        db.upsert_lane("/repos/vp", &mk("vp", "conductor"))
            .await
            .unwrap();
        db.upsert_lane("/repos/vp", &mk("vp", "foo")).await.unwrap();
        db.upsert_lane("/repos/nexus", &mk("nexus", "conductor"))
            .await
            .unwrap();

        let rows = db.list_lanes().await.unwrap();
        assert_eq!(rows.len(), 3, "3 lane descriptor が round-trip する");

        // descriptor が round-trip する (address / stand)
        let vp_conductor = rows
            .iter()
            .find(|(p, l)| p == "/repos/vp" && l.address.is_conductor())
            .expect("vp conductor が読める");
        assert_eq!(vp_conductor.1.address.to_string(), "vp/conductor");
        assert_eq!(vp_conductor.1.stand, "echoes");

        // 同 address の upsert は置換 (複合 UNIQUE、 件数は増えない)
        db.upsert_lane("/repos/vp", &mk("vp", "conductor"))
            .await
            .unwrap();
        assert_eq!(
            db.list_lanes().await.unwrap().len(),
            3,
            "同 address の upsert は置換"
        );

        // 単一 lane の削除 (Diff::Remove)
        db.delete_lane("/repos/vp", "vp/foo").await.unwrap();
        let rows = db.list_lanes().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            !rows.iter().any(|(_, l)| l.address.to_string() == "vp/foo"),
            "削除した lane は消える"
        );

        // snapshot 全置換 (register snapshot): /repos/vp を performer 2 つに置換
        db.replace_lanes_for_project("/repos/vp", &[mk("vp", "a"), mk("vp", "b")])
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
            vp_lanes.iter().all(|(_, l)| !l.address.is_conductor()),
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

        db.upsert_lane_lifecycle("/repos/vp", "vp/foo", "provisioning")
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
        db.upsert_lane_lifecycle("/repos/vp", "vp/foo", "ready")
            .await
            .unwrap();
        let rows = db.list_lane_lifecycles().await.unwrap();
        assert_eq!(rows.len(), 3, "同 address は置換、 件数は増えない");
        assert!(
            rows.iter()
                .any(|(p, a, lc)| p == "/repos/vp" && a == "vp/foo" && lc == "ready"),
            "provisioning → ready に置換される"
        );

        // 単一削除。
        db.delete_lane_lifecycle("/repos/vp", "vp/foo")
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
        db.upsert_process("/repos/vp", "vp", 33000, 1234, "running")
            .await
            .unwrap();

        // 一覧
        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0]["project_name"], "vp");
        assert_eq!(procs[0]["port"], 33000);

        // 更新（同じ path で upsert）
        db.upsert_process("/repos/vp", "vp", 33001, 5678, "running")
            .await
            .unwrap();
        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0]["port"], 33001);

        // 削除
        db.delete_process("/repos/vp").await.unwrap();
        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 0);
    }

    #[tokio::test]
    async fn test_processes_clear_all() {
        let db = make_test_db().await;

        db.upsert_process("/a", "a", 33000, 1, "running")
            .await
            .unwrap();
        db.upsert_process("/b", "b", 33001, 2, "running")
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

    // ===== board モデル (2026-07-15): scope 別 board CRUD の SurrealQL 検証 =====

    fn mk_item(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "content": id, "contentType": "markdown",
            "createdAt": "2026-07-15T00:00:00Z"
        })
    }

    /// append は item を head-push し（新→古）、 cursor を新 item に更新、 capacity で最古を落とす。
    #[tokio::test]
    async fn test_board_append_head_push_cursor_and_cap() {
        let db = make_test_db().await;
        for id in ["a", "b", "c"] {
            db.append_board_item("/repos/vp", "proj", "", "paisley-park", &mk_item(id), 2)
                .await
                .unwrap();
        }
        let rec = db
            .load_board("/repos/vp", "proj", "", "paisley-park")
            .await
            .unwrap()
            .expect("board 不在");
        // head-push: 最新 c が先頭、 cap=2 で最古 a が落ちる → [c, b]
        assert_eq!(rec["stack"]["items"].as_array().unwrap().len(), 2);
        assert_eq!(rec["stack"]["items"][0]["id"], "c");
        assert_eq!(rec["stack"]["items"][1]["id"], "b");
        assert_eq!(rec["stack"]["cursor"], "c");
    }

    /// delete: cursor が削除対象なら削除後の先頭に fallback、 非 cursor 削除は cursor 不変。
    #[tokio::test]
    async fn test_board_delete_item_cursor_fallback() {
        let db = make_test_db().await;
        for id in ["a", "b", "c"] {
            db.append_board_item(
                "/repos/vp",
                "lane",
                "wing",
                "paisley-park",
                &mk_item(id),
                10,
            )
            .await
            .unwrap();
        }
        // items=[c,b,a], cursor=c。 c を削除 → items=[b,a], cursor=b（先頭 fallback）。
        db.delete_board_item("/repos/vp", "lane", "wing", "paisley-park", "c")
            .await
            .unwrap();
        let rec = db
            .load_board("/repos/vp", "lane", "wing", "paisley-park")
            .await
            .unwrap()
            .unwrap();
        let items = rec["stack"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["id"], "b");
        assert_eq!(rec["stack"]["cursor"], "b");

        // items=[b,a], cursor=b。 a（非 cursor）削除 → cursor=b 不変。
        db.delete_board_item("/repos/vp", "lane", "wing", "paisley-park", "a")
            .await
            .unwrap();
        let rec = db
            .load_board("/repos/vp", "lane", "wing", "paisley-park")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            rec["stack"]["cursor"], "b",
            "非 cursor 削除で cursor は不変"
        );
        assert_eq!(rec["stack"]["items"].as_array().unwrap().len(), 1);
    }

    /// clear は board を空にする。
    #[tokio::test]
    async fn test_board_clear() {
        let db = make_test_db().await;
        db.append_board_item("/repos/vp", "proj", "", "paisley-park", &mk_item("a"), 10)
            .await
            .unwrap();
        db.clear_board("/repos/vp", "proj", "", "paisley-park")
            .await
            .unwrap();
        let rec = db
            .load_board("/repos/vp", "proj", "", "paisley-park")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rec["stack"]["items"].as_array().unwrap().len(), 0);
        assert!(rec["stack"]["cursor"].is_null());
    }

    /// lane board と proj board は同 project でも独立（scope 軸で分離）。
    #[tokio::test]
    async fn test_board_scope_isolation() {
        let db = make_test_db().await;
        db.append_board_item("/repos/vp", "lane", "", "paisley-park", &mk_item("L"), 10)
            .await
            .unwrap();
        db.append_board_item("/repos/vp", "proj", "", "paisley-park", &mk_item("P"), 10)
            .await
            .unwrap();
        let lane = db
            .load_board("/repos/vp", "lane", "", "paisley-park")
            .await
            .unwrap()
            .unwrap();
        let proj = db
            .load_board("/repos/vp", "proj", "", "paisley-park")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lane["stack"]["items"][0]["id"], "L");
        assert_eq!(proj["stack"]["items"][0]["id"], "P");
        // lane/proj は別 row
        assert_eq!(db.list_pane_contents("/repos/vp").await.unwrap().len(), 2);
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

    // =========================================================================
    // stale LOCK self-heal（clear_stale_lock）テスト
    // =========================================================================

    /// 誰も握っていない stale LOCK は削除され true。 LOCK 不在なら false（対象なし）。
    #[cfg(unix)]
    #[test]
    fn clear_stale_lock_removes_unheld_and_skips_missing() {
        let tmp = std::env::temp_dir().join(format!("vp-stale-lock-a-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let lock = tmp.join("LOCK");

        std::fs::write(&lock, b"stale").unwrap();
        assert!(
            super::VpDb::clear_stale_lock(&tmp),
            "unheld LOCK は stale 判定で削除されるべき"
        );
        assert!(!lock.exists(), "stale LOCK ファイルが削除されているべき");

        assert!(
            !super::VpDb::clear_stale_lock(&tmp),
            "LOCK 不在時は false（何もしない）"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    /// live holder が flock を握っている LOCK は削除されない（= 正常な排他を壊さない）。
    #[cfg(unix)]
    #[test]
    fn clear_stale_lock_keeps_held_lock() {
        use std::os::unix::io::AsRawFd;
        let tmp = std::env::temp_dir().join(format!("vp-stale-lock-b-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let lock = tmp.join("LOCK");

        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false) // flock を握るだけ。既存内容は無関係なので truncate しない（clippy::suspicious_open_options）
            .open(&lock)
            .unwrap();
        // 別 open file description で排他 flock を握る（live holder を模擬）
        let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(r, 0, "テスト前提: flock 取得成功");

        assert!(
            !super::VpDb::clear_stale_lock(&tmp),
            "live holder の LOCK は削除しない"
        );
        assert!(lock.exists(), "held LOCK ファイルは残るべき");

        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_UN) };
        drop(f);
        std::fs::remove_dir_all(&tmp).ok();
    }
}
