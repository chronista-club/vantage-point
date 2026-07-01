//! VP の config / data / state パス解決 (XDG Base Directory 準拠、全 OS 統一)。
//!
//! ## SSOT 化の経緯 (Stage 3)
//!
//! 元々この zone 解決 + legacy migration は `vantage_point::config` に直書きされ、
//! `vp-app` は `vantage-point` (SurrealDB / wry / midir 等の重量 crate) に意図的に
//! 依存しない方針のため `vp-app/src/paths.rs` に**複製**していた。手動同期が破綻して
//! 2 実装が drift していた (config.rs 側だけ tracing ログ付き) ため、軽量な本 crate に
//! 一本化し、`vantage-point` と `vp-app` の双方がここに依存する (循環なし)。
//!
//! ## パス方針 (canonical)
//!
//! | zone   | 環境変数            | default                  | 用途 |
//! |--------|---------------------|--------------------------|------|
//! | config | `$XDG_CONFIG_HOME`  | `~/.config/vp/`          | 人が編集 (config.kdl / projects.kdl / addresses.toml) |
//! | data   | `$XDG_DATA_HOME`    | `~/.local/share/vp/`     | 永続 data store (db / discs) |
//! | state  | `$XDG_STATE_HOME`   | `~/.local/state/vp/`     | runtime state + log (session.json / sessions/ / log/) |
//!
//! 旧 path (= macOS Application Support / Library/Logs / `dirs::config_dir()/vantage/`
//! 等) からの移行は [`migrate_legacy_paths`] が起動時に 1 回だけ冪等に行う。廃止 file
//! (running.json / vantage.db / config.toml / lanes/ / scripts/ 等) は同じ pass で delete。

use std::path::PathBuf;
use std::sync::OnceLock;

/// VP の実行 profile (`VP_PROFILE` env var)。
///
/// - 未設定 / 空文字 = `None` = **brew (一般ユーザ)** — 従来の `vp` namespace。
/// - `Some("dev")` = **開発者** — dev binary (`~/.cargo/bin`) を brew cask と混在させても
///   state を完全分離するための namespace suffix。
///
/// dev binary と brew cask は single-instance 前提で state (dir / world port / tmux socket) を
/// 共有するため、 両方走ると sp_LOCK 衝突・port 衝突・tmux adopt 混線を起こす (2026-07-01 実機事故)。
/// この profile が dir / port / socket の 3 レバー全ての分岐点。
///
/// env は継承で伝播する (dev shell → vp-app → daemon → SP → tmux)。 brew は LaunchAgent 起動で
/// env を持たないため自然に `None` = brew namespace になる。 値は起動時に 1 回だけ読む。
pub fn vp_profile() -> Option<&'static str> {
    static PROFILE: OnceLock<Option<String>> = OnceLock::new();
    PROFILE
        .get_or_init(|| match std::env::var("VP_PROFILE") {
            Ok(v) if !v.trim().is_empty() => Some(v.trim().to_string()),
            _ => None,
        })
        .as_deref()
}

/// config/data/state の dir 名 + tmux socket 名。 brew=`vp` / dev=`vp-dev`。
///
/// [`vp_profile`] が `Some(p)` なら `vp-{p}`、 `None` なら `vp`。 dir と tmux socket の
/// 両方がこれを参照して 1 元化する (= profile ごとに `~/.local/share/vp-dev/` 等へ芋づる分離)。
pub fn app_dir_name() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| match vp_profile() {
        Some(p) => format!("vp-{p}"),
        None => "vp".to_string(),
    })
}

/// world port の base 値 (brew の TheWorld port)。
pub const WORLD_PORT_BASE: u16 = 32000;

/// profile に応じた TheWorld の world port。 brew=32000 / dev=32100。
///
/// SP は portless (33000 番台は bind しない論理 identity) なので、 実 listener は world 単一。
/// この 1 本を profile でずらせば daemon bind / app connect / SP→world connect が芋づるで追随し、
/// dev daemon (32100) と brew daemon (32000) が衝突せず並列常駐できる。
///
/// 未設定 = 32000 (brew、 従来値で不変)。 `Some(_)` = base + 100。
/// 注: offset は現状「profile 有無」の 2 値 (dev=+100)。 複数 dev profile を同時常駐させる
/// 要件は無いため、 `dev` 以外の任意 profile も同じ +100 に落ちる (dir 名は分離されるが port は共有)。
pub fn default_world_port() -> u16 {
    match vp_profile() {
        Some(_) => WORLD_PORT_BASE + 100,
        None => WORLD_PORT_BASE,
    }
}

/// terminal 入力の二重化 (`a` → `aa`) 診断用トレース。
///
/// `VP_TERM_TRACE=1` の時だけ、 keystroke 入力の各 hop を byte preview 付きで `info` log する
/// (未設定時は完全無音 = 常用・nightly を汚さない)。 間欠再現時に log を `termtrace` で grep し、
/// 「どの hop で 1 keystroke が 2 回になるか」を特定する用途。
///
/// hop 命名規約: `A:app-dispatch`(vp-app 上り dispatch) → `B:sp-recv`(SP handle_terminal_write 受信)。
/// A=2 なら vp-app 内二重 / A=1・B=2 なら vp-app→World→SP 区間の二重 / 両方 1 なら SP 書込より下
/// (tmux adopt / PTY 層)。 env は継承で全 process (vp-app / daemon / SP) に伝播する。
pub fn term_trace(hop: &str, lane: &str, data: &[u8]) {
    static ON: OnceLock<bool> = OnceLock::new();
    if !ON.get_or_init(|| std::env::var("VP_TERM_TRACE").is_ok()) {
        return;
    }
    let preview: String = data.iter().take(16).map(|b| format!("{b:02x}")).collect();
    tracing::info!(
        target: "termtrace",
        "[termtrace] hop={hop} lane={lane} len={} bytes={preview}",
        data.len()
    );
}

/// VP の config zone (XDG `$XDG_CONFIG_HOME/vp/`、 default `~/.config/vp/`)。
///
/// 人が編集する設定 (config.kdl / projects.kdl / addresses.toml) の置き場。
/// `XDG_CONFIG_HOME` 環境変数を優先、 未設定なら `$HOME/.config/vp/`。 macOS
/// でも `~/Library/Application Support/` は使わない (= dotfile 一極集中方針)。
pub fn vp_config_dir() -> PathBuf {
    xdg_base("XDG_CONFIG_HOME", ".config")
}

/// VP の data zone (XDG `$XDG_DATA_HOME/vp/`、 default `~/.local/share/vp/`)。
///
/// 永続 data store (SurrealDB の `db/`、 Whitesnake `discs/`)。 失っても再生成
/// される類の cache ではなく、 失えない user data を置く。
pub fn vp_data_dir() -> PathBuf {
    xdg_base("XDG_DATA_HOME", ".local/share")
}

/// VP の state zone (XDG `$XDG_STATE_HOME/vp/`、 default `~/.local/state/vp/`)。
///
/// runtime state + log。 vp-app UI state (`session.json`)、 TUI SessionManager
/// (`sessions/{port}.json`)、 全 process の log (`log/`) を置く。 XDG spec で
/// log は state zone 配下が standard。
pub fn vp_state_dir() -> PathBuf {
    xdg_base("XDG_STATE_HOME", ".local/state")
}

/// log 出力先 (`vp_state_dir()/log/`)。 daemon / SP / vp-app の全 log を集約。
pub fn vp_log_dir() -> PathBuf {
    vp_state_dir().join("log")
}

/// TUI SessionManager の per-port state file 置き場 (`vp_state_dir()/sessions/`)。
pub fn vp_sessions_dir() -> PathBuf {
    vp_state_dir().join("sessions")
}

/// XDG base directory 解決ヘルパー。
///
/// `$env_name` 環境変数が absolute path なら採用、 そうでなければ
/// `$HOME/{home_relative}` を使う。 いずれも末尾に profile dir 名
/// ([`app_dir_name`] = `vp` or `vp-{profile}`) を join する。
/// `$HOME` 未取得時は `.` fallback (= test/sandbox 用)。
fn xdg_base(env_name: &str, home_relative: &str) -> PathBuf {
    let name = app_dir_name();
    if let Some(v) = std::env::var_os(env_name) {
        let p = PathBuf::from(v);
        if p.is_absolute() {
            return p.join(name);
        }
    }
    dirs::home_dir()
        .map(|h| h.join(home_relative))
        .unwrap_or_else(|| PathBuf::from("."))
        .join(name)
}

/// 旧 path から XDG 新 path への冪等な migration + 廃止物 cleanup。
///
/// VP は過去、 path 体系が複数世代並存していた:
/// - 世代 1: `~/.config/vp/` 直書き
/// - 世代 2: `dirs::config_dir()/vantage/`
/// - 世代 3 (VP-192): macOS `~/Library/Application Support/vp/` + `~/Library/Logs/Vantage/`
/// - 世代 4 (= 本 fn): XDG 統一 `~/.config/vp/` + `~/.local/share/vp/` + `~/.local/state/vp/`
///
/// 本 fn は世代 1〜3 から世代 4 への移行を 1 pass で行い、 同時に廃止 file/dir
/// (running.json / vantage.db / config.toml / lanes/ / scripts/ 等) を delete する。
///
/// 設計:
/// - **move (rename)** で旧データを新位置へ。 同一 device なら atomic、 跨ぐなら copy+remove。
/// - **冪等**。 新 path に既にデータがあれば旧側を skip + 空なら delete。
/// - **廃止物 delete** は code 参照ゼロ確認済 entry のみ (= safe)。
/// - 失敗しても起動阻害しない (warn ログのみ)。
///
/// main 初期化の早い段階で 1 回呼ぶこと。daemon (vp-cli) 側と vp-app の双方が起動時に
/// 呼ぶが、 冪等設計なので二重実行で一方が先に旧 path を空にしても問題ない。
pub fn migrate_legacy_paths() {
    // dev profile は独立 namespace (`vp-dev`)。 旧 default (`vp`) データを dev へ移行しない
    // (dev は空から始めて brew と完全分離するのが分離の趣旨。 default profile の時だけ移行する)。
    if vp_profile().is_some() {
        return;
    }

    let new_config = vp_config_dir();
    let new_data = vp_data_dir();
    let new_state = vp_state_dir();
    let new_log = vp_log_dir();

    // 世代 3: macOS `~/Library/Application Support/vp/` から各 zone へ拡散 move。
    if let Some(home) = dirs::home_dir() {
        let mac_app_support = home.join("Library/Application Support/vp");
        if mac_app_support.is_dir() {
            // config zone へ
            move_file_if_exists(
                &mac_app_support.join("config.kdl"),
                &new_config.join("config.kdl"),
            );
            move_file_if_exists(
                &mac_app_support.join("projects.kdl"),
                &new_config.join("projects.kdl"),
            );
            move_file_if_exists(
                &mac_app_support.join("addresses.toml"),
                &new_config.join("addresses.toml"),
            );
            // data zone へ
            move_dir_if_exists(&mac_app_support.join("db"), &new_data.join("db"));
            move_dir_if_exists(&mac_app_support.join("discs"), &new_data.join("discs"));
            // state zone へ (rename: session-state.json → session.json)
            move_file_if_exists(
                &mac_app_support.join("session-state.json"),
                &new_state.join("session.json"),
            );
            // TUI SessionManager: state/{port}.json → sessions/{port}.json
            migrate_state_subdir(&mac_app_support.join("state"), &vp_sessions_dir());
            // log: logs/debug.log → log/debug.log
            move_file_if_exists(
                &mac_app_support.join("logs/debug.log"),
                &new_log.join("debug.log"),
            );

            // 廃止 file delete
            for legacy in [
                "config.toml",  // KDL 統合済
                "running.json", // discovery 移行済
                "vantage.db",   // code 参照ゼロ
            ] {
                delete_file_if_exists(&mac_app_support.join(legacy));
            }
            for legacy_dir in ["lanes", "scripts", "logs"] {
                delete_dir_if_exists(&mac_app_support.join(legacy_dir));
            }
            // 空になった元 dir も掃除
            delete_dir_if_empty(&mac_app_support);
        }

        // 世代 3: macOS `~/Library/Logs/Vantage/` から log zone へ
        let mac_log_dir = home.join("Library/Logs/Vantage");
        if mac_log_dir.is_dir() {
            move_file_if_exists(
                &mac_log_dir.join("app.kdl.log"),
                &new_log.join("app.kdl.log"),
            );
            move_file_if_exists(
                &mac_log_dir.join("app.stdout.log"),
                &new_log.join("app.stdout.log"),
            );
            move_file_if_exists(
                &mac_log_dir.join("daemon.kdl.log"),
                &new_log.join("daemon.kdl.log"),
            );
            move_file_if_exists(
                &mac_log_dir.join("daemon.stdout.log"),
                &new_log.join("daemon.stdout.log"),
            );
            // 廃止 (rename 前の遺物)
            for legacy in ["vp-app.kdl.log", "vp-app.stdout.log", "vp-world.kdl.log"] {
                delete_file_if_exists(&mac_log_dir.join(legacy));
            }
            delete_dir_if_empty(&mac_log_dir);
        }
    }

    // 世代 1: 旧 `~/.config/vp/` 直書き世代と現 XDG `~/.config/vp/` は **同一 path**。
    // 既に新 path なので何もしない。

    // 世代 2: `dirs::config_dir()/vantage/` → 新 data zone
    if let Some(cfg) = dirs::config_dir() {
        let legacy_data = cfg.join("vantage");
        if legacy_data.is_dir() && legacy_data != new_data {
            move_dir_contents(&legacy_data, &new_data, "vantage→data");
            delete_dir_if_empty(&legacy_data);
        }
    }
}

/// `state/{port}.json` は TUI SessionManager 用 = 新 `sessions/{port}.json` へ。
/// `state/{port}-panes.json` / `state/{port}-canvas-layout.json` は D11 廃止遺物 = delete。
fn migrate_state_subdir(legacy_state: &std::path::Path, new_sessions: &std::path::Path) {
    if !legacy_state.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(legacy_state) else {
        return;
    };
    for entry in entries.flatten() {
        let from = entry.path();
        let Some(name) = from.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with("-panes.json") || name.ends_with("-canvas-layout.json") {
            delete_file_if_exists(&from);
        } else if name.ends_with(".json") {
            // {port}.json は active な SessionManager state
            move_file_if_exists(&from, &new_sessions.join(name));
        }
    }
    delete_dir_if_empty(legacy_state);
}

fn move_file_if_exists(from: &std::path::Path, to: &std::path::Path) {
    if !from.is_file() {
        return;
    }
    if to.exists() {
        // 新側に既にあれば旧は黙って消す (= 冪等、 新側 SSOT 維持)
        delete_file_if_exists(from);
        return;
    }
    if let Some(parent) = to.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(
            "path migration: parent create 失敗 ({}): {e}",
            parent.display()
        );
        return;
    }
    if let Err(e) = std::fs::rename(from, to) {
        // 跨デバイス等で rename 失敗 → copy + remove fallback
        if std::fs::copy(from, to).is_ok() {
            let _ = std::fs::remove_file(from);
            tracing::info!(
                "path migration (file copy+remove): {} → {}",
                from.display(),
                to.display()
            );
        } else {
            tracing::warn!(
                "path migration 失敗 ({} → {}): {e}",
                from.display(),
                to.display()
            );
        }
    } else {
        tracing::info!(
            "path migration (file move): {} → {}",
            from.display(),
            to.display()
        );
    }
}

fn move_dir_if_exists(from: &std::path::Path, to: &std::path::Path) {
    if !from.is_dir() {
        return;
    }
    if to.exists() && dir_has_entries(to) {
        // 新側にデータあり → 旧側を削除
        let _ = std::fs::remove_dir_all(from);
        return;
    }
    if let Some(parent) = to.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::rename(from, to) {
        // fallback: 再帰コピー + 削除
        if copy_dir_recursive(from, to).is_ok() {
            let _ = std::fs::remove_dir_all(from);
            tracing::info!(
                "path migration (dir copy+remove): {} → {}",
                from.display(),
                to.display()
            );
        } else {
            tracing::warn!(
                "path migration 失敗 ({} → {}): {e}",
                from.display(),
                to.display()
            );
        }
    } else {
        tracing::info!(
            "path migration (dir move): {} → {}",
            from.display(),
            to.display()
        );
    }
}

/// 中身を子単位で新 dir に move (= dir 自体は残す方針)。
fn move_dir_contents(from: &std::path::Path, to: &std::path::Path, label: &str) {
    if !from.is_dir() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(to) {
        tracing::warn!("path migration ({label}) parent create 失敗: {e}");
        return;
    }
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    for entry in entries.flatten() {
        let child_from = entry.path();
        let Some(name) = child_from.file_name() else {
            continue;
        };
        let child_to = to.join(name);
        if child_from.is_dir() {
            move_dir_if_exists(&child_from, &child_to);
        } else if child_from.is_file() {
            move_file_if_exists(&child_from, &child_to);
        }
    }
}

fn delete_file_if_exists(path: &std::path::Path) {
    if path.is_file()
        && let Err(e) = std::fs::remove_file(path)
    {
        tracing::warn!("path cleanup 失敗 ({}): {e}", path.display());
    }
}

fn delete_dir_if_exists(path: &std::path::Path) {
    if path.is_dir()
        && let Err(e) = std::fs::remove_dir_all(path)
    {
        tracing::warn!("path cleanup 失敗 ({}): {e}", path.display());
    }
}

fn delete_dir_if_empty(path: &std::path::Path) {
    if path.is_dir() && !dir_has_entries(path) {
        let _ = std::fs::remove_dir(path);
    }
}

/// `legacy` ディレクトリの中身を `target` にコピーする (冪等ヘルパー)。
///
/// - `target` が存在して空でなければ skip (= 移行済み)。
/// - `legacy` が存在しない、 または `legacy == target` (同一パス) なら skip。
/// - コピーは再帰的。 失敗は warn ログのみで握り潰す (起動を止めない)。
///
/// 現状 production からの呼び出しは無く ([`migrate_legacy_paths`] は `move_*` 系を使う)、
/// 旧 VP-192 世代の helper として保守 + test 維持のため公開している。
pub fn migrate_dir_if_needed(legacy: &std::path::Path, target: &std::path::Path, label: &str) {
    // 旧パスと新パスが同一 (= 既に正規パス) なら何もしない。
    if legacy == target {
        return;
    }
    // 旧データが無ければ移行不要。
    if !legacy.is_dir() {
        return;
    }
    // 新パスに既にデータがあれば移行済みとみなす (冪等)。
    if dir_has_entries(target) {
        return;
    }
    tracing::info!(
        "VP-192 path migration ({}): {} → {}",
        label,
        legacy.display(),
        target.display()
    );
    if let Err(e) = copy_dir_recursive(legacy, target) {
        tracing::warn!(
            "VP-192 path migration ({}) 失敗 ({} → {}): {} — 旧パスのまま継続",
            label,
            legacy.display(),
            target.display(),
            e
        );
    }
}

/// ディレクトリが存在し、 かつ 1 つ以上のエントリを持つか。
fn dir_has_entries(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

/// `src` の中身を `dst` に再帰コピーする。 `dst` は無ければ作成。
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vp_config_dir_ends_with_vp() {
        assert!(vp_config_dir().ends_with("vp"));
    }

    // 注: 以下の profile テストは VP_PROFILE 未設定 (= CI / 通常 test 環境) の default 挙動を
    // 検証する。 `vp_profile()` 等は OnceLock で 1 回だけ env を読むため、 同一プロセス内で
    // dev branch を別途 assert することはできない (dev 分岐はコード inspection + 実機検証で担保)。
    #[test]
    fn test_app_dir_name_default_is_vp() {
        // VP_PROFILE 未設定なら dir 名は素の "vp"
        assert_eq!(app_dir_name(), "vp");
    }

    #[test]
    fn test_default_world_port_default_is_base() {
        // VP_PROFILE 未設定なら world port は base (32000)
        assert_eq!(default_world_port(), WORLD_PORT_BASE);
        assert_eq!(default_world_port(), 32000);
    }

    #[test]
    fn test_vp_data_dir_ends_with_vp() {
        assert!(vp_data_dir().ends_with("vp"));
    }

    #[test]
    fn test_vp_state_dir_ends_with_vp() {
        assert!(vp_state_dir().ends_with("vp"));
    }

    #[test]
    fn test_vp_log_dir_ends_with_log() {
        assert!(vp_log_dir().ends_with("log"));
    }

    #[test]
    fn test_vp_sessions_dir_ends_with_sessions() {
        assert!(vp_sessions_dir().ends_with("sessions"));
    }

    #[test]
    fn test_copy_dir_recursive_copies_nested() {
        // 再帰コピーが nested file/dir を保持する
        let tmp = std::env::temp_dir().join(format!("vp192_copy_{}", std::process::id()));
        let src = tmp.join("src");
        let dst = tmp.join("dst");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), "hello").unwrap();
        std::fs::write(src.join("sub").join("b.txt"), "world").unwrap();

        copy_dir_recursive(&src, &dst).unwrap();

        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "hello");
        assert_eq!(
            std::fs::read_to_string(dst.join("sub").join("b.txt")).unwrap(),
            "world"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_migrate_dir_if_needed_idempotent() {
        // VP-192: migration は冪等 — 新パスに既存データがあれば旧データで上書きしない
        let tmp = std::env::temp_dir().join(format!("vp192_mig_{}", std::process::id()));
        let legacy = tmp.join("legacy");
        let target = tmp.join("target");
        let _ = std::fs::remove_dir_all(&tmp);

        // 旧パスに古いデータ、新パスに既存データを置く
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("data.txt"), "OLD").unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("data.txt"), "NEW").unwrap();

        // 1 回目: 新パスに中身があるので skip されるはず
        migrate_dir_if_needed(&legacy, &target, "test");
        assert_eq!(
            std::fs::read_to_string(target.join("data.txt")).unwrap(),
            "NEW",
            "既存データがあれば移行 skip (冪等)"
        );

        // 2 回目: 何度呼んでも変わらない
        migrate_dir_if_needed(&legacy, &target, "test");
        assert_eq!(
            std::fs::read_to_string(target.join("data.txt")).unwrap(),
            "NEW"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_migrate_dir_if_needed_copies_to_empty_target() {
        // VP-192: 新パスが空 (or 不在) なら旧データをコピーし、旧データは残す
        let tmp = std::env::temp_dir().join(format!("vp192_migc_{}", std::process::id()));
        let legacy = tmp.join("legacy");
        let target = tmp.join("target");
        let _ = std::fs::remove_dir_all(&tmp);

        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("data.txt"), "OLD").unwrap();
        // target は不在

        migrate_dir_if_needed(&legacy, &target, "test");

        assert_eq!(
            std::fs::read_to_string(target.join("data.txt")).unwrap(),
            "OLD",
            "新パスへコピーされる"
        );
        assert!(
            legacy.join("data.txt").exists(),
            "旧データは残る (move ではなく copy)"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_migrate_dir_if_needed_same_path_noop() {
        // legacy == target なら何もしない
        let tmp = std::env::temp_dir().join(format!("vp192_migs_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("data.txt"), "X").unwrap();
        // パニックしないこと、データが壊れないことを確認
        migrate_dir_if_needed(&tmp, &tmp, "test");
        assert_eq!(std::fs::read_to_string(tmp.join("data.txt")).unwrap(), "X");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_migrate_dir_if_needed_missing_legacy_noop() {
        // 旧パスが存在しなければ何もしない
        let tmp = std::env::temp_dir().join(format!("vp192_migm_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let legacy = tmp.join("nonexistent");
        let target = tmp.join("target");
        migrate_dir_if_needed(&legacy, &target, "test");
        assert!(!target.exists(), "旧データ不在なら新パスは作られない");
    }
}
