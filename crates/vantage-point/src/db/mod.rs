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
//! ## table ごとの永続操作は domain module へ移す途中（doc 62）
//!
//! 分離済みは `schema`（table 定義と起動時 migration）/ `board`（`pane_contents`）/
//! `lane`（descriptor・lifecycle・presence）/ `ledger`（Repo Host 帳簿 ①②③）。
//! process / repos / node / service_status の CRUD は
//! **まだこの file に同居している**（PR-5 で移設）。移設先はどれも `impl VpDb` を
//! 書き足すだけなので、[`VpDb`] は 1 型・接続も 1 本のまま。
//! 設計は [doc 62](../../../../docs/design/62-db-module-layout.md)。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;

mod board;
mod lane;
mod ledger;
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
/// 捨ててよいことは doc 44 §5.2 で 2026-07-20 に検証済み。実害は旧 DB の board
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
