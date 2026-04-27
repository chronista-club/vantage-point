use std::path::{Path, PathBuf};
use std::{env, fs, io};
use unison_kdl::KdlDeserialize;

const CONFIG_FILE: &str = ".claude/worker-files.kdl";

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

/// Parsed worker config
#[derive(Debug)]
pub struct WorkerConfig {
    pub symlinks: Vec<String>,
    pub copies: Vec<String>,
    pub symlink_patterns: Vec<String>,
    pub post_setup: Option<String>,
}

impl From<RawConfig> for WorkerConfig {
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

/// Load worker-files.kdl from the repo root
pub fn load_config(repo_root: &Path) -> Result<WorkerConfig, String> {
    let config_path = repo_root.join(CONFIG_FILE);
    if !config_path.exists() {
        return Err(format!(
            "{CONFIG_FILE} not found. Create it to define symlinks/copies for worker environments."
        ));
    }
    let content = fs::read_to_string(&config_path).map_err(|e| e.to_string())?;
    let raw: RawConfig = unison_kdl::from_str(&content).map_err(|e| e.to_string())?;
    Ok(raw.into())
}

/// Get the workers data directory (XDG_DATA_HOME compliant)
pub fn workers_dir() -> Result<PathBuf, String> {
    if let Ok(dir) = env::var("CCWS_WORKERS_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let data = env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home).join(".local/share")
        });
    Ok(data.join("ccws"))
}

/// Validate that a worker name is safe (allowlist: alphanumeric, hyphen, underscore)
pub fn validate_worker_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("worker name cannot be empty".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "invalid worker name: '{name}'. Only [a-zA-Z0-9_-] are allowed."
        ));
    }
    if name.starts_with('-') || name.starts_with('_') {
        return Err(format!(
            "invalid worker name: '{name}'. Must start with an alphanumeric character."
        ));
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

    // --- validate_worker_name ---

    #[test]
    fn valid_worker_names() {
        assert!(validate_worker_name("issue-42").is_ok());
        assert!(validate_worker_name("feature_login").is_ok());
        assert!(validate_worker_name("my-repo-fix-123").is_ok());
    }

    #[test]
    fn empty_name_rejected() {
        assert!(validate_worker_name("").is_err());
    }

    #[test]
    fn special_chars_rejected() {
        assert!(validate_worker_name("../etc/passwd").is_err());
        assert!(validate_worker_name("foo/bar").is_err());
        assert!(validate_worker_name("foo\\bar").is_err());
        assert!(validate_worker_name(".hidden").is_err());
        assert!(validate_worker_name("$(rm -rf)").is_err());
        assert!(validate_worker_name("foo;bar").is_err());
        assert!(validate_worker_name("foo bar").is_err());
    }

    #[test]
    fn leading_separator_rejected() {
        assert!(validate_worker_name("-leading").is_err());
        assert!(validate_worker_name("_leading").is_err());
    }

    // --- load_config (KDL parsing) ---

    /// Create a unique temp dir per test to avoid parallel test collisions
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ccws-test-{name}-{}", std::process::id()));
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
}
