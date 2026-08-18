use club_kdl::KdlDeserialize;
use std::path::{Path, PathBuf};
use std::{fs, io};

/// Sub 設定ファイル (新 path = `.vp/` 配下、 VP の永続化 boundary に整合)。
const SUB_CONFIG_NEW: &str = ".vp/sub-files.kdl";

/// Sub 設定ファイル (legacy path = `.claude/` 配下、 deprecation period 受理)。
const SUB_CONFIG_LEGACY: &str = ".claude/sub-files.kdl";

/// root/sub rename 前の旧ファイル名 (= `wing-files.kdl`)。既存 repo の
/// 設定を壊さないため legacy fallback として受理する (.vp / .claude 両方)。
const SUB_CONFIG_LEGACY_WING_VP: &str = ".vp/wing-files.kdl";
const SUB_CONFIG_LEGACY_WING_CLAUDE: &str = ".claude/wing-files.kdl";

/// Sub 設定不在時に auto-symlink する default file 群。
///
/// 選定基準: **gitignored で sub にも必要な per-user / secret file**。
/// shallow clone (= `git clone --depth 1`) では gitignored file は来ないので、
/// repo root から symlink して sub dir で同じ実行環境を再現する。
///
/// - `.mcp.json` — MCP server 接続定義 (= sub claude も同じ tool 群)
/// - `CLAUDE.local.md` — per-user の atlas / repo memory 設定
/// - `.env` — secrets (= API keys / DB password 等)
///
/// `.mise.toml` / `.tool-versions` は通常 git tracked なので clone で来る、 不要。
/// `.envrc` / `.claude/settings.local.json` 等は power user 用、 sub-files.kdl
/// で個別宣言する path。
const DEFAULT_SYMLINKS: &[&str] = &[".mcp.json", "CLAUDE.local.md", ".env"];

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

/// `base-ref "nightly"` — worktree lane の base branch (= dev trunk)。
///
/// worktree lane refactor: lane は `origin/<base-ref>` から `worktree add` する。
/// 未宣言なら `resolve_default_branch`(origin/HEAD) に fallback (commands.rs)。
/// 「GitHub default」 と decouple した「開発の幹」 を repo ごとに固定する用途
/// (= VP では `nightly`)。
#[derive(Debug, KdlDeserialize)]
#[kdl(name = "base-ref")]
struct BaseRef {
    #[kdl(argument)]
    pub name: String,
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

    #[kdl(child, name = "base-ref")]
    base_ref: Option<BaseRef>,
}

/// Parsed sub config
#[derive(Debug, Default)]
pub struct SubConfig {
    pub symlinks: Vec<String>,
    pub copies: Vec<String>,
    pub symlink_patterns: Vec<String>,
    pub post_setup: Option<String>,
    /// worktree lane の base branch (= dev trunk)。未宣言なら None →
    /// commands.rs の `resolve_default_branch` で origin/HEAD に fallback。
    pub base_ref: Option<String>,
}

impl From<RawConfig> for SubConfig {
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
            base_ref: raw.base_ref.map(|e| e.name),
        }
    }
}

/// Find the git repo root — **always the main worktree root**, even when called from a
/// linked worktree (`.vp/lanes/*` 等)。
///
/// `git rev-parse --show-toplevel` は呼び出し元の worktree 自身を返すため、 lane worktree の
/// 中から `vp lane` を実行すると repo 同一性がズレていた: lane が worktree 配下にネスト、
/// state file (engine_model / cc_session / console_mode) の repo key が repo 読み手
/// (`addr.repo` = main root basename) と mismatch、 Daemon handshake の repo_path が repo
/// 登録値と不一致。 全 worktree が共有する git-common-dir (`<main>/.git`) の親を main worktree
/// root として解決し、 どの worktree からでも同一 repo に着地させる (repo key 正規化)。
pub fn find_repo_root() -> io::Result<PathBuf> {
    find_repo_root_from(None)
}

/// [`find_repo_root`] の dir 注入版 (None = process cwd)。 テスト用に worktree path を渡せる。
fn find_repo_root_from(dir: Option<&Path>) -> io::Result<PathBuf> {
    let run = |args: &[&str]| -> io::Result<std::process::Output> {
        let mut cmd = std::process::Command::new("git");
        if let Some(d) = dir {
            cmd.arg("-C").arg(d);
        }
        cmd.args(args).output()
    };

    // 1. main worktree root = git-common-dir (全 worktree 共有の .git) の親。
    //    linked worktree の中からでも main root に着地する (`--show-toplevel` との違い)。
    if let Ok(out) = run(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
        && out.status.success()
    {
        let common = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        // 標準の non-bare repo は `<main>/.git`。 その親が main worktree root。
        // bare / 予期せぬ git-dir 形は下の show-toplevel fallback に委ねる。
        if common.file_name().is_some_and(|n| n == ".git")
            && let Some(root) = common.parent()
        {
            return Ok(root.to_path_buf());
        }
    }

    // 2. fallback: 従来の show-toplevel (古い git で --path-format 非対応、 bare 等)。
    let output = run(&["rev-parse", "--show-toplevel"])?;
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "not a git repository",
        ));
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(PathBuf::from(path))
}

/// repo root から sub-files.kdl を探す。
///
/// 探索順: 新 path (`.vp/sub-files.kdl`) → legacy path (`.claude/sub-files.kdl`) → None。
/// legacy hit 時は `tracing::info!` で move hint を出す (= 強制せず deprecation period 中)。
fn find_sub_config(repo_root: &Path) -> Option<PathBuf> {
    let new_path = repo_root.join(SUB_CONFIG_NEW);
    if new_path.is_file() {
        return Some(new_path);
    }
    let legacy_path = repo_root.join(SUB_CONFIG_LEGACY);
    if legacy_path.is_file() {
        tracing::info!(
            "sub-files.kdl: legacy path detected ({}). Consider moving to {} for clarity.",
            legacy_path.display(),
            new_path.display()
        );
        return Some(legacy_path);
    }
    // root/sub rename 前の旧名 (wing-files.kdl) も受理 (.vp → .claude)。
    for legacy_wing in [SUB_CONFIG_LEGACY_WING_VP, SUB_CONFIG_LEGACY_WING_CLAUDE] {
        let p = repo_root.join(legacy_wing);
        if p.is_file() {
            tracing::info!(
                "{legacy_wing}: 旧 wing-files.kdl を検出。{} へ rename を推奨。",
                new_path.display()
            );
            return Some(p);
        }
    }
    None
}

/// repo root に実在する default symlink 候補を返す。 不在 file は skip。
fn default_symlinks(repo_root: &Path) -> Vec<String> {
    DEFAULT_SYMLINKS
        .iter()
        .filter(|name| repo_root.join(name).exists())
        .map(|s| s.to_string())
        .collect()
}

/// sub-files.kdl を読み込む。 不在時は default symlinks を含む空 config を返す
/// (= zero-config sub 起動)。 parse error のみ Err。
///
/// 設計:
/// - 新 path (`.vp/sub-files.kdl`) 優先、 legacy (`.claude/sub-files.kdl`) も受理
/// - 両方不在 = repo root の `.mcp.json` / `CLAUDE.local.md` / `.env` を auto-symlink
/// - 明示宣言ある repo は zero-config に頼らず宣言通り (= default の merge は しない、
///   explicit override が筋。 default も欲しいなら config に明示書く)
pub fn load_config(repo_root: &Path) -> Result<SubConfig, String> {
    let Some(config_path) = find_sub_config(repo_root) else {
        // Zero-config: repo root の default file 群のみ auto-symlink
        let symlinks = default_symlinks(repo_root);
        if !symlinks.is_empty() {
            tracing::info!(
                "zero-config sub: auto-symlinking {} default file(s): {}",
                symlinks.len(),
                symlinks.join(", ")
            );
        }
        return Ok(SubConfig {
            symlinks,
            copies: vec![],
            symlink_patterns: vec![],
            post_setup: None,
            base_ref: None,
        });
    };
    let content = fs::read_to_string(&config_path).map_err(|e| e.to_string())?;
    let raw: RawConfig = club_kdl::from_str(&content).map_err(|e| e.to_string())?;
    Ok(raw.into())
}

/// Repo-local lane root を返す: `<repo_root>/.vp/lanes/`。
///
/// repo-local lane refactor PR 1: lane の正規 path。
/// - path に空白を含まない (= 旧 `~/Library/Application Support/vp/lanes/` の課題解消)
/// - repo 所属が path 階層で明示される (= repo prefix `<repo>-<name>` が不要)
/// - 親 repo の `.claude.json` trust が hierarchical に継承される (= claude folder
///   trust dialog が pre-grant なしで自動 skip)
pub fn repo_lanes_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".vp").join("lanes")
}

/// `<repo>/.gitignore` に `.vp/` ignore entry を idempotent に追記する。
///
/// repo-local lane refactor: lane workspace は nested git clone なので、 parent repo
/// から見ると untracked dir として `git status` に出てしまう。 これを抑制するため、
/// `vp lane new` 起動時に best-effort で `.gitignore` に `.vp/` を追記する。
///
/// 挙動:
/// - `.gitignore` 不在なら新規作成 (header コメント付き)
/// - 既に `.vp/` または `.vp` の行があれば skip (= idempotent)
/// - 失敗時は Err を返す (caller 側で best-effort 扱いするかは判断)
pub fn ensure_vp_gitignored(repo_root: &Path) -> Result<(), String> {
    let gi_path = repo_root.join(".gitignore");

    let existing = match fs::read_to_string(&gi_path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!(".gitignore 読み込み失敗: {e}")),
    };

    // 既に `.vp/` (末尾 slash 有無 / 行頭 `/` 有無) を ignore してれば skip
    let already_ignored = existing
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .any(|l| matches!(l, ".vp" | ".vp/" | "/.vp" | "/.vp/"));
    if already_ignored {
        return Ok(());
    }

    // 既存末尾の改行を保証してから append
    let mut new_content = existing;
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    if !new_content.is_empty() {
        new_content.push('\n');
    }
    new_content.push_str("# Vantage Point lane workspaces (repo-local lane refactor)\n");
    new_content.push_str(".vp/\n");

    fs::write(&gi_path, new_content).map_err(|e| format!(".gitignore 書込失敗: {e}"))
}

/// Validate that a sub name is safe (allowlist: alphanumeric, hyphen, underscore)
pub fn validate_sub_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("sub name cannot be empty".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "invalid sub name: '{name}'. Only [a-zA-Z0-9_-] are allowed."
        ));
    }
    if name.starts_with('-') || name.starts_with('_') {
        return Err(format!(
            "invalid sub name: '{name}'. Must start with an alphanumeric character."
        ));
    }
    // VP-166: 開発起点 lane の予約名は sub 名として使えない
    // (mailbox box key `<agent>#main` と衝突するため)。
    // 設計: docs/design/16-sub-lane-mailbox-recv.md
    //
    // doc 44 P2 以降、予約名の真実源は `ROOT_LANE_NAME` 定数。文字列直書きだと
    // 予約名を変えた時にここだけ古い値で残る (§6.4「型を経由しない文字列」の同型)。
    // ⚠️ **旧世代の予約名も禁止**。予約名は 2 度改名されており（`conductor` → `root` → `main`）、
    // 旧名で Sub を作れると `<repo>__root` のような**旧 state と衝突**する
    // （migration は「衝突時は触らない」ので、その lane の会話が永久に取り残される）。
    if name == crate::repo::lanes_state::ROOT_LANE_NAME
        || vp_paths::LEGACY_ROOT_LANE_NAMES.contains(&name)
    {
        return Err(format!(
            "invalid sub name: '{name}' is reserved for the origin lane (repo ごとに自動生成されるため create 不可). Pick another name."
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

    // --- validate_sub_name ---

    #[test]
    fn valid_sub_names() {
        assert!(validate_sub_name("issue-42").is_ok());
        assert!(validate_sub_name("feature_login").is_ok());
        assert!(validate_sub_name("my-repo-fix-123").is_ok());
    }

    #[test]
    fn empty_name_rejected() {
        assert!(validate_sub_name("").is_err());
    }

    #[test]
    fn special_chars_rejected() {
        assert!(validate_sub_name("../etc/passwd").is_err());
        assert!(validate_sub_name("foo/bar").is_err());
        assert!(validate_sub_name("foo\\bar").is_err());
        assert!(validate_sub_name(".hidden").is_err());
        assert!(validate_sub_name("$(rm -rf)").is_err());
        assert!(validate_sub_name("foo;bar").is_err());
        assert!(validate_sub_name("foo bar").is_err());
    }

    #[test]
    fn leading_separator_rejected() {
        assert!(validate_sub_name("-leading").is_err());
        assert!(validate_sub_name("_leading").is_err());
    }

    #[test]
    fn main_name_rejected() {
        // VP-166: `main` は main lane の予約名 (mailbox box key `<agent>#main` と衝突)
        assert!(validate_sub_name("root").is_err());
        // 部分一致や派生名は OK (= `main` 完全一致のみ禁止)
        assert!(validate_sub_name("leader").is_ok());
        assert!(validate_sub_name("my-root").is_ok());
        assert!(validate_sub_name("root-fix").is_ok());
    }

    // --- find_repo_root (worktree 正規化) ---

    #[test]
    fn find_repo_root_from_linked_worktree_returns_main_root() {
        use std::process::Command;
        let base = test_dir("worktree-root");
        fs::create_dir_all(&base).unwrap();
        let main = base.join("main");
        let main_s = main.to_str().unwrap();

        Command::new("git")
            .args(["init", "--initial-branch=main", main_s])
            .output()
            .unwrap();
        for (k, v) in [("user.email", "t@e.com"), ("user.name", "T")] {
            Command::new("git")
                .args(["-C", main_s, "config", k, v])
                .output()
                .unwrap();
        }
        fs::write(main.join("README.md"), "# x\n").unwrap();
        Command::new("git")
            .args(["-C", main_s, "add", "."])
            .output()
            .unwrap();
        Command::new("git")
            .args(["-C", main_s, "commit", "-m", "init"])
            .output()
            .unwrap();

        // linked worktree を生やす
        let linked = base.join("linked");
        Command::new("git")
            .args([
                "-C",
                main_s,
                "worktree",
                "add",
                "-b",
                "wt",
                linked.to_str().unwrap(),
            ])
            .output()
            .unwrap();

        // linked worktree の中からでも main worktree root に着地する (repo key 正規化)
        let from_linked = find_repo_root_from(Some(&linked)).unwrap();
        assert_eq!(
            dunce::canonicalize(&from_linked).unwrap(),
            dunce::canonicalize(&main).unwrap(),
            "linked worktree からは main worktree root を返すべき"
        );
        // main worktree 自身からも同一 root (回帰なし)
        let from_main = find_repo_root_from(Some(&main)).unwrap();
        assert_eq!(
            dunce::canonicalize(&from_main).unwrap(),
            dunce::canonicalize(&main).unwrap()
        );

        let _ = fs::remove_dir_all(&base);
    }

    // --- load_config (KDL parsing) ---

    /// Create a unique temp dir per test to avoid parallel test collisions
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lane-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir); // clean up leftover state
        dir
    }

    #[test]
    fn load_config_returns_empty_when_no_config_and_no_defaults() {
        // 設定 file 不在 + default 候補 file (.mcp.json 等) も不在 = 空 config を返す
        let tmp = test_dir("no-config-no-defaults");
        let _ = fs::create_dir_all(&tmp);
        let cfg = load_config(&tmp).expect("zero-config sub は Ok を返す");
        assert!(cfg.symlinks.is_empty());
        assert!(cfg.copies.is_empty());
        assert!(cfg.symlink_patterns.is_empty());
        assert!(cfg.post_setup.is_none());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_zero_config_auto_symlinks_defaults() {
        // 設定 file 不在で repo root に default file (.mcp.json / .env 等) が
        // 実在すれば、 それらが auto-symlink 候補として返る (= zero-config sub)
        let tmp = test_dir("zero-config-defaults");
        let _ = fs::create_dir_all(&tmp);
        fs::write(tmp.join(".mcp.json"), "{}").unwrap();
        fs::write(tmp.join(".env"), "KEY=value").unwrap();
        // CLAUDE.local.md は意図的に作らない (= 不在は skip される確認)

        let cfg = load_config(&tmp).unwrap();
        assert!(cfg.symlinks.contains(&".mcp.json".to_string()));
        assert!(cfg.symlinks.contains(&".env".to_string()));
        assert!(
            !cfg.symlinks.contains(&"CLAUDE.local.md".to_string()),
            "不在 file は skip"
        );
        assert_eq!(cfg.symlinks.len(), 2);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_prefers_vp_path_over_claude() {
        // 新 path (.vp/sub-files.kdl) と legacy path (.claude/sub-files.kdl) の
        // 両方が存在する場合、 新 path 優先
        let tmp = test_dir("vp-vs-claude");
        let _ = fs::create_dir_all(tmp.join(".vp"));
        let _ = fs::create_dir_all(tmp.join(".claude"));
        fs::write(tmp.join(".vp/sub-files.kdl"), r#"symlink ".new""#).unwrap();
        fs::write(tmp.join(".claude/sub-files.kdl"), r#"symlink ".legacy""#).unwrap();

        let cfg = load_config(&tmp).unwrap();
        assert_eq!(cfg.symlinks, vec![".new"], ".vp 配下が優先される");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_falls_back_to_legacy_claude_path() {
        // .vp/sub-files.kdl 不在で .claude/sub-files.kdl のみあれば legacy fallback
        let tmp = test_dir("legacy-only");
        let _ = fs::create_dir_all(tmp.join(".claude"));
        fs::write(tmp.join(".claude/sub-files.kdl"), r#"symlink ".env""#).unwrap();

        let cfg = load_config(&tmp).unwrap();
        assert_eq!(cfg.symlinks, vec![".env"]);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_falls_back_to_legacy_wing_files() {
        // 後方互換: root/sub rename 前の `wing-files.kdl` を持つ repo を受理する
        // (.vp 優先、 .claude も legacy fallback)
        let tmp = test_dir("legacy-wing-files");
        let _ = fs::create_dir_all(tmp.join(".vp"));
        fs::write(tmp.join(".vp/wing-files.kdl"), r#"symlink ".env""#).unwrap();

        let cfg = load_config(&tmp).unwrap();
        assert_eq!(
            cfg.symlinks,
            vec![".env"],
            ".vp/wing-files.kdl が legacy fallback として受理される"
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_vp_path_explicit_does_not_merge_defaults() {
        // sub-files.kdl が宣言されている場合、 default は merge しない (= explicit override)
        let tmp = test_dir("explicit-no-merge");
        let _ = fs::create_dir_all(tmp.join(".vp"));
        // repo root に .mcp.json (default) を置くが、 config では別 file 宣言
        fs::write(tmp.join(".mcp.json"), "{}").unwrap();
        fs::write(tmp.join(".vp/sub-files.kdl"), r#"symlink "custom.toml""#).unwrap();

        let cfg = load_config(&tmp).unwrap();
        assert_eq!(cfg.symlinks, vec!["custom.toml"]);
        assert!(
            !cfg.symlinks.contains(&".mcp.json".to_string()),
            "default は明示宣言と merge しない"
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn default_symlinks_filters_nonexistent() {
        // default 候補のうち実在 file のみ返す helper の単体 test
        let tmp = test_dir("default-filter");
        let _ = fs::create_dir_all(&tmp);
        fs::write(tmp.join(".env"), "X=1").unwrap();
        // .mcp.json / CLAUDE.local.md は作らない

        let result = default_symlinks(&tmp);
        assert_eq!(result, vec![".env"]);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_symlinks_and_copies() {
        let tmp = test_dir("symlinks-copies");
        let _ = fs::create_dir_all(tmp.join(".claude"));
        fs::write(
            tmp.join(".claude/sub-files.kdl"),
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
            tmp.join(".claude/sub-files.kdl"),
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
        fs::write(tmp.join(".claude/sub-files.kdl"), "").unwrap();

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
        fs::write(tmp.join(".claude/sub-files.kdl"), r#"symlink ".env"#).unwrap();

        let result = load_config(&tmp);
        assert!(result.is_err(), "unclosed string should return Err");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_config_invalid_kdl_syntax_error() {
        let tmp = test_dir("invalid-kdl-syntax");
        let _ = fs::create_dir_all(tmp.join(".claude"));
        // 不正な KDL 構文: 識別子の位置に記号
        fs::write(tmp.join(".claude/sub-files.kdl"), "= broken syntax {\n").unwrap();

        let result = load_config(&tmp);
        assert!(result.is_err(), "syntax error should return Err");

        let _ = fs::remove_dir_all(&tmp);
    }

    // --- repo_lanes_dir ---

    #[test]
    fn repo_lanes_dir_under_repo_root() {
        let repo = PathBuf::from("/tmp/some-repo");
        assert_eq!(repo_lanes_dir(&repo), repo.join(".vp").join("lanes"));
    }

    #[test]
    fn repo_lanes_dir_handles_trailing_slash_in_input() {
        // PathBuf::join は trailing slash を自然に扱う
        let repo = PathBuf::from("/tmp/some-repo/");
        assert_eq!(repo_lanes_dir(&repo), repo.join(".vp").join("lanes"));
    }

    // --- ensure_vp_gitignored ---

    #[test]
    fn ensure_vp_gitignored_creates_new_file() {
        let tmp = test_dir("gi-new");
        let _ = fs::create_dir_all(&tmp);

        ensure_vp_gitignored(&tmp).unwrap();
        let content = fs::read_to_string(tmp.join(".gitignore")).unwrap();
        assert!(content.contains(".vp/"));
        assert!(content.contains("# Vantage Point"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ensure_vp_gitignored_appends_to_existing() {
        let tmp = test_dir("gi-append");
        let _ = fs::create_dir_all(&tmp);
        fs::write(tmp.join(".gitignore"), "/target\nnode_modules/\n").unwrap();

        ensure_vp_gitignored(&tmp).unwrap();
        let content = fs::read_to_string(tmp.join(".gitignore")).unwrap();
        assert!(content.contains("/target"), "既存 entry 保持");
        assert!(content.contains("node_modules/"), "既存 entry 保持");
        assert!(content.contains(".vp/"), ".vp/ 追記");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ensure_vp_gitignored_idempotent_when_already_present() {
        let tmp = test_dir("gi-idempotent");
        let _ = fs::create_dir_all(&tmp);
        let original = "/target\n.vp/\nnode_modules/\n";
        fs::write(tmp.join(".gitignore"), original).unwrap();

        ensure_vp_gitignored(&tmp).unwrap();
        let content = fs::read_to_string(tmp.join(".gitignore")).unwrap();
        assert_eq!(content, original, "既存 .vp/ があれば content 不変");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ensure_vp_gitignored_recognizes_variant_forms() {
        // `.vp`, `.vp/`, `/.vp`, `/.vp/` 全て idempotent
        for entry in [".vp", ".vp/", "/.vp", "/.vp/"] {
            let slug = entry.replace('/', "_").replace('.', "");
            let tmp = test_dir(&format!("gi-variant-{slug}"));
            let _ = fs::create_dir_all(&tmp);
            let original = format!("/target\n{entry}\n");
            fs::write(tmp.join(".gitignore"), &original).unwrap();

            ensure_vp_gitignored(&tmp).unwrap();
            let content = fs::read_to_string(tmp.join(".gitignore")).unwrap();
            assert_eq!(
                content, original,
                "{entry} は既存 ignore とみなし content 不変であるべき"
            );
            let _ = fs::remove_dir_all(&tmp);
        }
    }

    #[test]
    fn ensure_vp_gitignored_handles_missing_trailing_newline() {
        let tmp = test_dir("gi-no-trail-nl");
        let _ = fs::create_dir_all(&tmp);
        // 末尾改行なし
        fs::write(tmp.join(".gitignore"), "/target").unwrap();

        ensure_vp_gitignored(&tmp).unwrap();
        let content = fs::read_to_string(tmp.join(".gitignore")).unwrap();
        assert!(content.starts_with("/target\n"), "既存末尾に改行を補う");
        assert!(content.contains(".vp/"), ".vp/ 追記");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ensure_vp_gitignored_ignores_commented_vp_line() {
        // `# .vp/` のような comment 行は ignore とみなさない (= 追記する)
        let tmp = test_dir("gi-commented");
        let _ = fs::create_dir_all(&tmp);
        fs::write(tmp.join(".gitignore"), "/target\n# .vp/\n").unwrap();

        ensure_vp_gitignored(&tmp).unwrap();
        let content = fs::read_to_string(tmp.join(".gitignore")).unwrap();
        // comment 行は残ったまま、 加えて real entry を追記
        assert!(content.contains("# .vp/"));
        let real_entries = content
            .lines()
            .map(|l| l.split('#').next().unwrap_or("").trim())
            .filter(|l| matches!(*l, ".vp/" | ".vp"))
            .count();
        assert_eq!(real_entries, 1, ".vp/ real entry を 1 行追記");

        let _ = fs::remove_dir_all(&tmp);
    }
}
