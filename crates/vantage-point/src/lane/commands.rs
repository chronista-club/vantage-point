use super::config;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// crossplat symlink (Unix: `symlink`。Windows: file/dir を `is_dir()` で判別)。
/// Windows で symlink を張るには Developer Mode もしくは管理者権限が必要。
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst)
    }
    #[cfg(windows)]
    {
        if src.is_dir() {
            std::os::windows::fs::symlink_dir(src, dst)
        } else {
            std::os::windows::fs::symlink_file(src, dst)
        }
    }
}

/// Lane workspace の隔離方式 (worktree lane refactor 2026-06-07)。
///
/// - **`Worktree`** (default): conductor の `.git` (objects/refs/remotes) を共有する
///   `git worktree`。 軽量・高速で、 cc / multi-agent 統合の土台。 `git worktree list`
///   が live registry になる。
/// - **`Clone`**: 旧来の `git clone --depth 1` (= 完全独立 .git)。 escape hatch
///   (`vp lane new --isolation clone`)。 worktree が使えない環境や完全分離が要る時用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Isolation {
    #[default]
    Worktree,
    Clone,
}

/// Create a new performer environment
///
/// `base`: worktree の分岐元 ref の per-invocation override (co-evolution #2)。
/// None なら従来通り performer-files.kdl の `base-ref` → origin/HEAD → "main"。
///
/// `model`: lane の claude model alias (co-evolution #1)。 Some なら `engine_model` へ永続し、
/// この lane が spawn される際に Act I claude の `--model` として読まれる。 None なら claude default。
/// worktree 作成のみ（spawn は SP が別途行う）なので、 ここでは state file を書くだけ。
pub fn new_performer(
    name: &str,
    branch: &str,
    force: bool,
    isolation: Isolation,
    base: Option<&str>,
    model: Option<&str>,
) -> Result<(), String> {
    let repo_root = config::find_repo_root().map_err(|e| e.to_string())?;
    let performer_dir = setup_performer(name, branch, &repo_root, force, isolation, base)?;
    persist_lane_model(&repo_root, name, model)?;
    println!("{}", performer_dir.display());
    Ok(())
}

/// Phase 4-X: SP-friendly wrapper. `repo_root` を明示的に受け取り、 performer dir の `PathBuf` を返す。
/// stdout への print なし、 lib call として完結。 SP server (lanes.rs) から直接呼ぶ用。
pub fn new_performer_in(
    repo_root: &Path,
    name: &str,
    branch: &str,
    force: bool,
    isolation: Isolation,
    base: Option<&str>,
) -> Result<PathBuf, String> {
    setup_performer(name, branch, repo_root, force, isolation, base)
}

/// Phase 4-X: SP-friendly remove。 repo_root を明示的に受け取り、 project-local 新 path で
/// performer dir を解決して削除する。
///
/// project-local lane refactor PR 1: `repo_name: &str` → `repo_root: &Path` に signature
/// 変更。 caller (sidebar 経由 DELETE 等) は state.project_dir を直接渡せる。
/// PR 4b: legacy global path dual-read 削除、 project-local 一本に。
pub fn remove_performer_in(repo_root: &Path, name: &str) -> Result<(), String> {
    config::validate_performer_name(name)?;
    let Some(performer_dir) = find_performer_dir(repo_root, name) else {
        // workspace が既に無くても state file だけ残る orphan は掃除する (leak の典型:
        // 手動 rm 済 dir + 残留 console_mode/cc_session)。Err semantics は維持。
        clear_lane_state_files(repo_root, name);
        return Err(format!(
            "performer not found: '{name}' (looked in {}/.vp/lanes/)",
            repo_root.display()
        ));
    };
    remove_performer_workspace(repo_root, &performer_dir)?;
    // state file GC: orchestrated 経路 (Phase 2a) と重複しても冪等。 project remove の
    // B-destroy reclaim (process_manager_capability) はここしか通らないので必須。
    clear_lane_state_files(repo_root, name);
    Ok(())
}

/// Fork current dirty state into a new performer environment
pub fn fork_performer(
    name: &str,
    branch: &str,
    force: bool,
    isolation: Isolation,
    base: Option<&str>,
    model: Option<&str>,
) -> Result<(), String> {
    let repo_root = config::find_repo_root().map_err(|e| e.to_string())?;

    // Capture dirty state as a diff BEFORE creating the performer
    let diff = capture_dirty_diff(&repo_root)?;

    let performer_dir = setup_performer(name, branch, &repo_root, force, isolation, base)?;
    persist_lane_model(&repo_root, name, model)?;

    // Apply the captured diff to the performer
    if let Some(patch) = diff {
        eprintln!("dirty state を適用中...");
        apply_patch(&performer_dir, &patch)?;
    } else {
        eprintln!("フォークする未コミット変更はありません。");
    }

    println!("{}", performer_dir.display());
    Ok(())
}

/// Common performer setup: clone, symlink, branch, post-setup.
/// Returns the performer directory path。
///
/// project-local lane refactor: 新 lane の配置先は `<repo_root>/.vp/lanes/<name>`。
/// parent repo の `.gitignore` に `.vp/` を best-effort で追記して nested git clone を
/// 隠蔽する。
fn setup_performer(
    name: &str,
    branch: &str,
    repo_root: &Path,
    force: bool,
    isolation: Isolation,
    base: Option<&str>,
) -> Result<PathBuf, String> {
    config::validate_performer_name(name)?;

    let cfg = config::load_config(repo_root)?;

    // parent repo の .gitignore に .vp/ を追記 (idempotent、 best-effort)。 失敗しても
    // performer 作成は続行する (= user が手動で .gitignore 編集する fallback path 残す)。
    if let Err(e) = config::ensure_vp_gitignored(repo_root) {
        eprintln!("⚠ .gitignore への .vp/ 追記失敗 (続行): {e}");
    }

    let performers_dir = config::project_lanes_dir(repo_root);
    let performer_dir = performers_dir.join(name);

    if performer_dir.exists() {
        if !force {
            return Err(format!(
                "パフォーマー '{name}' は既に存在します ({})。上書きするには --force を指定してください。",
                performer_dir.display()
            ));
        }
        eprintln!("既存パフォーマーを削除: {}", performer_dir.display());
        remove_performer_workspace(repo_root, &performer_dir)?;
    }

    fs::create_dir_all(&performers_dir).map_err(|e| e.to_string())?;

    // provisioning: worktree (default) は branch も atomic に作る。 clone は内部で checkout -b。
    // 以降の symlink/copy/post-setup は両者で共通。
    match isolation {
        Isolation::Worktree => provision_worktree(repo_root, &performer_dir, branch, &cfg, base)?,
        Isolation::Clone => {
            // clone は conductor HEAD の depth-1 複製 (= 任意 ref からの分岐に非対応)。
            // silent に無視せず明示 error (co-evolution #2 は worktree が対象)。
            if base.is_some_and(|b| !b.trim().is_empty()) {
                return Err(
                    "--base は isolation=worktree のみ対応 (clone は conductor HEAD の複製)"
                        .to_string(),
                );
            }
            provision_clone(repo_root, &performer_dir, branch)?
        }
    }

    // Symlinks
    for file in &cfg.symlinks {
        let src = repo_root.join(file);
        let dst = performer_dir.join(file);
        if src.exists() {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            // Remove existing file from clone (if it exists) before symlinking
            let _ = fs::remove_file(&dst);
            symlink(&src, &dst).map_err(|e| e.to_string())?;
            eprintln!("  symlink: {file}");
        }
    }

    // Copies
    for file in &cfg.copies {
        let src = repo_root.join(file);
        let dst = performer_dir.join(file);
        if src.exists() {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::copy(&src, &dst).map_err(|e| e.to_string())?;
            eprintln!("  copy: {file}");
        }
    }

    // Symlink patterns
    for pattern in &cfg.symlink_patterns {
        let matches =
            glob::glob(&format!("{}/{pattern}", repo_root.display())).map_err(|e| e.to_string())?;

        for entry in matches.flatten() {
            // Skip .git directory
            if entry.to_str().is_some_and(|s| s.contains("/.git/")) {
                continue;
            }
            if let Ok(rel) = entry.strip_prefix(repo_root) {
                let dst = performer_dir.join(rel);
                if !dst.exists() {
                    if let Some(parent) = dst.parent() {
                        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    symlink(&entry, &dst).map_err(|e| e.to_string())?;
                    eprintln!("  symlink (pattern): {}", rel.display());
                }
            }
        }
    }

    // Post-setup
    if let Some(cmd) = &cfg.post_setup {
        eprintln!("実行中: {cmd}");
        let status = Command::new("sh")
            .args(["-c", cmd])
            .current_dir(&performer_dir)
            .status()
            .map_err(|e| e.to_string())?;

        if !status.success() {
            return Err(format!("post-setup 失敗: {cmd}"));
        }
    }

    // project-local lane refactor PR 4a: PR #429 の `claude_trust::pre_grant_trust` 削除。
    // performer dir は `<repo>/.vp/lanes/<name>` に置かれ、 parent repo (= `<repo>`) の
    // `hasTrustDialogAccepted: true` が claude 側で **hierarchical 継承** されるので
    // pre-grant は不要 (2026-05-24 実証、 nested `.git/` でも継承)。
    Ok(performer_dir)
}

// ── provisioning (worktree / clone) ────────────────────────────────────────

/// worktree provisioning (default)。conductor の `.git` を共有する `git worktree` を
/// `origin/<base>` から `-b <branch>` で生やす。 remote 共有なので set-url 不要。
fn provision_worktree(
    repo_root: &Path,
    performer_dir: &Path,
    branch: &str,
    cfg: &config::PerformerConfig,
    base_override: Option<&str>,
) -> Result<(), String> {
    let base = resolve_base_ref(repo_root, cfg, base_override);
    // base を best-effort fetch (= offline でも local ref で worktree add は進める)。
    if let Err(e) = run_git_in(repo_root, &["fetch", "origin", &base]) {
        eprintln!("⚠ fetch origin {base} 失敗 (続行、 local ref で worktree 作成): {e}");
    }
    let start_point = resolve_start_point(repo_root, &base);
    eprintln!(
        "{} を worktree add 中 (branch={branch}, base={start_point})...",
        performer_dir.display()
    );
    worktree_add_with_retry(repo_root, performer_dir, branch, &start_point)
}

/// clone provisioning (escape hatch `--isolation clone`)。完全独立 .git。
fn provision_clone(repo_root: &Path, performer_dir: &Path, branch: &str) -> Result<(), String> {
    let remote_url = config::get_remote_url().map_err(|e| e.to_string())?;
    let repo_root_str = repo_root
        .to_str()
        .ok_or("リポジトリルートのパスが有効な UTF-8 ではありません")?;
    let performer_dir_str = performer_dir
        .to_str()
        .ok_or("パフォーマーディレクトリのパスが有効な UTF-8 ではありません")?;
    eprintln!("{} にクローン中...", performer_dir.display());
    run_git(&["clone", "--depth", "1", repo_root_str, performer_dir_str])?;
    // clone の origin は conductor repo path になるので GitHub URL に張り替える (= 旧挙動)。
    run_git_in(performer_dir, &["remote", "set-url", "origin", &remote_url])?;
    run_git_in(performer_dir, &["checkout", "-b", branch])
}

/// worktree lane の base branch (= dev trunk) を解決。
/// 優先順: per-invocation override (`--base` / API `base`、co-evolution #2) →
/// performer-files.kdl の `base-ref` → [`resolve_default_branch`] (origin/HEAD) → "main"。
///
/// override は未 push の local branch でもよい ([`resolve_start_point`] が
/// `origin/<base>` → `<base>` の順で probe するため、conductor の feature branch 上の
/// 未 merge 土台を wing に配れる)。
fn resolve_base_ref(
    repo_root: &Path,
    cfg: &config::PerformerConfig,
    base_override: Option<&str>,
) -> String {
    if let Some(b) = base_override {
        let b = b.trim();
        if !b.is_empty() {
            return b.to_string();
        }
    }
    if let Some(b) = &cfg.base_ref
        && !b.trim().is_empty()
    {
        return b.clone();
    }
    resolve_default_branch(repo_root).unwrap_or_else(|| "main".to_string())
}

/// origin の default branch を解決 (F 検証 F4 の fallback chain)。
///
/// 1. `git symbolic-ref --short refs/remotes/origin/HEAD` ("origin/main" → "main")
/// 2. 未設定なら `git remote set-head origin -a` で復旧して再試行
/// 3. `gh repo view` の defaultBranchRef
/// 4. `origin/main` → `origin/master` の存在 probe
pub fn resolve_default_branch(repo_root: &Path) -> Option<String> {
    if let Some(b) = git_symbolic_default(repo_root) {
        return Some(b);
    }
    let _ = run_git_in(repo_root, &["remote", "set-head", "origin", "-a"]);
    if let Some(b) = git_symbolic_default(repo_root) {
        return Some(b);
    }
    if let Some(b) = gh_default_branch(repo_root) {
        return Some(b);
    }
    for cand in ["main", "master"] {
        if git_rev_parse(repo_root, &format!("origin/{cand}")).is_some() {
            return Some(cand.to_string());
        }
    }
    None
}

/// `git symbolic-ref --short refs/remotes/origin/HEAD` → branch 名 ("origin/" を除去)。
fn git_symbolic_default(repo_root: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    s.strip_prefix("origin/")
        .map(|b| b.to_string())
        .filter(|b| !b.is_empty())
}

/// `gh repo view --json defaultBranchRef` fallback (gh 認証済 + GitHub remote 時のみ機能)。
fn gh_default_branch(repo_root: &Path) -> Option<String> {
    let out = Command::new("gh")
        .args([
            "repo",
            "view",
            "--json",
            "defaultBranchRef",
            "-q",
            ".defaultBranchRef.name",
        ])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// worktree add の start-point を解決。 `origin/<base>` → `<base>` → "HEAD" の順で
/// 最初に rev-parse できる ref を返す (= offline / fresh repo でも壊れない)。
fn resolve_start_point(repo_root: &Path, base: &str) -> String {
    for cand in [format!("origin/{base}"), base.to_string()] {
        if git_rev_parse(repo_root, &cand).is_some() {
            return cand;
        }
    }
    "HEAD".to_string()
}

/// `git worktree add -b <branch> <performer_dir> <start_point>` を lock 競合に備えリトライ実行。
///
/// F 検証で判明した制約を反映:
/// - **F2**: 同一 repo への並列 worktree add は ref/index lock で落ちうる → backoff retry。
/// - **F3**: branch 既存 / 他 worktree 使用中は retry 無意味 → 即 actionable error。
///
/// 各試行の結果を club-nostos（crate `nostos`）の三相 `Outcome` に写す（振る舞いは従来と同一）。
/// 成功 → `Done`、F3（branch 衝突）と その他の git / spawn 失敗 → `Failed`（terminal）、
/// F2（lock）→ backoff 後 `Reborn`。`drive_bounded(0, 4, …)` が `for attempt in 0..4` に対応し、
/// lock で 4 回使い切ると残余 `Reborn` を「lock で 4 回失敗」へ写す。
fn worktree_add_with_retry(
    repo_root: &Path,
    performer_dir: &Path,
    branch: &str,
    start_point: &str,
) -> Result<(), String> {
    use nostos::{Outcome, drive_bounded};

    let performer_str = performer_dir
        .to_str()
        .ok_or("パフォーマーディレクトリのパスが有効な UTF-8 ではありません")?;
    // lock で使い切った時の最終メッセージ用に直近 stderr を保持（FnMut が捕捉）。
    let mut last_err = String::new();

    let outcome = drive_bounded(0u64, 4, |attempt| {
        let out = match Command::new("git")
            .args(["worktree", "add", "-b", branch, performer_str, start_point])
            .current_dir(repo_root)
            .output()
        {
            Ok(out) => out,
            // git を起動すらできない = terminal。
            Err(e) => return Outcome::Failed(e.to_string()),
        };
        if out.status.success() {
            return Outcome::Done(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        // F3: branch 衝突 (既存 or 他 worktree 使用中) → retry 無駄、 actionable error。
        if stderr.contains("already exists") || stderr.contains("already used by worktree") {
            return Outcome::Failed(format!(
                "branch '{branch}' は既に存在 / 別の lane が使用中です。別の branch / performer 名を指定してください。\n  git: {}",
                stderr.trim()
            ));
        }
        // F2: lock 競合 → backoff して再生 (retry)。
        let is_lock = stderr.contains("lock")
            || stderr.contains("Unable to create")
            || stderr.contains("another git process");
        if is_lock {
            last_err = stderr;
            std::thread::sleep(std::time::Duration::from_millis(120 * (attempt + 1)));
            return Outcome::Reborn(attempt + 1);
        }
        // その他 → 即 terminal。
        Outcome::Failed(format!("git worktree add 失敗: {}", stderr.trim()))
    });

    match outcome {
        Outcome::Done(()) => Ok(()),
        Outcome::Failed(e) => Err(e),
        // 残余 Reborn = lock で 4 回使い切った。
        Outcome::Reborn(_) => Err(format!(
            "git worktree add が lock 競合で 4 回失敗しました: {}",
            last_err.trim()
        )),
    }
}

/// performer workspace を削除 (clone / worktree 両対応)。
///
/// - **worktree** (`.git` が file = gitdir ポインタ): `git worktree remove --force` +
///   `prune` で `.git/worktrees/<name>` 登録ごと除去。 **branch は残す** (未 push 保全、
///   設計 E)。 stale 登録時は fs 削除 + prune に fallback。
/// - **clone** (`.git` が dir): `fs::remove_dir_all`。
fn remove_performer_workspace(repo_root: &Path, performer_dir: &Path) -> Result<(), String> {
    let dotgit = performer_dir.join(".git");
    if dotgit.is_file() {
        let performer_str = performer_dir
            .to_str()
            .ok_or("パフォーマーディレクトリのパスが有効な UTF-8 ではありません")?;
        if run_git_in(repo_root, &["worktree", "remove", "--force", performer_str]).is_err() {
            // stale 登録等 → fs 削除 + prune で後始末 (best-effort)。
            let _ = fs::remove_dir_all(performer_dir);
        }
        let _ = run_git_in(repo_root, &["worktree", "prune"]);
        Ok(())
    } else {
        fs::remove_dir_all(performer_dir).map_err(|e| e.to_string())
    }
}

/// lane 単位 state file (console_mode / cc_session) の GC (best-effort)。
///
/// 削除系経路 (`remove_performer` / `remove_performer_in` / `cleanup_performers`) から呼ぶ。
/// 残すと同名 lane を作り直した時に旧 mode / 旧 session が蘇る (ghost file の state leak、
/// `delete_lane_orchestrated` Phase 2a と同旨)。orchestrated 経路と二重に呼ばれても clear は
/// 冪等 (未記録は no-op)。
///
/// ⚠️ `remove_performer_workspace` には置かない — `setup_performer` の `--force` 再作成も
/// あれを通るため、そこで cc_session を消すと workspace 再作成後の `--resume` 継続性を壊す。
///
/// キーは SP の書き手 (lanes_state::set_console_mode / stand_spawner の VP_PROJECT env、
/// create_performer_orchestrated 等) と同じ derivation: project = repo_root の basename、
/// lane = performer 名。
fn clear_lane_state_files(repo_root: &Path, lane: &str) {
    clear_lane_state_files_in(&crate::config::vp_state_dir(), repo_root, lane);
}

/// lane の claude model を `engine_model` へ永続する (co-evolution #1、CLI `vp lane new/fork --model`)。
///
/// project key は `clear_lane_state_files` / SP `create_performer_orchestrated` と同一
/// derivation (repo_root basename) — CLI で書いた model を SP spawn 経路が読めるようにする。
/// None は no-op (= claude default)。 不正な model 名は Err で早期に弾く (worktree は作成済だが
/// spawn 前に失敗を返す方が silent degrade より良い)。
///
/// ⚠️ 既知の制約 (既存 `clear_lane_state_files_in` と共通、根治は project key 正規化 = 別 task):
/// `find_repo_root()` は worktree 内では worktree 自身を返すため、 **lane worktree の中から**
/// `vp lane new/fork --model` を実行すると project key が SP 読み手 (conductor project basename)
/// とズレ、 model が silent に無視される。 conductor root からの実行 (= `vp lane new` の想定運用、
/// AGENTS.md) では一致する。 high-value 経路 (MCP add_performer / flow_handoff / SP) は
/// `create_performer_orchestrated` が `addr.project` で直書きするため常に一致する。
fn persist_lane_model(repo_root: &Path, lane: &str, model: Option<&str>) -> Result<(), String> {
    persist_lane_model_in(&crate::config::vp_state_dir(), repo_root, lane, model)
}

/// state base dir 注入版 (テスト用)。
fn persist_lane_model_in(
    base: &Path,
    repo_root: &Path,
    lane: &str,
    model: Option<&str>,
) -> Result<(), String> {
    let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) else {
        return Ok(());
    };
    if !super::engine_model::is_valid_model(model) {
        return Err(format!("model 名が不正です: {model:?}"));
    }
    let project = repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    super::engine_model::record_in(base, project, lane, model)
        .map_err(|e| format!("model 永続に失敗: {e}"))
}

/// state base dir 注入版 (テスト用)。
fn clear_lane_state_files_in(base: &Path, repo_root: &Path, lane: &str) {
    let project = repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    if let Err(e) = super::console_mode::clear_in(base, project, lane) {
        eprintln!("⚠ console_mode state の破棄に失敗 (file 残置): lane={lane} err={e}");
    }
    if let Err(e) = super::cc_session::clear_in(base, project, lane) {
        eprintln!("⚠ cc_session state の破棄に失敗 (file 残置): lane={lane} err={e}");
    }
    if let Err(e) = super::engine_model::clear_in(base, project, lane) {
        eprintln!("⚠ engine_model state の破棄に失敗 (file 残置): lane={lane} err={e}");
    }
}

/// List all performer environments under cwd の `<repo>/.vp/lanes/`。
///
/// project-local lane refactor PR 4b: legacy global path 列挙を削除、 cwd の repo
/// の project-local のみ表示。 cwd が git repo でない場合は空出力 (= 既存挙動と同様)。
pub fn list_performers() -> Result<(), String> {
    let Ok(repo_root) = config::find_repo_root() else {
        return Ok(());
    };
    let pl_dir = config::project_lanes_dir(&repo_root);
    if !pl_dir.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(&pl_dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let branch = get_branch(&path).unwrap_or_else(|| "-".to_string());
        println!("{name}\t{branch}\t{}", path.display());
    }
    Ok(())
}

/// disk 上で発見された Performer 環境 1 件 (lane Performer dir の structured view、 SP /api/lanes 用)。
///
/// PtySlot 起動の有無は問わない (= disk 存在のみ示す)。 lanes.rs:list_handler で
/// in-memory LanePool に居ない Performer を `LaneState::Inactive` として merge する時の中間 type。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InactivePerformerEntry {
    /// performer 名 (= `<repo>/.vp/lanes/<name>` の `<name>` 部分)
    pub name: String,
    /// 絶対 path
    pub path: String,
    /// `git branch --show-current` の結果。 取れない時 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// repo に紐づく Performer dir を `<repo>/.vp/lanes/` から disk scan して返す (SP /api/lanes 用)。
///
/// project-local lane refactor PR 4b: legacy global path scan + dedup logic を削除、
/// project-local のみ列挙に simplify。
///
/// 「基本は通らない防御パス」: 通常 lane clone は POST /api/lanes 経由で生成され、 同 session 内なら
/// LanePool に登録されている。 ただし vp-app crash 後の残骸 / 別 session での `vp lane new` 等で
/// disk に存在するが LanePool に居ない Performer が出ることがあり、 それを sidebar に inactive 状態で
/// surface するため。 click で activate (= POST /api/lanes に cwd 指定で attach) する想定。
///
/// fail-soft (= 防御パスのため read error は空 Vec 扱い)。
pub fn list_performers_for_repo(repo_root: &Path) -> Vec<InactivePerformerEntry> {
    let mut out = Vec::new();
    let pl_dir = config::project_lanes_dir(repo_root);
    let Ok(entries) = fs::read_dir(&pl_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = entry.file_name();
        out.push(InactivePerformerEntry {
            name: dir_name.to_string_lossy().into_owned(),
            path: path.to_string_lossy().into_owned(),
            branch: get_branch(&path),
        });
    }
    out
}

/// 名前指定の performer の lane_index (= alphabetical 順 + 1) を返す。
///
/// 「目的ベース port 解決」 の核 — caller (= `vp port show --performer <name>` 等) が
/// performer 名から port を引くために使う。
///
/// 設計:
/// - lane_index = conductor が 0、 performer は alphabetical sort + 1 (= 一意性確保)
/// - performer 追加削除で sort 順が変わる → **port が変動する**点に注意
/// - bookmark / URL 共有は name 経由 access (= `vp port url --performer <name>`) を推奨、
///   port 番号直書きは非推奨
/// - 永続 stable port が必要なら別 PR で performer slot registry (`.vp/performers.kdl`) を追加予定
pub fn resolve_lane_index_by_performer_name(repo_root: &Path, performer_name: &str) -> Option<u16> {
    let mut names: Vec<String> = list_performers_for_repo(repo_root)
        .into_iter()
        .map(|e| e.name)
        .collect();
    names.sort();
    names
        .iter()
        .position(|n| n == performer_name)
        .map(|i| (i + 1) as u16)
}

/// Print the path to a performer。
///
/// project-local lane refactor PR 4b: legacy global path fallback 削除、 cwd の repo
/// の `<repo>/.vp/lanes/<name>` のみ lookup。 cwd が git repo でない場合は error。
pub fn performer_path(name: &str) -> Result<(), String> {
    let repo_root = config::find_repo_root().map_err(|e| e.to_string())?;
    let Some(found) = find_performer_dir(&repo_root, name) else {
        return Err(format!(
            "パフォーマー '{name}' が見つかりません。`vp lane ls` で一覧を確認してください。"
        ));
    };
    println!("{}", found.display());
    Ok(())
}

/// Remove a performer environment。
///
/// project-local lane refactor PR 4b: legacy global path fallback 削除、 cwd の repo
/// の `<repo>/.vp/lanes/<name>` のみ対象。 cwd が git repo でない場合は error。
pub fn remove_performer(name: Option<&str>, all: bool, force: bool) -> Result<(), String> {
    let repo_root = config::find_repo_root().map_err(|e| e.to_string())?;
    let pl_dir = config::project_lanes_dir(&repo_root);

    if all {
        if !force {
            return Err("--all には --force が必要です（誤削除防止）".into());
        }
        if pl_dir.exists() {
            // state file GC 用に削除前の performer 名を控える (dir ごと消すと名前が失われる)。
            let names: Vec<String> = fs::read_dir(&pl_dir)
                .map(|entries| {
                    entries
                        .flatten()
                        .filter(|e| e.path().is_dir())
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect()
                })
                .unwrap_or_default();
            fs::remove_dir_all(&pl_dir).map_err(|e| e.to_string())?;
            // worktree lane: dir を消すと `.git/worktrees/<name>` 登録が stale で残るので prune。
            let _ = run_git_in(&repo_root, &["worktree", "prune"]);
            for name in &names {
                clear_lane_state_files(&repo_root, name);
            }
            eprintln!("project-local パフォーマー全削除: {}", pl_dir.display());
        } else {
            eprintln!("削除対象のパフォーマーはありませんでした");
        }
        return Ok(());
    }

    let name = name.ok_or("パフォーマー名を指定するか --all --force を使用してください")?;
    config::validate_performer_name(name)?;

    let Some(performer_dir) = find_performer_dir(&repo_root, name) else {
        // workspace が既に無くても state file だけ残る orphan は掃除する (Err semantics は維持)。
        clear_lane_state_files(&repo_root, name);
        return Err(format!(
            "パフォーマー '{name}' が見つかりません。`vp lane ls` で一覧を確認してください。"
        ));
    };
    remove_performer_workspace(&repo_root, &performer_dir)?;
    clear_lane_state_files(&repo_root, name);
    eprintln!("削除: {}", performer_dir.display());
    Ok(())
}

/// Show status of all performer environments under cwd の `<repo>/.vp/lanes/`。
///
/// project-local lane refactor PR 4b: legacy global block 削除、 project-local 一本に。
pub fn status_performers() -> Result<(), String> {
    let mut found = false;

    if let Ok(repo_root) = config::find_repo_root() {
        let pl_dir = config::project_lanes_dir(&repo_root);
        if pl_dir.exists()
            && let Ok(entries) = fs::read_dir(&pl_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() || !path.join(".git").exists() {
                    continue;
                }
                found = true;
                print_performer_status_row(&path, &entry.file_name().to_string_lossy());
            }
        }
    }

    if !found {
        eprintln!("パフォーマーはありません。`vp lane new <name> <branch>` で作成できます。");
    }

    Ok(())
}

/// Remove performers whose branch is merged into main (cwd の `<repo>/.vp/lanes/` 対象)。
///
/// project-local lane refactor PR 4b: legacy global block 削除、 project-local 一本に。
pub fn cleanup_performers(force: bool) -> Result<(), String> {
    let Ok(repo_root) = config::find_repo_root() else {
        eprintln!("クリーンアップ対象はありません。");
        return Ok(());
    };
    let pl_dir = config::project_lanes_dir(&repo_root);

    // (name, path, branch)。 branch は worktree cleanup 時の `git branch -d` 用。
    let mut to_remove: Vec<(String, std::path::PathBuf, Option<String>)> = Vec::new();
    let mut kept: Vec<(String, String)> = Vec::new();

    if pl_dir.exists()
        && let Ok(entries) = fs::read_dir(&pl_dir)
    {
        for entry in entries.flatten() {
            classify_performer_for_cleanup(entry, &mut to_remove, &mut kept);
        }
    }

    if to_remove.is_empty() && kept.is_empty() {
        eprintln!("クリーンアップ対象はありません。");
        return Ok(());
    }
    if to_remove.is_empty() {
        eprintln!("クリーンアップ対象はありません。");
        for (name, reason) in &kept {
            eprintln!("  保持: {name} ({reason})");
        }
        return Ok(());
    }

    for (name, _, _) in &to_remove {
        eprintln!("  削除可能: {name} (マージ済み)");
    }
    for (name, reason) in &kept {
        eprintln!("  保持: {name} ({reason})");
    }

    if !force {
        eprintln!("\n実際に削除するには `vp lane cleanup --force` を実行してください。");
        return Ok(());
    }

    for (name, path, branch) in &to_remove {
        remove_performer_workspace(&repo_root, path)?;
        clear_lane_state_files(&repo_root, name);
        // worktree: merged branch を共有 .git から `-d` で安全に掃除 (設計 E)。
        // clone: branch は独立 .git 内なので親 repo では no-op (失敗は握り潰す)。
        if let Some(b) = branch {
            let _ = run_git_in(&repo_root, &["branch", "-d", b]);
        }
        eprintln!("  削除: {name}");
    }

    eprintln!("{} パフォーマーを削除しました。", to_remove.len());
    Ok(())
}

/// `status_performers` 内の 1 performer 行表示 helper
fn print_performer_status_row(path: &Path, name: &str) {
    let branch = get_branch(path).unwrap_or_else(|| "-".to_string());
    let changes = count_changes(path);
    let ahead_behind = get_ahead_behind(path);
    let last_commit = get_last_commit(path);
    let changes_str = if changes > 0 {
        format!("{changes} files")
    } else {
        "clean".to_string()
    };
    println!("{name}\t{branch}\t{changes_str}\t{ahead_behind}\t{last_commit}");
}

/// `cleanup_performers` 内の 1 performer 分類 helper
fn classify_performer_for_cleanup(
    entry: fs::DirEntry,
    to_remove: &mut Vec<(String, std::path::PathBuf, Option<String>)>,
    kept: &mut Vec<(String, String)>,
) {
    let path = entry.path();
    // `.git` は clone なら dir / worktree なら file。 `exists()` は両方 true。
    if !path.is_dir() || !path.join(".git").exists() {
        return;
    }
    let name = entry.file_name().to_string_lossy().to_string();
    let _ = run_git_in(&path, &["fetch", "--quiet"]);
    if is_branch_merged(&path) {
        let branch = get_branch(&path);
        to_remove.push((name, path, branch));
    } else {
        let changes = count_changes(&path);
        let reason = if changes > 0 {
            format!("アクティブ ({changes} files changed)")
        } else {
            "未マージ".to_string()
        };
        kept.push((name, reason));
    }
}

/// `<repo>/.vp/lanes/<name>` の performer dir を返す。 dir 不在なら None。
///
/// project-local lane refactor PR 4b: PR 1 で導入した `find_performer_dir_dual` の legacy
/// global path fallback (= step 2/3) を削除し、 project-local 一本に simplify。
/// performer_path / remove_performer / remove_performer_in が共有。
fn find_performer_dir(repo_root: &Path, name: &str) -> Option<PathBuf> {
    let dir = config::project_lanes_dir(repo_root).join(name);
    if dir.is_dir() { Some(dir) } else { None }
}

// --- helpers ---

/// Capture uncommitted changes (staged + unstaged + untracked) as a combined diff.
/// Returns None if there are no changes.
fn capture_dirty_diff(repo_root: &Path) -> Result<Option<String>, String> {
    // Staged + unstaged tracked changes.
    // `--no-ext-diff`: user の global `diff.external`（例: sem-cli dogfood の sem-diff-wrapper）を
    // 無視して git native の unified diff を得る。この出力は後段 `apply_patch`（`git apply`）に
    // 食わせるため、 external diff driver の semantic 出力だと適用不能になる（correctness 要件）。
    let tracked = Command::new("git")
        .args(["diff", "--no-ext-diff", "HEAD"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| e.to_string())?;

    if !tracked.status.success() {
        return Err("git diff HEAD に失敗しました".to_string());
    }

    let diff = String::from_utf8_lossy(&tracked.stdout).to_string();

    // Untracked files — generate diff with --no-index /dev/null <file>
    let untracked = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| e.to_string())?;

    let mut full_diff = diff;

    if untracked.status.success() {
        for file in String::from_utf8_lossy(&untracked.stdout).lines() {
            let file = file.trim();
            if file.is_empty() {
                continue;
            }
            // Use git diff --no-index to generate a proper patch (handles binary, no-newline, etc.)
            // `--no-ext-diff` で global `diff.external`（sem 等）を無視（上の tracked と同理由）。
            let file_diff = Command::new("git")
                .args([
                    "diff",
                    "--no-ext-diff",
                    "--no-index",
                    "--",
                    "/dev/null",
                    file,
                ])
                .current_dir(repo_root)
                .output()
                .ok();
            if let Some(output) = file_diff {
                // --no-index exits 1 when files differ (expected), only skip on spawn failure
                let patch = String::from_utf8_lossy(&output.stdout);
                if !patch.is_empty() {
                    // Rewrite paths: /dev/null → a/<file>, <file> → b/<file>
                    for line in patch.lines() {
                        if line.starts_with("+++ ") && !line.contains("/dev/null") {
                            full_diff.push_str(&format!("+++ b/{file}\n"));
                        } else if line.starts_with("--- /dev/null") {
                            full_diff.push_str("--- /dev/null\n");
                        } else if line.starts_with("diff --git") {
                            full_diff.push_str(&format!("diff --git a/{file} b/{file}\n"));
                        } else {
                            full_diff.push_str(line);
                            full_diff.push('\n');
                        }
                    }
                }
            }
        }
    }

    if full_diff.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(full_diff))
    }
}

/// Apply a unified diff patch to a directory
fn apply_patch(performer_dir: &Path, patch: &str) -> Result<(), String> {
    let mut child = Command::new("git")
        .args(["apply", "--allow-empty", "-"])
        .current_dir(performer_dir)
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(patch.as_bytes())
            .map_err(|e| e.to_string())?;
    }

    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git apply failed: {stderr}"));
    }

    Ok(())
}

fn run_git(args: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {} failed: {stderr}", args.join(" ")));
    }
    Ok(())
}

fn run_git_in(dir: &std::path::Path, args: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {} failed: {stderr}", args.join(" ")));
    }
    Ok(())
}

fn count_changes(dir: &std::path::Path) -> usize {
    let output = Command::new("git")
        .args(["status", "--short"])
        .current_dir(dir)
        .output()
        .ok();
    match output {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .count(),
        _ => 0,
    }
}

fn get_ahead_behind(dir: &std::path::Path) -> String {
    let output = Command::new("git")
        .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .current_dir(dir)
        .output()
        .ok();
    match output {
        Some(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let parts: Vec<&str> = s.split('\t').collect();
            if parts.len() == 2 {
                let ahead: i32 = parts[0].parse().unwrap_or(0);
                let behind: i32 = parts[1].parse().unwrap_or(0);
                match (ahead, behind) {
                    (0, 0) => "up-to-date".to_string(),
                    (a, 0) => format!("↑{a}"),
                    (0, b) => format!("↓{b}"),
                    (a, b) => format!("↑{a}↓{b}"),
                }
            } else {
                "-".to_string()
            }
        }
        _ => "local".to_string(),
    }
}

fn get_last_commit(dir: &std::path::Path) -> String {
    let output = Command::new("git")
        .args(["log", "--oneline", "-1"])
        .current_dir(dir)
        .output()
        .ok();
    match output {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "-".to_string(),
    }
}

/// Check if HEAD in the performer dir is merged into origin/main (or origin/master).
///
/// A performer is "merged" only if:
///   1. HEAD is an ancestor of origin/<main> (merge-base --is-ancestor), AND
///   2. The performer has diverged (has at least 1 local commit beyond origin/<main>)
///
/// This prevents false positives on freshly-created performers (HEAD == origin/main).
fn is_branch_merged(performer_dir: &std::path::Path) -> bool {
    for branch in &["main", "master"] {
        let remote_ref = format!("origin/{branch}");

        // Check if HEAD is ancestor of remote main
        let ancestor = Command::new("git")
            .args(["merge-base", "--is-ancestor", "HEAD", &remote_ref])
            .current_dir(performer_dir)
            .output()
            .ok();
        if !matches!(ancestor, Some(ref o) if o.status.success()) {
            continue;
        }

        // Guard: skip if HEAD is exactly the same commit as origin/main
        // (freshly created performer that hasn't diverged yet)
        let head_sha = git_rev_parse(performer_dir, "HEAD");
        let remote_sha = git_rev_parse(performer_dir, &remote_ref);
        if head_sha == remote_sha {
            continue;
        }

        return true;
    }
    false
}

fn git_rev_parse(dir: &std::path::Path, rev: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(dir)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

fn get_branch(dir: &std::path::Path) -> Option<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(dir)
        .output()
        .ok()?;

    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if branch.is_empty() {
            None
        } else {
            Some(branch)
        }
    } else {
        None
    }
}

// ── Phase 5-D D1: Performer status (struct 返却、 SP API exposure 用) ───────────

/// Performer workspace の git 状態 snapshot。 `performer_status(path)` で取得、
/// `/api/lanes` の LaneInfo に embed して sidebar に表示する。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PerformerStatus {
    /// 現在のブランチ (detached HEAD 時 None)
    pub branch: Option<String>,
    /// `git status --short` の non-empty lines (= 変更ファイル数、 0 なら clean)
    pub dirty_count: usize,
    /// upstream に対する ahead commit 数 (upstream 無い時 0)
    pub ahead: u32,
    /// upstream に対する behind commit 数
    pub behind: u32,
    /// upstream tracking 自体があるか (`local` / detached の判別用)
    pub has_upstream: bool,
    /// 最新 commit `{sha} {message}` (`git log --oneline -1`)、 取得失敗時 "-"
    pub last_commit: String,
    /// origin/main (or origin/master) に merge 済みで cleanup 候補か
    pub is_merged: bool,
}

/// Performer workspace dir から status snapshot を取得 (SP API 用)。 git 関連 subprocess を
/// 5-7 個並列に呼ぶので 1 回 ~50-100ms 程度。 多数 performer 時は SP 側で並列化検討。
pub fn performer_status(dir: &Path) -> PerformerStatus {
    let branch = get_branch(dir);
    let dirty_count = count_changes(dir);
    let (ahead, behind, has_upstream) = get_ahead_behind_counts(dir);
    let last_commit = get_last_commit(dir);
    let is_merged = is_branch_merged(dir);
    PerformerStatus {
        branch,
        dirty_count,
        ahead,
        behind,
        has_upstream,
        last_commit,
        is_merged,
    }
}

/// `get_ahead_behind` の数値版 ── upstream tracking が無いと `(0, 0, false)` を返す。
fn get_ahead_behind_counts(dir: &Path) -> (u32, u32, bool) {
    let output = Command::new("git")
        .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .current_dir(dir)
        .output()
        .ok();
    match output {
        Some(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let parts: Vec<&str> = s.split('\t').collect();
            if parts.len() == 2 {
                let ahead: u32 = parts[0].parse().unwrap_or(0);
                let behind: u32 = parts[1].parse().unwrap_or(0);
                (ahead, behind, true)
            } else {
                (0, 0, false)
            }
        }
        _ => (0, 0, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as Cmd;

    #[test]
    fn clear_lane_state_files_uses_repo_basename_key() {
        // キー凍結: project = repo_root の basename (SP 書き手の derivation と一致、
        // create_performer_orchestrated 等参照)。ズレると GC が空振りして leak が再発する。
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        let repo_root = tmp.path().join("parent").join("vp");
        crate::lane::console_mode::record_in(
            base,
            "vp",
            "feat",
            crate::lane::console_mode::ConsoleMode::Chat,
        )
        .expect("record mode");
        crate::lane::cc_session::record_in(base, "vp", "feat", "sess-1").expect("record session");

        clear_lane_state_files_in(base, &repo_root, "feat");

        assert_eq!(crate::lane::console_mode::last_in(base, "vp", "feat"), None);
        assert_eq!(crate::lane::cc_session::last_in(base, "vp", "feat"), None);
        // 未記録 lane / 二重呼び出しでも panic しない (best-effort 冪等)
        clear_lane_state_files_in(base, &repo_root, "feat");
    }

    #[test]
    fn persist_lane_model_writes_engine_model_with_basename_key() {
        // co-evolution #1: CLI `--model` は SP spawn 経路が読む engine_model へ、
        // repo basename を project key として書く (key derivation は clear と同一)。
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        let repo_root = tmp.path().join("parent").join("vp");

        // None は no-op（未記録のまま = claude default）
        persist_lane_model_in(base, &repo_root, "feat", None).expect("None は Ok");
        assert_eq!(crate::lane::engine_model::last_in(base, "vp", "feat"), None);

        // 空白のみも no-op
        persist_lane_model_in(base, &repo_root, "feat", Some("  ")).expect("空白は Ok");
        assert_eq!(crate::lane::engine_model::last_in(base, "vp", "feat"), None);

        // 有効な model は engine_model に basename key で書かれる
        persist_lane_model_in(base, &repo_root, "feat", Some("claude-fable-5")).expect("record");
        assert_eq!(
            crate::lane::engine_model::last_in(base, "vp", "feat").as_deref(),
            Some("claude-fable-5"),
            "SP spawn が読む project=basename('vp') key に書かれる"
        );

        // 不正 model は Err（worktree 作成後でも spawn 前に弾く）
        let err =
            persist_lane_model_in(base, &repo_root, "feat", Some("opus; rm -rf /")).unwrap_err();
        assert!(err.contains("不正"), "injection 形は Err: {err}");
    }

    // --- test helpers ---

    /// Create a unique temp dir per test to avoid parallel test collisions
    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lane-cmd-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Initialize a git repo with an initial commit in the given directory.
    /// Configures local user.name/email to avoid system config dependency.
    fn git_init_with_commit(dir: &std::path::Path) {
        Cmd::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .output()
            .unwrap();
        // initial commit
        fs::write(dir.join("README.md"), "# test\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "initial commit"])
            .current_dir(dir)
            .output()
            .unwrap();
        // Ensure we are on 'main'
        Cmd::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    // --- capture_dirty_diff ---

    #[test]
    fn capture_dirty_diff_no_changes_returns_none() {
        let repo = test_dir("dirty-diff-clean");
        git_init_with_commit(&repo);

        let result = capture_dirty_diff(&repo).unwrap();
        assert!(result.is_none(), "clean repo should return None");

        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn capture_dirty_diff_tracked_change_returns_some() {
        let repo = test_dir("dirty-diff-tracked");
        git_init_with_commit(&repo);

        // tracked ファイルを変更
        fs::write(repo.join("README.md"), "# modified\n").unwrap();

        let result = capture_dirty_diff(&repo).unwrap();
        assert!(result.is_some(), "tracked change should return Some diff");
        let diff = result.unwrap();
        assert!(
            diff.contains("README.md"),
            "diff should mention the changed file"
        );

        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn capture_dirty_diff_untracked_file_included() {
        let repo = test_dir("dirty-diff-untracked");
        git_init_with_commit(&repo);

        // untracked ファイルを追加
        fs::write(repo.join("new_file.txt"), "hello world\n").unwrap();

        let result = capture_dirty_diff(&repo).unwrap();
        assert!(result.is_some(), "untracked file should produce Some diff");
        let diff = result.unwrap();
        assert!(
            diff.contains("new_file.txt"),
            "diff should include the untracked file"
        );

        let _ = fs::remove_dir_all(&repo);
    }

    // --- is_branch_merged ---

    /// bare repo → clone 構成で origin/main を持つパフォーマーを作る
    fn setup_merged_performer_repos(
        base: &std::path::Path,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        // 1. bare repo（origin の代替）を作成
        //    --initial-branch=main で CI runner (init.defaultBranch=master 可能性) でも
        //    origin/HEAD が main に固定されるようにする
        let bare = base.join("bare.git");
        fs::create_dir_all(&bare).unwrap();
        Cmd::new("git")
            .args(["init", "--bare", "--initial-branch=main"])
            .current_dir(&bare)
            .output()
            .unwrap();

        // 2. bare を clone してメイン repo を作る
        let main_repo = base.join("main_repo");
        Cmd::new("git")
            .args(["clone", bare.to_str().unwrap(), main_repo.to_str().unwrap()])
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&main_repo)
            .output()
            .unwrap();

        // initial commit を main_repo で作り、bare に push
        fs::write(main_repo.join("README.md"), "# init\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        // main ブランチにリネーム
        Cmd::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(&main_repo)
            .output()
            .unwrap();

        // 3. performer repo を bare から clone
        let performer_repo = base.join("performer");
        Cmd::new("git")
            .args([
                "clone",
                bare.to_str().unwrap(),
                performer_repo.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();

        (main_repo, performer_repo)
    }

    #[test]
    fn is_branch_merged_returns_true_after_merge() {
        let base = test_dir("merged-true");
        let (main_repo, performer_repo) = setup_merged_performer_repos(&base);

        // performer で feature ブランチを作りコミット
        Cmd::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();
        fs::write(performer_repo.join("feature.txt"), "feature work\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(&performer_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "feature commit"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();

        // performer の feature を bare に push
        Cmd::new("git")
            .args(["push", "origin", "feature"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();

        // main_repo で feature を main に merge して push
        Cmd::new("git")
            .args(["fetch", "origin"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["merge", "origin/feature", "--no-edit"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        // main にさらに1コミット追加して origin/main を feature より先に進める
        fs::write(main_repo.join("extra.txt"), "extra\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "post-merge commit"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["push", "origin", "main"])
            .current_dir(&main_repo)
            .output()
            .unwrap();

        // performer が fetch して origin/main を最新化
        // performer の HEAD は feature のまま（origin/main より古い）
        Cmd::new("git")
            .args(["fetch", "origin"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();

        // performer HEAD は origin/main の祖先 + 分岐あり → merged = true
        assert!(
            is_branch_merged(&performer_repo),
            "merged feature branch should return true"
        );

        let _ = fs::remove_dir_all(&base);
    }

    // --- find_performer_dir (project-local lane refactor PR 4b: legacy global path 撤去) ---

    /// 共通 fixture: temp 領域に偽 repo + project-local lane dir (.vp/lanes/) を作る。
    fn setup_pl_fixture(slug: &str) -> (PathBuf, PathBuf) {
        let repo = test_dir(&format!("pl-{slug}"));
        let pl = repo.join(".vp").join("lanes");
        fs::create_dir_all(&pl).unwrap();
        (repo, pl)
    }

    #[test]
    fn find_performer_dir_returns_project_local() {
        let (repo, pl) = setup_pl_fixture("found");
        let performer = pl.join("foo");
        fs::create_dir_all(&performer).unwrap();

        let resolved = find_performer_dir(&repo, "foo");
        assert_eq!(resolved.as_deref(), Some(performer.as_path()));

        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn find_performer_dir_returns_none_when_missing() {
        let (repo, _pl) = setup_pl_fixture("missing");
        let resolved = find_performer_dir(&repo, "absent");
        assert!(resolved.is_none());
        let _ = fs::remove_dir_all(&repo);
    }

    // --- list_performers_for_repo (PR 4b: project-local 一本) ---

    // --- resolve_lane_index_by_performer_name (= 「目的ベース port 解決」 の核) ---

    #[test]
    fn resolve_lane_index_alphabetical_sort() {
        // performer が alphabetical sort 順で lane_index 1, 2, 3 に並ぶ
        let (repo, pl) = setup_pl_fixture("resolve-alpha");
        fs::create_dir_all(pl.join("charlie").join(".git")).unwrap();
        fs::create_dir_all(pl.join("alpha").join(".git")).unwrap();
        fs::create_dir_all(pl.join("bravo").join(".git")).unwrap();

        assert_eq!(
            resolve_lane_index_by_performer_name(&repo, "alpha"),
            Some(1)
        );
        assert_eq!(
            resolve_lane_index_by_performer_name(&repo, "bravo"),
            Some(2)
        );
        assert_eq!(
            resolve_lane_index_by_performer_name(&repo, "charlie"),
            Some(3)
        );

        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn resolve_lane_index_returns_none_for_missing_performer() {
        let (repo, pl) = setup_pl_fixture("resolve-missing");
        fs::create_dir_all(pl.join("foo").join(".git")).unwrap();

        assert_eq!(resolve_lane_index_by_performer_name(&repo, "bar"), None);
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn resolve_lane_index_returns_none_when_no_performers() {
        let (repo, _pl) = setup_pl_fixture("resolve-empty");
        assert_eq!(resolve_lane_index_by_performer_name(&repo, "any"), None);
        let _ = fs::remove_dir_all(&repo);
    }

    // --- list_performers_for_repo (PR 4b: project-local 一本) ---

    #[test]
    fn list_performers_for_repo_lists_project_local_only() {
        let (repo, pl) = setup_pl_fixture("list");
        fs::create_dir_all(pl.join("foo").join(".git")).unwrap();
        fs::create_dir_all(pl.join("bar").join(".git")).unwrap();

        let mut listed: Vec<String> = list_performers_for_repo(&repo)
            .into_iter()
            .map(|e| e.name)
            .collect();
        listed.sort();
        assert_eq!(listed, vec!["bar", "foo"]);

        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn list_performers_for_repo_returns_empty_when_dir_missing() {
        // <repo>/.vp/lanes が無い場合は空 Vec (= read error は fail-soft)
        let repo = test_dir("list-no-pl");
        fs::create_dir_all(&repo).unwrap();
        let listed = list_performers_for_repo(&repo);
        assert!(listed.is_empty());
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn is_branch_merged_returns_false_when_head_equals_origin_main() {
        let base = test_dir("merged-false-fresh");
        let (_, performer_repo) = setup_merged_performer_repos(&base);

        // performer に local commit なし（HEAD == origin/main）
        // false-positive ガード: is_branch_merged は false を返すべき
        assert!(
            !is_branch_merged(&performer_repo),
            "fresh performer (HEAD == origin/main) should return false"
        );

        let _ = fs::remove_dir_all(&base);
    }

    // --- worktree lane (setup_performer Isolation::Worktree / resolve_default_branch / remove) ---

    /// bare(origin) + conductor clone を作り conductor repo path を返す (worktree lane test 用)。
    /// origin/HEAD = main を明示設定して resolve_default_branch の経路を通す。
    fn setup_worktree_fixture(slug: &str) -> (PathBuf, PathBuf) {
        let base = test_dir(&format!("wt-{slug}"));
        fs::create_dir_all(&base).unwrap();
        let bare = base.join("bare.git");
        fs::create_dir_all(&bare).unwrap();
        Cmd::new("git")
            .args(["init", "--bare", "--initial-branch=main"])
            .current_dir(&bare)
            .output()
            .unwrap();
        let conductor = base.join("conductor");
        Cmd::new("git")
            .args(["clone", bare.to_str().unwrap(), conductor.to_str().unwrap()])
            .output()
            .unwrap();
        for (k, v) in [("user.email", "test@example.com"), ("user.name", "Test")] {
            Cmd::new("git")
                .args(["config", k, v])
                .current_dir(&conductor)
                .output()
                .unwrap();
        }
        fs::write(conductor.join("README.md"), "# init\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(&conductor)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&conductor)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(&conductor)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(&conductor)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["remote", "set-head", "origin", "main"])
            .current_dir(&conductor)
            .output()
            .unwrap();
        (base, conductor)
    }

    #[test]
    fn resolve_default_branch_returns_main() {
        let (base, conductor) = setup_worktree_fixture("resolve-default");
        assert_eq!(resolve_default_branch(&conductor).as_deref(), Some("main"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn setup_performer_worktree_creates_shared_worktree() {
        let (base, conductor) = setup_worktree_fixture("create");
        let performer = setup_performer(
            "feat",
            "mako/feat",
            &conductor,
            false,
            Isolation::Worktree,
            None,
        )
        .unwrap();
        // worktree marker: .git は file (gitdir pointer)、clone なら dir
        assert!(
            performer.join(".git").is_file(),
            "worktree の .git は file であるべき"
        );
        assert_eq!(get_branch(&performer).as_deref(), Some("mako/feat"));
        // git worktree list に登録される
        let out = Cmd::new("git")
            .args(["worktree", "list"])
            .current_dir(&conductor)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("mako/feat"),
            "worktree list に branch が出る"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn remove_performer_workspace_worktree_keeps_branch() {
        let (base, conductor) = setup_worktree_fixture("remove-keeps-branch");
        let performer = setup_performer(
            "rm",
            "mako/rm",
            &conductor,
            false,
            Isolation::Worktree,
            None,
        )
        .unwrap();
        assert!(performer.exists());

        remove_performer_workspace(&conductor, &performer).unwrap();
        assert!(!performer.exists(), "worktree dir は削除される");

        // 設計 E: branch は worktree remove 後も残す (未 push 保全)
        let branches = Cmd::new("git")
            .args(["branch", "--list", "mako/rm"])
            .current_dir(&conductor)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&branches.stdout).contains("mako/rm"),
            "branch は残る"
        );

        // prune 済で stale worktree 登録が残らない
        let wl = Cmd::new("git")
            .args(["worktree", "list"])
            .current_dir(&conductor)
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&wl.stdout).contains("/.vp/lanes/rm"),
            "stale worktree 登録は prune 済"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn setup_performer_worktree_duplicate_branch_errors() {
        // F3: 同名 branch で 2 つ目の worktree を作ろうとすると actionable error
        let (base, conductor) = setup_worktree_fixture("dup-branch");
        setup_performer(
            "first",
            "mako/dup",
            &conductor,
            false,
            Isolation::Worktree,
            None,
        )
        .unwrap();
        let err = setup_performer(
            "second",
            "mako/dup",
            &conductor,
            false,
            Isolation::Worktree,
            None,
        )
        .unwrap_err();
        assert!(
            err.contains("既に存在") || err.contains("使用中") || err.contains("mako/dup"),
            "branch 衝突は分かりやすい error を返すべき: {err}"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn setup_performer_worktree_base_override_uses_local_branch() {
        // co-evolution #2 の dogfood シナリオ: conductor の未 push feature branch を
        // base に wing を切る (origin に無い ref でも resolve_start_point の local probe で通る)。
        let (base, conductor) = setup_worktree_fixture("base-override");
        Cmd::new("git")
            .args(["checkout", "-b", "mako/feature-base"])
            .current_dir(&conductor)
            .output()
            .unwrap();
        fs::write(conductor.join("feature.txt"), "土台 ADT\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(&conductor)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "feature base"])
            .current_dir(&conductor)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["checkout", "main"])
            .current_dir(&conductor)
            .output()
            .unwrap();

        let performer = setup_performer(
            "wing",
            "mako/wing",
            &conductor,
            false,
            Isolation::Worktree,
            Some("mako/feature-base"),
        )
        .unwrap();
        assert!(
            performer.join("feature.txt").exists(),
            "wing は feature branch の内容 (未 merge 土台) から分岐すべき"
        );
        assert_eq!(get_branch(&performer).as_deref(), Some("mako/wing"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_base_ref_priority_override_then_kdl() {
        // 優先順: override → performer-files.kdl base-ref → origin/HEAD。空白 override は無視。
        let (base, conductor) = setup_worktree_fixture("base-priority");
        let cfg = config::PerformerConfig {
            base_ref: Some("kdl-base".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_base_ref(&conductor, &cfg, Some("cli-base")),
            "cli-base"
        );
        assert_eq!(resolve_base_ref(&conductor, &cfg, Some("  ")), "kdl-base");
        assert_eq!(resolve_base_ref(&conductor, &cfg, None), "kdl-base");
        let no_kdl = config::PerformerConfig::default();
        assert_eq!(resolve_base_ref(&conductor, &no_kdl, None), "main");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn setup_performer_clone_with_base_errors() {
        // clone isolation は conductor HEAD の depth-1 複製で base 分岐に非対応 → 明示 error
        let (base, conductor) = setup_worktree_fixture("clone-base");
        let err = setup_performer(
            "cl",
            "mako/cl",
            &conductor,
            false,
            Isolation::Clone,
            Some("main"),
        )
        .unwrap_err();
        assert!(
            err.contains("worktree"),
            "clone + base は worktree のみ対応の error を返すべき: {err}"
        );
        let _ = fs::remove_dir_all(&base);
    }
}
