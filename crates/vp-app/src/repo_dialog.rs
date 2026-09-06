//! Repo add / clone ダイアログ — native folder picker + git clone + daemon control plane。
//!
//! sidebar の「+ Add Repo」「+ Clone Repository」操作の裏側。 rfd の folder picker
//! は blocking なので別スレッドで実行し、 結果を `AppEvent` で main thread に返す。
//!
//! VP-194 (app.rs module split、 doc 11 R-3): app.rs の event loop に同居していた
//! repo ダイアログ群 (`resolve_default_repo_root` / `spawn_add_repo_picker` /
//! `spawn_clone_repo` / `derive_repo_name`) を本 module に
//! 切り出した。 挙動は不変 (= 純粋な module 移動、 picker / API / event 送出は同一)。
//!
//! doc 45 段 3: daemon 呼び出しを HTTP から Unison (`daemon-control`) に差し替えた。
//! これに伴い picker thread 内の使い捨て tokio runtime を廃し、 async 部分は
//! app の shared runtime (`rt_handle`) に投げる — Unison の共有 QUIC connection は
//! そこで駆動されているので、 別 runtime から触ってはいけない。
//! blocking な picker / `git clone` は従来どおり専用 OS thread に残る。

use std::thread;

use tao::event_loop::EventLoopProxy;

use crate::app::SharedDaemonConn;
use crate::pane::SidebarState;
use crate::settings::Settings;
use crate::terminal::AppEvent;

/// Settings + 既存 repoから picker の初期ディレクトリを解決。
///
/// 優先順位:
/// 1. `Settings.default_repo_root` が指定されていて存在する → それ
/// 2. **既存登録 repoの親ディレクトリ** (= "vp のレポジトリホーム" 推定)
///    `sidebar_state.processes` の最初の repo の parent dir。多くは
///    `~/repos` か `C:\Users\<user>\repos` 等の repos 親。
/// 3. `~/repos` が存在する → それ
/// 4. `~` (home) → それ
/// 5. それ以外 → `None`
pub(crate) fn resolve_default_repo_root(
    settings: &Settings,
    sidebar_state: &SidebarState,
) -> Option<std::path::PathBuf> {
    // 1. Settings explicit
    if let Some(s) = &settings.default_repo_root {
        let p = std::path::PathBuf::from(s);
        if p.exists() {
            return Some(p);
        }
        tracing::warn!(
            "default_repo_root が設定されているが存在しない: {} → 推定にフォールバック",
            s
        );
    }
    // 2. 既存 repo の parent dir = "vp レポジトリホーム" 推定
    for proj in &sidebar_state.processes {
        let path = std::path::PathBuf::from(&proj.path);
        if let Some(parent) = path.parent()
            && parent.exists()
        {
            tracing::debug!(
                "default picker dir 推定: {} (repo '{}' の parent)",
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

/// VP-100 follow-up: 「+ Add Repo」クリック時の native folder picker + API 呼出。
///
/// rfd の picker は blocking なので別スレッドで実行。folder 選択後:
/// 1. `control.add_repo(name, path)` を呼ぶ (Unison `daemon-control.repos/add`)
/// 2. `control.start_process(name)` で repo を起動し、即「稼働中」にする (VP-203)
/// 3. `fetch_repos_with_ports` で runtime port 付きで再取得 → `AppEvent::ReposLoaded`
///
/// User キャンセル / RPC 失敗時は何もしない (sidebar は変化しない)。
/// `initial_dir` が `Some` なら picker の初期表示ディレクトリに設定。
pub(crate) fn spawn_add_repo_picker(
    proxy: EventLoopProxy<AppEvent>,
    initial_dir: Option<std::path::PathBuf>,
    rt_handle: tokio::runtime::Handle,
    conn: SharedDaemonConn,
) {
    let _ = thread::Builder::new()
        .name("add-repo-picker".into())
        .spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title("repo フォルダを選択");
            if let Some(d) = initial_dir.as_ref() {
                dialog = dialog.set_directory(d);
            }
            let folder = match dialog.pick_folder() {
                Some(p) => p,
                None => {
                    tracing::debug!("repo:add canceled by user");
                    return;
                }
            };
            let name = folder
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "repo".to_string());
            let path = folder.to_string_lossy().into_owned();
            tracing::info!("repo:add picker → name={} path={}", name, path);

            // picker thread (blocking) から shared runtime に async work を渡す。
            // `Handle::spawn` は runtime 外の thread からも呼べる。
            rt_handle.spawn(async move {
                let control = match conn.control().await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("add_repo: {}", e);
                        return;
                    }
                };
                if let Err(e) = control.add_repo(&name, &path).await {
                    tracing::warn!("add_repo RPC 失敗: {}", e);
                    return;
                }
                // VP-203: 登録後に repo を起動し即「稼働中」にする。起動に失敗しても
                // 登録自体は成功しているので、sidebar 反映 (list_repos) は続行する。
                if let Err(e) = control.start_process(&name).await {
                    tracing::warn!("add_repo 後の start_process 失敗: {}", e);
                }
                tracing::info!("add_repo + start_process 完了 → repos 再 fetch");
                // runtime port を merge する helper 経由で送る (= port を None で潰さない、
                // restart / stop / delete callback と同じ invariant)。
                match crate::app::fetch_repos_with_ports(&control).await {
                    Ok(repos) => {
                        let _ = proxy.send_event(AppEvent::ReposLoaded(repos));
                    }
                    Err(e) => {
                        tracing::warn!("add_repo 後の repos fetch 失敗: {}", e);
                    }
                }
            });
        });
}

/// 設定ページの「Add Repo の初期フォルダ」picker（doc 59 P1）。
///
/// rfd は blocking なので専用スレッドで回す（本 module の他 picker と同じ理由 =
/// event loop = main thread を塞がない）。結果は [`AppEvent::SettingsRepoRootPicked`] で戻す。
///
/// ⚠️ **キャンセルは `None`** — 呼び手が既存値を保持する（
/// 「選ばなかった」を「空にした」と取り違えると、設定が黙って消える）。
pub(crate) fn spawn_repo_root_picker(
    proxy: EventLoopProxy<AppEvent>,
    initial_dir: Option<std::path::PathBuf>,
) {
    let _ = thread::Builder::new()
        .name("settings-repo-root-picker".into())
        .spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title("Add Repo の初期フォルダを選択");
            if let Some(d) = initial_dir.as_ref() {
                dialog = dialog.set_directory(d);
            }
            let picked = dialog
                .pick_folder()
                .map(|p| p.to_string_lossy().into_owned());
            if picked.is_none() {
                tracing::debug!("settings:pick_repo_root canceled by user");
            }
            let _ = proxy.send_event(AppEvent::SettingsRepoRootPicked(picked));
        });
}

/// VP-100 follow-up: 「+ Clone Repository」クリック時の git clone + API 呼出。
///
/// 1. `git clone <url> <target>` を実行 (target は override 優先、無ければ
///    `<default_root>/<repo_name>`)
/// 2. 成功なら `add_repo` で daemon に register
/// 3. `fetch_repos_with_ports` で runtime port 付きで再取得 → `AppEvent::ReposLoaded`
///
/// `target_override` が `Some` ならそれを target とする (user が picker で選択した
/// folder)。`None` なら `default_root` + repo 名で auto 決定。後者で `default_root`
/// も `None` の場合は何もしない (default_repo_root が解決できないケース)。
/// git バイナリが PATH に無い場合も spawn 失敗で終わる。
pub(crate) fn spawn_clone_repo(
    proxy: EventLoopProxy<AppEvent>,
    url: String,
    default_root: Option<std::path::PathBuf>,
    target_override: Option<std::path::PathBuf>,
    rt_handle: tokio::runtime::Handle,
    conn: SharedDaemonConn,
) {
    // Phase 2.x-a: #210 の target_override を取り込み + Phase 1 の `process:` prefix を維持。
    // priority: 1) explicit target_override (picker 選択 path)、 2) default_root + repo 名
    let target = if let Some(t) = target_override {
        t
    } else if let Some(root) = default_root {
        root.join(derive_repo_name(&url))
    } else {
        tracing::warn!("process:clone but default_repo_root is unresolved (set in settings)");
        return;
    };
    let _ = thread::Builder::new()
        .name("clone-repo".into())
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
            // Register — repo 名は target folder の末尾セグメントから (override 時は
            // user が選んだ folder 名、default 時は repo 名と同一になる)
            let repo_name = target
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| derive_repo_name(&url));
            let path_str = target.to_string_lossy().into_owned();
            // clone (blocking) 完了後の daemon 呼び出しは shared runtime に渡す
            // (picker 経路と同じ理由 — 共有 QUIC connection はそこで駆動されている)。
            rt_handle.spawn(async move {
                let control = match conn.control().await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("clone 後の add_repo: {}", e);
                        return;
                    }
                };
                if let Err(e) = control.add_repo(&repo_name, &path_str).await {
                    tracing::warn!("clone 後の add_repo 失敗: {}", e);
                    return;
                }
                tracing::info!("clone + add_repo 成功 → repos 再 fetch");
                // runtime port を merge する helper 経由で送る (= add_repo picker 経路と同じ invariant)。
                match crate::app::fetch_repos_with_ports(&control).await {
                    Ok(repos) => {
                        let _ = proxy.send_event(AppEvent::ReposLoaded(repos));
                    }
                    Err(e) => {
                        tracing::warn!("clone 後の repos fetch 失敗: {}", e);
                    }
                }
            });
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
        .unwrap_or("repo")
        .trim_end_matches(".git");
    if last.is_empty() {
        "repo".to_string()
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
        // segment が空 / 異常系は "repo" fallback
        assert_eq!(derive_repo_name(""), "repo");
        assert_eq!(derive_repo_name(".git"), "repo");
    }
}
