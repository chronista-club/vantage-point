use club_kdl::KdlDeserialize;
use std::path::{Path, PathBuf};
use std::{env, fs, io};

/// Wing 設定ファイル名 (primary)。 Wing workspace に symlink/copy するファイルを定義。
const CONFIG_FILE: &str = ".claude/wing-files.kdl";
/// Worker → Wing rename (2026-05-18) 前の旧ファイル名。 legacy fallback で受理する。
const LEGACY_CONFIG_FILE: &str = ".claude/worker-files.kdl";

#[derive(Debug, KdlDeserialize)]
#[kdl(name = "symlink")]
struct SymlinkEntry {
    #[kdl(argument)]
    pub path: String,
}

#[derive(Debug, KdlDeserialize)]
#[kdl(name = "copy")]
struct CopyEntry {
    #[kdl(argument)]
    pub path: String,
}

#[derive(Debug, KdlDeserialize)]
#[kdl(name = "symlink-pattern")]
struct SymlinkPatternEntry {
    #[kdl(argument)]
    pub pattern: String,
}

#[derive(Debug, KdlDeserialize)]
#[kdl(name = "post-setup")]
struct PostSetup {
    #[kdl(argument)]
    pub command: String,
}

#[derive(Debug, KdlDeserialize)]
#[kdl(document)]
struct RawConfig {
    #[kdl(children, name = "symlink")]
    symlinks: Vec<SymlinkEntry>,

    #[kdl(children, name = "copy")]
    copies: Vec<CopyEntry>,

    #[kdl(children, name = "symlink-pattern")]
    symlink_patterns: Vec<SymlinkPatternEntry>,

    #[kdl(child)]
    post_setup: Option<PostSetup>,
}

/// Parsed wing config
#[derive(Debug)]
pub struct WingConfig {
    pub symlinks: Vec<String>,
    pub copies: Vec<String>,
    pub symlink_patterns: Vec<String>,
    pub post_setup: Option<String>,
}

impl From<RawConfig> for WingConfig {
    fn from(raw: RawConfig) -> Self {
        Self {
            symlinks: raw.symlinks.into_iter().map(|e| e.path).collect(),
            copies: raw.copies.into_iter().map(|e| e.path).collect(),
            symlink_patterns: raw
                .symlink_patterns
                .into_iter()
                .map(|e| e.pattern)
                .collect(),
            post_setup: raw.post_setup.map(|e| e.command),
        }
    }
}

/// Find the git repo root from the current directory
pub fn find_repo_root() -> io::Result<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;

    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "not a git repository",
        ));
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(PathBuf::from(path))
}

/// Load wing-files.kdl (legacy: worker-files.kdl) from the repo root.
///
/// `.claude/wing-files.kdl` を優先し、 無ければ Worker → Wing rename 前の
/// `.claude/worker-files.kdl` に legacy fallback する。
pub fn load_config(repo_root: &Path) -> Result<WingConfig, String> {
    let primary = repo_root.join(CONFIG_FILE);
    let config_path = if primary.exists() {
        primary
    } else {
        repo_root.join(LEGACY_CONFIG_FILE)
    };
    if !config_path.exists() {
        return Err(format!(
            "{CONFIG_FILE} not found. Create it to define symlinks/copies for wing environments."
        ));
    }
    let content = fs::read_to_string(&config_path).map_err(|e| e.to_string())?;
    let raw: RawConfig = club_kdl::from_str(&content).map_err(|e| e.to_string())?;
    Ok(raw.into())
}

/// Lane データディレクトリを返す。
///
/// VP-196 Phase 2: 旧 `~/.local/share/ccws/` から `vp_data_dir()/lanes/` へ移行。
/// VP-192 で確立した data path SSOT (`vp_data_dir()`) 配下に統一する。
/// - macOS: `~/Library/Application Support/vp/lanes/`
/// - Linux: `~/.local/share/vp/lanes/`
///
/// `VP_LANES_DIR` 環境変数で明示上書き可能。
pub fn wings_dir() -> Result<PathBuf, String> {
    if let Ok(dir) = env::var("VP_LANES_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(crate::config::vp_data_dir().join("lanes"))
}

/// 旧 lane データディレクトリ (`~/.local/share/ccws/`) から新パスへの冪等な移行。
///
/// VP-196 Phase 2: lane 環境の置き場所を `vp_data_dir()/lanes/` に移す。
/// 旧パスが存在し新パスが無ければ、ディレクトリごと `rename` (move) する。
/// - `rename` は同一 volume なら atomic かつ瞬時。
/// - 冪等: 新パスが既にあれば skip。何度呼んでも安全。
/// - 失敗は warn ログのみ (起動を阻害しない)。
/// - `VP_LANES_DIR` で明示上書き中はユーザー管理下なので触らない。
///
/// 起動時 (`migrate_legacy_paths()`) に 1 回呼ぶこと。
pub fn migrate_legacy_lanes_dir() {
    if env::var("VP_LANES_DIR").is_ok() {
        return;
    }
    let Ok(new_dir) = wings_dir() else {
        return;
    };
    if new_dir.exists() {
        return;
    }
    // 旧パスは HOME ベースの固定 `~/.local/share/ccws/` (移行前 wings_dir の default)。
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let legacy = home.join(".local/share/ccws");
    if !legacy.is_dir() {
        return;
    }
    if let Some(parent) = new_dir.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::rename(&legacy, &new_dir) {
        Ok(()) => tracing::info!(
            "VP-196 lane migration: {} → {}",
            legacy.display(),
            new_dir.display()
        ),
        Err(e) => tracing::warn!(
            "VP-196 lane migration 失敗 (skip): {} → {}: {e}",
            legacy.display(),
            new_dir.display(),
        ),
    }
}

/// Validate that a wing name is safe (allowlist: alphanumeric, hyphen, underscore)
pub fn validate_wing_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("wing name cannot be empty".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "invalid wing name: '{name}'. Only [a-zA-Z0-9_-] are allowed."
        ));
    }
    if name.starts_with('-') || name.starts_with('_') {
        return Err(format!(
            "invalid wing name: '{name}'. Must start with an alphanumeric character."
        ));
    }
    // VP-166: `lead` は lead lane の予約名 (mailbox box key `<stand>#lead` と衝突するため)。
    // wing 名として使えない。設計: docs/design/16-wing-lane-mailbox-recv.md
    if name == "lead" {
        return Err(
            "invalid wing name: 'lead' is reserved for the lead lane. Pick another name.".into(),
        );
    }
    Ok(())
}

/// Get the repo name (basename of repo root)
pub fn repo_name() -> Option<String> {
    find_repo_root()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
}

/// Get the origin remote URL
pub fn get_remote_url() -> io::Result<String> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()?;

    if !output.status.success() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no origin remote"));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- validate_wing_name ---

    #[test]
    fn valid_wing_names() {
        assert!(validate_wing_name("issue-42").is_ok());
        assert!(validate_wing_name("feature_login").is_ok());
        assert!(validate_wing_name("my-repo-fix-123").is_ok());
    }

    #[test]
    fn empty_name_rejected() {
        assert!(validate_wing_name("").is_err());
    }

    #[test]
    fn special_chars_rejected() {
        assert!(validate_wing_name("../etc/passwd").is_err());
        assert!(validate_wing_name("foo/bar").is_err());
        assert!(validate_wing_name("foo\\bar").is_err());
        assert!(validate_wing_name(".hidden").is_err());
        assert!(validate_wing_name("$(rm -rf)").is_err());
        assert!(validate_wing_name("foo;bar").is_err());
        assert!(validate_wing_name("foo bar").is_err());
    }

    #[test]
    fn leading_separator_rejected() {
        assert!(validate_wing_name("-leading").is_err());
        assert!(validate_wing_name("_leading").is_err());
    }

    #[test]
    fn lead_name_rejected() {
        // VP-166: `lead` は lead lane の予約名 (mailbox box key `<stand>#lead` と衝突)
        assert!(validate_wing_name("lead").is_err());
        // 部分一致や派生名は OK (= `lead` 完全一致のみ禁止)
        assert!(validate_wing_name("leader").is_ok());
        assert!(validate_wing_name("my-lead").is_ok());
        assert!(validate_wing_name("lead-fix").is_ok());
    }

    // --- load_config (KDL parsing) ---

    /// Create a unique temp dir per test to avoid parallel test collisions
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lane-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir); // clean up leftover state
        dir
    }

    #[test]
    fn load_config_missing_file() {
        let tmp = test_dir("no-config");
        let _ = fs::create_dir_all(&tmp);
        let result = load_config(&tmp);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_symlinks_and_copies() {
        let tmp = test_dir("symlinks-copies");
        let _ = fs::create_dir_all(tmp.join(".claude"));
        fs::write(
            tmp.join(".claude/worker-files.kdl"),
            r#"symlink ".env"
symlink ".mcp.json"
copy "config/dev.toml"
symlink-pattern "**/*.local.*"
"#,
        )
        .unwrap();

        let cfg = load_config(&tmp).unwrap();
        assert_eq!(cfg.symlinks, vec![".env", ".mcp.json"]);
        assert_eq!(cfg.copies, vec!["config/dev.toml"]);
        assert_eq!(cfg.symlink_patterns, vec!["**/*.local.*"]);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_post_setup() {
        let tmp = test_dir("post-setup");
        let _ = fs::create_dir_all(tmp.join(".claude"));
        fs::write(
            tmp.join(".claude/worker-files.kdl"),
            "post-setup \"bun install\"\n",
        )
        .unwrap();

        let cfg = load_config(&tmp).unwrap();
        assert_eq!(cfg.post_setup.as_deref(), Some("bun install"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_empty_kdl() {
        let tmp = test_dir("empty-kdl");
        let _ = fs::create_dir_all(tmp.join(".claude"));
        fs::write(tmp.join(".claude/worker-files.kdl"), "").unwrap();

        let cfg = load_config(&tmp).unwrap();
        assert!(cfg.symlinks.is_empty());
        assert!(cfg.copies.is_empty());
        assert!(cfg.post_setup.is_none());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_invalid_kdl_unclosed_string() {
        let tmp = test_dir("invalid-kdl-unclosed");
        let _ = fs::create_dir_all(tmp.join(".claude"));
        // 閉じていない文字列リテラル
        fs::write(tmp.join(".claude/worker-files.kdl"), r#"symlink ".env"#).unwrap();

        let result = load_config(&tmp);
        assert!(result.is_err(), "unclosed string should return Err");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_invalid_kdl_syntax_error() {
        let tmp = test_dir("invalid-kdl-syntax");
        let _ = fs::create_dir_all(tmp.join(".claude"));
        // 不正な KDL 構文: 識別子の位置に記号
        fs::write(tmp.join(".claude/worker-files.kdl"), "= broken syntax {\n").unwrap();

        let result = load_config(&tmp);
        assert!(result.is_err(), "syntax error should return Err");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_wing_files_primary() {
        // 新ファイル名 `.claude/wing-files.kdl` を読めること。
        let tmp = test_dir("wing-primary");
        let _ = fs::create_dir_all(tmp.join(".claude"));
        fs::write(tmp.join(".claude/wing-files.kdl"), "copy \"x.toml\"\n").unwrap();

        let cfg = load_config(&tmp).unwrap();
        assert_eq!(cfg.copies, vec!["x.toml"]);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_prefers_wing_files_over_legacy() {
        // wing-files.kdl と 旧 worker-files.kdl が両方ある時、 wing 側を優先。
        let tmp = test_dir("wing-vs-legacy");
        let _ = fs::create_dir_all(tmp.join(".claude"));
        fs::write(tmp.join(".claude/wing-files.kdl"), "symlink \".env\"\n").unwrap();
        fs::write(
            tmp.join(".claude/worker-files.kdl"),
            "symlink \".legacy\"\n",
        )
        .unwrap();

        let cfg = load_config(&tmp).unwrap();
        assert_eq!(cfg.symlinks, vec![".env"], "wing-files.kdl を優先すべき");

        let _ = fs::remove_dir_all(&tmp);
    }
}
