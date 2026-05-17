//! cc session display name (custom-title) を `~/.claude/projects/<encoded-cwd>/<latest>.jsonl`
//! から読み出すための utility。
//!
//! ## VP-143 起点
//! cc の `/rename` は **silent** ── OSC を一切 emit しない (2026-05-08 dogfood で確認、
//! `osc_notification_capture.md` の observation catalog を update した)。 sidebar Lane 行に
//! session display name を反映するには、 cc が永続化する JSONL を読みに行く fallback path
//! が必要。 main_area.rs:773 の comment で予見されていた path を物理化したのが本 module。
//!
//! ## Stage 1 (本 module、 cwd ベース推定)
//! 1. lane の `cwd` を `~/.claude/projects/` 配下の encoded directory に変換
//! 2. 配下の `.jsonl` から **最新 modified** を選択 (= 1 lane / 1 cwd 仮定)
//! 3. file 内の **最後** の `{"type":"custom-title","customTitle":"..."}` entry を抽出
//!
//! 同 cwd に lead + 同所の worker 等で複数 lane が並ぶ場合は collision risk あり、
//! Stage 2 (`--session-id <uuid>` 強制 inject) で解消予定。
//!
//! ## 参考
//! - `tui/session.rs` (vantage-point crate) も `~/.claude/projects/` を読むが、 そちらは
//!   session 一覧 picker 用途で本 module とは別軸。 cwd encoding 規約は共通。

use std::path::{Path, PathBuf};

/// `cwd` を `~/.claude/projects/` 配下の encoded directory 名に変換。
///
/// claude CLI の慣例: path separator (`/`) と dot (`.`) を `-` に置換する。
///
/// 例:
/// - `/Users/makoto/repos/vantage-point` → `-Users-makoto-repos-vantage-point`
/// - `/Users/makoto/.config/vp/foo` → `-Users-makoto--config-vp-foo` (`.config` の dot 由来で `--`)
pub fn encode_cwd(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| match c {
            '/' | '.' => '-',
            other => other,
        })
        .collect()
}

/// `~/.claude/projects/<encoded(cwd)>/` の絶対 path を返す。 home 解決失敗時は `None`。
pub fn jsonl_dir_for_cwd(cwd: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let encoded = encode_cwd(cwd);
    Some(home.join(".claude").join("projects").join(encoded))
}

/// directory 配下の `.jsonl` files から最新 modified を選択。 dir 不在 / 空時は `None`。
pub fn latest_jsonl(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let modified = match entry.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => continue,
        };
        match &best {
            Some((_, t)) if modified <= *t => {}
            _ => best = Some((path, modified)),
        }
    }
    best.map(|(p, _)| p)
}

/// JSONL file 内の最後の `{"type":"custom-title","customTitle":"..."}` entry の
/// `customTitle` 値を抽出。
///
/// claude は session 中に `/rename` 毎に新しい `custom-title` entry を append するので、
/// **最後** の occurrence が「現在の表示名」。 file 中盤の旧 custom-title は超越する形。
pub fn extract_custom_title(jsonl: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(jsonl).ok()?;
    let reader = BufReader::new(file);
    let mut last_title: Option<String> = None;
    for line in reader.lines().map_while(Result::ok) {
        // file は数 MB ありうるので、 substring check で full JSON parse を skip。
        if !line.contains("\"custom-title\"") {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("custom-title") {
            continue;
        }
        if let Some(title) = v.get("customTitle").and_then(|t| t.as_str()) {
            last_title = Some(title.to_string());
        }
    }
    last_title
}

/// 完全フロー: `cwd` → 最新 jsonl → custom-title 抽出。
/// title 未設定 / file 不在時は `None`。
pub fn resolve_title_for_cwd(cwd: &Path) -> Option<String> {
    let dir = jsonl_dir_for_cwd(cwd)?;
    let jsonl = latest_jsonl(&dir)?;
    extract_custom_title(&jsonl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn encode_cwd_replaces_slash_and_dot() {
        let p = Path::new("/Users/makoto/repos/vantage-point");
        assert_eq!(encode_cwd(p), "-Users-makoto-repos-vantage-point");
    }

    #[test]
    fn encode_cwd_handles_dotted_segments() {
        // `.config` の dot 由来で連続 dash `--config` になる (claude CLI 慣例)
        let p = Path::new("/Users/makoto/.config/vp/foo");
        assert_eq!(encode_cwd(p), "-Users-makoto--config-vp-foo");
    }

    #[test]
    fn extract_custom_title_picks_last_entry() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("session.jsonl");
        let mut f = fs::File::create(&jsonl).unwrap();
        // mixed entries: 通常 prompt / 古い rename / 新しい rename
        writeln!(
            f,
            r#"{{"type":"last-prompt","leafUuid":"abc","sessionId":"s1"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"custom-title","customTitle":"old-name","sessionId":"s1"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"user","message":{{"role":"user","content":"hi"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"custom-title","customTitle":"new-name","sessionId":"s1"}}"#
        )
        .unwrap();
        drop(f);

        let title = extract_custom_title(&jsonl);
        assert_eq!(title.as_deref(), Some("new-name"));
    }

    #[test]
    fn extract_custom_title_returns_none_without_entry() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("plain.jsonl");
        let mut f = fs::File::create(&jsonl).unwrap();
        writeln!(
            f,
            r#"{{"type":"user","message":{{"role":"user","content":"hi"}}}}"#
        )
        .unwrap();
        drop(f);

        assert!(extract_custom_title(&jsonl).is_none());
    }

    #[test]
    fn latest_jsonl_picks_newest() {
        let dir = tempfile::tempdir().unwrap();
        let older = dir.path().join("older.jsonl");
        let newer = dir.path().join("newer.jsonl");
        fs::write(&older, "{}").unwrap();
        // ファイルシステム mtime 解像度を超えるよう短時間スリープ
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&newer, "{}").unwrap();
        // non-jsonl はスキップ
        fs::write(dir.path().join("noise.txt"), "x").unwrap();

        let picked = latest_jsonl(dir.path()).unwrap();
        assert_eq!(picked, newer);
    }

    #[test]
    fn latest_jsonl_empty_dir_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(latest_jsonl(dir.path()).is_none());
    }
}
