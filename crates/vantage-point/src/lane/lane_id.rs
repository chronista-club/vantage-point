//! lane ごとの位置独立な安定 id の永続化 (I1、 doc 24 §7 / §10 Phase 2)。
//!
//! Lane の identity を path / port / PID から切り離す第一歩。Lane の cwd が
//! 動こうと repo が rename されようと、 この id は変わらない (= 発端バグの
//! path=identity を断つ種)。
//!
//! **strangler 注意**: 生成した id は **まだ pool key には使わない** (operative key は
//! [`crate::repo::lanes_state::LaneAddress`])。「id を持つが id で引かない」中間状態
//! の土台 — 後続 increment で徐々に id へ寄せる。
//!
//! - **書き手 / 読み手**: lane spawn 経路 (`LanePool::with_root` / `lane_spawn_actor` /
//!   `routes::lanes` の sub create) が [`load_or_create`] を呼ぶ。初回は生成 + 永続、
//!   2 回目以降 (= 再起動後の同 lane re-spawn) は disk から復元 → **再起動を越えて安定**。
//! - 置き場: `vp_state_dir()/lane_ids/<repo>__<lane>` (1 lane 1 file 1 行)。
//!   cc_session (`<repo>__<lane>`) と同じ命名規則。
//!
//! 設計は [`crate::lane::cc_session`] を mirror (純関数 `*_in(base)` + 本番 wrapper)。

use std::path::{Path, PathBuf};

use crate::repo::lanes_state::LaneId;

/// file 名に使えない文字を潰す ([`crate::lane::cc_session`] と同一規則)。
/// separator (`/` `\`) と `.` を `-` に置換し、 path traversal を自明に防ぐ。
fn sanitize(part: &str) -> String {
    part.chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == '.' {
                '-'
            } else {
                c
            }
        })
        .collect()
}

/// state base dir 配下の id file path (純関数、 テスト用に base 注入)。
pub fn id_file_in(base: &Path, repo: &str, lane: &str) -> PathBuf {
    base.join("lane_ids")
        .join(format!("{}__{}", sanitize(repo), sanitize(lane)))
}

/// (repo, lane) に対応する安定 id を取得する。
///
/// - 既存 file があり非空なら **それを復元** (= 再起動を越えて安定)。
/// - 無い / 空なら **新規生成して永続** し、 その id を返す。
///
/// 永続は best-effort (write 失敗時も生成した id を返す — cc_session の tolerant
/// 方針と同じで crash させない。 次回 load で再生成され得るのは degrade として許容)。
pub fn load_or_create_in(base: &Path, repo: &str, lane: &str) -> LaneId {
    let path = id_file_in(base, repo, lane);
    if let Ok(raw) = std::fs::read_to_string(&path) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return LaneId::from(trimmed.to_string());
        }
    }
    let id = LaneId::generate();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, id.as_str()) {
        tracing::warn!(
            "lane_id: 永続に失敗 (id は in-memory で続行、 次回再生成され得る) path={} err={}",
            path.display(),
            e
        );
    }
    id
}

/// 本番 base (`vp_state_dir`) での load_or_create (lane spawn 経路から呼ぶ)。
pub fn load_or_create(repo: &str, lane: &str) -> LaneId {
    load_or_create_in(&crate::config::vp_state_dir(), repo, lane)
}

/// lane 削除時に id file を消す (不在は no-op、 best-effort)。base 注入版。
///
/// lane-scoped state の一元 GC ([`crate::lane::commands::clear_lane_state_in`]) が呼ぶ。
/// 残すと同名 lane を作り直した時に旧 lane の安定 id が復元され、 別物のはずの新 lane が
/// 旧 identity を名乗る (position-independent id の目的に反する ghost leak)。
pub fn clear_in(base: &Path, repo: &str, lane: &str) -> std::io::Result<()> {
    match std::fs::remove_file(id_file_in(base, repo, lane)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_file_name_sanitizes_repo_and_lane() {
        // cc_session と同じ命名規則 (`.` `/` を `-` に)
        let p = id_file_in(Path::new("/base"), "creo.memories", "root");
        assert_eq!(p, Path::new("/base/lane_ids/creo-memories__root"));
    }

    #[test]
    fn load_or_create_is_stable_across_calls() {
        // 同 (repo, lane) は 2 回目以降 disk から復元 → 同じ id (= 再起動越え安定の核)
        let tmp = tempfile::tempdir().expect("tempdir");
        let first = load_or_create_in(tmp.path(), "vp", "root");
        let second = load_or_create_in(tmp.path(), "vp", "root");
        assert_eq!(first, second, "同 lane は同じ安定 id を返す");
        assert!(!first.is_empty(), "生成された id は非空");
    }

    #[test]
    fn load_or_create_distinct_lanes_differ() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let a = load_or_create_in(tmp.path(), "vp", "root");
        let b = load_or_create_in(tmp.path(), "vp", "sub-foo");
        assert_ne!(a, b, "別 lane は別 id");
    }

    #[test]
    fn load_or_create_recovers_existing_id() {
        // 既存 file の値をそのまま復元する (再起動後 re-spawn の模擬)
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("lane_ids");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vp__root"), "preset-stable-id").unwrap();
        let id = load_or_create_in(tmp.path(), "vp", "root");
        assert_eq!(id.as_str(), "preset-stable-id");
    }
}
