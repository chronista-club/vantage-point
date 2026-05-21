//! Project add / clone ダイアログ — native folder picker + git clone + TheWorld API。
//!
//! sidebar の「+ Add Project」「+ Clone Repository」操作の裏側。 rfd の folder picker
//! は blocking なので別スレッドで実行し、 結果を `AppEvent` で main thread に返す。
//!
//! VP-194 (app.rs module split、 doc 11 R-3): app.rs の event loop に同居していた
//! project ダイアログ群 (`resolve_default_project_root` / `spawn_add_project_picker` /
//! `spawn_clone_project` / `spawn_clone_path_picker` / `derive_repo_name`) を本 module に
//! 切り出した。 挙動は不変 (= 純粋な module 移動、 picker / API / event 送出は同一)。

use std::thread;

use tao::event_loop::EventLoopProxy;

use crate::client::TheWorldClient;
use crate::pane::SidebarState;
use crate::settings::Settings;
use crate::terminal::AppEvent;

/// Settings + 既存プロジェクトから picker の初期ディレクトリを解決。
///
/// 優先順位:
/// 1. `Settings.default_project_root` が指定されていて存在する → それ
/// 2. **既存登録プロジェクトの親ディレクトリ** (= "vp のレポジトリホーム" 推定)
///    `sidebar_state.processes` の最初の project の parent dir。多くは
///    `~/repos` か `C:\Users\<user>\repos` 等の repos 親。
/// 3. `~/repos` が存在する → それ
/// 4. `~` (home) → それ
/// 5. それ以外 → `None`
pub(crate) fn resolve_default_project_root(
    settings: &Settings,
    sidebar_state: &SidebarState,
) -> Option<std::path::PathBuf> {
    // 1. Settings explicit
    if let Some(s) = &settings.default_project_root {
        let p = std::path::PathBuf::from(s);
        if p.exists() {
            return Some(p);
        }
        tracing::warn!(
            "default_project_root が設定されているが存在しない: {} → 推定にフォールバック",
            s
        );
    }
    // 2. 既存 project の parent dir = "vp レポジトリホーム" 推定
    for proj in &sidebar_state.processes {
        let path = std::path::PathBuf::from(&proj.path);
        if let Some(parent) = path.parent()
            && parent.exists()
        {
            tracing::debug!(
                "default picker dir 推定: {} (project '{}' の parent)",
                parent.display(),
                proj.name
            );
            return Some(parent.to_path_buf());
        }
    }
    // 3. ~/repos fallback
    let home = dirs::home_dir()?;
    let repos = home.join("repos");
    if repos.exists() {
        Some(repos)
    } else {
        Some(home)
    }
}

/// VP-100 follow-up: 「+ Add Project」クリック時の native folder picker + API 呼出。
///
/// rfd の picker は blocking なので別スレッドで実行。folder 選択後:
/// 1. `client.add_project(name, path)` を呼ぶ (TheWorld の `/api/world/projects` POST)
/// 2. `client.start_process(name)` で SP を起動し、即「稼働中」にする (VP-203)
/// 3. `client.list_projects()` で再取得 → `AppEvent::ProcessesLoaded`
///
/// User キャンセル / API 失敗時は何もしない (sidebar は変化しない)。
/// `initial_dir` が `Some` なら picker の初期表示ディレクトリに設定。
pub(crate) fn spawn_add_project_picker(
    proxy: EventLoopProxy<AppEvent>,
    initial_dir: Option<std::path::PathBuf>,
) {
    let _ = thread::Builder::new()
        .name("add-project-picker".into())
        .spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title("プロジェクトフォルダを選択");
            if let Some(d) = initial_dir.as_ref() {
                dialog = dialog.set_directory(d);
            }
            let folder = match dialog.pick_folder() {
                Some(p) => p,
                None => {
                    tracing::debug!("process:add canceled by user");
                    return;
                }
            };
            let name = folder
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "project".to_string());
            let path = folder.to_string_lossy().into_owned();
            tracing::info!("process:add picker → name={} path={}", name, path);

            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("add-project tokio runtime 作成失敗: {}", e);
                    return;
                }
            };
            rt.block_on(async move {
                let client = TheWorldClient::default();
                if let Err(e) = client.add_project(&name, &path).await {
                    tracing::warn!("add_project API 失敗: {}", e);
                    return;
                }
                // VP-203: 登録後に SP を起動し即「稼働中」にする。起動に失敗しても
                // 登録自体は成功しているので、sidebar 反映 (list_projects) は続行する。
                if let Err(e) = client.start_process(&name).await {
                    tracing::warn!("add_project 後の start_process 失敗: {}", e);
                }
                tracing::info!("add_project + start_process 完了 → projects 再 fetch");
                match client.list_projects().await {
                    Ok(projects) => {
                        let _ = proxy.send_event(AppEvent::ProcessesLoaded(projects));
                    }
                    Err(e) => {
                        tracing::warn!("add_project 後の list_projects 失敗: {}", e);
                    }
                }
            });
        });
}

/// VP-100 follow-up: 「+ Clone Repository」クリック時の git clone + API 呼出。
///
/// 1. `git clone <url> <target>` を実行 (target は override 優先、無ければ
///    `<default_root>/<repo_name>`)
/// 2. 成功なら `add_project` で TheWorld に register
/// 3. `list_projects` で再取得 → `AppEvent::ProcessesLoaded`
///
/// `target_override` が `Some` ならそれを target とする (user が picker で選択した
/// folder)。`None` なら `default_root` + repo 名で auto 決定。後者で `default_root`
/// も `None` の場合は何もしない (default_project_root が解決できないケース)。
/// git バイナリが PATH に無い場合も spawn 失敗で終わる。
pub(crate) fn spawn_clone_project(
    proxy: EventLoopProxy<AppEvent>,
    url: String,
    default_root: Option<std::path::PathBuf>,
    target_override: Option<std::path::PathBuf>,
) {
    // Phase 2.x-a: #210 の target_override を取り込み + Phase 1 の `process:` prefix を維持。
    // priority: 1) explicit target_override (picker 選択 path)、 2) default_root + repo 名
    let target = if let Some(t) = target_override {
        t
    } else if let Some(root) = default_root {
        root.join(derive_repo_name(&url))
    } else {
        tracing::warn!("process:clone but default_project_root is unresolved (set in settings)");
        return;
    };
    let _ = thread::Builder::new()
        .name("clone-project".into())
        .spawn(move || {
            tracing::info!("git clone {} {}", url, target.display());
            let status = std::process::Command::new("git")
                .arg("clone")
                .arg(&url)
                .arg(&target)
                .status();
            let success = match status {
                Ok(s) if s.success() => true,
                Ok(s) => {
                    tracing::warn!("git clone failed: exit code {:?}", s.code());
                    false
                }
                Err(e) => {
                    tracing::warn!("git clone spawn 失敗 (git PATH 確認): {}", e);
                    false
                }
            };
            if !success {
                let _ = notify_rust::Notification::new()
                    .summary("Vantage Point")
                    .body(&format!("Clone 失敗: {}", url))
                    .show();
                return;
            }
            // Register — project 名は target folder の末尾セグメントから (override 時は
            // user が選んだ folder 名、default 時は repo 名と同一になる)
            let project_name = target
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| derive_repo_name(&url));
            let path_str = target.to_string_lossy().into_owned();
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("clone-project tokio runtime 失敗: {}", e);
                    return;
                }
            };
            rt.block_on(async move {
                let client = TheWorldClient::default();
                if let Err(e) = client.add_project(&project_name, &path_str).await {
                    tracing::warn!("clone 後の add_project 失敗: {}", e);
                    return;
                }
                tracing::info!("clone + add_project 成功 → projects 再 fetch");
                match client.list_projects().await {
                    Ok(projects) => {
                        let _ = proxy.send_event(AppEvent::ProcessesLoaded(projects));
                    }
                    Err(e) => {
                        tracing::warn!("list_projects 失敗: {}", e);
                    }
                }
            });
        });
}

/// Clone 先 folder picker。選択結果 (キャンセル時は None) を `AppEvent::ClonePathPicked`
/// で main thread に送り、sidebar JS の `window.setClonePath()` に push する。
///
/// `initial_dir` が `Some` なら picker の初期表示ディレクトリに設定。
pub(crate) fn spawn_clone_path_picker(
    proxy: EventLoopProxy<AppEvent>,
    initial_dir: Option<std::path::PathBuf>,
) {
    let _ = thread::Builder::new()
        .name("clone-path-picker".into())
        .spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title("Clone 先フォルダを選択");
            if let Some(d) = initial_dir.as_ref() {
                dialog = dialog.set_directory(d);
            }
            let folder = dialog.pick_folder();
            let payload = folder.map(|p| p.to_string_lossy().into_owned());
            let _ = proxy.send_event(AppEvent::ClonePathPicked(payload));
        });
}

/// URL から repo 名を推定する (`/` or `:` の最後の segment、`.git` 末尾を除去)
///
/// 例:
/// - `https://github.com/user/repo.git` → `repo`
/// - `git@github.com:user/repo.git` → `repo`
/// - `https://gitlab.com/group/sub/repo` → `repo`
fn derive_repo_name(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let last = trimmed
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("project")
        .trim_end_matches(".git");
    if last.is_empty() {
        "project".to_string()
    } else {
        last.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `derive_repo_name` の URL → repo 名 推定 (`.git` 除去 / `/` `:` 区切り)。
    #[test]
    fn derive_repo_name_variants() {
        assert_eq!(derive_repo_name("https://github.com/user/repo.git"), "repo");
        assert_eq!(derive_repo_name("git@github.com:user/repo.git"), "repo");
        assert_eq!(
            derive_repo_name("https://gitlab.com/group/sub/repo"),
            "repo"
        );
        assert_eq!(derive_repo_name("https://github.com/user/repo/"), "repo");
        // segment が空 / 異常系は "project" fallback
        assert_eq!(derive_repo_name(""), "project");
        assert_eq!(derive_repo_name(".git"), "project");
    }
}
