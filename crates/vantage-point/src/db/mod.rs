//! DB の接続と、crate へ開く面。
//!
//! VP の状態管理を **プロセス内の SurrealDB** に統一する。surrealkv backend を使い、
//! 外部 `surreal` バイナリは不要。
//!
//! ## 接続方式
//!
//! - 本番: `surrealkv://<data_dir>` で in-process embedded DB
//! - テスト: `kv-mem` で in-memory embedded DB
//!
//! 単一プロセスが DB を保持する single-writer モデル。複数 Process が同時に書く
//! ユースケースは現状ない（daemon が集約点）。handle は **1 本**で、repo の次元は
//! table の `repo_path` 列が持つ（doc 44 §5.2）。
//!
//! ## この file が持つもの
//!
//! 接続（[`VpDb::connect_embedded`] / [`VpDb::connect_mem`]）、LOCK の後始末
//! （`clear_stale_lock`）、path 解決（[`db_data_dir_for_machine`] / [`reclaim_legacy_repo_dbs`]）、
//! そして crate に開く面（[`VpDb`] / [`SharedVpDb`] / [`Action`]）。
//!
//! ## table ごとの永続操作は domain module にある
//!
//! [`schema`] が table 定義と起動時 migration を持ち、以下が table ごとの CRUD を持つ。
//! いずれも `impl VpDb` を書き足すだけなので、[`VpDb`] は 1 型・接続も 1 本のまま。
//! 設計は [doc 62](../../../../docs/design/62-db-module-layout.md)。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;

mod schema;

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

/// VP 唯一の DB ディレクトリ (`vp_data_dir()/db/machine`)
///
/// doc 44 P1 PR4 (DB 統合): 旧構成では Daemon (`db/machine/`) と repo (`db/sp_{slug}/`) が
/// **別ディレクトリ**だった。理由は VP-182 — surrealkv は OS レベル排他ロック
/// (`try_lock_exclusive`) を持つため、別プロセスの daemon と repo が同一ディレクトリを
/// open すると LOCK 衝突で 2 番目が失敗する。
///
/// fold-in で repo プロセスが消え、daemon が全 repo を同一プロセス内に抱えるようになった
/// 時点でこの分離理由は消滅した（同一プロセスからの open は handle 共有で足りる）。
/// repo 次元は**ディレクトリではなく table の `repo_path` 列**が持つ
/// （repo 固有 table も元から全て `repo_path` を持っており、クエリも全てそれで絞る）。
///
/// dir 名は `machine`（machine に 1 つの単一 DB）。旧 `db/world/` からの rename は
/// **意図的に migration なし**（命名エピック 4/9、mako 承認 2026-07-27 — doc 54 §8.1 の
/// 「legacy データは初期化」policy。旧 dir は disk に残るが参照されない。doc 44 P1 の
/// DB 統合と同型の割り切り）。
pub fn db_data_dir_for_machine() -> PathBuf {
    db_root().join("machine")
}

/// 旧 per-repo DB ディレクトリ (`db/sp_{slug}/`) を回収する（doc 44 P1 の後始末）。
/// 戻り値は削除した dir 数。
///
/// fold-in（#823）で repo プロセスが消え、repo 次元は table の `repo_path` 列が持つように
/// なったため、`db/sp_*` は **1 バイトも読まれない残骸**になった。だが撤去されたのは
/// 「開くコード」だけで、既に disk にある dir はそのまま残っていた（実機で 23 dir / 約 1.2 GB）。
///
/// 捨ててよいことは doc 44 §5.2 で 2026-07-20 に検証済み。実害は旧 DB の board board
/// (`pane_contents`) が引き継がれないことだけで、これは fold-in の破壊的変更として
/// 既に出荷・周知されている（board は空から始まる）。
///
/// - `daemon` は名前で除外する（prefix `sp_` の dir だけを対象にする）
/// - best-effort: 個々の削除失敗は warn して残置し、他は続行する
/// - 冪等: 残骸が無ければ 0 を返すだけ
pub fn reclaim_legacy_repo_dbs_in(root: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0usize;
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.starts_with("sp_") {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(err) => {
                tracing::warn!("旧 repo DB の回収に失敗（残置）: {} err={err}", name);
            }
        }
    }
    removed
}

/// 本番 root での [`reclaim_legacy_repo_dbs_in`]（Daemon boot から 1 回）。
pub fn reclaim_legacy_repo_dbs() -> usize {
    reclaim_legacy_repo_dbs_in(&db_root())
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
        // doc 44 P1 PR4 以前は、これを typed marker (`DbLockHeldByLiveHolder`) で返し repo 起動路が
        // downcast して「重複 spawn」と判定していた。db が単一化された今、この db を開くのは
        // Daemon だけで、daemon の単一性は :32000 の port bind (`bind_dual_stack` は SO_REUSEADDR
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

    /// ヘルスチェック（DB に接続できているか確認）
    pub async fn health(&self) -> bool {
        self.db.query("RETURN true").await.is_ok()
    }

    /// 内部の Surreal への参照を取得
    pub fn inner(&self) -> &Surreal<Any> {
        &self.db
    }

    // =========================================================================
    // Daemon identity (federation L2、 ADR-020 D2): home-node の位置独立 安定 id `nd_xxx`。
    // db/machine の singleton row (固定 record id node_identity:self)。daemon が初回起動で
    // 1 度だけ発行し永続、 以降の再起動は復元する。[`crate::lane::lane_id::load_or_create`] の
    // db 版 — lane は (repo,lane) ごと file 永続、 daemon は daemon に 1 つなので db singleton。
    // =========================================================================

    /// home-node の node_id を取得する (無ければ生成して永続)。
    ///
    /// - 既存 singleton row があり非空なら **それを復元** (= 再起動を越えて安定)。
    /// - 無い / 空なら **新規生成して永続** し、 その id を返す。
    ///
    /// daemon は single-writer (db comment 参照) かつ boot で 1 度だけ呼ぶため race は無い。
    /// 書き込みは REMOVE FIELD (旧 catalog 掃除、 下記) + DELETE+CREATE を単一 query に
    /// まとめて行う (空 row が残っていた場合も確実に上書き。 万一 DDL だけ効いて DML が
    /// 失敗しても「制約が外れて row が無い」だけの状態なので、 次回 boot の再発行で自己修復する)。
    pub async fn load_or_create_node_id(&self) -> Result<crate::node::NodeId> {
        // 既存 singleton row の node_id を読む (存在しなければ空配列)。
        let mut result = self
            .db
            .query("SELECT VALUE node_id FROM node_identity:self")
            .await
            .map_err(|e| anyhow::anyhow!("node_id 取得失敗: {}", e))?;
        // 旧 schema 行 (v0.56.0 以前 = `wld_id` field のみ) では node_id が NONE になり、
        // Vec<String> への take は Err を返す。これは「未発行」と同義なので既定値 (空) に
        // 潰して下の再発行経路へ落とす (ADR-021 P5 — field rename 自体が移行機構。 Err の
        // まま返すと呼び出し側が degraded 継続し、 空 node_id で hub に register してしまう)。
        let existing: Vec<String> = result.take(0).unwrap_or_default();
        if let Some(id) = existing.into_iter().find(|s| !s.trim().is_empty()) {
            return Ok(crate::node::NodeId::from(id));
        }

        // 無ければ新規発行して永続する。 先に旧 catalog の残存 field 定義を外す —
        // v0.56.0 以前の DB は SCHEMAFULL に `wld_id: string` (必須) を定義しており、
        // migration DDL (DEFINE IF NOT EXISTS) は既存定義に触らないため rename 後も残る。
        // 残ったままだと node_id のみの新行 CREATE が必須違反で弾かれ、 再発行が Err →
        // 呼び出し側の degraded 継続 = 空 node_id register に化ける (mito-mba 実 live 二段目)。
        let id = crate::node::NodeId::generate();
        self.db
            .query(
                "REMOVE FIELD IF EXISTS wld_id ON node_identity;
                 DELETE node_identity:self;
                 CREATE node_identity:self CONTENT {
                    node_id: $node_id,
                    created_at: time::now()
                 }",
            )
            .bind(("node_id", id.as_str().to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("node_id 永続失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("node_id 永続エラー: {}", e))?;
        tracing::info!("home-node identity 発行: node_id={}", id);
        Ok(id)
    }

    // VP-188: Repos CRUD は撤去。 registered repos の SSOT は embedded DB から
    // `~/.config/vp/repos.kdl` に移行 (= VP-182 の「DB dir 変更で repos 消失」
    // regression を構造的に解消、 council 2026-05-16)。 repos 永続化は
    // `crate::repos_file::ReposFile` が担う。

    // =========================================================================
    // Processes CRUD
    // =========================================================================

    /// 稼働中プロセスを登録（UPSERT）
    pub async fn upsert_process(
        &self,
        repo_path: &str,
        repo_name: &str,
        port: u16,
        pid: u32,
        status: &str,
    ) -> Result<()> {
        self.db
            .query(
                "INSERT INTO processes {
                    repo_path: $repo_path,
                    repo_name: $repo_name,
                    port: $port,
                    pid: $pid,
                    status: $status,
                    started_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    repo_name = $input.repo_name,
                    port = $input.port,
                    pid = $input.pid,
                    status = $input.status",
            )
            .bind(("repo_path", repo_path.to_string()))
            .bind(("repo_name", repo_name.to_string()))
            .bind(("port", port as i64))
            .bind(("pid", pid as i64))
            .bind(("status", status.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("process upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("process upsert エラー: {}", e))?;
        Ok(())
    }

    /// プロセスを登録解除（repo_path で特定）
    pub async fn delete_process(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM processes WHERE repo_path = $path")
            .bind(("path", repo_path.to_string()))
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
    // Active lane (presence、 Model Q): repo ごとの選択中 lane を daemon-canonical に
    // =========================================================================

    /// active lane を upsert (repo_path → lane_address)。
    pub async fn upsert_active_lane(&self, repo_path: &str, lane_address: &str) -> Result<()> {
        self.db
            .query(
                "INSERT INTO active_lane {
                    repo_path: $repo_path,
                    lane_address: $lane_address,
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    lane_address = $input.lane_address,
                    updated_at = time::now()",
            )
            .bind(("repo_path", repo_path.to_string()))
            .bind(("lane_address", lane_address.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("active_lane upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("active_lane upsert エラー: {}", e))?;
        Ok(())
    }

    /// 全 active lane を (repo_path, lane_address) で返す (boot 時の load 用)。
    pub async fn list_active_lanes(&self) -> Result<Vec<(String, String)>> {
        // list_processes と同じく serde_json::Value で受ける (surrealdb の SurrealValue 制約回避)。
        let mut result = self
            .db
            .query("SELECT repo_path, lane_address FROM active_lane")
            .await
            .map_err(|e| anyhow::anyhow!("active_lane 取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        Ok(rows
            .into_iter()
            .filter_map(|v| {
                let path = v.get("repo_path")?.as_str()?.to_string();
                let addr = v.get("lane_address")?.as_str()?.to_string();
                Some((path, addr))
            })
            .collect())
    }

    /// active lane を削除する (repo remove 時の presence 回収、 §4.6 含有=所有=寿命)。
    pub async fn delete_active_lane(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM active_lane WHERE repo_path = $path")
            .bind(("path", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("active_lane 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("active_lane 削除エラー: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Repo Host 帳簿①: 開発起点ポインタ (doc 44 D4)。
    //
    // active_lane (注視) と形は同じ 1-repo-1-row だが意味が違う (D5 が分けた
    // 「注視の切替」と「起点の再指定」)。値が **lane_id** なのは rename 耐性のため。
    // =========================================================================

    /// 開発起点ポインタを upsert する (repo_path → lane_id)。
    pub async fn upsert_host_origin(&self, repo_path: &str, lane_id: &str) -> Result<()> {
        self.db
            .query(
                "INSERT INTO host_origin {
                    repo_path: $repo_path,
                    lane_id: $lane_id,
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    lane_id = $input.lane_id,
                    updated_at = time::now()",
            )
            .bind(("repo_path", repo_path.to_string()))
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
    pub async fn get_host_origin(&self, repo_path: &str) -> Result<Option<String>> {
        let mut result = self
            .db
            .query("SELECT lane_id FROM host_origin WHERE repo_path = $path")
            .bind(("path", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_origin 取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        Ok(rows
            .first()
            .and_then(|v| v.get("lane_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()))
    }

    /// lane の並び順を repo 単位で**全置換**する（doc 44 D5）。
    ///
    /// 並び順は集合なので replace 型（`replace_lanes` と同じ考え方）。差分 upsert にすると
    /// 「並びから外れた lane の古い ord」が残り、次に現れた時に意図しない位置に挿さる。
    pub async fn replace_lane_order(&self, repo_path: &str, order: &[String]) -> Result<()> {
        self.db
            .query("DELETE host_lane_order WHERE repo_path = $p")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("lane 並び順の削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane 並び順の削除エラー: {}", e))?;
        for (i, lane_id) in order.iter().enumerate() {
            self.db
                .query(
                    "CREATE host_lane_order SET
                        repo_path = $p, lane_id = $id, ord = $ord, updated_at = time::now()",
                )
                .bind(("p", repo_path.to_string()))
                .bind(("id", lane_id.clone()))
                .bind(("ord", i as i64))
                .await
                .map_err(|e| anyhow::anyhow!("lane 並び順の永続失敗: {}", e))?
                .check()
                .map_err(|e| anyhow::anyhow!("lane 並び順の永続エラー: {}", e))?;
        }
        Ok(())
    }

    /// lane の並び順を引く（`lane_id` → `ord`）。未指定 repo は空。
    pub async fn list_lane_order(
        &self,
        repo_path: &str,
    ) -> Result<std::collections::HashMap<String, i64>> {
        let mut result = self
            .db
            .query("SELECT lane_id, ord FROM host_lane_order WHERE repo_path = $p")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("lane 並び順の取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        Ok(rows
            .into_iter()
            .filter_map(|v| {
                let id = v.get("lane_id")?.as_str()?.to_string();
                let ord = v.get("ord")?.as_i64()?;
                Some((id, ord))
            })
            .collect())
    }

    /// lane の並び順を repo ごと回収する（`delete_host_origin` と対、§4.6 含有=所有=寿命）。
    pub async fn delete_lane_order_for_repo(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE host_lane_order WHERE repo_path = $p")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("lane 並び順の全削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane 並び順の全削除エラー: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Repo Host 帳簿③: 見送りの記録 (doc 44 §7.5 / §8.5)。
    //
    // 「いつ何を見送ったか」(= lane を消したので survey では復元できない) と
    // 「AskHuman がいつから何回続いているか」(= 観測の履歴なので計算できない) の 2 つ。
    // key は host_origin / host_lane_order と同じ **lane_id**。
    // =========================================================================

    /// DB の 1 行を [`crate::host::ledger::FarewellEntry`] に写す (壊れた行は `None`)。
    ///
    /// 1 行の欠損で一覧全体を落とさない (帳簿は best-effort read)。
    fn farewell_row(v: &serde_json::Value) -> Option<crate::host::ledger::FarewellEntry> {
        Some(crate::host::ledger::FarewellEntry {
            lane_id: v.get("lane_id")?.as_str()?.to_string(),
            lane_name: v
                .get("lane_name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string(),
            kind: crate::host::ledger::FarewellKind::from_label(v.get("kind")?.as_str()?)?,
            reason: v
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or_default()
                .to_string(),
            streak: v.get("streak").and_then(|s| s.as_u64()).unwrap_or(1) as u32,
            first_seen_at: v
                .get("first_seen_at")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string(),
            last_seen_at: v
                .get("last_seen_at")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string(),
            ongoing: v.get("ongoing").and_then(|o| o.as_bool()).unwrap_or(false),
        })
    }

    /// 継続中の滞留を引く (repo × lane に高々 1 行)。
    pub async fn get_open_farewell(
        &self,
        repo_path: &str,
        lane_id: &str,
    ) -> Result<Option<crate::host::ledger::FarewellEntry>> {
        let mut result = self
            .db
            .query(
                "SELECT * FROM host_farewell
                 WHERE repo_path = $p AND lane_id = $id AND ongoing = true LIMIT 1",
            )
            .bind(("p", repo_path.to_string()))
            .bind(("id", lane_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_farewell 取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        Ok(rows.first().and_then(Self::farewell_row))
    }

    /// 帳簿に 1 行足す (滞留の起票 / 見送りの記録)。
    pub async fn create_farewell_entry(
        &self,
        repo_path: &str,
        entry: &crate::host::ledger::FarewellEntry,
    ) -> Result<()> {
        self.db
            .query(
                "CREATE host_farewell SET
                    repo_path = $p, lane_id = $id, lane_name = $name, kind = $kind,
                    reason = $reason, streak = $streak,
                    first_seen_at = $first, last_seen_at = $last, ongoing = $ongoing",
            )
            .bind(("p", repo_path.to_string()))
            .bind(("id", entry.lane_id.clone()))
            .bind(("name", entry.lane_name.clone()))
            .bind(("kind", entry.kind.as_str().to_string()))
            .bind(("reason", entry.reason.clone()))
            .bind(("streak", entry.streak as i64))
            .bind(("first", entry.first_seen_at.clone()))
            .bind(("last", entry.last_seen_at.clone()))
            .bind(("ongoing", entry.ongoing))
            .await
            .map_err(|e| anyhow::anyhow!("host_farewell 追加失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("host_farewell 追加エラー: {}", e))?;
        Ok(())
    }

    /// 継続中の滞留を伸ばす (連続回数と直近観測時刻の更新)。
    ///
    /// `lane_name` は**更新しない** — 記録時点のスナップショットなので rename で動かさない
    /// (doc 44 §8.5)。`first_seen_at` も同じ理由で不変。
    pub async fn extend_open_farewell(
        &self,
        repo_path: &str,
        lane_id: &str,
        streak: u32,
        reason: &str,
        last_seen_at: &str,
    ) -> Result<()> {
        self.db
            .query(
                "UPDATE host_farewell SET streak = $streak, reason = $reason, last_seen_at = $last
                 WHERE repo_path = $p AND lane_id = $id AND ongoing = true",
            )
            .bind(("p", repo_path.to_string()))
            .bind(("id", lane_id.to_string()))
            .bind(("streak", streak as i64))
            .bind(("reason", reason.to_string()))
            .bind(("last", last_seen_at.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_farewell 更新失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("host_farewell 更新エラー: {}", e))?;
        Ok(())
    }

    /// 継続中の滞留を閉じる (判定が判断待ちから外れた / lane を見送った)。
    ///
    /// 行は消さない — 「いつからいつまで判断待ちだったか」は履歴として残す。
    pub async fn close_open_farewell(&self, repo_path: &str, lane_id: &str) -> Result<()> {
        self.db
            .query(
                "UPDATE host_farewell SET ongoing = false
                 WHERE repo_path = $p AND lane_id = $id AND ongoing = true",
            )
            .bind(("p", repo_path.to_string()))
            .bind(("id", lane_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_farewell 終端失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("host_farewell 終端エラー: {}", e))?;
        Ok(())
    }

    /// 継続中の滞留を repo 単位で列挙する (`vp lane cleanup` の滞留表示)。
    pub async fn list_open_farewells(
        &self,
        repo_path: &str,
    ) -> Result<Vec<crate::host::ledger::FarewellEntry>> {
        let mut result = self
            .db
            .query("SELECT * FROM host_farewell WHERE repo_path = $p AND ongoing = true")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_farewell 滞留取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        Ok(rows.iter().filter_map(Self::farewell_row).collect())
    }

    /// 帳簿を新しい順に読む (`vp lane history`)。`limit` 0 は無制限。
    pub async fn list_farewell_entries(
        &self,
        repo_path: &str,
        limit: usize,
    ) -> Result<Vec<crate::host::ledger::FarewellEntry>> {
        let mut result = self
            .db
            .query("SELECT * FROM host_farewell WHERE repo_path = $p ORDER BY last_seen_at DESC")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_farewell 履歴取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        let mut entries: Vec<crate::host::ledger::FarewellEntry> =
            rows.iter().filter_map(Self::farewell_row).collect();
        if limit > 0 {
            entries.truncate(limit);
        }
        Ok(entries)
    }

    /// 見送りの記録を repo ごと回収する (`delete_host_origin` と対、§4.6 含有=所有=寿命)。
    pub async fn delete_farewell_entries_for_repo(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE host_farewell WHERE repo_path = $p")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_farewell 全削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("host_farewell 全削除エラー: {}", e))?;
        Ok(())
    }

    /// 開発起点ポインタを削除する (repo remove 時の回収、`delete_active_lane` と対)。
    pub async fn delete_host_origin(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM host_origin WHERE repo_path = $path")
            .bind(("path", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_origin 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("host_origin 削除エラー: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Lane descriptor (doc 24 §10 Phase 2: LanePool authority 反転 repo→daemon)。
    // repo push の cache だった lane_registry を daemon-canonical な durable truth に。
    // repo disconnect では drop せず、 daemon 再起動は db から re-animate する (§3.3 / §4.1)。
    // =========================================================================

    /// 1 lane descriptor を upsert する (repo の Diff::Add / Diff::Update 反映)。
    ///
    /// (repo_path, address) 複合 key で一意。 ON DUPLICATE の composite 挙動に依存せず、
    /// DELETE→CREATE を 1 query (= 中間状態を他読みに晒さない) で行う。 info は LaneInfo を
    /// 丸ごと JSON object 化して持つ (descriptor truth)。
    pub async fn upsert_lane(
        &self,
        repo_path: &str,
        lane: &crate::repo::lane::LaneInfo,
    ) -> Result<()> {
        let address = lane.address.to_string();
        let descriptor = serde_json::to_value(lane)
            .map_err(|e| anyhow::anyhow!("lane descriptor serialize 失敗: {}", e))?;
        self.db
            .query(
                "DELETE lane WHERE repo_path = $p AND address = $a;
                 CREATE lane CONTENT {
                    repo_path: $p,
                    address: $a,
                    descriptor: $descriptor,
                    updated_at: time::now()
                 }",
            )
            .bind(("p", repo_path.to_string()))
            .bind(("a", address))
            .bind(("descriptor", descriptor))
            .await
            .map_err(|e| anyhow::anyhow!("lane upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane upsert エラー: {}", e))?;
        Ok(())
    }

    /// 1 lane descriptor を削除する (repo の Diff::Remove 反映 / 単一 lane の destroy)。
    pub async fn delete_lane(&self, repo_path: &str, address: &str) -> Result<()> {
        self.db
            .query("DELETE lane WHERE repo_path = $p AND address = $a")
            .bind(("p", repo_path.to_string()))
            .bind(("a", address.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("lane 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane 削除エラー: {}", e))?;
        Ok(())
    }

    /// 1 repo の lane descriptor を全削除する (repo remove 時の回収、 §4.6 含有=所有=寿命)。
    pub async fn delete_lanes_for_repo(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE lane WHERE repo_path = $p")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("repo lane 全削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("repo lane 全削除エラー: {}", e))?;
        Ok(())
    }

    /// 1 repo の lane descriptor を snapshot で全置換する (repo register snapshot 反映)。
    ///
    /// snapshot は「その時点の repo の全 lane」なので、 既存を消してから入れ直す repo 単位
    /// replace 型 (active_lane の高頻度 1 行 upsert と違い、 lane は集合なので全置換が自然)。
    pub async fn replace_lanes_for_repo(
        &self,
        repo_path: &str,
        lanes: &[crate::repo::lane::LaneInfo],
    ) -> Result<()> {
        self.delete_lanes_for_repo(repo_path).await?;
        for lane in lanes {
            self.upsert_lane(repo_path, lane).await?;
        }
        Ok(())
    }

    /// 全 lane descriptor を (repo_path, LaneInfo) で返す (boot 時の load 用)。
    ///
    /// list_processes と同じく serde_json::Value で受け、 info object を LaneInfo に
    /// deserialize する。 壊れた行は warn して skip (boot を止めない、 §4.6 ゆるやか統治)。
    pub async fn list_lanes(&self) -> Result<Vec<(String, crate::repo::lane::LaneInfo)>> {
        let mut result = self
            .db
            .query("SELECT repo_path, descriptor FROM lane")
            .await
            .map_err(|e| anyhow::anyhow!("lane 取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        let mut out = Vec::with_capacity(rows.len());
        for v in rows {
            let Some(path) = v.get("repo_path").and_then(|x| x.as_str()) else {
                continue;
            };
            let Some(desc_val) = v.get("descriptor") else {
                continue;
            };
            match serde_json::from_value::<crate::repo::lane::LaneInfo>(desc_val.clone()) {
                Ok(info) => out.push((path.to_string(), info)),
                Err(e) => tracing::warn!("lane descriptor deserialize 失敗 (skip): {}", e),
            }
        }
        Ok(out)
    }

    // =========================================================================
    // Lane lifecycle (doc 24 §4.6: durable lifecycle state machine、 軽量 WAL)。
    // descriptor (lane table) とは別 table — repo push に clobber されない daemon-internal。
    // =========================================================================

    /// lane の lifecycle を upsert する (provisioning / ready / dead)。
    ///
    /// team-b #2: active_lane が `INSERT ON DUPLICATE KEY UPDATE` (単一 key) なのに対し、 lane 系は
    /// **複合 key (repo_path, address)** で ON DUPLICATE の発火が不確実なため DELETE+CREATE を使う
    /// (upsert_lane / lane table と同方針)。 2 statement は単一 `query()` = 1 transaction で
    /// atomic に走る (DELETE 後 CREATE 前に row が消える窓は無い)。
    pub async fn upsert_lane_lifecycle(
        &self,
        repo_path: &str,
        address: &str,
        lifecycle: &str,
    ) -> Result<()> {
        self.db
            .query(
                "DELETE lane_lifecycle WHERE repo_path = $p AND address = $a;
                 CREATE lane_lifecycle CONTENT {
                    repo_path: $p,
                    address: $a,
                    lifecycle: $lc,
                    updated_at: time::now()
                 }",
            )
            .bind(("p", repo_path.to_string()))
            .bind(("a", address.to_string()))
            .bind(("lc", lifecycle.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("lane_lifecycle upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane_lifecycle upsert エラー: {}", e))?;
        Ok(())
    }

    /// 全 lane lifecycle を (repo_path, address, lifecycle) で返す (boot reconcile 用)。
    pub async fn list_lane_lifecycles(&self) -> Result<Vec<(String, String, String)>> {
        let mut result = self
            .db
            .query("SELECT repo_path, address, lifecycle FROM lane_lifecycle")
            .await
            .map_err(|e| anyhow::anyhow!("lane_lifecycle 取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        Ok(rows
            .into_iter()
            .filter_map(|v| {
                let p = v.get("repo_path")?.as_str()?.to_string();
                let a = v.get("address")?.as_str()?.to_string();
                let lc = v.get("lifecycle")?.as_str()?.to_string();
                Some((p, a, lc))
            })
            .collect())
    }

    /// 1 lane の lifecycle を削除 (lane destroy / lifecycle 回収)。
    pub async fn delete_lane_lifecycle(&self, repo_path: &str, address: &str) -> Result<()> {
        self.db
            .query("DELETE lane_lifecycle WHERE repo_path = $p AND address = $a")
            .bind(("p", repo_path.to_string()))
            .bind(("a", address.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("lane_lifecycle 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane_lifecycle 削除エラー: {}", e))?;
        Ok(())
    }

    /// 1 repo の lane lifecycle を全削除 (repo remove 時の回収、 §4.6 含有=所有=寿命)。
    pub async fn delete_lane_lifecycles_for_repo(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE lane_lifecycle WHERE repo_path = $p")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("repo lane_lifecycle 全削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("repo lane_lifecycle 全削除エラー: {}", e))?;
        Ok(())
    }

    /// 全プロセスを削除（daemon 再起動時のクリーンアップ用）
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
    // Repos CRUD（PoC: VP-188 revert、 db/machine 真実源 + repos.kdl 一方向 export）
    // =========================================================================

    /// 登録 repo を UPSERT（path で一意）。 ord = sidebar 並び順。
    pub async fn upsert_repo(
        &self,
        path: &str,
        name: &str,
        enabled: Option<bool>,
        slot: Option<u16>,
        ord: i64,
    ) -> Result<()> {
        self.db
            .query(
                "INSERT INTO repos {
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
            .map_err(|e| anyhow::anyhow!("repo upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("repo upsert エラー: {}", e))?;
        Ok(())
    }

    /// 登録 repo を削除（path で特定）。
    pub async fn delete_repo(&self, path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM repos WHERE path = $path")
            .bind(("path", path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("repo 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("repo 削除エラー: {}", e))?;
        Ok(())
    }

    /// 登録 repo 一覧を ord 昇順（= sidebar 並び順）で取得。
    pub async fn list_repos(&self) -> Result<Vec<serde_json::Value>> {
        let mut result = self
            .db
            .query("SELECT * FROM repos ORDER BY ord ASC")
            .await
            .map_err(|e| anyhow::anyhow!("repos 取得失敗: {}", e))?;
        let records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records)
    }

    /// DB の repos を RepoEntry 列に export（ord 昇順、 PoC: 一方向 export）。
    pub async fn export_repos(&self) -> Result<Vec<crate::repos_file::RepoEntry>> {
        let rows = self.list_repos().await?;
        Ok(rows
            .iter()
            .map(|v| crate::repos_file::RepoEntry {
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

    /// RepoEntry 列を DB に import（出現順を ord に焼く、 PoC: 復旧用）。
    pub async fn import_repos(&self, entries: &[crate::repos_file::RepoEntry]) -> Result<()> {
        for (i, e) in entries.iter().enumerate() {
            self.upsert_repo(&e.path, &e.name, e.enabled, e.slot, i as i64)
                .await?;
        }
        Ok(())
    }

    /// repos テーブルを `entries` で全置換する（DELETE → import、 ord = 出現順）。
    ///
    /// `persist_repos` の全置換セマンティクスを 1 メソッドに閉じる。 in-memory を真実源として
    /// DB を上書きするため、 in-memory から消えた repo は DB からも消える (= upsert のみでは
    /// 残ってしまう削除分を確実に反映)。
    ///
    /// DELETE と import の間に空を読む窓が理論上あるが、 daemon は単一プロセスで reload/persist を
    /// 直列実行するため実害なし。 完全な単一トランザクション化は follow-up (epic memory のリスク表)。
    pub async fn replace_all_repos(&self, entries: &[crate::repos_file::RepoEntry]) -> Result<()> {
        self.db
            .query("DELETE FROM repos")
            .await
            .map_err(|e| anyhow::anyhow!("repos 全削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("repos 全削除エラー: {}", e))?;
        self.import_repos(entries).await
    }

    // =========================================================================
    // Pane Contents CRUD（Canvas ペイン状態の永続化）
    // =========================================================================

    /// ペイン状態を保存（UPSERT: repo_path + pane_id で一意）
    pub async fn upsert_pane_content(
        &self,
        repo_path: &str,
        pane_id: &str,
        content_type: &str,
        content: &str,
        title: Option<&str>,
    ) -> Result<()> {
        // lane_name='' (= main sentinel) の row として upsert。 新 schema (lane_name/stack/ui_state) は
        // ON DUPLICATE KEY UPDATE 句で **触らない** — 旧 caller (= 純粋な content / title 更新)
        // が board Canvas Stack の stack / ui_state を巻き戻さないようにする。
        self.db
            .query(
                "INSERT INTO pane_contents {
                    repo_path: $repo_path,
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
            .bind(("repo_path", repo_path.to_string()))
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

    /// board Canvas Stack Model の lane scope な永続状態を upsert する (= doc 19 + pp-content-persist)。
    ///
    /// - `lane_name`: None なら main (= 内部で `''` sentinel)、 Some(name) なら sub。 UNIQUE INDEX は
    ///   (repo_path, lane_name, pane_id) のため root/sub は別 record として独立。
    /// - `stack`: Canvas Stack (= items + cursor + capacity)。 None なら未保存。
    /// - `ui_state`: visibility/collapsed/サイズ等。 None なら未保存。
    /// - `content` / `content_type` / `title` は **現在 main pane で render 中の item の reflection**
    ///   (= 旧 caller 互換)。 stack が主、 content は seek 用 fallback。
    #[allow(clippy::too_many_arguments)] // pane_contents の field count に追従、 caller (route handler) も flat に展開する
    pub async fn upsert_board_state(
        &self,
        repo_path: &str,
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
                    repo_path: $repo_path,
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
            .bind(("repo_path", repo_path.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("lane_name", lane_sentinel.to_string()))
            .bind(("content_type", content_type.to_string()))
            .bind(("content", content.to_string()))
            .bind(("title", title.map(|s| s.to_string())))
            .bind(("stack", stack.cloned()))
            .bind(("ui_state", ui_state.cloned()))
            .await
            .map_err(|e| anyhow::anyhow!("board_state upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("board_state upsert エラー: {}", e))?;
        Ok(())
    }

    /// 特定 (repo_path, lane_name, pane_id) の board state を 1 件取得。 不在なら Ok(None)。
    ///
    /// 旧 record (= lane_name field なし) は schema DEFAULT '' で self-heal され、 main として読める。
    pub async fn load_board_state(
        &self,
        repo_path: &str,
        lane_name: Option<&str>,
        pane_id: &str,
    ) -> Result<Option<serde_json::Value>> {
        let lane_sentinel = lane_name.unwrap_or("");
        let mut result = self
            .db
            .query(
                "SELECT * FROM pane_contents
                 WHERE repo_path = $path
                   AND pane_id = $pane_id
                   AND lane_name = $lane
                 LIMIT 1",
            )
            .bind(("path", repo_path.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("lane", lane_sentinel.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("board_state load 失敗: {}", e))?;
        let mut records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records.pop())
    }

    // =========================================================================
    // board モデル (2026-07-15): scope 別 Canvas board の CRUD
    //
    // board = board Canvas に show した item の scope 別永続リスト（repo が唯一の truth を持つ）。
    // stack = { items: [...新→古], cursor: <id|NONE> } を pane_contents.stack に保存する。
    // キーは (repo_path, scope, lane_name, pane_id)。 lane board は lane_name で lane ごとに
    // 分離、 proj board は lane_name='' (repo 共有)。
    // =========================================================================

    /// board に item を atomic に head-push する（= mcp__show 着信 1 件）。
    ///
    /// - item を items の先頭に追加し（新→古）、 `capacity` を超えた末尾（最古）を切り、
    ///   cursor を新 item に更新する。
    /// - RMW を避け ON DUPLICATE KEY UPDATE 内の array 関数で atomic に行う
    ///   （人/agent が連続 show した際の read-modify-write race を排除）。
    /// - `item` は webview の BoardItem 形（camelCase: id/content/contentType/title/createdAt）。
    ///   top-level content/content_type/title は「現在 main で見せる item の reflection」(seek fallback)。
    pub async fn append_board_item(
        &self,
        repo_path: &str,
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
                    repo_path: $repo_path,
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
                        -- cursor 据え置き（scrollback）は 3 条件を全て満たすときだけ: (1) NONE でない
                        -- (2) 旧 head でない（head を見ていたら follow）(3) capacity trim 後も生き残る。
                        -- (3) が無いと、最古 item を pin した状態で show が来ると cursor が指す item が
                        -- evict されて孤児化し主画面が無言で空白化する（team-b review, doc 52 §5「流されない」に反する）。
                        cursor: IF stack.cursor IS NOT NONE
                            AND stack.cursor != (stack.items ?? [])[0].id
                            AND stack.cursor IN array::slice(array::prepend(stack.items ?? [], $item), 0, $cap).id
                            THEN stack.cursor ELSE $item_id END
                    },
                    content_type = $input.content_type,
                    content = $input.content,
                    title = $input.title,
                    updated_at = time::now()",
            )
            .bind(("repo_path", repo_path.to_string()))
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
        repo_path: &str,
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
                 WHERE repo_path = $path AND scope = $scope
                   AND lane_name = $lane AND pane_id = $pane_id",
            )
            .bind(("path", repo_path.to_string()))
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

    /// board の item を id で **in-place 置換**する（= mcp__update、doc 52 §5）。
    ///
    /// stack 内の id 一致 item の content / contentType だけ差し替え、 id / title / createdAt は
    /// 保つ（計器の更新 = 位置も生成時刻も動かさない。fresh 判定 createdAt<BOOT_TS のままなので
    /// focus を奪わない）。cursor が対象を指していれば top-level reflection も更新する。
    /// id 不一致は array::map が no-op（呼び出し側が事前に存在確認して loud error にする）。
    // (repo_path, scope, lane_name, pane_id) の board-key 4 分割は sibling（append/delete）と
    // 揃えているため、item_id + content + content_type を足すと 8 引数になる。bundle すると
    // 兄弟と不整合になるので許容する。
    #[allow(clippy::too_many_arguments)]
    pub async fn update_board_item(
        &self,
        repo_path: &str,
        scope: &str,
        lane_name: &str,
        pane_id: &str,
        item_id: &str,
        content: &str,
        content_type: &str,
    ) -> Result<()> {
        self.db
            .query(
                "UPDATE pane_contents SET
                    stack.items = array::map(stack.items ?? [], |$it|
                        IF $it.id = $item_id
                        THEN {
                            id: $it.id,
                            content: $content,
                            contentType: $content_type,
                            title: $it.title,
                            createdAt: $it.createdAt,
                            updatedAt: $updated_at
                        }
                        ELSE $it END),
                    content = IF stack.cursor = $item_id THEN $content ELSE content END,
                    content_type = IF stack.cursor = $item_id THEN $content_type ELSE content_type END,
                    updated_at = time::now()
                 WHERE repo_path = $path AND scope = $scope
                   AND lane_name = $lane AND pane_id = $pane_id",
            )
            .bind(("path", repo_path.to_string()))
            .bind(("scope", scope.to_string()))
            .bind(("lane", lane_name.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("item_id", item_id.to_string()))
            .bind(("content", content.to_string()))
            .bind(("content_type", content_type.to_string()))
            // updatedAt は RFC3339 文字列で stamp（show の createdAt と型を揃える = 額縁が
            // 一様に parse できる。time::now() の datetime 型だと read 時に型がばらつく）。
            .bind(("updated_at", chrono::Utc::now().to_rfc3339()))
            .await
            .map_err(|e| anyhow::anyhow!("board update 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("board update エラー: {}", e))?;
        Ok(())
    }

    /// board の cursor（= 注視 = main に出す item）を id で更新する（doc 52 §5 — cursor の
    /// server 昇格。thumbnail click / scrollback で mako の注視を repo truth にする）。
    ///
    /// cursor が指す item の content / contentType を top-level reflection にも写す
    /// （update_board_item の cursor 一致時と同じ扱い）。存在確認は呼び出し側が read-first で
    /// 行う（無い id を渡すと WHERE の item 条件で no-op になり cursor は動かない = 安全側）。
    pub async fn set_board_cursor(
        &self,
        repo_path: &str,
        scope: &str,
        lane_name: &str,
        pane_id: &str,
        item_id: &str,
    ) -> Result<()> {
        self.db
            .query(
                "UPDATE pane_contents SET
                    stack.cursor = $item_id,
                    content = (array::filter(stack.items ?? [], |$it| $it.id = $item_id)[0].content) ?? content,
                    content_type = (array::filter(stack.items ?? [], |$it| $it.id = $item_id)[0].contentType) ?? content_type,
                    updated_at = time::now()
                 WHERE repo_path = $path AND scope = $scope
                   AND lane_name = $lane AND pane_id = $pane_id
                   AND $item_id IN (stack.items ?? []).id",
            )
            .bind(("path", repo_path.to_string()))
            .bind(("scope", scope.to_string()))
            .bind(("lane", lane_name.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("item_id", item_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("board set_cursor 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("board set_cursor エラー: {}", e))?;
        Ok(())
    }

    /// board を空にする（= mcp__clear / Clear ボタン）。
    pub async fn clear_board(
        &self,
        repo_path: &str,
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
                 WHERE repo_path = $path AND scope = $scope
                   AND lane_name = $lane AND pane_id = $pane_id",
            )
            .bind(("path", repo_path.to_string()))
            .bind(("scope", scope.to_string()))
            .bind(("lane", lane_name.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("board clear 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("board clear エラー: {}", e))?;
        Ok(())
    }

    /// 特定 (repo_path, scope, lane_name, pane_id) の board を 1 件取得。 不在なら Ok(None)。
    pub async fn load_board(
        &self,
        repo_path: &str,
        scope: &str,
        lane_name: &str,
        pane_id: &str,
    ) -> Result<Option<serde_json::Value>> {
        let mut result = self
            .db
            .query(
                "SELECT * FROM pane_contents
                 WHERE repo_path = $path AND scope = $scope
                   AND lane_name = $lane AND pane_id = $pane_id
                 LIMIT 1",
            )
            .bind(("path", repo_path.to_string()))
            .bind(("scope", scope.to_string()))
            .bind(("lane", lane_name.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("board load 失敗: {}", e))?;
        let mut records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records.pop())
    }

    /// repoの全ペイン状態を取得
    pub async fn list_pane_contents(&self, repo_path: &str) -> Result<Vec<serde_json::Value>> {
        let mut result = self
            .db
            .query("SELECT * FROM pane_contents WHERE repo_path = $path")
            .bind(("path", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("pane_contents 取得失敗: {}", e))?;
        let records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records)
    }

    /// repoの全ペイン状態を削除
    pub async fn clear_pane_contents(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM pane_contents WHERE repo_path = $path")
            .bind(("path", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("pane_contents 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("pane_contents 削除エラー: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Agent Status CRUD
    // =========================================================================

    /// Agent ステータスを更新（UPSERT）
    pub async fn upsert_service_status(
        &self,
        repo_path: &str,
        agent_key: &str,
        status: &str,
        detail: Option<&serde_json::Value>,
    ) -> Result<()> {
        self.db
            .query(
                "INSERT INTO service_status {
                    repo_path: $repo_path,
                    agent_key: $agent_key,
                    status: $status,
                    detail: $detail,
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    status = $input.status,
                    detail = $input.detail,
                    updated_at = time::now()",
            )
            .bind(("repo_path", repo_path.to_string()))
            .bind(("agent_key", agent_key.to_string()))
            .bind(("status", status.to_string()))
            .bind(("detail", detail.cloned()))
            .await
            .map_err(|e| anyhow::anyhow!("service_status upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("service_status upsert エラー: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // LIVE SELECT（リアルタイム変更通知）
    // =========================================================================

    /// processes テーブルの LIVE SELECT を開始
    ///
    /// INSERT/UPDATE/DELETE のたびに `Notification<serde_json::Value>` を返すストリーム。
    /// daemon が購読して DistributedNotification に変換する。
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

    /// repoの全 Agent ステータスを取得
    pub async fn list_service_status(&self, repo_path: &str) -> Result<Vec<serde_json::Value>> {
        let mut result = self
            .db
            .query("SELECT * FROM service_status WHERE repo_path = $path")
            .bind(("path", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("service_status 取得失敗: {}", e))?;
        let records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records)
    }
}

// =============================================================================
// テスト
// =============================================================================

/// テスト用ヘルパー: kv-mem VpDb をスキーマ付きで作成
///
/// `db/` の domain module が共有する唯一の fixture（doc 62 §5）。ここに 1 本だけ置く。
#[cfg(test)]
pub(crate) async fn make_test_db() -> VpDb {
    let db = VpDb::connect_mem().await.unwrap();
    db.define_schema().await.unwrap();
    db
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧 per-repo DB の回収: `sp_*` だけを消し、`daemon` と無関係な dir / file は残す。
    /// 冪等（2 回目は 0）。掃除は「消えたか」でなく「**残っていないか**」で検証する。
    #[test]
    fn reclaim_legacy_repo_dbs_removes_only_sp_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        for d in ["machine", "sp_vp", "sp_nexus", "backups"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // 中身のある dir も丸ごと消える（remove_dir_all）
        std::fs::write(root.join("sp_vp").join("00000.sst"), b"data").unwrap();
        // dir でない `sp_` 始まりの file は対象外
        std::fs::write(root.join("sp_not_a_dir"), b"x").unwrap();

        assert_eq!(reclaim_legacy_repo_dbs_in(root), 2);

        assert!(root.join("machine").exists(), "machine は残る");
        assert!(root.join("backups").exists(), "無関係な dir は残る");
        assert!(root.join("sp_not_a_dir").exists(), "file は対象外");
        assert!(!root.join("sp_vp").exists(), "sp_ dir は中身ごと消える");
        assert!(!root.join("sp_nexus").exists());
        // 残っていないことの確認（列挙して sp_ dir が 0）
        let leftovers: Vec<_> = std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir() && e.file_name().to_string_lossy().starts_with("sp_"))
            .collect();
        assert!(leftovers.is_empty(), "sp_ dir が残っていない");

        assert_eq!(reclaim_legacy_repo_dbs_in(root), 0, "冪等");
    }

    /// doc 44 P1 PR4: DB ディレクトリは `vp_data_dir()/db/machine` の**単一**であること。
    ///
    /// 旧テストは「daemon と repo の dir が分離されていること」を固定していた（VP-182 の
    /// LOCK 衝突回避）。fold-in で repo がプロセスでなくなり handle 共有になったため、
    /// 固定すべき性質が「分離」から「単一」に反転した。
    #[test]
    fn test_db_data_dir_is_single_daemon_dir() {
        let daemon = db_data_dir_for_machine();

        // VP-192: vp_data_dir()/db 配下
        assert!(
            daemon.starts_with(crate::config::vp_data_dir()),
            "DB dir は vp_data_dir() 配下であるべき: {}",
            daemon.display()
        );
        assert!(
            daemon.parent().is_some_and(|p| p.ends_with("db")),
            "DB dir の親は 'db' であるべき: {}",
            daemon.display()
        );
        assert!(
            daemon.ends_with("machine"),
            "DB dir は 'machine' で終わるべき: {}",
            daemon.display()
        );
    }

    #[test]
    fn test_constants() {
        assert_eq!(NS, "vp");
        assert_eq!(DB_NAME, "vp");
    }

    #[tokio::test]
    async fn test_daemon_id_load_or_create_is_stable() {
        // federation L2: node_id singleton の発行 → 復元 round-trip。
        let db = make_test_db().await;

        // 初回は生成して永続 (EntId 形式 nd_1.. )。
        let first = db.load_or_create_node_id().await.unwrap();
        assert!(
            first.as_str().starts_with("nd_1"),
            "EntId 形式 nd_1.. のはず: {first}"
        );

        // 2 回目以降は同じ id を復元する (= singleton、 再起動越え安定の核)。
        let second = db.load_or_create_node_id().await.unwrap();
        assert_eq!(first, second, "node_id は singleton で安定して復元される");
    }

    /// ADR-021 P5 の実マシン移行経路: v0.56.0 以前の DB の上でも nd_ id が再発行されること。
    /// 実 live で mito-mba が二段構えで踏んだ regression の固定 (2026-07-27):
    /// ① 旧行 (node_id field 無し) の SELECT で take が Err → degraded 継続に化けて
    ///    **空 node_id で hub に register**。
    /// ② ①を直しても、 旧 catalog の残存 field 定義 (SCHEMAFULL `wld_id: string` 必須。
    ///    DEFINE IF NOT EXISTS は既存定義に触らないため rename 後も DB に残る) が
    ///    再発行の CREATE (node_id のみの行) を必須違反で弾き、 結局 Err → 空 register。
    /// fresh test db は旧「行」だけ模しても②を検出できない — 旧 **catalog** ごと模す。
    #[tokio::test]
    async fn test_node_id_reissues_over_legacy_row() {
        let db = make_test_db().await;

        // v0.56.0 の DB を模す: catalog を旧定義 (wld_id 必須・node_id 無し) に巻き戻して
        // 旧行を作り、 その上に v0.57.0 boot の migration DDL (node_id 追加) を適用する。
        db.db
            .query(
                "REMOVE FIELD node_id ON node_identity;
                 DEFINE FIELD wld_id ON node_identity TYPE string;
                 DELETE node_identity:self;
                 CREATE node_identity:self CONTENT { wld_id: 'wld_legacy', created_at: time::now() };
                 DEFINE FIELD IF NOT EXISTS node_id ON node_identity TYPE string;",
            )
            .await
            .unwrap()
            .check()
            .unwrap();

        // Err でも空でもなく、 nd_ を再発行して返す。
        let id = db.load_or_create_node_id().await.unwrap();
        assert!(
            id.as_str().starts_with("nd_1"),
            "旧行の上から nd_ を再発行するはず: {id}"
        );

        // 再発行後は通常の singleton 復元に合流する。
        let again = db.load_or_create_node_id().await.unwrap();
        assert_eq!(id, again, "再発行した id は以降安定して復元される");
    }

    /// doc 44 D4: 開発起点ポインタの round-trip（upsert → get → 上書き → 削除）。
    ///
    /// **削除まで見る**のは、`remove_repo` が repo namespace を倒す時にこの行を
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

        // repo ごとに独立（1 repo 1 ポインタ）
        db.upsert_host_origin("/repos/nexus", "id-beta")
            .await
            .unwrap();
        assert_eq!(
            db.get_host_origin("/repos/vp").await.unwrap().as_deref(),
            Some("id-alpha"),
            "他 repo の指定に引きずられない"
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

        // repo 回収でポインタも消える
        db.delete_host_origin("/repos/vp").await.unwrap();
        assert!(db.get_host_origin("/repos/vp").await.unwrap().is_none());
        assert_eq!(
            db.get_host_origin("/repos/nexus").await.unwrap().as_deref(),
            Some("id-beta"),
            "削除は repo scope に閉じる"
        );
    }

    #[tokio::test]
    async fn test_active_lane_upsert_and_list() {
        // Model Q: active lane (presence) の daemon-canonical round-trip。
        let db = make_test_db().await;

        // 初期は空
        assert!(db.list_active_lanes().await.unwrap().is_empty());

        // repo ごとに upsert
        db.upsert_active_lane("/repos/vp", "vp/root").await.unwrap();
        db.upsert_active_lane("/repos/nexus", "nexus/sub/foo")
            .await
            .unwrap();

        let mut rows = db.list_active_lanes().await.unwrap();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                ("/repos/nexus".to_string(), "nexus/sub/foo".to_string()),
                ("/repos/vp".to_string(), "vp/root".to_string()),
            ]
        );

        // 同 repo の upsert は置換 (UNIQUE index、 per-repo に 1 つ)
        db.upsert_active_lane("/repos/vp", "vp/sub/bar")
            .await
            .unwrap();
        let rows = db.list_active_lanes().await.unwrap();
        assert_eq!(rows.len(), 2, "同 repo は置換、 件数は増えない");
        assert!(rows.contains(&("/repos/vp".to_string(), "vp/sub/bar".to_string())));

        // §4.6 含有=所有=寿命: repo remove 時の presence 回収 (delete_active_lane)。
        db.delete_active_lane("/repos/vp").await.unwrap();
        let rows = db.list_active_lanes().await.unwrap();
        assert_eq!(rows.len(), 1, "削除した repo の active_lane は消える");
        assert_eq!(rows[0].0, "/repos/nexus", "他 repo は残る");
        // 不在 repo の削除は no-op (冪等)
        db.delete_active_lane("/repos/absent").await.unwrap();
        assert_eq!(db.list_active_lanes().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_lane_upsert_list_and_delete() {
        use crate::repo::lane::{LaneAddress, LaneInfo, LaneState};
        // doc 24 §10 Phase 2: lane descriptor の daemon-canonical durable round-trip。
        let db = make_test_db().await;

        // 初期は空
        assert!(db.list_lanes().await.unwrap().is_empty());

        // テスト用 LaneInfo builder (live 値 pid は埋めるが、 検証は descriptor 中心)。
        let mk = |repo: &str, name: &str| LaneInfo {
            id: Default::default(),
            address: LaneAddress::new(repo, name),
            state: LaneState::Running,
            agent: "claude".to_string(),
            created_at: "2026-06-20T00:00:00Z".to_string(),
            pid: Some(1234),
            cwd: "/tmp".to_string(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
            flow_state: None,
        };

        // 2 repo に lane を入れる
        db.upsert_lane("/repos/vp", &mk("vp", "main"))
            .await
            .unwrap();
        db.upsert_lane("/repos/vp", &mk("vp", "foo")).await.unwrap();
        db.upsert_lane("/repos/nexus", &mk("nexus", "main"))
            .await
            .unwrap();

        let rows = db.list_lanes().await.unwrap();
        assert_eq!(rows.len(), 3, "3 lane descriptor が round-trip する");

        // descriptor が round-trip する (address / agent)
        let vp_main = rows
            .iter()
            .find(|(p, l)| p == "/repos/vp" && l.address.is_root())
            .expect("vp root が読める");
        assert_eq!(vp_main.1.address.to_string(), "vp/lane/main");
        assert_eq!(vp_main.1.agent, "claude");

        // 同 address の upsert は置換 (複合 UNIQUE、 件数は増えない)
        db.upsert_lane("/repos/vp", &mk("vp", "main"))
            .await
            .unwrap();
        assert_eq!(
            db.list_lanes().await.unwrap().len(),
            3,
            "同 address の upsert は置換"
        );

        // 単一 lane の削除 (Diff::Remove)
        db.delete_lane("/repos/vp", "vp/lane/foo").await.unwrap();
        let rows = db.list_lanes().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            !rows
                .iter()
                .any(|(_, l)| l.address.to_string() == "vp/lane/foo"),
            "削除した lane は消える"
        );

        // snapshot 全置換 (register snapshot): /repos/vp を sub 2 つに置換
        db.replace_lanes_for_repo("/repos/vp", &[mk("vp", "a"), mk("vp", "b")])
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
            vp_lanes.iter().all(|(_, l)| !l.address.is_root()),
            "snapshot 後は root が消え sub のみ"
        );

        // §4.6 含有=所有=寿命: repo remove 時の回収 (delete_lanes_for_repo)。
        db.delete_lanes_for_repo("/repos/vp").await.unwrap();
        let rows = db.list_lanes().await.unwrap();
        assert_eq!(rows.len(), 1, "削除した repo の lane は消える");
        assert_eq!(rows[0].0, "/repos/nexus", "他 repo は残る");
    }

    #[tokio::test]
    async fn test_lane_lifecycle_upsert_list_delete() {
        // doc 24 §4.6: lane lifecycle (別 table) の round-trip。
        let db = make_test_db().await;
        assert!(db.list_lane_lifecycles().await.unwrap().is_empty());

        db.upsert_lane_lifecycle("/repos/vp", "vp/foo", "provisioning")
            .await
            .unwrap();
        db.upsert_lane_lifecycle("/repos/vp", "vp/sub/bar", "ready")
            .await
            .unwrap();
        db.upsert_lane_lifecycle("/repos/nexus", "nexus/sub/x", "ready")
            .await
            .unwrap();
        assert_eq!(db.list_lane_lifecycles().await.unwrap().len(), 3);

        // 同 (repo, address) の upsert は置換 (複合 UNIQUE)。
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

        // repo 単位削除 (§4.6 含有=所有=寿命)。
        db.delete_lane_lifecycles_for_repo("/repos/vp")
            .await
            .unwrap();
        let rows = db.list_lane_lifecycles().await.unwrap();
        assert_eq!(rows.len(), 1, "削除した repo の lifecycle は消える");
        assert_eq!(rows[0].0, "/repos/nexus", "他 repo は残る");
    }

    // VP-188: Repos CRUD テストは撤去 (= repos は repos.kdl に移行、
    // crate::repos_file 側の round-trip test でカバー)。

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
        assert_eq!(procs[0]["repo_name"], "vp");
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
    // Processes エッジケーステスト
    // =========================================================================

    /// 存在しない repo_path を delete_process してもエラーにならない
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

    /// 同一 (repo_path, pane_id) で再度 upsert → content が更新される（UPSERT 冪等性）
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

    /// 異なる repo_path のペインは list_pane_contents で見えない（repo分離）
    #[tokio::test]
    async fn test_pane_contents_repo_isolation() {
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

    /// clear_pane_contents は対象 repo_path のみ削除（他repoに影響なし）
    #[tokio::test]
    async fn test_pane_contents_clear_isolates_repos() {
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
    // board Canvas Stack Model (lane scope) — pp-content-persist
    // =========================================================================

    /// 新 API: lane_name=None (main) と Some(name) (sub) が独立 record として共存できる
    #[tokio::test]
    async fn test_board_state_main_and_sub_independent() {
        let db = make_test_db().await;

        let main_stack = serde_json::json!({
            "items": [{"id":"i1","content":"# root\n","contentType":"markdown","createdAt":"2026-05-28T00:00:00Z"}],
            "cursor": "i1",
            "capacity": 10
        });
        let sub_stack = serde_json::json!({
            "items": [{"id":"i2","content":"# sub\n","contentType":"markdown","createdAt":"2026-05-28T00:00:01Z"}],
            "cursor": "i2",
            "capacity": 10
        });
        let ui =
            serde_json::json!({"visible": true, "collapsed": false, "width": 480, "height": 720});

        db.upsert_board_state(
            "/repos/vp",
            None,
            "board",
            "markdown",
            "# root\n",
            None,
            Some(&main_stack),
            Some(&ui),
        )
        .await
        .unwrap();
        db.upsert_board_state(
            "/repos/vp",
            Some("foo"),
            "board",
            "markdown",
            "# sub\n",
            None,
            Some(&sub_stack),
            Some(&ui),
        )
        .await
        .unwrap();

        // main 読み込み
        let main = db
            .load_board_state("/repos/vp", None, "board")
            .await
            .unwrap()
            .expect("root record 不在");
        assert_eq!(
            main["lane_name"], "",
            "root は lane_name='' sentinel (= None)"
        );
        assert_eq!(main["stack"]["cursor"], "i1");

        // sub 読み込み — main と独立した record
        let sub = db
            .load_board_state("/repos/vp", Some("foo"), "board")
            .await
            .unwrap()
            .expect("sub record 不在");
        assert_eq!(sub["lane_name"], "foo");
        assert_eq!(sub["stack"]["cursor"], "i2");

        // list_pane_contents は両方見える (repo scope)
        let all = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(all.len(), 2, "root + sub で 2 record");
    }

    /// upsert_board_state は同 (repo_path, lane_name, pane_id) で stack を上書きする (= roundtrip)
    #[tokio::test]
    async fn test_board_state_upsert_roundtrip() {
        let db = make_test_db().await;
        let stack_v1 = serde_json::json!({"items": [], "cursor": null, "capacity": 10});
        let stack_v2 = serde_json::json!({
            "items": [{"id":"a","content":"x","contentType":"markdown","createdAt":"2026-05-28T00:00:00Z"}],
            "cursor": "a",
            "capacity": 10
        });

        db.upsert_board_state(
            "/repos/vp",
            None,
            "board",
            "markdown",
            "",
            None,
            Some(&stack_v1),
            None,
        )
        .await
        .unwrap();
        db.upsert_board_state(
            "/repos/vp",
            None,
            "board",
            "markdown",
            "x",
            None,
            Some(&stack_v2),
            None,
        )
        .await
        .unwrap();

        let rec = db
            .load_board_state("/repos/vp", None, "board")
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
    async fn test_board_state_legacy_upsert_keeps_stack() {
        let db = make_test_db().await;
        let stack = serde_json::json!({
            "items": [{"id":"keep","content":"keep","contentType":"markdown","createdAt":"2026-05-28T00:00:00Z"}],
            "cursor": "keep",
            "capacity": 10
        });

        // 新 API で stack を先に保存
        db.upsert_board_state(
            "/repos/vp",
            None,
            "board",
            "markdown",
            "keep",
            Some("t"),
            Some(&stack),
            None,
        )
        .await
        .unwrap();

        // 旧 API (content / title だけ更新)。 stack は触らないことを期待
        db.upsert_pane_content("/repos/vp", "board", "markdown", "updated", Some("t2"))
            .await
            .unwrap();

        let rec = db
            .load_board_state("/repos/vp", None, "board")
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

    /// load_board_state: 不在の (repo_path, lane_name, pane_id) は Ok(None)
    #[tokio::test]
    async fn test_board_state_load_missing_returns_none() {
        let db = make_test_db().await;
        let v = db
            .load_board_state("/repos/vp", None, "missing")
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
            db.append_board_item("/repos/vp", "proj", "", "board", &mk_item(id), 2)
                .await
                .unwrap();
        }
        let rec = db
            .load_board("/repos/vp", "proj", "", "board")
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
            db.append_board_item("/repos/vp", "lane", "wing", "board", &mk_item(id), 10)
                .await
                .unwrap();
        }
        // items=[c,b,a], cursor=c。 c を削除 → items=[b,a], cursor=b（先頭 fallback）。
        db.delete_board_item("/repos/vp", "lane", "wing", "board", "c")
            .await
            .unwrap();
        let rec = db
            .load_board("/repos/vp", "lane", "wing", "board")
            .await
            .unwrap()
            .unwrap();
        let items = rec["stack"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["id"], "b");
        assert_eq!(rec["stack"]["cursor"], "b");

        // items=[b,a], cursor=b。 a（非 cursor）削除 → cursor=b 不変。
        db.delete_board_item("/repos/vp", "lane", "wing", "board", "a")
            .await
            .unwrap();
        let rec = db
            .load_board("/repos/vp", "lane", "wing", "board")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            rec["stack"]["cursor"], "b",
            "非 cursor 削除で cursor は不変"
        );
        assert_eq!(rec["stack"]["items"].as_array().unwrap().len(), 1);
    }

    /// scrollback で最古 item を pin した状態で show が来て、その item が capacity trim で
    /// evict される場合、cursor は孤児化せず新 head に fallback する（team-b review、doc 52 §5
    /// 「流されない」に反する孤児 cursor = 主画面が無言で空白化するのを防ぐ）。
    #[tokio::test]
    async fn test_board_cursor_survives_capacity_eviction() {
        let db = make_test_db().await;
        // capacity=2 で a, b を貼る → items=[b,a]、cursor は head 追従で b。
        db.append_board_item("/repos/vp", "lane", "", "board", &mk_item("a"), 2)
            .await
            .unwrap();
        db.append_board_item("/repos/vp", "lane", "", "board", &mk_item("b"), 2)
            .await
            .unwrap();
        // 最古の a を pin（scrollback で遡って見ている状態）。
        db.set_board_cursor("/repos/vp", "lane", "", "board", "a")
            .await
            .unwrap();
        // c を貼る → items=[c,b]（a が evict）。cursor=a は消えるので新 head c に fallback。
        db.append_board_item("/repos/vp", "lane", "", "board", &mk_item("c"), 2)
            .await
            .unwrap();
        let rec = db
            .load_board("/repos/vp", "lane", "", "board")
            .await
            .unwrap()
            .unwrap();
        let items = rec["stack"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert!(
            !items.iter().any(|i| i["id"] == "a"),
            "最古 a は capacity trim で evict される"
        );
        assert_eq!(
            rec["stack"]["cursor"], "c",
            "evict された cursor は孤児化せず新 head c に fallback（主画面が空白化しない）"
        );
    }

    /// update は id 一致 item の content/contentType を in-place 置換し、id/createdAt/位置は保つ。
    /// cursor が対象を指していれば top-level content も反映する（doc 52 §5）。
    #[tokio::test]
    async fn test_board_update_item_in_place() {
        let db = make_test_db().await;
        for id in ["a", "b", "c"] {
            db.append_board_item("/repos/vp", "lane", "", "board", &mk_item(id), 10)
                .await
                .unwrap();
        }
        // items=[c,b,a], cursor=c。 非 cursor の b を html で更新。
        db.update_board_item(
            "/repos/vp",
            "lane",
            "",
            "board",
            "b",
            "updated-body",
            "html",
        )
        .await
        .unwrap();
        let rec = db
            .load_board("/repos/vp", "lane", "", "board")
            .await
            .unwrap()
            .unwrap();
        let items = rec["stack"]["items"].as_array().unwrap();
        // 位置不変（[c,b,a]）、b だけ content/contentType 差し替え、id/createdAt 保持。
        assert_eq!(items.len(), 3);
        assert_eq!(items[1]["id"], "b");
        assert_eq!(items[1]["content"], "updated-body");
        assert_eq!(items[1]["contentType"], "html");
        assert_eq!(items[1]["createdAt"], "2026-07-15T00:00:00Z");
        // cursor(c) 非対象なので top-level reflection は不変。
        assert_eq!(rec["stack"]["cursor"], "c");
        // 他 item は無傷。
        assert_eq!(items[0]["id"], "c");
        assert_eq!(items[0]["content"], "c");

        // cursor(c) を更新 → top-level content も反映。
        db.update_board_item(
            "/repos/vp",
            "lane",
            "",
            "board",
            "c",
            "c-latest",
            "markdown",
        )
        .await
        .unwrap();
        let rec = db
            .load_board("/repos/vp", "lane", "", "board")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            rec["content"], "c-latest",
            "cursor 対象の更新は top-level に反映"
        );
    }

    /// clear は board を空にする。
    #[tokio::test]
    async fn test_board_clear() {
        let db = make_test_db().await;
        db.append_board_item("/repos/vp", "proj", "", "board", &mk_item("a"), 10)
            .await
            .unwrap();
        db.clear_board("/repos/vp", "proj", "", "board")
            .await
            .unwrap();
        let rec = db
            .load_board("/repos/vp", "proj", "", "board")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rec["stack"]["items"].as_array().unwrap().len(), 0);
        assert!(rec["stack"]["cursor"].is_null());
    }

    /// lane board と proj board は同 repo でも独立（scope 軸で分離）。
    #[tokio::test]
    async fn test_board_scope_isolation() {
        let db = make_test_db().await;
        db.append_board_item("/repos/vp", "lane", "", "board", &mk_item("L"), 10)
            .await
            .unwrap();
        db.append_board_item("/repos/vp", "proj", "", "board", &mk_item("P"), 10)
            .await
            .unwrap();
        let lane = db
            .load_board("/repos/vp", "lane", "", "board")
            .await
            .unwrap()
            .unwrap();
        let proj = db
            .load_board("/repos/vp", "proj", "", "board")
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
    // Agent Status CRUD テスト
    // =========================================================================

    /// 基本的な INSERT → SELECT フロー
    #[tokio::test]
    async fn test_stand_status_basic_crud() {
        let db = make_test_db().await;

        db.upsert_service_status("/repos/vp", "heaven-door", "running", None)
            .await
            .unwrap();

        let statuses = db.list_service_status("/repos/vp").await.unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["agent_key"], "heaven-door");
        assert_eq!(statuses[0]["status"], "running");
    }

    /// 同一 (repo_path, agent_key) で再度 upsert → status が更新される
    #[tokio::test]
    async fn test_stand_status_upsert_updates_status() {
        let db = make_test_db().await;

        db.upsert_service_status("/repos/vp", "heaven-door", "running", None)
            .await
            .unwrap();

        db.upsert_service_status("/repos/vp", "heaven-door", "stopped", None)
            .await
            .unwrap();

        let statuses = db.list_service_status("/repos/vp").await.unwrap();
        // レコード数は1のまま（UPSERT）
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["status"], "stopped");
    }

    /// detail が None → NULL で保存できる
    #[tokio::test]
    async fn test_stand_status_detail_null() {
        let db = make_test_db().await;

        db.upsert_service_status("/repos/vp", "board", "idle", None)
            .await
            .unwrap();

        let statuses = db.list_service_status("/repos/vp").await.unwrap();
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

        db.upsert_service_status("/repos/vp", "board", "running", Some(&detail))
            .await
            .unwrap();

        let statuses = db.list_service_status("/repos/vp").await.unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["detail"]["canvas_open"], true);
        assert_eq!(statuses[0]["detail"]["pane_count"], 3);
    }

    /// 異なる repo_path の service_status は分離される
    #[tokio::test]
    async fn test_stand_status_repo_isolation() {
        let db = make_test_db().await;

        db.upsert_service_status("/repos/vp", "heaven-door", "running", None)
            .await
            .unwrap();
        db.upsert_service_status("/repos/creo", "heaven-door", "stopped", None)
            .await
            .unwrap();

        let vp_statuses = db.list_service_status("/repos/vp").await.unwrap();
        assert_eq!(vp_statuses.len(), 1);
        assert_eq!(vp_statuses[0]["status"], "running");

        let creo_statuses = db.list_service_status("/repos/creo").await.unwrap();
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
