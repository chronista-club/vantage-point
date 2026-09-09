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
//! ## table ごとの永続操作は domain module にある（doc 62）
//!
//! | module | table |
//! |---|---|
//! | `schema` | 全 18（table 定義と起動時 migration） |
//! | `node` | `node_identity` |
//! | `process` | `processes`（LIVE SELECT も） |
//! | `repos` | `repos` |
//! | `lane` | `lane` / `lane_lifecycle` / `active_lane` |
//! | `ledger` | `host_origin` / `host_lane_order` / `host_farewell` |
//! | `board` | `pane_contents` |
//! | `service_status` | `service_status` |
//!
//! 表に出ている 11 table 以外の 7 つ（`prompts` / `notifications` / wire 4 / `delegations`）は
//! `schema` が定義するだけで [`VpDb`] に method が無い。wire と delegation は
//! `capability/` 側が [`VpDb::inner`] 経由で持ち、`prompts` / `notifications` は誰も触っていない。
//!
//! どれも `impl VpDb` を書き足すだけなので、[`VpDb`] は **1 型・接続も 1 本**のまま。
//! 子 module は親の private field を見られるので、各 module は `self.db` を直接使う
//! （[`VpDb::inner`] は外向けの escape hatch で、`db/` の**本番コードでは使わない**。
//! 旧形 row を直に流し込む `schema` の test だけが例外）。
//! 設計は [doc 62](../../../../docs/design/62-db-module-layout.md)。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;

mod board;
mod lane;
mod ledger;
mod node;
mod process;
mod repos;
mod schema;
mod service_status;

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
