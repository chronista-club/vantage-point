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
/// この lane が spawn される際に tui claude の `--model` として読まれる。 None なら claude default。
/// worktree 作成のみ（spawn は repo が別途行う）なので、 ここでは state file を書くだけ。
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

/// Phase 4-X: repo-friendly wrapper. `repo_root` を明示的に受け取り、 performer dir の `PathBuf` を返す。
/// stdout への print なし、 lib call として完結。 repo server (lanes.rs) から直接呼ぶ用。
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

/// Phase 4-X: repo-friendly remove。 repo_root を明示的に受け取り、 repo-local 新 path で
/// performer dir を解決して削除する。
///
/// repo-local lane refactor PR 1: `repo_name: &str` → `repo_root: &Path` に signature
/// 変更。 caller (sidebar 経由 DELETE 等) は state.repo_dir を直接渡せる。
/// PR 4b: legacy global path dual-read 削除、 repo-local 一本に。
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
    // state file GC: orchestrated 経路 (Phase 2a) と重複しても冪等。 repo remove の
    // B-destroy reclaim (repo_manager_capability) はここしか通らないので必須。
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
/// repo-local lane refactor: 新 lane の配置先は `<repo_root>/.vp/lanes/<name>`。
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

    let performers_dir = config::repo_lanes_dir(repo_root);
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
                    "--base は isolation=worktree のみ対応 (clone は root HEAD の複製)".to_string(),
                );
            }
            provision_clone(repo_root, &performer_dir, branch)?
        }
    }

    // parent repo の .gitignore に .vp/ を追記 (idempotent、 best-effort)。 失敗しても
    // performer 作成は続行する (= user が手動で .gitignore 編集する fallback path 残す)。
    //
    // ⚠️ **provisioning の後に置く**こと。 これは repo を書き換える action なので、
    // 「lane の実体が実際に建った」= `repo_root` が本物の repo だと git 操作が実証した
    // 後にだけ走らせる。 入口 (検証前) に置くと、 **失敗する create でも .gitignore を
    // 書いてしまう**: `repo_dir` 未設定で repo_root が process cwd に落ちると、
    // 無関係な dir に `.vp/` 記載を撒く（VP repo で `cargo test` するたび
    // `crates/vantage-point/.gitignore` が湧いていた実害。 2026-07-23 に特定）。
    if let Err(e) = config::ensure_vp_gitignored(repo_root) {
        eprintln!("⚠ .gitignore への .vp/ 追記失敗 (続行): {e}");
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

    // repo-local lane refactor PR 4a: PR #429 の `claude_trust::pre_grant_trust` 削除。
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
        // dep symlink 防御 (defense-in-depth の壁 2): find_performer_dir で既に弾かれるが、
        // 万一 symlink path が渡っても `remove_dir_all` を走らせず明示 Err で止める。
        // `.git` は上の is_file 分岐で false = symlink か clone dir。symlink_metadata で確定する。
        let ft = fs::symlink_metadata(performer_dir)
            .map_err(|e| e.to_string())?
            .file_type();
        if ft.is_symlink() {
            return Err(format!(
                "{} は dependency symlink です。performer ではありません。意図的な削除は rm で行ってください。",
                performer_dir.display()
            ));
        }
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
/// キーは repo の書き手 (lanes_state::set_console_mode / agent_spawner の VP_REPO env、
/// create_performer_orchestrated 等) と同じ derivation: repo = repo_root の basename、
/// lane = performer 名。
fn clear_lane_state_files(repo_root: &Path, lane: &str) {
    clear_lane_state_files_in(&crate::config::vp_state_dir(), repo_root, lane);
}

/// lane の claude model を `engine_model` へ永続する (co-evolution #1、CLI `vp lane new/fork --model`)。
///
/// repo key は `clear_lane_state_files` / repo `create_performer_orchestrated` と同一
/// derivation (repo_root basename) — CLI で書いた model を repo spawn 経路が読めるようにする。
/// 明示 `--model` が無ければ config の `default-lane-model`（未設定なら Opus）にフォールバックして
/// record する（内部 helper `persist_lane_model_in` は従来通り None=no-op、既定解決はこの wrapper が担う）。
/// 不正な model 名は Err で早期に弾く (worktree は作成済だが spawn 前に失敗を返す方が silent degrade より良い)。
///
/// `repo_root` は [`config::find_repo_root`] 由来で **常に main worktree root** に正規化される
/// (lane worktree の中から呼んでも repo 読み手の `addr.repo` = main root basename と一致する。
/// 旧: worktree 内実行で repo key mismatch → model が silent 無視。repo key 正規化で解消)。
fn persist_lane_model(repo_root: &Path, lane: &str, model: Option<&str>) -> Result<(), String> {
    // doc 54 §8-11: 明示 `--model` > config `default-lane-model` > 無記録（engine 側の
    // user 既定に委ねる）。mcp / sidebar 経路（create_performer_orchestrated）と同じ既定規則。
    let cfg = crate::config::Config::load().unwrap_or_default();
    let effective = super::engine_model::resolve_default(model, cfg.default_lane_model());
    persist_lane_model_in(
        &crate::config::vp_state_dir(),
        repo_root,
        lane,
        effective.as_deref(),
    )
}

/// state base dir 注入版 (テスト用)。
///
/// 記録先は registry の初期 session（key=1）の `SessionEntry.model`（2026-07-27 に per-lane
/// `engine_model` file から session 紐づけへ移行）。CLI 作成 lane は既定 agent = claude
/// （`--agent` を持つのは orchestrated 経路のみ — `agent_store` の書き手が routes 側だけ
/// であることに対応）。model 未指定なら registry file を作らない（set_model_in が
/// 変化なし = no-save に倒す）。
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
    let repo = repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    super::session_registry::set_model_in(base, repo, lane, "claude", 1, Some(model))
        .map(|_| ())
        .map_err(|e| format!("model 永続に失敗: {e}"))
}

/// lane-scoped state file の repo key（= repo_root の basename）。
///
/// repo 書き手の derivation（`addr.repo`）と一致する前提で 2 経路が動いている
/// （`clear_lane_state_in` の doc 参照）。**同じ derivation を要る場所が増えたので関数に畳んだ**
/// — 各 call site が個別に basename を取ると 1 箇所ズレた時に無音で別 key を触る
/// （lane_id は帳簿の key なので、ズレると履歴が別 lane のものになる）。
fn lane_state_repo_key(repo_root: &Path) -> &str {
    repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
}

/// state base dir 注入版 (テスト用)。repo key を repo basename から導き、一元 GC に委譲。
fn clear_lane_state_files_in(base: &Path, repo_root: &Path, lane: &str) {
    clear_lane_state_in(base, lane_state_repo_key(repo_root), lane);
}

/// lane の安定 id を引く（Repo Host の帳簿の key、doc 44 §8.2）。
///
/// SSOT は `lane_ids/<repo>__<lane>` state file で、daemon 側の `LaneInfo.id` も
/// 同じ関数（[`super::lane_id::load_or_create`]）から来る = **同じ lane なら同じ id**。
/// だから CLI 側で解決した id をそのまま帳簿に送れる。
///
/// ⚠️ **lane を消す前に**引くこと。`clear_lane_state_files` が id file ごと消すので、
/// 削除後に引くと別の id が生える（= 見送りの記録が「知らない lane」になる）。
/// 逆に言うと、同名 lane を作り直すと必ず別 id になるので**前の履歴と混ざらない**。
fn lane_stable_id(repo_root: &Path, lane: &str) -> String {
    super::lane_id::load_or_create(lane_state_repo_key(repo_root), lane).to_string()
}

/// 本番 base での lane-scoped state 一元 GC (repo `delete_lane_orchestrated` から呼ぶ)。
pub(crate) fn clear_lane_state(repo: &str, lane: &str) {
    clear_lane_state_in(&crate::config::vp_state_dir(), repo, lane);
}

/// lane-scoped state file の**一元** GC (best-effort、 冪等)。
///
/// 削除系 2 経路の**唯一の破棄リスト**: CLI 側 (`clear_lane_state_files_in` 経由 =
/// `remove_performer` / `cleanup_performers`) と repo 側 (`delete_lane_orchestrated` Phase 2a)。
/// 従来は両経路が別々のリストを持ち、片方に足した clear がもう片方から漏れていた
/// (replay_log / terminal_replay が repo delete で残り、 同名 lane 再作成で旧 replay が蘇る
/// ghost leak、 moody 観察 2026-07-19)。ここに集約して両経路が同じリストを共有する。
///
/// `repo` / `lane` (= lane label) は各呼び手が自分の derivation で解決して渡す
/// (CLI = repo_root basename、 repo = `addr.repo` + `lane_label(addr)`)。両者は repo 書き手の
/// key derivation と一致する (既存 2 経路が既にこの前提で動いていた)。
///
/// 破棄対象 = 同名 lane 再作成で蘇ってはならない全 lane-scoped state (計 6 種):
/// session_registry (会話 id と Mode の SSOT) / engine_model / agent (engine 種別) /
/// conversation_replay (session label 単位) / terminal_replay (slot の scrollback) / lane_id (安定 id)。
///
/// best-effort: 個々の失敗は warn して残置し、他の破棄は続行する (1 file の fs error で
/// 残り 5 種の GC を落とさない)。冪等 = 未記録 / 二重呼び出しは全て no-op。
fn clear_lane_state_in(base: &Path, repo: &str, lane: &str) {
    // ① conversation_replay は **session label 単位** (`<lane>` + `<lane>#<n>`)。registry を消す前に
    //    全 session を列挙して各 label の replay log を消す (残すと transcript を持たない engine の
    //    replay 源が同名 lane に蘇る)。default_agent は registry file 不在時の N=1 既定形にしか
    //    効かず、 その唯一 session の label は素の lane 名 (下の console / terminal_replay と同鍵)
    //    なので列挙値は問わない。
    let reg = super::session_registry::load_in(base, repo, lane, "claude");
    for s in &reg.sessions {
        let label = super::session_registry::session_label(lane, s.key);
        if let Err(e) = crate::conversation::replay_log::clear_in(base, repo, &label) {
            tracing::warn!(
                "lane state GC: replay log の破棄に失敗 (残置): lane={lane} session={} err={e}",
                s.key
            );
        }
    }
    // ② session_registry (会話 id と Mode の SSOT — 残すと旧 session / 旧会話 id / 旧 Mode が蘇る)。
    //    ①の列挙後に消す。
    if let Err(e) = super::session_registry::clear_in(base, repo, lane) {
        tracing::warn!("lane state GC: session registry の破棄に失敗 (残置): lane={lane} err={e}");
    }
    // ③ (退役) engine_model — model は registry（SessionEntry.model）に移行済みで ② が併せ消す。
    // ④ agent (engine 種別 — repo 再起動またぎの spawn agent)
    if let Err(e) = super::agent_store::clear_in(base, repo, lane) {
        tracing::warn!("lane state GC: agent の破棄に失敗 (残置): lane={lane} err={e}");
    }
    // ⑤ terminal_replay (slot の scrollback の replay seed)
    if let Err(e) = crate::daemon::pty_slot::clear_replay_in(base, repo, lane) {
        tracing::warn!("lane state GC: terminal replay の破棄に失敗 (残置): lane={lane} err={e}");
    }
    // ⑥ lane_id (位置独立 安定 id)
    if let Err(e) = super::lane_id::clear_in(base, repo, lane) {
        tracing::warn!("lane state GC: lane_id の破棄に失敗 (残置): lane={lane} err={e}");
    }
}

/// `.vp/lanes/` の [`fs::DirEntry`] が **実 performer lane** かを判定する (dep symlink を除外)。
///
/// `.vp/lanes/` には 2 種のエントリが同居する:
/// - **performer lane** = `vp lane new` が `git worktree add` で作る**実ディレクトリ**
///   (`.git` は gitdir ポインタの file)
/// - **dep symlink** = webview の `file:../../../../{creoui,club-unison}` 依存を解決するための
///   sibling repo への**シンボリックリンク** (例 `.vp/lanes/creoui -> ~/repos/creoui`)
///
/// **不変条件**: 実 performer lane は必ず実ディレクトリで symlink にはならない。よって
/// 「`.vp/lanes/` 内の symlink ⟺ dep」が成立し、symlink を弾けば dep が cockpit 列挙 /
/// cleanup 誤判定 / delete 誤操作から構造的に消える。
///
/// `Path::is_dir()` は内部で `stat(2)` を呼び **symlink を辿る**ため、dep symlink を実 dir と
/// 誤認する (= このバグの物理的な根)。対して [`fs::DirEntry::file_type`] は `readdir` の d_type /
/// `lstat(2)` 由来で **symlink 自体の型**を返す (辿らない) ので、これで symlink を先に落とす。
/// file_type 取得不能 (Err) は防御的に「非 performer」扱い (列挙から除外)。
fn is_performer_entry(entry: &fs::DirEntry) -> bool {
    match entry.file_type() {
        Ok(ft) => !ft.is_symlink() && ft.is_dir(),
        Err(_) => false,
    }
}

/// List all performer environments under cwd の `<repo>/.vp/lanes/`。
///
/// repo-local lane refactor PR 4b: legacy global path 列挙を削除、 cwd の repo
/// の repo-local のみ表示。 cwd が git repo でない場合は空出力 (= 既存挙動と同様)。
pub fn list_performers() -> Result<(), String> {
    let Ok(repo_root) = config::find_repo_root() else {
        return Ok(());
    };
    let pl_dir = config::repo_lanes_dir(&repo_root);
    if !pl_dir.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(&pl_dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        if !is_performer_entry(&entry) {
            continue; // dep symlink を除外 (is_performer_entry doc 参照)
        }
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let branch = get_branch(&path).unwrap_or_else(|| "-".to_string());
        println!("{name}\t{branch}\t{}", path.display());
    }
    Ok(())
}

/// disk 上で発見された Performer 環境 1 件 (lane Performer dir の structured view、 repo /api/lanes 用)。
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

/// repo に紐づく Performer dir を `<repo>/.vp/lanes/` から disk scan して返す (repo /api/lanes 用)。
///
/// repo-local lane refactor PR 4b: legacy global path scan + dedup logic を削除、
/// repo-local のみ列挙に simplify。
///
/// 「基本は通らない防御パス」: 通常 lane clone は POST /api/lanes 経由で生成され、 同 session 内なら
/// LanePool に登録されている。 ただし vp-app crash 後の残骸 / 別 session での `vp lane new` 等で
/// disk に存在するが LanePool に居ない Performer が出ることがあり、 それを sidebar に inactive 状態で
/// surface するため。 click で activate (= POST /api/lanes に cwd 指定で attach) する想定。
///
/// fail-soft (= 防御パスのため read error は空 Vec 扱い)。
pub fn list_performers_for_repo(repo_root: &Path) -> Vec<InactivePerformerEntry> {
    let mut out = Vec::new();
    let pl_dir = config::repo_lanes_dir(repo_root);
    let Ok(entries) = fs::read_dir(&pl_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        if !is_performer_entry(&entry) {
            continue; // dep symlink を除外 (repo snapshot / sidebar / flow progress の choke point)
        }
        let path = entry.path();
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
/// repo-local lane refactor PR 4b: legacy global path fallback 削除、 cwd の repo
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
/// repo-local lane refactor PR 4b: legacy global path fallback 削除、 cwd の repo
/// の `<repo>/.vp/lanes/<name>` のみ対象。 cwd が git repo でない場合は error。
pub fn remove_performer(name: Option<&str>, all: bool, force: bool) -> Result<(), String> {
    let repo_root = config::find_repo_root().map_err(|e| e.to_string())?;
    let pl_dir = config::repo_lanes_dir(&repo_root);

    if all {
        if !force {
            return Err("--all には --force が必要です（誤削除防止）".into());
        }
        if !pl_dir.exists() {
            eprintln!("削除対象のパフォーマーはありませんでした");
            return Ok(());
        }
        // 実 performer entry のみ対象 (dep symlink は温存 — creoui/club-unison は build 生命線)。
        // 旧 `fs::remove_dir_all(&pl_dir)` の一括削除は dep symlink まで unlink するため使わない
        // (symlink 自体を除去、 target repo は残るが webview の依存解決が壊れる)。
        let performers: Vec<(String, PathBuf)> = fs::read_dir(&pl_dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(is_performer_entry)
                    .map(|e| (e.file_name().to_string_lossy().to_string(), e.path()))
                    .collect()
            })
            .unwrap_or_default();
        if performers.is_empty() {
            eprintln!("削除対象のパフォーマーはありませんでした");
            return Ok(());
        }
        for (name, dir) in &performers {
            // worktree / clone を問わず正しく後始末 (remove_performer_workspace が `.git` で判別)。
            if let Err(e) = remove_performer_workspace(&repo_root, dir) {
                eprintln!("⚠ パフォーマー削除に失敗: {name} ({e})");
            }
            clear_lane_state_files(&repo_root, name);
        }
        eprintln!("repo-local パフォーマー全削除: {} 件", performers.len());
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
/// repo-local lane refactor PR 4b: legacy global block 削除、 repo-local 一本に。
pub fn status_performers() -> Result<(), String> {
    let mut found = false;

    if let Ok(repo_root) = config::find_repo_root() {
        let pl_dir = config::repo_lanes_dir(&repo_root);
        if pl_dir.exists()
            && let Ok(entries) = fs::read_dir(&pl_dir)
        {
            for entry in entries.flatten() {
                if !is_performer_entry(&entry) {
                    continue; // dep symlink を除外
                }
                let path = entry.path();
                if !path.join(".git").exists() {
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

/// 見送り判定に渡す開発起点 lane 名を決める（doc 44 D4）。
///
/// 帳簿は daemon が持つ（DB は surrealkv の OS 排他ロックで Daemon 専有）ので、CLI からは
/// repo-proxy 越しに問い合わせる。Daemon 不在 / 応答不正なら **予約名にフォールバック**し、
/// その旨を告げる。
///
/// なぜ黙って落とさないか: 起点が確認できないまま見送ると、**移動済みの起点 lane を
/// 消しうる**。実害の確率は低い（起点が merged かつ clean かつ停止中である必要がある）が、
/// 「確認できなかった」という事実は人に見せる（Host は推測しない）。
fn origin_for_cleanup(repo_root: &Path) -> String {
    let reserved = crate::repo::lanes_state::ROOT_LANE_NAME.to_string();
    let Some(repo_path) = repo_root.to_str() else {
        return reserved;
    };
    let resp = crate::commands::process_client::daemon_repo_request_blocking(
        crate::cli::daemon_port(),
        repo_path,
        "lane_origin_get",
        serde_json::json!({}),
    );
    match resp.and_then(|v| Ok(serde_json::from_value::<crate::host::ledger::Origin>(v)?)) {
        Ok(origin) => origin.name,
        Err(e) => {
            eprintln!(
                "[vp] 開発起点を帳簿から確認できませんでした（既定 '{reserved}' として続行）: {e}"
            );
            reserved
        }
    }
}

/// 見送り判定に渡す「今動いている lane」を daemon に問い合わせる（doc 44 §7.5）。
///
/// lane の生死は git からは知れないので、[`crate::host::farewell`] は外からの供給に頼る。
/// CLI は daemon の "daemon-repo" channel に `list_all_lanes` を ask し、応答から
/// **この repo の分だけ**を [`crate::host::liveness::running_lanes_in`] で取り出す。
///
/// なぜ repo-proxy (`lanes_list`) ではないか: あちらは対象 repo の repo が daemon に
/// 登録されていないと逆引きに失敗して error になり、「repo が動いていない（= 稼働 lane 0）」と
/// 「daemon に訊けなかった（= 不明）」が区別できない。cross-project 一覧なら前者は**答え**として返る。
///
/// 失敗は [`Liveness::Unknown`] で返し、**空リストには畳まない** — それが P3 第一スライスで
/// guard を never-fire にしていた形そのもの。
///
/// `VP_TEST_NO_RUNNING_LANES=1` は **e2e テスト専用**の注入口で、daemon に訊かずに
/// 「稼働 lane 0」を**答え**として返す。これが無いと `vp lane cleanup` の e2e は
/// daemon 常駐マシンでしか通らない（開発機では通り CI だけ 10s timeout で落ちる =
/// 手元の daemon が failure をマスクする形）。`Unknown` ではなく `Known(空)` を返すのが
/// 要点 — 「daemon に訊けなかった」ではなく「訊いた結果 0 件だった」を模す。
fn liveness_for_cleanup(repo_root: &Path) -> crate::host::liveness::Liveness {
    use crate::host::liveness::Liveness;
    if skip_daemon_for_test() {
        return Liveness::Known(Vec::new());
    }
    let Some(repo_path) = repo_root.to_str() else {
        return Liveness::Unknown("repo path に invalid UTF-8".to_string());
    };
    match crate::commands::process_client::daemon_lanes_snapshot_blocking(crate::cli::daemon_port())
    {
        Ok(snapshot) => Liveness::Known(crate::host::liveness::running_lanes_in(
            &snapshot,
            Path::new(repo_path),
        )),
        Err(e) => Liveness::Unknown(e.to_string()),
    }
}

/// 見送りの帳簿（daemon が持つ）への読み書き（doc 44 §7.5）。
///
/// trait にしているのは **「稼働状況が不明で保留した時に帳簿へ 1 文字も書かない」を
/// テストで固定する**ため。実装が直接 RPC を撃つ形だと、書かなかったことを検証できない
/// （事実が無い状態を履歴に残さない、が要件）。
pub(crate) trait FarewellLedger {
    /// 判定を記録し、**反映後の滞留一覧**を返す（Daemon 不達なら空 = 注記を諦めて続行）。
    fn observe(
        &mut self,
        repo_root: &Path,
        observations: &[crate::host::ledger::FarewellObservation],
    ) -> Vec<crate::host::ledger::FarewellEntry>;

    /// 実際に見送った lane を記録する（終端 event）。
    fn reclaimed(&mut self, repo_root: &Path, entries: &[crate::host::ledger::FarewellObservation]);
}

/// 本番の帳簿 — daemon の daemon-control channel 越しに読み書きする。
///
/// 帳簿は db/machine にあり surrealkv の OS 排他ロックで daemon が専有するので、CLI からは
/// この経路しかない（doc 44 §8.4）。**失敗しても見送りは止めない**（best-effort）— 記録は
/// 判断材料であって、それが取れないことは lane を消してよいかの判断を変えない。
///
/// ⚠️ `cleanup` の Daemon 依存は **2 本ある**（稼働状況 = [`liveness_for_cleanup`] と、この帳簿）。
/// `VP_TEST_NO_RUNNING_LANES` は両方を塞ぐ — 片方だけだと e2e は「通るが Daemon 接続の
/// timeout ぶん遅い」状態になる（実測 45s → 157s）。fail-open なので結果は正しく、
/// **遅さでしか気付けない**。
struct DaemonFarewellLedger;

/// e2e テスト用に daemon への問い合わせを丸ごと省くか（[`liveness_for_cleanup`] と同じ口）。
fn skip_daemon_for_test() -> bool {
    std::env::var("VP_TEST_NO_RUNNING_LANES").as_deref() == Ok("1")
}

impl FarewellLedger for DaemonFarewellLedger {
    fn observe(
        &mut self,
        repo_root: &Path,
        observations: &[crate::host::ledger::FarewellObservation],
    ) -> Vec<crate::host::ledger::FarewellEntry> {
        if skip_daemon_for_test() {
            return Vec::new();
        }
        let Some(path) = repo_root.to_str() else {
            return Vec::new();
        };
        crate::daemon_client::farewell_observe_blocking(path, observations).unwrap_or_default()
    }

    fn reclaimed(
        &mut self,
        repo_root: &Path,
        entries: &[crate::host::ledger::FarewellObservation],
    ) {
        if skip_daemon_for_test() {
            return;
        }
        let Some(path) = repo_root.to_str() else {
            return;
        };
        if crate::daemon_client::farewell_reclaimed_blocking(path, entries).is_none() {
            eprintln!("[vp] 見送りを帳簿に記録できませんでした（削除自体は完了しています）");
        }
    }
}

/// 見送りの実行結果（何が起きたかをテストから見るための戻り値）。
///
/// 出力は eprintln なので、戻り値が無いと「保留した」と「1 件も対象が無かった」を
/// テストで区別できない（= 保留の regression が静かに入る）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CleanupOutcome {
    /// 稼働状況を確認できず**保留**（判定も削除も行っていない）
    Held,
    /// 判定対象が無かった
    Nothing,
    /// 判定のみ（`--force` 無し、または削除可能 0 件）
    Surveyed { reclaimable: usize },
    /// 実際に見送った
    Removed { count: usize },
}

/// Remove performers whose branch is merged into the repo's default branch
/// (cwd の `<repo>/.vp/lanes/` 対象)。
///
/// repo-local lane refactor PR 4b: legacy global block 削除、 repo-local 一本に。
/// co-evolution #3: default branch を `resolve_default_branch` (origin/HEAD) で解決し、
/// squash merge も検出する (旧: `origin/main` ハードコード + ancestry only)。
/// doc 44 P3: 判定は **Repo Host に移管**した（`host::farewell`）。
///
/// 本関数は Host の判定を人間に見せて実行する薄い surface になった。旧実装は
/// 収集・判定・分類を 1 関数（`classify_performer_for_cleanup`）に混ぜており:
/// - git subprocess を内部で呼ぶためテストできなかった
/// - 判定が 2 値（削除 / 保持）で「事実だけで決まらないもの」を表現できず、
///   **merged なら未コミット変更を見ずに削除候補**へ入れていた（= Host は推測しない、の違反）
///
/// Host 版は 3 値（reclaim / keep / ask_human）で、判定は I/O ゼロの純関数
/// （[`crate::host::farewell::judge_farewell`]）に分離済み。
///
/// doc 44 §7.5: 判定に要る事実（開発起点 / 稼働中 lane）は本関数が daemon から集めて渡す。
/// **稼働状況が確認できない場合は判定に進まず保留する**（[`cleanup_performers_with`]）。
/// 判定と実行は Repo Host の帳簿に記録され、`AskHuman` の滞留として出力に戻ってくる。
pub fn cleanup_performers(force: bool) -> Result<(), String> {
    let Ok(repo_root) = config::find_repo_root() else {
        eprintln!("クリーンアップ対象はありません。");
        return Ok(());
    };
    let liveness = liveness_for_cleanup(&repo_root);
    cleanup_performers_with(
        &mut std::io::stderr(),
        &repo_root,
        force,
        &liveness,
        origin_for_cleanup,
        &mut DaemonFarewellLedger,
    )
    .map(|_| ())
}

/// [`cleanup_performers`] の本体（daemon から取る事実は注入、I/O 境界を外に出した形）。
///
/// `liveness` を引数で受けるのは、**「稼働状況が不明なら見送らない」をテストで固定する**ため
/// （daemon を立てずに `Unknown` を注入できる）。
///
/// `resolve_origin` が値ではなく関数なのは**順序が意味を持つ**から: 稼働状況が不明なら
/// 保留して抜けるので、その先の起点照会（daemon への 2 度目の ask）まで行ってはいけない。
/// `ledger` も同じ理由で注入する — 保留したなら**帳簿にも触らない**（事実が無い状態を
/// 履歴に残さない）ことを、spy でテストから見る。
///
/// `out` を注入するのは、滞留の注記が**実際に出力に出ること**をテストで見るため。
/// 帳簿への書き込みだけをテストすると「読み手のない書き込み」に戻る（doc 44 §8.5）。
fn cleanup_performers_with(
    out: &mut dyn std::io::Write,
    repo_root: &Path,
    force: bool,
    liveness: &crate::host::liveness::Liveness,
    resolve_origin: impl FnOnce(&Path) -> String,
    ledger: &mut dyn FarewellLedger,
) -> Result<CleanupOutcome, String> {
    use crate::host::farewell::FarewellVerdict;

    // 稼働状況が確認できないなら**判定にすら進まない**。
    //
    // 不明 = 「稼働 lane が無い」ではないので、空として扱うと daemon が落ちている時にだけ
    // 稼働中 lane の保護が消える（一番危ない条件で guard が外れる形）。判定を出して
    // 「削除可能」と表示するのも嘘になりうるので、survey ごと止めて人に告げる。
    //
    // `--force` でも通さない: `--force` は「判定結果を実行する」意思表示であって
    // 「事実が無くてよい」ではない（1 flag に 2 仕事を兼ねさせない）。
    let running = match liveness.lanes_for_survey() {
        Ok(lanes) => lanes,
        Err(reason) => {
            let _ = writeln!(
                out,
                "稼働状況を確認できないため見送りを保留しました（1 件も削除していません）。"
            );
            let _ = writeln!(out, "  理由: {reason}");
            let _ = writeln!(
                out,
                "  daemon を起動してから再実行してください（`vp daemon status` / `vp daemon start`）。"
            );
            return Ok(CleanupOutcome::Held);
        }
    };

    let origin = resolve_origin(repo_root);
    let reports = crate::host::farewell::survey_repo(repo_root, running, &origin);
    if reports.is_empty() {
        let _ = writeln!(out, "クリーンアップ対象はありません。");
        return Ok(CleanupOutcome::Nothing);
    }

    // 帳簿の key（安定 id）は **lane を消す前に**解決する — 削除すると id の state file が
    // 消えるため、後から引くと別 id が生えて記録が「知らない lane」になる。
    let observations: Vec<crate::host::ledger::FarewellObservation> = reports
        .iter()
        .map(|r| crate::host::ledger::FarewellObservation {
            lane_id: lane_stable_id(repo_root, &r.facts.name),
            lane_name: r.facts.name.clone(),
            verdict: r.verdict.clone(),
        })
        .collect();
    // 判定を帳簿に反映し、反映後の滞留（何回目 / 初回いつ）を受け取る。
    // key は id なので、rename されていても同じ滞留に繋がる。
    let pending: std::collections::HashMap<String, crate::host::ledger::FarewellEntry> = ledger
        .observe(repo_root, &observations)
        .into_iter()
        .map(|e| (e.lane_id.clone(), e))
        .collect();

    let mut to_remove: Vec<(
        &crate::host::farewell::FarewellReport,
        &crate::host::ledger::FarewellObservation,
    )> = Vec::new();
    let mut ask_human = 0usize;
    for (r, obs) in reports.iter().zip(observations.iter()) {
        match &r.verdict {
            FarewellVerdict::Reclaim { reason } => {
                let _ = writeln!(out, "  削除可能: {} ({})", r.facts.name, reason);
                to_remove.push((r, obs));
            }
            FarewellVerdict::AskHuman { reason } => {
                // 滞留の注記は帳簿から。lane 名は survey が持つ**生きた名前**を出す
                // （帳簿の名前は記録時点のスナップショットなので、rename 後は古い）。
                let note = pending
                    .get(&obs.lane_id)
                    .and_then(crate::host::ledger::stagnation_note)
                    .map(|n| format!(" — {n}"))
                    .unwrap_or_default();
                let _ = writeln!(out, "  ⚠️ 要判断: {} ({reason}){note}", r.facts.name);
                ask_human += 1;
            }
            FarewellVerdict::Keep { reason } => {
                let _ = writeln!(out, "  保持: {} ({})", r.facts.name, reason);
            }
        }
    }

    if to_remove.is_empty() {
        let _ = writeln!(out, "\n自動で削除できる performer はありません。");
        if ask_human > 0 {
            let _ = writeln!(
                out,
                "{ask_human} 件は事実だけで判断できないため、人の確認が要ります。"
            );
        }
        return Ok(CleanupOutcome::Surveyed { reclaimable: 0 });
    }

    if !force {
        let _ = writeln!(
            out,
            "\n実際に削除するには `vp lane cleanup --force` を実行してください。"
        );
        if ask_human > 0 {
            let _ = writeln!(out, "（⚠️ の {ask_human} 件は --force でも削除しません）");
        }
        return Ok(CleanupOutcome::Surveyed {
            reclaimable: to_remove.len(),
        });
    }

    let mut reclaimed: Vec<crate::host::ledger::FarewellObservation> = Vec::new();
    for (r, obs) in &to_remove {
        let path = config::repo_lanes_dir(repo_root).join(&r.facts.name);
        remove_performer_workspace(repo_root, &path)?;
        clear_lane_state_files(repo_root, &r.facts.name);
        // worktree: merged branch を共有 .git から `-d` で安全に掃除 (設計 E)。
        // clone: branch は独立 .git 内なので親 repo では no-op (失敗は握り潰す)。
        //
        // ⚠️ branch 名は **facts（削除前に収集済）から取る**。ここで `get_branch(&path)` を
        // 呼び直してはいけない — `remove_performer_workspace` は worktree ディレクトリごと
        // 消すため cwd が存在せず、`git` の起動自体が Err になって常に None に落ちる
        // （P3 初版がこれで `branch -d` を never-fire にしていた）。
        if let Some(b) = r.facts.branch.as_deref() {
            let _ = run_git_in(repo_root, &["branch", "-d", b]);
        }
        let _ = writeln!(out, "  削除: {}", r.facts.name);
        reclaimed.push((*obs).clone());
    }

    // 「いつ何を見送ったか」は消した後では survey で復元できない = 帳簿に残す唯一の事実。
    // 削除の**後**に記録するのは、判定ではなく**実行**を書いているから（実行に失敗した
    // lane を「見送った」と書かない — 失敗は上で `?` 抜けする）。
    ledger.reclaimed(repo_root, &reclaimed);

    let _ = writeln!(out, "{} パフォーマーを削除しました。", to_remove.len());
    if ask_human > 0 {
        let _ = writeln!(out, "⚠️ {ask_human} 件は人の確認待ちのため残しました。");
    }
    Ok(CleanupOutcome::Removed {
        count: to_remove.len(),
    })
}

/// `vp lane history` — Repo Host の帳簿（見送りの記録）を読む（doc 44 §7.5）。
///
/// board UI を待たずに帳簿の**読み手**を用意するための面。書いた事実に読み手が無いと、
/// `LaneId` が 2 年間そうだったように「誰も見ない書き込み」になる（doc 44 §8.2）。
pub fn show_farewell_history(limit: usize) -> Result<(), String> {
    let repo_root = config::find_repo_root().map_err(|_| "git repo の中で実行してください")?;
    let path = repo_root
        .to_str()
        .ok_or_else(|| "repo path に invalid UTF-8".to_string())?;
    let entries = crate::daemon_client::farewell_log_blocking(path, limit).ok_or_else(|| {
        "帳簿は daemon が専有しているため、daemon 稼働中のみ読めます（`vp daemon start`）"
            .to_string()
    })?;
    if entries.is_empty() {
        eprintln!("見送りの記録はまだありません（`vp lane cleanup` を走らせると記録されます）。");
        return Ok(());
    }
    for entry in &entries {
        println!("{}", crate::host::ledger::format_history_line(entry));
    }
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

// doc 44 P3: `classify_performer_for_cleanup` は撤去（Repo Host に移管）。
//
// 収集（git subprocess）・判定・分類が 1 関数に混ざっており、テスト不能かつ判定が 2 値だった。
// 後継は `host::farewell` の 3 層構成:
//   collect_facts（actions） → judge_farewell（純関数・全分岐テスト済） → survey_repo（集約）
//
// 旧実装が抱えていた実質的な欠陥: **merged なら未コミット変更を見ずに削除候補**へ入れていた
// （= 取り込み済み branch 上に残った作業を黙って捨てうる）。Host 版は dirty を merged より
// 先に見て `AskHuman` に回す。

/// `<repo>/.vp/lanes/<name>` の performer dir を返す。 dir 不在なら None。
///
/// repo-local lane refactor PR 4b: PR 1 で導入した `find_performer_dir_dual` の legacy
/// global path fallback (= step 2/3) を削除し、 repo-local 一本に simplify。
/// performer_path / remove_performer / remove_performer_in が共有。
fn find_performer_dir(repo_root: &Path, name: &str) -> Option<PathBuf> {
    let dir = config::repo_lanes_dir(repo_root).join(name);
    // dep symlink は performer ではない (delete が dep を対象に取るのを防ぐ、defense-in-depth の壁 1)。
    // `symlink_metadata` は symlink を辿らないので、symlink を弾いた上で実 dir のみ Some。
    match fs::symlink_metadata(&dir) {
        Ok(md) if !md.file_type().is_symlink() && md.is_dir() => Some(dir),
        _ => None,
    }
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

pub(crate) fn run_git_in(dir: &std::path::Path, args: &[&str]) -> Result<(), String> {
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

pub(crate) fn count_changes(dir: &std::path::Path) -> usize {
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

/// Check if HEAD in the performer dir is merged into origin/<default_branch> (ancestry sense)。
///
/// "merged" と判定するのは:
///   1. HEAD が origin/<default> の ancestor (`merge-base --is-ancestor`)、 かつ
///   2. divergence あり (HEAD != origin/<default>、 = fresh performer の誤判定防止)
///
/// merge-commit / fast-forward merge を検出する。 **squash merge は元 commit を歴史に残さない
/// ため ancestry では false** になる — squash / rebase merge は [`is_branch_squash_merged`] で別途判定。
///
/// `default_branch`: repo の default branch 名 (`resolve_default_branch` 由来、 例 "nightly")。
/// 旧実装の `origin/main`/`origin/master` ハードコードを廃し、 nightly 等の非 main trunk に対応
/// (co-evolution #3)。 origin/<default> が解決不能なら keep 安全側 (false)。
pub(crate) fn is_branch_merged(performer_dir: &std::path::Path, default_branch: &str) -> bool {
    let remote_ref = format!("origin/{default_branch}");
    let remote_sha = git_rev_parse(performer_dir, &remote_ref);
    if remote_sha.is_none() {
        return false;
    }
    // fresh (未 divergence): HEAD == origin/<default> → keep
    if git_rev_parse(performer_dir, "HEAD") == remote_sha {
        return false;
    }
    git_is_ancestor(performer_dir, "HEAD", &remote_ref)
}

/// Check if the performer's branch was squash/rebase-merged into origin/<default_branch>。
///
/// squash merge は元 commit を歴史に残さないため [`is_branch_merged`] (ancestry) では false。
/// 別経路で「branch の内容が既に取り込まれたか」を判定する (co-evolution #3):
///   1. **gh PR state (内容照合付き)**: この branch を head とする merged PR の head commit
///      (`headRefOid`) を取り、 HEAD がその commit に**含まれる** (HEAD がその ancestor) 場合のみ
///      merged 扱い。 ⚠️ 名前一致だけだと `mako/{slug}` 規約で同名 branch を再利用 (同じ課題を
///      再着手) した時に、 過去の merged PR を拾って未 merge の新規 work を `--force` 削除しうる
///      (moody 指摘 #1)。 commit ancestry で確認して「HEAD が merged tip より進んでいる = 新規
///      work あり」なら keep に倒す。
///   2. **git cherry fallback**: branch の全 commit が upstream に patch-equivalent ('-') なら merged。
///      rebase merge / 単一 commit squash を拾う (複数 commit squash は組合せ patch-id が個別と
///      一致せず取りこぼすため gh が主、 cherry は gh 不在 / headRefOid 照合不能時の補助)。 cherry も
///      内容ベースなので、 名前一致だけの誤判定は起きない。
pub(crate) fn is_branch_squash_merged(performer_dir: &Path, default_branch: &str) -> bool {
    if let Some(branch) = get_branch(performer_dir)
        && let Some(head_oid) = gh_merged_pr_head_oid(performer_dir, &branch)
        && head_contained_in_merged_commit(performer_dir, &head_oid) == Some(true)
    {
        return true;
    }
    // gh 不在 / 非 GitHub / merged PR なし / headRefOid が local 不在 / HEAD が merged tip より
    // 進んでいる (新規 work) → git cherry の patch-equivalent 判定 (内容ベース) に委ねる。
    all_commits_patch_equivalent(performer_dir, &format!("origin/{default_branch}"))
}

/// `git merge-base --is-ancestor <a> <b>` (a が b の祖先か)。
fn git_is_ancestor(dir: &Path, a: &str, b: &str) -> bool {
    Command::new("git")
        .args(["merge-base", "--is-ancestor", a, b])
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// この branch を head とする **merged** PR の head commit SHA を返す (gh CLI)。
/// squash merge 後は remote branch が消えていても PR は merged state で残るため
/// `gh pr list --head <branch> --state merged` で引ける。 `headRefOid` は squash 前の
/// branch tip commit。 gh 不在 / 非 GitHub / 未認証 / merged PR なしは None。
///
/// 出力は `-q .[0].headRefOid` で SHA 1 個に絞り、 hex のみを受理する (空配列時の "null" や
/// 予期せぬ出力を弾く = spoof 耐性)。
fn gh_merged_pr_head_oid(dir: &Path, branch: &str) -> Option<String> {
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "merged",
            "--limit",
            "1",
            "--json",
            "headRefOid",
            "-q",
            ".[0].headRefOid",
        ])
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let oid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // SHA は hex のみ。 空 / "null" / 予期せぬ出力は None。
    (!oid.is_empty() && oid.chars().all(|c| c.is_ascii_hexdigit())).then_some(oid)
}

/// gh が返した merged PR の head commit が現在の HEAD を含む (HEAD がその ancestor) か。
///
/// - `Some(true)`: HEAD の作業は merged commit に含まれる (HEAD == merged tip、 または HEAD が
///   より古い) → merged。
/// - `Some(false)`: HEAD が merged tip より進んでいる / 分岐 → **未 merge の新規 work あり** → keep。
/// - `None`: head commit が local に無い → 照合不能 (cherry fallback に委ねる)。
fn head_contained_in_merged_commit(dir: &Path, head_oid: &str) -> Option<bool> {
    // `git rev-parse` は 40 桁 hex を実在確認せずそのまま返すので、 `cat-file -e` で commit
    // object の実在を確認する (存在しない oid で誤って ancestry 判定に進まない)。
    let exists = Command::new("git")
        .args(["cat-file", "-e", &format!("{head_oid}^{{commit}}")])
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !exists {
        return None; // commit が local に無い → 内容照合不能
    }
    Some(git_is_ancestor(dir, "HEAD", head_oid))
}

/// branch の全 commit が origin/<default> に patch-equivalent か (`git cherry <upstream> HEAD`)。
///
/// 出力の全行が '-' (upstream に等価 commit あり) かつ 1 行以上なら true。 単一 commit squash /
/// rebase merge を拾う。 commit 無し (空出力) は false (ancestry 側で処理済のはず)。
fn all_commits_patch_equivalent(dir: &Path, remote_ref: &str) -> bool {
    let output = Command::new("git")
        .args(["cherry", remote_ref, "HEAD"])
        .current_dir(dir)
        .output()
        .ok();
    match output {
        Some(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout);
            let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
            !lines.is_empty() && lines.iter().all(|l| l.starts_with('-'))
        }
        _ => false,
    }
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

pub(crate) fn get_branch(dir: &std::path::Path) -> Option<String> {
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

// ── Phase 5-D D1: Performer status (struct 返却、 repo API exposure 用) ───────────

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
    /// default branch (origin/HEAD) に ancestry merge 済みで cleanup 候補か。
    /// sidebar hot path のため squash merge は含まない (`vp lane cleanup` は squash も検出)。
    pub is_merged: bool,
}

/// Performer workspace dir から status snapshot を取得 (repo API 用)。 git 関連 subprocess を
/// 5-7 個並列に呼ぶので 1 回 ~50-100ms 程度。 多数 performer 時は repo 側で並列化検討。
pub fn performer_status(dir: &Path) -> PerformerStatus {
    let branch = get_branch(dir);
    let dirty_count = count_changes(dir);
    let (ahead, behind, has_upstream) = get_ahead_behind_counts(dir);
    let last_commit = get_last_commit(dir);
    // sidebar hot path: default branch は解決するが (nightly 対応)、 squash 判定 (gh) は
    // 引かない (per-refresh の gh network call を避ける)。 squash-merged lane は sidebar 上
    // 未マージ表示になるが、 明示的な `vp lane cleanup` は squash も検出する (co-evolution #3)。
    let default_branch = resolve_default_branch(dir).unwrap_or_else(|| "main".to_string());
    let is_merged = is_branch_merged(dir, &default_branch);
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

    /// 回帰固定（doc 44 §7.5）: **稼働状況が確認できないときは 1 件も見送らない**。
    ///
    /// 「不明」を空リストに畳むと、daemon が落ちている時にだけ稼働中 lane の保護が消える
    /// （= 一番危ない条件で guard が外れる）。`--force` でも通さない — `--force` は
    /// 「判定結果を実行する」意思であって「事実が無くてよい」ではない。
    ///
    /// 保留が**無条件ではない**ことも同時に見る（Known なら判定まで進む）。片方だけだと
    /// 「常に保留」= 見送り機能が死んだ状態も緑になる。
    ///
    /// doc 44 §7.5（帳簿）: **保留したら帳簿にも 1 文字も書かない**ことを同時に固定する。
    /// 事実が無い状態（稼働状況を確認できていない）を履歴に残すと、後から見た人が
    /// 「その日は判断待ちが 0 件だった」と読んでしまう。
    #[test]
    fn cleanup_holds_when_liveness_is_unknown() {
        use crate::host::liveness::Liveness;

        let root = std::env::temp_dir().join(format!("vp-cleanup-hold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let lane_dir = config::repo_lanes_dir(&root).join("w1");
        std::fs::create_dir_all(&lane_dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&lane_dir)
                .output()
                .expect("git 実行");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(lane_dir.join("a.txt"), "one").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);

        // 起点照会は daemon を叩くので注入する（保留経路では呼ばれないこと自体も要件）。
        let origin = |_: &Path| crate::repo::lanes_state::ROOT_LANE_NAME.to_string();
        let mut ledger = SpyLedger::default();
        let mut out = Vec::new();

        // 不明 + --force でも保留（判定にも進まない）
        let held = cleanup_performers_with(
            &mut out,
            &root,
            true,
            &Liveness::Unknown("Daemon 不達".to_string()),
            |_| panic!("保留するなら起点照会まで進んではいけない"),
            &mut ledger,
        )
        .expect("保留は Err ではない");
        assert_eq!(held, CleanupOutcome::Held);
        assert!(lane_dir.exists(), "保留中に lane を消してはいけない");
        assert!(
            ledger.observed.is_empty() && ledger.reclaimed.is_empty(),
            "保留したら帳簿に書かない（事実が無い状態を履歴に残さない）: {ledger:?}"
        );

        // 稼働 lane 0 件は「答え」なので判定に進む（保留は無条件ではない）
        let surveyed = cleanup_performers_with(
            &mut out,
            &root,
            false,
            &Liveness::Known(Vec::new()),
            origin,
            &mut ledger,
        )
        .expect("判定は Err ではない");
        assert!(
            !matches!(surveyed, CleanupOutcome::Held),
            "0 件は不明ではない: {surveyed:?}"
        );
        assert!(lane_dir.exists(), "dry-run では消えない");
        assert_eq!(
            ledger.observed.len(),
            1,
            "判定まで進んだら観測は帳簿へ送られる（= 保留の 0 件が『書けない』ではないことの裏取り）"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 帳簿の spy（daemon を立てずに「何を書いたか / 書かなかったか」を見る）。
    #[derive(Debug, Default)]
    struct SpyLedger {
        observed: Vec<Vec<crate::host::ledger::FarewellObservation>>,
        reclaimed: Vec<Vec<crate::host::ledger::FarewellObservation>>,
        /// `observe` が返す滞留（daemon が持っている体の帳簿）
        pending: Vec<crate::host::ledger::FarewellEntry>,
    }

    impl FarewellLedger for SpyLedger {
        fn observe(
            &mut self,
            _repo_root: &Path,
            observations: &[crate::host::ledger::FarewellObservation],
        ) -> Vec<crate::host::ledger::FarewellEntry> {
            self.observed.push(observations.to_vec());
            self.pending.clone()
        }

        fn reclaimed(
            &mut self,
            _repo_root: &Path,
            entries: &[crate::host::ledger::FarewellObservation],
        ) {
            self.reclaimed.push(entries.to_vec());
        }
    }

    /// 回帰固定（doc 44 §7.5）: **滞留が `vp lane cleanup` の出力に実際に出る**。
    ///
    /// 帳簿は「書いたら読まれる」ことまで含めて 1 本。書き込みだけをテストすると、
    /// `LaneId` が 2 年間そうだったように**読み手のない書き込み**になる（§8.2）。
    /// ここでは要判断の lane に対し、帳簿が返した滞留が行に載ることを見る。
    ///
    /// 表示する lane 名は survey が持つ**生きた名前**で、帳簿の（記録時点の）名前ではない
    /// ことも同時に固定する — 帳簿の名前を出すと rename 後に古い名前が現在の一覧に出る。
    #[test]
    fn cleanup_output_carries_stagnation_from_ledger() {
        use crate::host::liveness::Liveness;

        let _state = crate::test_env::state_dir(); // lane_id state file を tempdir に隔離

        let root = std::env::temp_dir().join(format!("vp-cleanup-stag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let lane_dir = config::repo_lanes_dir(&root).join("w1");
        std::fs::create_dir_all(&lane_dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&lane_dir)
                .output()
                .expect("git 実行");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(lane_dir.join("a.txt"), "one").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        // 未コミットの変更 = AskHuman（滞留の対象）
        std::fs::write(lane_dir.join("a.txt"), "two").unwrap();

        // 帳簿は id で引く。CLI と同じ derivation で id を作って spy に積む。
        let lane_id = lane_stable_id(&root, "w1");
        let mut ledger = SpyLedger {
            pending: vec![crate::host::ledger::FarewellEntry {
                lane_id: lane_id.clone(),
                lane_name: "古い名前".to_string(),
                kind: crate::host::ledger::FarewellKind::Pending,
                reason: "未コミットの変更".to_string(),
                streak: 3,
                first_seen_at: "2026-07-15T00:00:00+00:00".to_string(),
                last_seen_at: "2026-07-21T00:00:00+00:00".to_string(),
                ongoing: true,
            }],
            ..Default::default()
        };

        let mut out = Vec::new();
        cleanup_performers_with(
            &mut out,
            &root,
            false,
            &Liveness::Known(Vec::new()),
            |_| crate::repo::lanes_state::ROOT_LANE_NAME.to_string(),
            &mut ledger,
        )
        .expect("判定は Err ではない");

        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.contains("⚠️ 要判断: w1"), "要判断行が出る: {text}");
        assert!(
            text.contains("3 回連続、初回 2026-07-15"),
            "帳簿の滞留が出力に出る（読み手が効いている）: {text}"
        );
        assert!(
            !text.contains("古い名前"),
            "表示名は survey の生きた名前（帳簿のスナップショットではない）: {text}"
        );

        // 観測は帳簿へ送られ、key は安定 id（名前ではない）
        let sent = ledger.observed.first().expect("観測が送られる");
        let w1 = sent
            .iter()
            .find(|o| o.lane_name == "w1")
            .expect("w1 の観測");
        assert_eq!(w1.lane_id, lane_id, "帳簿の key は安定 id");
        assert!(!w1.lane_id.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 回帰固定: **同名 lane を作り直すと安定 id が変わる**（帳簿が混ざらない仕組みの土台）。
    ///
    /// `clear_lane_state_in` が `lane_ids` file を消すので、作り直した lane は新しい id を
    /// 名乗る。これが崩れると、見送った lane の履歴を同名の新 lane が引き継ぐ
    /// （= 作ったばかりの lane が「3 回連続で判断待ち」と表示される）。
    #[test]
    fn recreated_lane_gets_a_fresh_stable_id() {
        let _state = crate::test_env::state_dir();
        let root = std::env::temp_dir().join(format!("vp-cleanup-freshid-{}", std::process::id()));

        let first = lane_stable_id(&root, "w1");
        assert_eq!(lane_stable_id(&root, "w1"), first, "同じ lane は同じ id");

        clear_lane_state_files(&root, "w1"); // = 見送り（lane 削除）の後始末
        let second = lane_stable_id(&root, "w1");
        assert_ne!(
            first, second,
            "同名で作り直した lane は別 id（前の履歴と混ざらない）"
        );
    }

    #[test]
    fn clear_lane_state_files_uses_repo_basename_key() {
        // キー凍結: repo = repo_root の basename (repo 書き手の derivation と一致、
        // create_performer_orchestrated 等参照)。ズレると GC が空振りして leak が再発する。
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        let repo_root = tmp.path().join("parent").join("vp");
        // doc 47 §4: Mode も session registry（root の mode）に入った。GC が registry file ごと
        // 消すので、Mode の記録も一緒に終端する。
        crate::lane::session_registry::set_root_mode_in(
            base,
            "vp",
            "feat",
            "claude",
            crate::lane::session_registry::SessionMode::Gui,
        )
        .expect("record mode");
        // doc 40 PR-2: 会話 id は session registry が SSOT。GC が registry file を消すことを固定する。
        crate::lane::session_registry::set_conversation_in(
            base,
            "vp",
            "feat",
            "claude",
            1,
            Some("sess-1"),
        )
        .expect("record session conversation");

        clear_lane_state_files_in(base, &repo_root, "feat");

        assert_eq!(
            crate::lane::session_registry::root_mode_in(base, "vp", "feat"),
            crate::lane::session_registry::SessionMode::Tui,
            "registry file が消え、Mode も既定（Tui）に戻る"
        );
        assert_eq!(
            crate::lane::session_registry::load_in(base, "vp", "feat", "claude").sessions[0]
                .conversation,
            None,
            "registry file が消え、既定形（会話 id なし）に戻る"
        );
        // 未記録 lane / 二重呼び出しでも panic しない (best-effort 冪等)
        clear_lane_state_files_in(base, &repo_root, "feat");
    }

    /// 一元 GC の凍結: lane 削除後、当該 lane の **全 6 種**の lane-scoped state file が消える。
    /// replay_log は session label 単位 (#1 + #2) で消し、 他 lane の state は巻き添えにしない。
    /// 従来 repo 経路から漏れていた replay_log / terminal_replay / lane_id の欠落再発を防ぐ回帰。
    ///
    /// doc 47 §4 で console_mode file は退役し、Mode は registry の中（root の mode）に入った —
    /// 破棄対象が 7 種から 6 種に減ったのは leak が増えたのではなく、state が 1 つ畳まれたため。
    #[test]
    fn clear_lane_state_removes_all_six_state_files_and_is_scoped() {
        use crate::conversation::{ConversationEvent, replay_log};
        use crate::daemon::pty_slot;
        use crate::lane::{agent_store, lane_id, session_registry};

        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();

        // 対象 lane (vp/feat) と巻き添え確認用の別 lane (vp/other) に同じ state 群を積む helper。
        let seed = |lane: &str| {
            // ① session_registry: Mode(root) + 会話 id + #2 session (label 列挙の対象を作る)
            session_registry::set_root_mode_in(
                base,
                "vp",
                lane,
                "claude",
                session_registry::SessionMode::Gui,
            )
            .expect("registry mode");
            session_registry::set_conversation_in(base, "vp", lane, "claude", 1, Some("sess-1"))
                .expect("registry #1");
            session_registry::create_in(
                base,
                "vp",
                lane,
                "claude",
                "codex",
                session_registry::SessionMode::Gui,
                true,
            )
            .expect("registry #2");
            // ② model は registry（SessionEntry.model）に同居（engine_model file は退役済）
            session_registry::set_model_in(base, "vp", lane, "claude", 1, Some("sonnet"))
                .expect("session model");
            // ③ agent
            agent_store::record_in(base, "vp", lane, "codex").expect("agent");
            // ④ conversation_replay: #1 (素の lane 名) と #2 (`<lane>#2`) の両 label に 1 行ずつ
            let ev = ConversationEvent::MessageChunk {
                text: "hi".to_string(),
            };
            replay_log::append_in(base, "vp", lane, &ev).expect("replay #1");
            let label2 = session_registry::session_label(lane, 2);
            replay_log::append_in(base, "vp", &label2, &ev).expect("replay #2");
            // ⑤ terminal_replay: writer は flush task 経由なので path に直書きで模擬
            let rp = pty_slot::replay_file_path_in(base, "vp", lane);
            std::fs::create_dir_all(rp.parent().unwrap()).unwrap();
            std::fs::write(&rp, b"scrollback").unwrap();
            // ⑤b terminal_replay の非 root session file（doc 50 §4.6 A6 — session file も掃除対象）
            let rp2 = pty_slot::replay_file_path_session_in(base, "vp", lane, 2);
            std::fs::write(&rp2, b"scrollback-2").unwrap();
            // ⑥ lane_id
            lane_id::load_or_create_in(base, "vp", lane);
        };
        seed("feat");
        seed("other");

        clear_lane_state_in(base, "vp", "feat");

        // 対象 lane: 全 6 種が消えている。
        let reg = session_registry::load_in(base, "vp", "feat", "claude");
        assert_eq!(reg.sessions.len(), 1, "①registry が既定形 N=1 に戻る");
        assert_eq!(reg.sessions[0].conversation, None, "①会話 id も消える");
        assert_eq!(
            reg.sessions[0].model, None,
            "②model も消える（registry 同居）"
        );
        assert_eq!(
            session_registry::root_mode_in(base, "vp", "feat"),
            session_registry::SessionMode::Tui,
            "①Act も既定 (Tui) に戻る"
        );
        assert_eq!(agent_store::last_in(base, "vp", "feat"), None, "③agent");
        assert!(
            replay_log::load_in(base, "vp", "feat").is_empty(),
            "④replay_log #1"
        );
        assert!(
            replay_log::load_in(base, "vp", "feat#2").is_empty(),
            "④replay_log #2 (label 単位で消える)"
        );
        assert!(
            !pty_slot::replay_file_path_in(base, "vp", "feat").exists(),
            "⑤terminal_replay (root)"
        );
        assert!(
            !pty_slot::replay_file_path_session_in(base, "vp", "feat", 2).exists(),
            "⑤b terminal_replay (非 root session file も消える)"
        );
        assert!(
            !lane_id::id_file_in(base, "vp", "feat").exists(),
            "⑥lane_id"
        );

        // 別 lane (vp/other) は巻き添えにならない (scoped)。
        assert_eq!(
            session_registry::root_mode_in(base, "vp", "other"),
            session_registry::SessionMode::Gui,
            "別 lane の Mode は残る"
        );
        assert_eq!(
            agent_store::last_in(base, "vp", "other").as_deref(),
            Some("codex")
        );
        assert!(
            !replay_log::load_in(base, "vp", "other").is_empty(),
            "別 lane の replay_log は残る"
        );
        assert!(pty_slot::replay_file_path_in(base, "vp", "other").exists());
        assert!(
            pty_slot::replay_file_path_session_in(base, "vp", "other", 2).exists(),
            "別 lane の session file は残る（誤爆しない）"
        );
        assert!(lane_id::id_file_in(base, "vp", "other").exists());

        // 未記録 lane / 二重呼び出しでも panic しない (best-effort 冪等)。
        clear_lane_state_in(base, "vp", "feat");
        clear_lane_state_in(base, "vp", "never-existed");
    }

    #[test]
    fn persist_lane_model_writes_initial_session_model_with_basename_key() {
        // co-evolution #1 → session 紐づけ（2026-07-27）: CLI `--model` は registry の初期
        // session（key=1）の model へ、repo basename を repo key として書く（key derivation は
        // clear と同一）。
        use crate::lane::session_registry;

        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();
        let repo_root = tmp.path().join("parent").join("vp");
        let model_of = |lane: &str| {
            session_registry::load_in(base, "vp", lane, "claude").sessions[0]
                .model
                .clone()
        };

        // None は no-op（未記録のまま = engine 既定）
        persist_lane_model_in(base, &repo_root, "feat", None).expect("None は Ok");
        assert_eq!(model_of("feat"), None);

        // 空白のみも no-op
        persist_lane_model_in(base, &repo_root, "feat", Some("  ")).expect("空白は Ok");
        assert_eq!(model_of("feat"), None);

        // 有効な model は初期 session（key=1）に basename key で書かれる
        persist_lane_model_in(base, &repo_root, "feat", Some("claude-fable-5")).expect("record");
        assert_eq!(
            model_of("feat").as_deref(),
            Some("claude-fable-5"),
            "repo spawn が読む repo=basename('vp') key の session 1 に書かれる"
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
            is_branch_merged(&performer_repo, "main"),
            "merged feature branch should return true"
        );

        let _ = fs::remove_dir_all(&base);
    }

    // --- find_performer_dir (repo-local lane refactor PR 4b: legacy global path 撤去) ---

    /// 共通 fixture: temp 領域に偽 repo + repo-local lane dir (.vp/lanes/) を作る。
    fn setup_pl_fixture(slug: &str) -> (PathBuf, PathBuf) {
        let repo = test_dir(&format!("pl-{slug}"));
        let pl = repo.join(".vp").join("lanes");
        fs::create_dir_all(&pl).unwrap();
        (repo, pl)
    }

    #[test]
    fn find_performer_dir_returns_repo_local() {
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

    // --- list_performers_for_repo (PR 4b: repo-local 一本) ---

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

    // --- list_performers_for_repo (PR 4b: repo-local 一本) ---

    #[test]
    fn list_performers_for_repo_lists_repo_local_only() {
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

    // --- dep symlink 隔離 (lane kind 分離): 「.vp/lanes/ 内の symlink ⟺ dep」不変条件 ---

    /// 実測 characterization: `std::fs::remove_dir_all(symlink→dir)` は **symlink 自体を
    /// unlink するだけで target とその中身は破壊しない** (rustc 1.96 / macOS で確認)。
    ///
    /// ただしこの挙動は std version / OS 依存で保証が弱い (conductor が「確信持てない」と保留した点)。
    /// よって [`remove_performer_workspace`] は std 挙動に依存せず明示 Err で止める設計にした
    /// (下の `remove_performer_workspace_refuses_symlink`)。本テストは万一 std が target 破壊に
    /// 退行したら赤で気付くための canary。
    #[cfg(unix)]
    #[test]
    fn remove_dir_all_on_symlink_preserves_target() {
        let base = test_dir("rmall-symlink");
        let target = base.join("target");
        fs::create_dir_all(&target).unwrap();
        let sentinel = target.join("SENTINEL");
        fs::write(&sentinel, b"alive").unwrap();
        let link = base.join("link");
        symlink(&target, &link).unwrap();

        let res = fs::remove_dir_all(&link);
        assert!(res.is_ok(), "remove_dir_all(symlink) は Ok: {res:?}");
        assert!(
            fs::symlink_metadata(&link).is_err(),
            "symlink 自体は unlink される"
        );
        assert!(target.is_dir(), "target dir は生存する");
        assert!(sentinel.exists(), "target 内の sentinel は破壊されない");

        let _ = fs::remove_dir_all(&base);
    }

    /// 列挙 (repo snapshot / sidebar / flow progress の choke point) が dep symlink を除外する。
    #[cfg(unix)]
    #[test]
    fn list_performers_for_repo_excludes_dep_symlink() {
        let (repo, pl) = setup_pl_fixture("list-dep");
        // 実 performer lane (worktree 相当、 .git を持つ)。
        fs::create_dir_all(pl.join("feat").join(".git")).unwrap();
        // dep target (sibling repo 相当、 .git dir を持つ) を repo 外に用意。
        let sibling = test_dir("list-dep-sibling");
        fs::create_dir_all(sibling.join(".git")).unwrap();
        // dep symlink: .vp/lanes/creoui -> sibling。
        symlink(&sibling, &pl.join("creoui")).unwrap();

        let listed: Vec<String> = list_performers_for_repo(&repo)
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(listed, vec!["feat"], "symlink (dep) は列挙されない");

        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&sibling);
    }

    /// delete lookup が dep symlink に None を返す (delete が dep を対象に取れない、壁 1)。
    #[cfg(unix)]
    #[test]
    fn find_performer_dir_returns_none_for_symlink() {
        let (repo, pl) = setup_pl_fixture("find-dep");
        let sibling = test_dir("find-dep-sibling");
        fs::create_dir_all(sibling.join(".git")).unwrap();
        symlink(&sibling, &pl.join("creoui")).unwrap();

        assert!(
            find_performer_dir(&repo, "creoui").is_none(),
            "dep symlink は performer として解決されない"
        );

        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&sibling);
    }

    /// 壁 2: symlink が渡っても remove_performer_workspace は remove_dir_all せず明示 Err、
    /// target とその中身は無傷。
    #[cfg(unix)]
    #[test]
    fn remove_performer_workspace_refuses_symlink() {
        let (repo, pl) = setup_pl_fixture("rmws-dep");
        let sibling = test_dir("rmws-dep-sibling");
        fs::create_dir_all(sibling.join(".git")).unwrap();
        let sentinel = sibling.join("SENTINEL");
        fs::write(&sentinel, b"alive").unwrap();
        let link = pl.join("creoui");
        symlink(&sibling, &link).unwrap();

        let err = remove_performer_workspace(&repo, &link).unwrap_err();
        assert!(
            err.contains("dependency symlink"),
            "symlink は明示 Err で拒否: {err}"
        );
        assert!(fs::symlink_metadata(&link).is_ok(), "symlink 自体は残る");
        assert!(sentinel.exists(), "target 内の sentinel は無傷");

        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&sibling);
    }

    #[test]
    fn is_branch_merged_returns_false_when_head_equals_origin_main() {
        let base = test_dir("merged-false-fresh");
        let (_, performer_repo) = setup_merged_performer_repos(&base);

        // performer に local commit なし（HEAD == origin/main）
        // false-positive ガード: is_branch_merged は false を返すべき
        assert!(
            !is_branch_merged(&performer_repo, "main"),
            "fresh performer (HEAD == origin/main) should return false"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn is_branch_squash_merged_detects_single_commit_squash() {
        // co-evolution #3: squash merge は ancestry では検出できないが、 git cherry
        // (patch-equivalent) で拾える。 gh は test 環境 (local bare remote) で不在扱いになり
        // cherry fallback が効く。
        let base = test_dir("squash-merged");
        let (main_repo, performer_repo) = setup_merged_performer_repos(&base);

        // performer: feature branch + 1 commit を push
        Cmd::new("git")
            .args(["checkout", "-b", "feat-squash"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();
        fs::write(performer_repo.join("sq.txt"), "squash me\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(&performer_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "squash target"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["push", "origin", "feat-squash"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();

        // main_repo: feat-squash を squash merge (新 commit = 元 commit は歴史に残らない)
        Cmd::new("git")
            .args(["fetch", "origin"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["merge", "--squash", "origin/feat-squash"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "squashed feat-squash (#1)"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["push", "origin", "main"])
            .current_dir(&main_repo)
            .output()
            .unwrap();

        // performer が fetch して origin/main に squash commit を取り込む
        Cmd::new("git")
            .args(["fetch", "origin"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();

        // ancestry では検出できない (旧実装の false negative)
        assert!(
            !is_branch_merged(&performer_repo, "main"),
            "squash merge は ancestry (is_branch_merged) では false"
        );
        // squash 検出経路 (git cherry fallback) が true を返す
        assert!(
            is_branch_squash_merged(&performer_repo, "main"),
            "squash merge は is_branch_squash_merged で検出されるべき"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn head_contained_in_merged_commit_protects_new_work() {
        // moody 指摘 #1: gh の branch 名一致だけだと同名 branch 再利用で未 merge work を
        // 誤削除しうる。 head commit の ancestry で内容照合し、 HEAD が merged tip より進んで
        // いれば keep に倒す — その content-safety を gh 非依存で検証する。
        let base = test_dir("merged-oid-guard");
        let (_, performer_repo) = setup_merged_performer_repos(&base);

        // commit A = 過去 PR の merged head 相当
        fs::write(performer_repo.join("a.txt"), "a\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(&performer_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "A (merged tip)"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();
        let a = git_rev_parse(&performer_repo, "HEAD").unwrap();

        // HEAD == A: merged tip そのもの → contained (merged 扱い OK)
        assert_eq!(
            head_contained_in_merged_commit(&performer_repo, &a),
            Some(true),
            "HEAD == merged tip は contained"
        );

        // commit B を A の上に積む (= 同名 branch 再利用の新規未 merge work 相当)
        fs::write(performer_repo.join("b.txt"), "b\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(&performer_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "B (new unmerged work)"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();

        // HEAD == B, merged tip == A: B は A より進んでいる → not contained → keep (誤削除防止)
        assert_eq!(
            head_contained_in_merged_commit(&performer_repo, &a),
            Some(false),
            "merged tip より進んだ HEAD は未 merge work あり → false (削除しない)"
        );

        // local に無い commit は照合不能 → None (cherry fallback に委ねる)
        assert_eq!(
            head_contained_in_merged_commit(
                &performer_repo,
                "0000000000000000000000000000000000000000"
            ),
            None,
            "未知 oid は照合不能 → None"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn is_branch_merged_respects_non_main_default() {
        // co-evolution #3: default が main でない (nightly) repo でも ancestry merge を検出、
        // かつ default branch を正しく尊重する (main には未 merge なので main 指定では false)。
        let base = test_dir("nightly-default");
        let (main_repo, performer_repo) = setup_merged_performer_repos(&base);

        // main_repo: main から nightly を派生して push (= dev trunk)
        Cmd::new("git")
            .args(["checkout", "-b", "nightly"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["push", "-u", "origin", "nightly"])
            .current_dir(&main_repo)
            .output()
            .unwrap();

        // performer: feature branch + commit を push
        Cmd::new("git")
            .args(["checkout", "-b", "feat-nl"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();
        fs::write(performer_repo.join("nl.txt"), "night\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(&performer_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "night work"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["push", "origin", "feat-nl"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();

        // main_repo: feat-nl を nightly に (通常) merge + 追加 commit で先に進める
        Cmd::new("git")
            .args(["fetch", "origin"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["merge", "origin/feat-nl", "--no-edit"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        fs::write(main_repo.join("nl-extra.txt"), "x\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "post"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["push", "origin", "nightly"])
            .current_dir(&main_repo)
            .output()
            .unwrap();

        Cmd::new("git")
            .args(["fetch", "origin"])
            .current_dir(&performer_repo)
            .output()
            .unwrap();

        // default=nightly では merged (ancestry)、 default=main では未 merge (default 尊重)
        assert!(
            is_branch_merged(&performer_repo, "nightly"),
            "nightly に merge 済 → default=nightly では true"
        );
        assert!(
            !is_branch_merged(&performer_repo, "main"),
            "main には未 merge → default=main では false (default branch を尊重)"
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
        let conductor = base.join("root");
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

    /// 回帰固定: **失敗する create は `.gitignore` を書かない**。
    ///
    /// `ensure_vp_gitignored` は repo を書き換える action なので、 provisioning（= lane の
    /// 実体が建ち、 `repo_root` が本物の repo だと git 操作が実証する）より**後**に
    /// 置かねばならない。 入口に置くと、 `repo_dir` 未設定で `repo_root` が process cwd に
    /// 落ちた時に無関係な dir を汚す — VP repo で `cargo test` するたび
    /// `crates/vantage-point/.gitignore` が湧いていた（2026-07-23 に
    /// `reservation_removed_after_failed_create` 経由と特定）。
    ///
    /// git repo ですらない dir を渡して provisioning を確実に失敗させ、 その dir に
    /// `.gitignore` が生まれないことを見る。
    #[test]
    fn failed_create_does_not_write_gitignore() {
        let tmp = test_dir("no-gitignore-on-failure");
        fs::create_dir_all(&tmp).unwrap();
        // git repo でないので worktree add は必ず失敗する
        let res = setup_performer("x", "mako/x", &tmp, false, Isolation::Worktree, None);
        assert!(res.is_err(), "git repo でない dir では create は失敗する");
        assert!(
            !tmp.join(".gitignore").exists(),
            "失敗した create が .gitignore を書いた（action が検証より前に走っている）"
        );
        let _ = fs::remove_dir_all(&tmp);
    }
}
