//! spawn する子プロセス用の PATH 補強 — 補正の単一の入口 (SSOT)
//!
//! ## なぜ必要か
//!
//! vp-app (`.app`) を GUI / launchd / Dock 経由で起動すると、 プロセスの PATH が
//! `/usr/bin:/bin:/usr/sbin:/sbin` の最小集合になる。 この痩せた PATH は spawn chain
//! (vp-app → daemon → SP → mise → tmux → claude) を伝播し、 user-installed tool
//! (特に `mise`、 conductor lane = `mise run vp:stand:echoes` の program) を見つけられず
//! spawn が失敗 → lane が即 Dead 化 → Echoes コンソールが出ない、 という症状の根因になる。
//!
//! ## 方針 — 補正は「spawn の最上流」で一度
//!
//! 痩せた PATH を末端 (pty_slot) だけで補正しても、 その手前の daemon / SP プロセス
//! 自身の PATH が痩せたままだと別経路 (例: `collect_mise_env` の `mise` 解決) で破綻する。
//! #498 / #501 はこの末端 1 箇所を対症療法したが再発した。 そこで
//! **プロセスを spawn する全経路 (daemon spawn / SP spawn) で、 子に渡す PATH をここで
//! 補強する**。 子プロセスの PATH が augmented になれば、 その下の全 spawn が正しい PATH を
//! 継承する。 末端 pty_slot の補正は二重保険に降格する。
//!
//! ## SSOT
//!
//! この純関数 [`augment_path`] が PATH 補強の唯一の定義。 crate 境界の都合で
//! vp-app (`daemon_launcher.rs`) は同一ロジックを複製しているが、 変更時は両者を
//! 同期させること (`vp-app/src/paths.rs` の `migrate_legacy_paths` 複製と同じ慣習)。

/// 既知の user tool location。 base PATH に無いものだけを先頭に前置する。
/// 順序 = PATH 探索の優先順位 (mise 本体 → mise shims → cargo → homebrew → usr-local)。
fn user_tool_prefixes(home: Option<&str>) -> Vec<String> {
    let mut prefixes: Vec<String> = Vec::new();
    if let Some(home) = home {
        prefixes.push(format!("{home}/.local/bin")); // mise 本体 / user-installed tools
        prefixes.push(format!("{home}/.local/share/mise/shims")); // mise 管理ツール (claude 等) の shim
        prefixes.push(format!("{home}/.cargo/bin")); // vp 自身 / rust tools
    }
    prefixes.push("/opt/homebrew/bin".to_string()); // Apple Silicon homebrew
    prefixes.push("/usr/local/bin".to_string()); // Intel homebrew / 一般
    prefixes
}

/// `base_path` に user tool location を前置して補強した PATH 文字列を返す。
///
/// base に既に含まれる prefix は重複させない (PATH 肥大化を避ける)。
/// 純関数 (env / fs に触れない) なので test しやすい。
pub fn augment_path(base_path: &str, home: Option<&str>) -> String {
    let base_segments: std::collections::HashSet<&str> = base_path.split(':').collect();
    let new_prefixes: Vec<String> = user_tool_prefixes(home)
        .into_iter()
        .filter(|p| !base_segments.contains(p.as_str()))
        .collect();
    match (new_prefixes.is_empty(), base_path.is_empty()) {
        (true, _) => base_path.to_string(),
        (false, true) => new_prefixes.join(":"),
        (false, false) => format!("{}:{}", new_prefixes.join(":"), base_path),
    }
}

/// 現プロセスの PATH (`$PATH`) を base に補強した文字列を返す。
///
/// 子プロセスを spawn する箇所で `Command::env("PATH", augmented_spawn_path())` の形で
/// 使う、 補正の唯一の入口。 痩せた PATH が spawn chain を伝播するのをここで断つ。
pub fn augmented_spawn_path() -> String {
    let base = std::env::var("PATH").unwrap_or_default();
    let home = std::env::var("HOME").ok();
    augment_path(&base, home.as_deref())
}

/// 子プロセス（特に launchd 起動 daemon → SP → tmux）に渡す UTF-8 ロケールを返す。
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

    #[test]
    fn prepends_user_tool_locations_to_minimal_path() {
        // GUI/launchd の最小 PATH に user tool location が優先順位どおり前置される。
        let r = augment_path("/usr/bin:/bin", Some("/Users/x"));
        assert_eq!(
            r,
            "/Users/x/.local/bin:/Users/x/.local/share/mise/shims:/Users/x/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
        );
    }

    #[test]
    fn skips_prefixes_already_present() {
        // base に既存の prefix は重複させない (PATH 肥大化を避ける)。
        let r = augment_path("/opt/homebrew/bin:/usr/bin", Some("/Users/x"));
        assert_eq!(
            r,
            "/Users/x/.local/bin:/Users/x/.local/share/mise/shims:/Users/x/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin"
        );
    }

    #[test]
    fn builds_from_prefixes_when_base_empty() {
        // base が空でも prefix だけで PATH を構築できる。
        let r = augment_path("", Some("/Users/x"));
        assert_eq!(
            r,
            "/Users/x/.local/bin:/Users/x/.local/share/mise/shims:/Users/x/.cargo/bin:/opt/homebrew/bin:/usr/local/bin"
        );
    }

    #[test]
    fn without_home_only_system_prefixes() {
        // HOME 不明なら home 系 prefix は前置しない (homebrew / usr-local のみ)。
        let r = augment_path("/usr/bin", None);
        assert_eq!(r, "/opt/homebrew/bin:/usr/local/bin:/usr/bin");
    }

    #[test]
    fn all_prefixes_present_returns_base_unchanged() {
        // 全 prefix が既に base にあるなら base をそのまま返す (= 健全な開発 shell)。
        let base = "/Users/x/.local/bin:/Users/x/.local/share/mise/shims:/Users/x/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin";
        assert_eq!(augment_path(base, Some("/Users/x")), base);
    }

    #[test]
    fn resolve_utf8_locale_respects_utf8_lang() {
        // UTF-8 codeset の LANG はそのまま尊重する (ja_JP 派の設定を壊さない)。
        assert_eq!(resolve_utf8_locale(Some("ja_JP.UTF-8")), "ja_JP.UTF-8");
        assert_eq!(resolve_utf8_locale(Some("en_US.UTF-8")), "en_US.UTF-8");
        assert_eq!(resolve_utf8_locale(Some("en_GB.utf8")), "en_GB.utf8");
    }

    #[test]
    fn resolve_utf8_locale_falls_back_for_non_utf8() {
        // 未設定 / C / POSIX / 非 UTF-8 codeset は en_US.UTF-8 に倒す (tmux utf8=1 を保証)。
        assert_eq!(resolve_utf8_locale(None), "en_US.UTF-8");
        assert_eq!(resolve_utf8_locale(Some("C")), "en_US.UTF-8");
        assert_eq!(resolve_utf8_locale(Some("POSIX")), "en_US.UTF-8");
        assert_eq!(resolve_utf8_locale(Some("ja_JP.eucJP")), "en_US.UTF-8");
    }
}
