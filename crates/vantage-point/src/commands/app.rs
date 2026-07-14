//! `vp app` コマンドの実行ロジック
//!
//! Mac 主軸切替 (2026-04-27, mem_1CaSjv5QQUNDxsEMjAicJ7):
//! vp-app crate (Rust + wry + xterm.js + creo-ui) を spawn する。
//! 旧 Swift VantagePoint.app 起動経路は廃止。

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum AppCommands {
    /// vp-app GUI を起動 (background spawn、 即 exit)
    Start,
    /// vp-app を停止 (SIGTERM、 idempotent)
    Stop,
    /// Start Menu に shortcut を置いて OS の「アプリ」として登録 (Windows、 idempotent)
    Install,
    /// Start Menu shortcut を除去 (Windows、 idempotent)
    Uninstall,
}

pub fn execute(cmd: AppCommands) -> Result<()> {
    match cmd {
        AppCommands::Start => start(),
        AppCommands::Stop => stop(),
        AppCommands::Install => install(),
        AppCommands::Uninstall => uninstall(),
    }
}

/// vp-app を background spawn + 親即 exit。
///
/// 設計判断: `Command::status()` (= `wait()` 相当の blocking) ではなく `spawn()` で
/// child handle を drop することで、 parent (= `vp app start`) は child の終了を
/// 待たない。 stdout/stderr は log file に redirect、 unix では `process_group(0)`
/// (`setsid` 相当) で child を新しい process group に分離 ── parent shell が
/// SIGHUP / exit しても child は生存し続ける (D12: daemon lifecycle 独立性)。
fn start() -> Result<()> {
    // ghost project の除去のみ実行（cwd の自動登録はしない）。
    // 以前は cwd を project として自動登録していたが、~/等の意図しないディレクトリが
    // 登録されてしまう問題があったため、sync は登録なし(None)で呼ぶ。
    let outcome = match crate::world_client::notify_world_sync() {
        Some(o) => Ok(o),
        None => crate::projects_file::ProjectsFile::sync(),
    };
    if let Ok(outcome) = outcome {
        for name in &outcome.removed {
            println!("🧹 ghost project を除去: {name}");
        }
    }

    let bin = find_vp_app_binary().context(
        "vp-app binary not found. \
         Build it first: 'cargo build --release -p vp-app' \
         or install: 'cargo install --path crates/vp-app'",
    )?;

    // Phase A: log dir 統一 (~/Library/Logs/Vantage/ on macOS)
    let log_dir = log_dir_path();
    std::fs::create_dir_all(&log_dir).ok();
    let daemon_log = log_dir.join("daemon.kdl.log");
    let stdout_log = log_dir.join("app.stdout.log");

    println!("🚀 Launching vp-app: {}", bin.display());
    println!("   daemon log: {}", daemon_log.display());
    println!("   stdout log: {}", stdout_log.display());

    let mut cmd = std::process::Command::new(&bin);
    cmd.env("VP_DAEMON_LOG_FILE", &daemon_log);

    // stdout/stderr を log file に redirect (parent が exit しても child の出力を
    // 失わないため、 file descriptor を OS に渡す)。
    let stdout_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stdout_log)
        .with_context(|| format!("Failed to open stdout log: {}", stdout_log.display()))?;
    let stderr_file = stdout_file
        .try_clone()
        .context("Failed to clone stdout file for stderr")?;
    cmd.stdout(stdout_file);
    cmd.stderr(stderr_file);

    // Unix: setsid 相当 (新 process group で child を分離、 親 shell の SIGHUP から守る)。
    // Windows: CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS で console を切り離し、
    //          親 (vp.exe) が exit しても vp-app GUI が独立稼働する。
    //          daemon_launcher.rs と同パターン。
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn vp-app at {}", bin.display()))?;
    let pid = child.id();
    println!("✅ vp-app launched (PID={pid})");
    println!("   logs: vp logs (or `tail -F {}`)", log_dir.display());

    // child handle drop = parent は child の終了を wait しない (= 即 exit)。
    drop(child);
    Ok(())
}

/// vp-app を停止 (Unix: SIGTERM via pkill、 Windows: taskkill /F /IM)。
/// process が存在しなくても error にしない (idempotent)。
fn stop() -> Result<()> {
    #[cfg(unix)]
    {
        let status = std::process::Command::new("pkill")
            .args(["-f", "vp-app$"])
            .status()
            .context("Failed to invoke pkill")?;
        match status.code() {
            Some(0) => println!("📴 vp-app stopped (SIGTERM sent)"),
            Some(1) => println!("(no vp-app process running)"),
            Some(c) => println!("(pkill exit code {c})"),
            None => println!("(pkill terminated by signal)"),
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        // Windows: `taskkill /F /IM vp-app.exe`。 SIGTERM 相当の graceful 経路は
        // window message (WM_CLOSE) 送信が必要だが、 まずは /F (hard kill) で
        // idempotent 停止を提供 (pkill -f 相当の挙動)。
        let status = std::process::Command::new("taskkill")
            .args(["/F", "/IM", "vp-app.exe"])
            .status()
            .context("Failed to invoke taskkill")?;
        match status.code() {
            Some(0) => println!("📴 vp-app stopped (taskkill /F)"),
            Some(128) => println!("(no vp-app process running)"),
            Some(c) => println!("(taskkill exit code {c})"),
            None => println!("(taskkill terminated by signal)"),
        }
        Ok(())
    }
}

/// `vp app install` — Start Menu に shortcut を置き、 OS の「アプリ」として登録する。
///
/// mac は `.app` bundle 自体が Launchpad / Spotlight の登録単位なので不要 (no-op)。
#[cfg(not(windows))]
fn install() -> Result<()> {
    println!("`vp app install` は Windows 専用です (mac は .app bundle が登録単位)。");
    Ok(())
}

#[cfg(not(windows))]
fn uninstall() -> Result<()> {
    println!("`vp app uninstall` は Windows 専用です。");
    Ok(())
}

/// Start Menu shortcut を作る (Windows、 idempotent = 既存があれば上書き)。
///
/// これで検索 (Win キー → "Vantage Point") / ピン留めから起動できるようになる。
#[cfg(windows)]
fn install() -> Result<()> {
    let target = find_vp_app_binary().context(
        "vp-app binary not found. \
         Build it first: 'cargo build --release -p vp-app' \
         or install: 'cargo install --path crates/vp-app'",
    )?;
    let lnk = shortcut::install(&target)?;
    println!("📌 Start Menu shortcut を作成しました: {}", lnk.display());
    println!("   target: {}", target.display());
    println!("   AppUserModelID: {}", vp_paths::app_user_model_id());
    println!("   Win キー → \"Vantage Point\" で起動 / タスクバーにピン留めできます。");
    Ok(())
}

/// Start Menu shortcut を除去 (Windows、 idempotent)。
#[cfg(windows)]
fn uninstall() -> Result<()> {
    match shortcut::uninstall()? {
        Some(lnk) => println!("🧹 Start Menu shortcut を除去しました: {}", lnk.display()),
        None => println!("(Start Menu shortcut は存在しません)"),
    }
    Ok(())
}

/// Windows の Start Menu shortcut (.lnk) を COM で読み書きする。
///
/// `.lnk` は単なる path の別名ではなく、 **AppUserModelID (AUMID) を焼ける入れ物**である点が
/// 重要。 process 側 (`vp_app::icon::set_app_user_model_id`) と shortcut 側に同じ AUMID が
/// 入って初めて、 taskbar が「ピン留めした shortcut」と「起動中の window」を同一の app と
/// 認識する (= ピン留めが機能する)。 AUMID を焼くには `IPropertyStore` が要るため、
/// `WScript.Shell` (PowerShell) では代替できず COM を直接叩いている。
#[cfg(windows)]
mod shortcut {
    use std::path::{Path, PathBuf};

    use anyhow::{Context, Result};
    // windows-rs 0.62 での在り処: PKEY_* は EnhancedStorage、 PROPVARIANT は
    // Com::StructuredStorage に居る (名前から想像する Shell::PropertiesSystem ではない)。
    use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize, IPersistFile,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, PropertiesSystem::IPropertyStore, ShellLink};
    use windows::core::{HSTRING, Interface};

    /// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Vantage Point.lnk`
    ///
    /// user 単位 (= 管理者権限不要)。 profile が付いていれば名前を分けて dev / release の
    /// shortcut が共存できるようにする (dir / port / AUMID の分離と同じ思想)。
    fn shortcut_path() -> Result<PathBuf> {
        let appdata = std::env::var_os("APPDATA").context("APPDATA env が取得できない")?;
        let name = match vp_paths::vp_profile() {
            Some(p) => format!("Vantage Point ({p}).lnk"),
            None => "Vantage Point.lnk".to_string(),
        };
        Ok(PathBuf::from(appdata)
            .join(r"Microsoft\Windows\Start Menu\Programs")
            .join(name))
    }

    /// COM を apartment-threaded で初期化し、 drop 時に必ず `CoUninitialize` する guard。
    struct ComGuard;

    impl ComGuard {
        fn new() -> Self {
            // 既に別 mode で初期化済み (RPC_E_CHANGED_MODE) でも、 CoCreateInstance は動くので続行する。
            // SAFETY: プロセス開始直後の CLI から呼ぶだけ。
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .ok();
            Self
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            // SAFETY: new() の CoInitializeEx と 1 対 1 で対応する。
            unsafe { CoUninitialize() };
        }
    }

    /// shortcut を作成する (既存があれば上書き = idempotent)。
    pub fn install(target: &Path) -> Result<PathBuf> {
        let lnk = shortcut_path()?;
        if let Some(dir) = lnk.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Start Menu dir の作成に失敗: {}", dir.display()))?;
        }

        let _com = ComGuard::new();
        let target_h = HSTRING::from(target.as_os_str());

        // SAFETY: 以下は全て「有効な COM object に、 有効な wide string を渡す」だけの呼び出し。
        // 各呼び出しの失敗は HRESULT で返るので ? で伝播する。
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .context("ShellLink の生成に失敗")?;
            link.SetPath(&target_h).context("SetPath に失敗")?;
            link.SetDescription(&HSTRING::from(
                "Vantage Point — AI native development environment",
            ))
            .context("SetDescription に失敗")?;
            // icon は exe に焼いた icon resource (build.rs) の index 0 を引く。
            link.SetIconLocation(&target_h, 0)
                .context("SetIconLocation に失敗")?;
            if let Some(dir) = target.parent() {
                link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))
                    .context("SetWorkingDirectory に失敗")?;
            }

            // AUMID を焼く (これがピン留めの成立条件)。
            let store: IPropertyStore = link.cast().context("IPropertyStore の取得に失敗")?;
            let aumid = PROPVARIANT::from(vp_paths::app_user_model_id());
            store
                .SetValue(&PKEY_AppUserModel_ID, &aumid)
                .context("AppUserModelID の設定に失敗")?;
            store.Commit().context("IPropertyStore の Commit に失敗")?;

            let persist: IPersistFile = link.cast().context("IPersistFile の取得に失敗")?;
            persist
                .Save(&HSTRING::from(lnk.as_os_str()), true)
                .with_context(|| format!("shortcut の保存に失敗: {}", lnk.display()))?;
        }

        Ok(lnk)
    }

    /// shortcut を除去する。 存在しなければ `None` (idempotent)。
    pub fn uninstall() -> Result<Option<PathBuf>> {
        let lnk = shortcut_path()?;
        if !lnk.is_file() {
            return Ok(None);
        }
        std::fs::remove_file(&lnk)
            .with_context(|| format!("shortcut の削除に失敗: {}", lnk.display()))?;
        Ok(Some(lnk))
    }

    /// test 用: 保存済み shortcut の target path を読み戻す。
    #[cfg(test)]
    pub fn read_target(lnk: &Path) -> Result<PathBuf> {
        use windows::Win32::System::Com::STGM_READ;
        use windows::Win32::UI::Shell::SLGP_RAWPATH;

        let _com = ComGuard::new();
        // SAFETY: 保存済み .lnk を読み、 target path を固定長 buffer に受ける。
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            let persist: IPersistFile = link.cast()?;
            persist.Load(&HSTRING::from(lnk.as_os_str()), STGM_READ)?;
            let mut buf = [0u16; 260];
            link.GetPath(&mut buf, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)?;
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            Ok(PathBuf::from(String::from_utf16_lossy(&buf[..end])))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// install → 読み戻し → uninstall が一巡すること (実際の Start Menu を触る)。
        /// AUMID / icon は COM の SetValue が Ok を返した時点で焼けているため、
        /// ここでは「shortcut が作られ target が保たれ、 消せる」ことを確認する。
        #[test]
        fn shortcut_roundtrip() {
            let target = std::env::current_exe().expect("current_exe");
            let lnk = install(&target).expect("install");
            assert!(
                lnk.is_file(),
                "shortcut が作られていない: {}",
                lnk.display()
            );

            let read = read_target(&lnk).expect("read_target");
            assert_eq!(
                read.file_name(),
                target.file_name(),
                "shortcut の target が一致しない"
            );

            assert!(uninstall().expect("uninstall").is_some());
            assert!(!lnk.is_file(), "shortcut が消えていない");
            // idempotent: 2 回目は None
            assert!(uninstall().expect("uninstall 2").is_none());
        }
    }
}

/// vp-app binary を探す:
/// 1. `VP_APP_BIN` env (mise task / dogfood で `target/release/vp-app` を直接渡す path)
/// 2. PATH 上の `vp-app` (cargo install で入った場合)
/// 3. 自分 (vp) の隣 (`~/.cargo/bin/vp` や `target/release/vp`、
///    Homebrew cask 配布なら `VantagePoint.app/Contents/MacOS/vp` の同 dir)
///
/// Windows では `.exe` 拡張子のついた binary を併せて探す。
fn find_vp_app_binary() -> Option<PathBuf> {
    // `VP_APP_BIN` env が指す path が file として存在すれば最優先。
    // cargo install を毎回挟まずに `cargo build --release -p vp-app` 直後の binary を
    // 即座に呼べる。 dogfood loop の rebuild → restart を高速化する目的 ((γ) 設計)。
    if let Some(p) = std::env::var_os("VP_APP_BIN") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    for name in binary_candidates("vp-app") {
        if let Some(p) = find_in_path(&name) {
            return Some(p);
        }
        // 自分 (vp) の隣を探す。Homebrew cask 配布では vp が
        // `/opt/homebrew/bin/vp` → `VantagePoint.app/Contents/MacOS/vp` の symlink
        // として実行され、macOS の `current_exe()` は symlink を解決せず link path を
        // そのまま返す。そのまま parent を見ると `/opt/homebrew/bin/` (vp-app 不在) を
        // 指してしまうため、canonicalize で実体 (bundle 内) に解決してから隣を見る。
        // canonicalize 失敗時は raw path の隣に fallback。
        if let Ok(self_exe) = std::env::current_exe() {
            let resolved = dunce::canonicalize(&self_exe).unwrap_or(self_exe);
            if let Some(dir) = resolved.parent() {
                let candidate = dir.join(&name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// platform に応じた binary 名候補。 Windows は `.exe` 付きも試す。
fn binary_candidates(name: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        vec![format!("{name}.exe"), name.to_string()]
    }
    #[cfg(unix)]
    {
        vec![name.to_string()]
    }
}

/// PATH を OS の区切り (`:` Unix / `;` Windows) で split して name を含む path を返す。
///
/// version manager の shim dir (`.../shims/`) は除外して **実体 binary** を掴む。 shim は
/// 実体を exec するだけの wrapper で、 VersionInfo も icon resource も持たない:
///   - `vp app install` が焼く shortcut は `SetIconLocation(target, 0)` で target の exe から
///     icon を引くため、 shim を指すと Start Menu が generic icon になる (= icon 対応が無に帰す)
///   - `vp app start` も wrapper プロセスが 1 枚余計に挟まるだけで得が無い
/// PATH 上の shim を飛ばしても、 実体は同じ PATH の後段 (`~/.cargo/bin` 等) か vp の隣で拾える。
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    // `std::env::split_paths` は OS の PATH 区切り文字を正しく扱う (Unix=`:`, Windows=`;`)。
    std::env::split_paths(&path_var)
        .filter(|d| !is_shim_dir(d))
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// version manager (mise / asdf / rtx) の shim dir か。 いずれも `shims` という dir 名で
/// wrapper を配る慣習なので、 dir 名 1 本で判定する。
fn is_shim_dir(dir: &std::path::Path) -> bool {
    dir.file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("shims"))
}

/// log 出力先 — XDG state zone 配下 (`vp_log_dir()` = `~/.local/state/vp/log/`)。
fn log_dir_path() -> PathBuf {
    crate::config::vp_log_dir()
}
