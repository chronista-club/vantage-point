//! lane ごとの Echoes session registry 永続（doc 38 — 1 Lane = N session）
//!
//! doc 38 §1 の 3 層分離の「session 層」を担う:
//!
//! ```text
//! 床（Act I の PTY）     = lane の設備（1 枚）。本 module の管轄外
//! session               = 会話の実体。identity は VP 採番のローカル key（1, 2, …）← ここ
//! 会話 id（cc_session 等）= session の Option 属性。各 engine の session_store が持つ
//! ```
//!
//! - **disk = 唯一の真実源**（doc 38 §5 原則「供給を 1 系統に」）。in-memory cache は持たない。
//!   registry の読み書きは全て本 module 経由（`LanePool` も RPC もここを読む）
//! - 置き場: `vp_state_dir()/echoes_sessions/<project>__<lane>.json`（1 lane 1 file）
//! - **file 不在 = N=1 の特殊ケース**: 「lane の stand で session #1 のみ・focused=1」に
//!   解決される。既存 install は registry file を持たないが従来どおり動く（既存動作不変の要）
//! - **session #1 の store label は素の lane 名**（[`session_label`]）。既存の
//!   `cc_sessions/<project>__<lane>` file がそのまま session #1 の会話 id になる =
//!   Act I（床の hook 書き込み）とも無改修で整合する
//! - 会話 id 自体は本 registry に**持たない**（[`super::session_store`] が SSOT のまま）。
//!   registry が持つのは key / engine 種別（stand）/ focused だけ

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::session_store::sanitize;

/// session の VP 採番ローカル key（1 始まり、lane 内で単調増加・再利用しない）。
pub type SessionKey = u32;

/// registry 上の 1 session（会話 id は持たない — 各 engine の session_store が SSOT）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEntry {
    /// VP 採番のローカル key。
    pub key: SessionKey,
    /// engine 種別（stand 名: "echoes" / "cursor" / "codex" / "agy"）。
    pub stand: String,
}

/// lane の session 一覧 + focused（disk に JSON でそのまま永続される形）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRegistry {
    /// 現在 focus されている session の key（常に `sessions` 内に実在する）。
    pub focused: SessionKey,
    /// 次に採番する key（単調増加。fresh reset まで再利用しない）。
    pub next: SessionKey,
    /// session 一覧（生成順）。空にはならない（最低 1 本）。
    pub sessions: Vec<SessionEntry>,
}

impl SessionRegistry {
    /// N=1 の特殊ケース（file 不在時の既定形）: lane の stand で session #1 のみ。
    fn single(default_stand: &str) -> Self {
        Self {
            focused: 1,
            next: 2,
            sessions: vec![SessionEntry {
                key: 1,
                stand: default_stand.to_string(),
            }],
        }
    }

    /// 不変条件の検証: 非空・key は 1 以上で重複なし・focused 実在・next は最大 key より大きい。
    /// 手編集や部分破損で崩れた file を「壊れた state で動き続ける」より default に戻す方が安全。
    fn is_valid(&self) -> bool {
        !self.sessions.is_empty()
            && self.sessions.iter().all(|s| s.key >= 1)
            && self
                .sessions
                .iter()
                .enumerate()
                .all(|(i, s)| !self.sessions[..i].iter().any(|t| t.key == s.key))
            && self.sessions.iter().any(|s| s.key == self.focused)
            && self.sessions.iter().all(|s| s.key < self.next)
    }
}

/// session の store label（各 engine session_store / host の記録キー）。
///
/// - **key 1 = 素の lane 名**: 既存 file（`cc_sessions/<project>__<lane>`）との後方互換 +
///   Act I（床）の hook 書き込み先と一致（doc 38 の「床は session #1 を既定で化身」）
/// - key 2 以降 = `<lane>#<n>`（doc 36 実証: `#` は [`sanitize`] で置換されない = file 名安全）
pub fn session_label(lane_label: &str, key: SessionKey) -> String {
    if key <= 1 {
        lane_label.to_string()
    } else {
        format!("{lane_label}#{key}")
    }
}

/// state base dir 配下の registry file path（純関数、テスト用に base 注入）。
fn registry_file_in(base: &Path, project: &str, lane: &str) -> PathBuf {
    base.join("echoes_sessions")
        .join(format!("{}__{}.json", sanitize(project), sanitize(lane)))
}

/// registry を読む。file 不在 / 破損 / 不変条件違反は N=1 の既定形に解決（Err にしない —
/// 読めない registry で lane 全体を止めるより、既定形で動き続ける方が復旧可能性が高い）。
pub fn load_in(base: &Path, project: &str, lane: &str, default_stand: &str) -> SessionRegistry {
    let Ok(raw) = std::fs::read_to_string(registry_file_in(base, project, lane)) else {
        return SessionRegistry::single(default_stand);
    };
    match serde_json::from_str::<SessionRegistry>(&raw) {
        Ok(reg) if reg.is_valid() => reg,
        _ => {
            tracing::warn!(
                "session registry が不正のため既定形に解決（project={project}, lane={lane}）"
            );
            SessionRegistry::single(default_stand)
        }
    }
}

/// registry を書く（上書き）。不変条件違反は書かずに Err（壊れた state を disk に固定しない）。
pub fn save_in(
    base: &Path,
    project: &str,
    lane: &str,
    reg: &SessionRegistry,
) -> std::io::Result<()> {
    if !reg.is_valid() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("session registry が不変条件違反（project={project}, lane={lane}）"),
        ));
    }
    let path = registry_file_in(base, project, lane);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(reg).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

/// session を 1 本追加して key を返す（`focus=true` なら focused も移す）。
pub fn create_in(
    base: &Path,
    project: &str,
    lane: &str,
    default_stand: &str,
    stand: &str,
    focus: bool,
) -> std::io::Result<SessionKey> {
    let mut reg = load_in(base, project, lane, default_stand);
    let key = reg.next;
    reg.next += 1;
    reg.sessions.push(SessionEntry {
        key,
        stand: stand.to_string(),
    });
    if focus {
        reg.focused = key;
    }
    save_in(base, project, lane, &reg)?;
    Ok(key)
}

/// focused を切り替える。実在しない key は Err（黙って据え置くと「切替えたつもり」の誤配送になる）。
pub fn focus_in(
    base: &Path,
    project: &str,
    lane: &str,
    default_stand: &str,
    key: SessionKey,
) -> std::io::Result<()> {
    let mut reg = load_in(base, project, lane, default_stand);
    if !reg.sessions.iter().any(|s| s.key == key) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("session が存在しません（project={project}, lane={lane}, session={key}）"),
        ));
    }
    reg.focused = key;
    save_in(base, project, lane, &reg)
}

/// focused key だけを軽量に読む（file 不在 / 破損は 1 = N=1 特殊ケース）。
/// `LaneInfo::refresh_engine_session_id` のような enrich 経路用（default stand 不要）。
pub fn focused_in(base: &Path, project: &str, lane: &str) -> SessionKey {
    let Ok(raw) = std::fs::read_to_string(registry_file_in(base, project, lane)) else {
        return 1;
    };
    match serde_json::from_str::<SessionRegistry>(&raw) {
        Ok(reg) if reg.is_valid() => reg.focused,
        _ => 1,
    }
}

/// registry を捨てる（fresh reset）。file 不在は no-op。
///
/// 「fresh = N=1 の既定形へ戻す」の state 側表現（doc 38 落とし穴②「fresh が副 session を
/// 知らない」の再演防止 — 個別 field の初期化でなく file ごと捨てて既定形に収束させる）。
/// 採番 counter も 1 からやり直しになる（fresh 後の会話 id は全 store で消えている前提）。
pub fn clear_in(base: &Path, project: &str, lane: &str) -> std::io::Result<()> {
    match std::fs::remove_file(registry_file_in(base, project, lane)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

// ---- 本番 base（vp_state_dir）での wrapper ----

/// 本番 base での load。
pub fn load(project: &str, lane: &str, default_stand: &str) -> SessionRegistry {
    load_in(&crate::config::vp_state_dir(), project, lane, default_stand)
}

/// 本番 base での create。
pub fn create(
    project: &str,
    lane: &str,
    default_stand: &str,
    stand: &str,
    focus: bool,
) -> std::io::Result<SessionKey> {
    create_in(
        &crate::config::vp_state_dir(),
        project,
        lane,
        default_stand,
        stand,
        focus,
    )
}

/// 本番 base での focus。
pub fn focus(
    project: &str,
    lane: &str,
    default_stand: &str,
    key: SessionKey,
) -> std::io::Result<()> {
    focus_in(
        &crate::config::vp_state_dir(),
        project,
        lane,
        default_stand,
        key,
    )
}

/// 本番 base での focused。
pub fn focused(project: &str, lane: &str) -> SessionKey {
    focused_in(&crate::config::vp_state_dir(), project, lane)
}

/// 本番 base での clear。
pub fn clear(project: &str, lane: &str) -> std::io::Result<()> {
    clear_in(&crate::config::vp_state_dir(), project, lane)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// file 不在 = N=1 の特殊ケース（lane の stand で session #1・focused=1）。
    /// 既存 install が registry file 無しで従来どおり動くことの根拠。
    #[test]
    fn load_without_file_resolves_to_single_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(
            reg,
            SessionRegistry {
                focused: 1,
                next: 2,
                sessions: vec![SessionEntry {
                    key: 1,
                    stand: "echoes".to_string()
                }],
            }
        );
        assert_eq!(focused_in(tmp.path(), "vp", "conductor"), 1);
    }

    /// create → 採番 2・focus 追随 → roundtrip 永続。focus=false は focused を据え置く。
    #[test]
    fn create_assigns_monotonic_keys_and_persists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let k2 = create_in(tmp.path(), "vp", "conductor", "echoes", "codex", true).expect("create");
        assert_eq!(k2, 2);
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.focused, 2, "focus=true は新 session に focus が移る");
        assert_eq!(reg.sessions.len(), 2);
        assert_eq!(reg.sessions[0].stand, "echoes", "session #1 は lane stand");
        assert_eq!(reg.sessions[1].stand, "codex");

        let k3 =
            create_in(tmp.path(), "vp", "conductor", "echoes", "echoes", false).expect("create");
        assert_eq!(k3, 3);
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.focused, 2, "focus=false は focused を動かさない");
        assert_eq!(reg.next, 4);
    }

    /// focus は実在 key のみ受理。不在 key は Err（黙って据え置かない）。
    #[test]
    fn focus_rejects_unknown_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        create_in(tmp.path(), "vp", "conductor", "echoes", "codex", false).expect("create");
        focus_in(tmp.path(), "vp", "conductor", "echoes", 2).expect("実在 key への focus");
        assert_eq!(focused_in(tmp.path(), "vp", "conductor"), 2);
        assert!(
            focus_in(tmp.path(), "vp", "conductor", "echoes", 99).is_err(),
            "不在 key への focus は Err"
        );
    }

    /// 破損 file / 不変条件違反は既定形に解決（壊れた state で動き続けない）。
    #[test]
    fn corrupt_or_invalid_file_falls_back_to_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("echoes_sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("vp__conductor.json");

        // 非 JSON
        std::fs::write(&file, "not json").unwrap();
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.sessions.len(), 1);
        assert_eq!(reg.focused, 1);

        // focused が不在 key（不変条件違反）
        std::fs::write(
            &file,
            r#"{"focused":9,"next":3,"sessions":[{"key":1,"stand":"echoes"}]}"#,
        )
        .unwrap();
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.focused, 1, "focused 不在の file は既定形に解決");
        assert_eq!(focused_in(tmp.path(), "vp", "conductor"), 1);

        // key 重複（不変条件違反）
        std::fs::write(
            &file,
            r#"{"focused":1,"next":3,"sessions":[{"key":1,"stand":"echoes"},{"key":1,"stand":"codex"}]}"#,
        )
        .unwrap();
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.sessions.len(), 1, "key 重複の file は既定形に解決");
    }

    /// clear = fresh reset。file が消えて既定形に戻り、採番も 1 からやり直し。冪等。
    #[test]
    fn clear_resets_to_default_and_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        create_in(tmp.path(), "vp", "conductor", "echoes", "codex", true).expect("create");
        clear_in(tmp.path(), "vp", "conductor").expect("clear");
        let reg = load_in(tmp.path(), "vp", "conductor", "echoes");
        assert_eq!(reg.sessions.len(), 1);
        assert_eq!(reg.next, 2, "fresh 後は採番もやり直し");
        // 未記録の clear は no-op（session_store と同じ原則）
        clear_in(tmp.path(), "vp", "conductor").expect("二重 clear は Ok");
    }

    /// session label: key 1 = 素の lane 名（既存 file 互換）、key 2+ = `<lane>#<n>`。
    /// `#` は sanitize で置換されない = session_store の file 名にそのまま安全に使える
    /// （doc 36 実証の固定化）。
    #[test]
    fn session_label_is_bare_for_key1_and_hash_suffixed_after() {
        assert_eq!(session_label("conductor", 1), "conductor");
        assert_eq!(session_label("conductor", 2), "conductor#2");
        assert_eq!(session_label("feat-x", 10), "feat-x#10");
        assert_eq!(sanitize("conductor#2"), "conductor#2", "# は sanitize 安全");
    }

    /// registry file 名も sanitize が効く（session_store と同じ規則）。
    #[test]
    fn registry_file_sanitizes_project_and_lane() {
        let p = registry_file_in(Path::new("/base"), "creo.memories", "conductor");
        assert_eq!(
            p,
            Path::new("/base/echoes_sessions/creo-memories__conductor.json")
        );
        let p = registry_file_in(Path::new("/base"), "a/b", "../evil");
        assert_eq!(p, Path::new("/base/echoes_sessions/a-b__---evil.json"));
    }
}
