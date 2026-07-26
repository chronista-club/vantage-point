//! spawn する子プロセス用の PATH 補強 + ロケール解決 — 補正の単一の入口 (SSOT)。
//!
//! ## なぜ vp-paths に置くか
//!
//! vp-app (`.app`) を GUI / launchd / Dock 経由で起動すると、 プロセスの PATH が
//! `/usr/bin:/bin:/usr/sbin:/sbin` の最小集合になる。 この痩せた PATH は spawn chain
//! (vp-app → daemon → repo → PtySlot → login shell → claude) を伝播し、 user-installed tool
//! (claude、 Windows は `claude.exe`) を見つけられず spawn が失敗 → lane が即 Dead 化 →
//! Echoes コンソールが出ない、 という症状の根因になる。
//!
//! note: mise shims を PATH に足すのは「user が mise で claude を管理していても見つかる」
//! **許容**であり、 vp が mise に依存するわけではない (tmux decoupling PR2 で vp runtime は
//! mise-free — mise を exec する箇所は product に存在しない)。
//!
//! この補正は **vantage-point (daemon / repo / PtySlot) と vp-app (daemon_launcher) の双方**が
//! spawn 最上流で必要とする。 かつては `vantage_point::spawn_env` が SSOT で vp-app が手動同期
//! レプリカを持っていたが (drift 源)、 path 解決の SSOT である本 crate (vantage-point + vp-app
//! 共有、 循環なし) に一本化した。 `vantage_point::spawn_env` は本 module を re-export する。
//!
//! ## OS 別の user tool location
//!
//! | OS | prefix (優先順) |
//! |----|-----------------|
//! | Unix (macOS/Linux) | `{home}/.local/bin` → `{home}/.local/share/mise/shims` → `{home}/.cargo/bin` → `/opt/homebrew/bin` → `/usr/local/bin` |
//! | Windows | `{home}\.local\bin` (claude native installer) → `{home}\AppData\Local\mise\shims` (winget mise) → `{home}\.cargo\bin` |
//!
//! PATH 区切りも OS 別 (`:` Unix / `;` Windows)。 home は `dirs::home_dir()` で解決するため
//! Windows で `HOME` 未設定 (通常 `USERPROFILE` のみ) でも正しく引ける。

use std::collections::HashSet;

/// PATH の区切り文字 (Unix `:` / Windows `;`)。
#[cfg(windows)]
const PATH_SEP: char = ';';
/// PATH の区切り文字 (Unix `:` / Windows `;`)。
#[cfg(not(windows))]
const PATH_SEP: char = ':';

/// 既知の user tool location (OS 別)。 base PATH に無いものだけを先頭に前置する。
///
/// 順序 = PATH 探索の優先順位。 Windows は homebrew / usr-local を持たず、 mise shims が
/// `%LOCALAPPDATA%\mise\shims` (= 慣習的に `{home}\AppData\Local\mise\shims`) に居る点が Unix と異なる。
fn user_tool_prefixes(home: Option<&str>) -> Vec<String> {
    let mut prefixes: Vec<String> = Vec::new();
    #[cfg(windows)]
    {
        if let Some(home) = home {
            prefixes.push(format!(r"{home}\.local\bin")); // claude native installer / user tools
            prefixes.push(format!(r"{home}\AppData\Local\mise\shims")); // winget mise 管理ツールの shim
            prefixes.push(format!(r"{home}\.cargo\bin")); // vp 自身 / rust tools
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = home {
            prefixes.push(format!("{home}/.local/bin")); // mise 本体 / user-installed tools
            prefixes.push(format!("{home}/.local/share/mise/shims")); // mise 管理ツール (claude 等) の shim
            prefixes.push(format!("{home}/.cargo/bin")); // vp 自身 / rust tools
        }
        prefixes.push("/opt/homebrew/bin".to_string()); // Apple Silicon homebrew
        prefixes.push("/usr/local/bin".to_string()); // Intel homebrew / 一般
    }
    prefixes
}

/// 補強ロジックの純粋核 — separator と prefixes を注入するので OS 非依存でテストできる。
///
/// base に既に含まれる prefix は重複させない (PATH 肥大化を避ける)。
fn augment_path_with(base_path: &str, prefixes: &[String], sep: char) -> String {
    let base_segments: HashSet<&str> = base_path.split(sep).collect();
    let new_prefixes: Vec<&str> = prefixes
        .iter()
        .map(String::as_str)
        .filter(|p| !base_segments.contains(p))
        .collect();
    let sep_str = sep.to_string();
    match (new_prefixes.is_empty(), base_path.is_empty()) {
        (true, _) => base_path.to_string(),
        (false, true) => new_prefixes.join(&sep_str),
        (false, false) => format!("{}{}{}", new_prefixes.join(&sep_str), sep, base_path),
    }
}

/// `base_path` に user tool location を前置して補強した PATH 文字列を返す。
///
/// `home` は呼び出し側が注入する (test 容易性のため)。 OS 別の prefix / 区切りは内部で解決。
/// 純関数 (env / fs に触れない)。
pub fn augment_path(base_path: &str, home: Option<&str>) -> String {
    augment_path_with(base_path, &user_tool_prefixes(home), PATH_SEP)
}

/// `dirs::home_dir()` を文字列で返す (`HOME` env に依存せず Windows の `USERPROFILE` も引く)。
fn home_string() -> Option<String> {
    dirs::home_dir().map(|p| p.to_string_lossy().into_owned())
}

/// 現プロセスの PATH (`$PATH`) を user tool location で補強した文字列を返す。
///
/// 子プロセスを spawn する箇所で `Command::env("PATH", augmented_spawn_path())` の形で
/// 使う、 補正の唯一の入口。 痩せた PATH が spawn chain を伝播するのをここで断つ。
pub fn augmented_spawn_path() -> String {
    let base = std::env::var("PATH").unwrap_or_default();
    augment_path(&base, home_string().as_deref())
}

/// caller が明示した `base_path` を、 現ユーザの home で補強する。
///
/// PtySlot のように「caller env の PATH override を base にしたい」経路用。 home は
/// [`augmented_spawn_path`] と同じく `dirs::home_dir()` で解決する (`HOME` 非依存)。
pub fn augment_path_env(base_path: &str) -> String {
    augment_path(base_path, home_string().as_deref())
}

/// 子プロセス（特に launchd 起動 daemon → repo → tmux）に渡す UTF-8 ロケールを返す。
///
/// ## なぜ必要か
///
/// launchd 自動起動の daemon は LANG が剥がれて C ロケールになり、 配下の tmux client が
/// `utf8=0` になる。 すると日本語など CJK が `_` に潰れる（CJK blackout）。 これは PATH
/// 痩せ (#498) / TERM 不在 (#630) と同根の「launchd env stripping」の三つ子の最後の一本で、
/// `vp daemon install` の plist EnvironmentVariables に LANG/LC_CTYPE を焼くことで断つ。
///
/// ## 方針
///
/// インストール時の `$LANG` が UTF-8 codeset ならユーザ設定を尊重する（例: `ja_JP.UTF-8`）。
/// 未設定 / 非 UTF-8 なら `en_US.UTF-8` に fallback する（codeset が `.UTF-8` でありさえ
/// すれば tmux は utf8=1 になる — 言語部は CJK 表示の可否に影響しない）。
pub fn utf8_locale() -> String {
    resolve_utf8_locale(std::env::var("LANG").ok().as_deref())
}

/// 純関数: LANG 候補から UTF-8 ロケールを解決する（env / fs に触れない、 test 容易）。
pub fn resolve_utf8_locale(lang: Option<&str>) -> String {
    const FALLBACK: &str = "en_US.UTF-8";
    match lang {
        Some(l) if is_utf8_locale(l) => l.to_string(),
        _ => FALLBACK.to_string(),
    }
}

/// codeset が UTF-8 か（`.UTF-8` / `.utf8`、 大文字小文字無視）。
fn is_utf8_locale(lang: &str) -> bool {
    let lower = lang.to_ascii_lowercase();
    lower.ends_with(".utf-8") || lower.ends_with(".utf8")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── OS 非依存の純粋核テスト (separator / prefixes 注入) ──

    #[test]
    fn augment_path_with_prepends_missing_prefixes() {
        let prefixes = vec!["/a".to_string(), "/b".to_string()];
        assert_eq!(
            augment_path_with("/usr/bin", &prefixes, ':'),
            "/a:/b:/usr/bin"
        );
    }

    #[test]
    fn augment_path_with_skips_present_prefixes() {
        let prefixes = vec!["/a".to_string(), "/b".to_string()];
        // /a は base に既存 → 前置しない。
        assert_eq!(
            augment_path_with("/a:/usr/bin", &prefixes, ':'),
            "/b:/a:/usr/bin"
        );
    }

    #[test]
    fn augment_path_with_empty_base() {
        let prefixes = vec!["/a".to_string(), "/b".to_string()];
        assert_eq!(augment_path_with("", &prefixes, ':'), "/a:/b");
    }

    #[test]
    fn augment_path_with_windows_separator() {
        let prefixes = vec![r"C:\a".to_string(), r"C:\b".to_string()];
        // `;` 区切りで `C:\...` (colon 入り) を壊さず結合できる。
        assert_eq!(
            augment_path_with(r"C:\Windows", &prefixes, ';'),
            r"C:\a;C:\b;C:\Windows"
        );
    }

    #[test]
    fn augment_path_with_all_present_returns_base() {
        let prefixes = vec!["/a".to_string()];
        assert_eq!(
            augment_path_with("/a:/usr/bin", &prefixes, ':'),
            "/a:/usr/bin"
        );
    }

    // ── Unix 固有の prefix セットと区切り ──

    #[cfg(not(windows))]
    #[test]
    fn augment_path_unix_prepends_user_tool_locations() {
        let r = augment_path("/usr/bin:/bin", Some("/Users/x"));
        assert_eq!(
            r,
            "/Users/x/.local/bin:/Users/x/.local/share/mise/shims:/Users/x/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn augment_path_unix_without_home_only_system_prefixes() {
        assert_eq!(
            augment_path("/usr/bin", None),
            "/opt/homebrew/bin:/usr/local/bin:/usr/bin"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn augment_path_unix_skips_prefixes_already_present() {
        // Mac 回帰 pin: 健全な dev shell で base に既存の prefix (homebrew 等) は重複させない。
        let r = augment_path("/opt/homebrew/bin:/usr/bin", Some("/Users/x"));
        assert_eq!(
            r,
            "/Users/x/.local/bin:/Users/x/.local/share/mise/shims:/Users/x/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn augment_path_unix_builds_from_prefixes_when_base_empty() {
        // Mac 回帰 pin: base が空でも prefix だけで PATH を構築できる。
        assert_eq!(
            augment_path("", Some("/Users/x")),
            "/Users/x/.local/bin:/Users/x/.local/share/mise/shims:/Users/x/.cargo/bin:/opt/homebrew/bin:/usr/local/bin"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn augment_path_unix_all_prefixes_present_returns_base_unchanged() {
        // Mac 回帰 pin: 全 prefix が既に base にあれば base をそのまま返す (肥大化しない)。
        let base = "/Users/x/.local/bin:/Users/x/.local/share/mise/shims:/Users/x/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin";
        assert_eq!(augment_path(base, Some("/Users/x")), base);
    }

    // ── Windows 固有の prefix セットと区切り ──

    #[cfg(windows)]
    #[test]
    fn augment_path_windows_prepends_user_tool_locations() {
        // `;` 区切り、 backslash prefix、 claude native (.local\bin) / winget mise shims / cargo。
        let r = augment_path(r"C:\Windows\System32;C:\Windows", Some(r"C:\Users\x"));
        assert_eq!(
            r,
            r"C:\Users\x\.local\bin;C:\Users\x\AppData\Local\mise\shims;C:\Users\x\.cargo\bin;C:\Windows\System32;C:\Windows"
        );
    }

    #[cfg(windows)]
    #[test]
    fn augment_path_windows_without_home_returns_base() {
        // Windows は home 無しだと prefix が空 → base をそのまま返す。
        assert_eq!(augment_path(r"C:\Windows", None), r"C:\Windows");
    }

    // ── ロケール解決 (OS 非依存) ──

    #[test]
    fn resolve_utf8_locale_respects_utf8_lang() {
        assert_eq!(resolve_utf8_locale(Some("ja_JP.UTF-8")), "ja_JP.UTF-8");
        assert_eq!(resolve_utf8_locale(Some("en_GB.utf8")), "en_GB.utf8");
    }

    #[test]
    fn resolve_utf8_locale_falls_back_for_non_utf8() {
        assert_eq!(resolve_utf8_locale(None), "en_US.UTF-8");
        assert_eq!(resolve_utf8_locale(Some("C")), "en_US.UTF-8");
        assert_eq!(resolve_utf8_locale(Some("ja_JP.eucJP")), "en_US.UTF-8");
    }
}
