//! engine session id 永続の共通機構（cc_session / codex_session の共通核）
//!
//! engine が増えるたびに「state file への record / last / clear + injection 防壁」が
//! 複製されていた（claude → codex で 2 枚目）。差分は **dir 名と id 検証関数の 2 点だけ**
//! なので、それをパラメータ化した [`SessionStore`] に畳む。各 engine module
//! （`cc_session` / `codex_session`）は自分の `STORE` 定数と検証関数、
//! engine 固有の操作（claude の transcript 探索）だけを持つ。
//!
//! 共通の設計原則（全 engine で不変、doc 37 §1「engine = session 束縛」の物理面）:
//! - **書き読み両側で同じ検証**: state file は常に正規形（壊れた値を `--resume` に渡さない
//!   + shell single-quote 埋め込みの injection 防壁）
//! - **形式外は書かずに Ok**: 既存の正常な記録を壊れた値で上書きしない（silent 退化防止）
//! - **未記録の clear は no-op**: 二重 fresh を Err にしない
//! - 置き場: `vp_state_dir()/<dir>/<repo>__<lane>`（1 lane 1 file 1 行 = id）

use std::path::{Path, PathBuf};

/// engine session id の state file 束（1 engine = 1 定数）。
pub(crate) struct SessionStore {
    /// `vp_state_dir()` 配下の subdirectory 名（例: `"cc_sessions"`）。
    dir: &'static str,
    /// id の正規形検証。書き込み・読み出しの両側で使う。
    validate: fn(&str) -> bool,
}

impl SessionStore {
    pub(crate) const fn new(dir: &'static str, validate: fn(&str) -> bool) -> Self {
        Self { dir, validate }
    }

    /// state base dir 配下の session file path（純関数、テスト用に base 注入）。
    pub(crate) fn file_in(&self, base: &Path, repo: &str, lane: &str) -> PathBuf {
        base.join(self.dir)
            .join(format!("{}__{}", sanitize(repo), sanitize(lane)))
    }

    /// id を記録する（上書き、1 行）。形式外は**書かずに** Ok。
    pub(crate) fn record_in(
        &self,
        base: &Path,
        repo: &str,
        lane: &str,
        id: &str,
    ) -> std::io::Result<()> {
        if !(self.validate)(id) {
            return Ok(());
        }
        let path = self.file_in(base, repo, lane);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, id)
    }

    /// 最後に記録された id を返す。無い / 空 / 形式外は None。
    pub(crate) fn last_in(&self, base: &Path, repo: &str, lane: &str) -> Option<String> {
        let raw = std::fs::read_to_string(self.file_in(base, repo, lane)).ok()?;
        let trimmed = raw.trim();
        if !(self.validate)(trimmed) {
            return None;
        }
        Some(trimmed.to_string())
    }

    /// 記録を消す（未記録なら no-op）。「素の新規 session」（= fresh）の state 側表現。
    pub(crate) fn clear_in(&self, base: &Path, repo: &str, lane: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.file_in(base, repo, lane)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }
}

/// file 名に使えない文字を潰す。separator (`/` `\`) と `.` を `-` に置換 —
/// `__` 結合後も単一 path segment なので join での traversal は元々起きないが、
/// `..` を残さない方が読み手に安全性が自明。
pub(crate) fn sanitize(part: &str) -> String {
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

/// CLI 実行パスの明示解決（launchd 起動 daemon の細い PATH 対策、`agent::get_claude_cli_path` と同根）。
///
/// daemon は launchd 起動だと PATH が細く、Rust から直接 exec する経路（codex app-server の
/// 常駐 spawn 等）は login shell を経由しない（slot の login shell 注入は tui にだけ効く）。
///
/// 1. 現在の PATH で `which <name>` が当たればそれ
/// 2. `well_known`（インストール先の定番）に実在するものがあればそれ
/// 3. どちらも無ければ素の name（PATH に委ねる）
pub(crate) fn resolve_cli(name: &str, well_known: &[PathBuf]) -> String {
    if let Ok(output) = std::process::Command::new("which").arg(name).output()
        && output.status.success()
        && let Ok(path) = String::from_utf8(output.stdout)
    {
        let path = path.trim();
        if !path.is_empty() {
            return path.to_string();
        }
    }
    for cand in well_known {
        if cand.exists() {
            return cand.to_string_lossy().into_owned();
        }
    }
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の代表 store（検証規則は英数 + ハイフン）。
    fn valid(id: &str) -> bool {
        !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    }
    const STORE: SessionStore = SessionStore::new("test_sessions", valid);

    #[test]
    fn file_name_sanitizes_repo_and_lane() {
        let p = STORE.file_in(Path::new("/base"), "creo.memories", "root");
        assert_eq!(p, Path::new("/base/test_sessions/creo-memories__root"));
        let p = STORE.file_in(Path::new("/base"), "a/b", "../evil");
        assert_eq!(p, Path::new("/base/test_sessions/a-b__---evil"));
    }

    #[test]
    fn record_rejects_invalid_id() {
        // 形式外は書かない — 既存の正常な記録を壊れた値で上書きしない。
        let tmp = tempfile::tempdir().expect("tempdir");
        STORE
            .record_in(tmp.path(), "vp", "root", "good-id")
            .expect("record");
        STORE
            .record_in(tmp.path(), "vp", "root", "")
            .expect("空は no-op");
        STORE
            .record_in(tmp.path(), "vp", "root", "bad id'; rm")
            .expect("形式外は no-op");
        assert_eq!(
            STORE.last_in(tmp.path(), "vp", "root").as_deref(),
            Some("good-id"),
            "正常な記録が保持される"
        );
    }

    #[test]
    fn clear_removes_record_and_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        STORE
            .record_in(tmp.path(), "vp", "root", "old-id")
            .expect("record");
        STORE.clear_in(tmp.path(), "vp", "root").expect("clear");
        assert_eq!(STORE.last_in(tmp.path(), "vp", "root"), None);
        // 未記録 lane の clear は no-op（二重 fresh を Err にしない）。
        STORE
            .clear_in(tmp.path(), "vp", "root")
            .expect("未記録の clear は Ok");
        // 他 lane の記録は巻き添えにしない。
        STORE
            .record_in(tmp.path(), "vp", "sub-a", "keep-id")
            .expect("record");
        STORE.clear_in(tmp.path(), "vp", "root").expect("clear");
        assert_eq!(
            STORE.last_in(tmp.path(), "vp", "sub-a").as_deref(),
            Some("keep-id")
        );
    }

    #[test]
    fn record_and_last_roundtrip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        STORE
            .record_in(tmp.path(), "vp", "root", "0196-id")
            .expect("record");
        assert_eq!(
            STORE.last_in(tmp.path(), "vp", "root").as_deref(),
            Some("0196-id")
        );
        assert_eq!(STORE.last_in(tmp.path(), "vp", "w1"), None);
        // 上書き（最新が勝つ）。
        STORE
            .record_in(tmp.path(), "vp", "root", "newer-id")
            .expect("record 2");
        assert_eq!(
            STORE.last_in(tmp.path(), "vp", "root").as_deref(),
            Some("newer-id")
        );
    }

    #[test]
    fn last_rejects_garbage() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("test_sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vp__root"), "  \n").unwrap();
        assert_eq!(STORE.last_in(tmp.path(), "vp", "root"), None);
        std::fs::write(dir.join("vp__root"), "abc'; rm -rf /'").unwrap();
        assert_eq!(STORE.last_in(tmp.path(), "vp", "root"), None);
        std::fs::write(dir.join("vp__root"), "0196-abc\n").unwrap();
        assert_eq!(
            STORE.last_in(tmp.path(), "vp", "root").as_deref(),
            Some("0196-abc")
        );
    }

    #[test]
    fn resolve_cli_falls_back_to_bare_name() {
        // which でも well-known でも見つからない → 素の name（PATH 委ね）。
        let got = resolve_cli(
            "definitely-not-a-real-cli-név",
            &[PathBuf::from("/nonexistent/xyz")],
        );
        assert_eq!(got, "definitely-not-a-real-cli-név");
    }
}
