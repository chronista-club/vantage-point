//! `vp config` コマンドの実行ロジック

use anyhow::Result;

use crate::config::Config;

/// `vp config` を実行
///
/// daemon 接続時は API からrepo 一覧を取得。
/// 未接続時は config / repos.kdl にフォールバック。
pub fn execute(config: &Config) -> Result<()> {
    println!("Config file: {}", Config::config_path().display());
    println!();

    // daemon API からrepo 一覧を取得（フォールバック: repos.kdl）
    let (repos, source) = match fetch_repos_from_thedaemon() {
        Some(repos) => (repos, "daemon API"),
        None => {
            let repos: Vec<(String, String)> = config
                .repos
                .iter()
                .map(|p| (p.name.clone(), p.path.clone()))
                .collect();
            (repos, "repos.kdl (daemon offline)")
        }
    };

    println!("Source: {}", source);
    println!();

    if repos.is_empty() {
        println!("No repos registered.");
    } else {
        // 稼働中プロセスを取得
        let running = fetch_running_processes();

        println!("Registered repos:");
        println!("  #  NAME                STATUS    PATH");
        println!("  ─  ────                ──────    ────");
        for (i, (name, path)) in repos.iter().enumerate() {
            let status = if running.iter().any(|r| r == path) {
                "●"
            } else {
                "○"
            };
            let path_display = if path.len() > 40 {
                format!("...{}", &path[path.len() - 37..])
            } else {
                path.clone()
            };
            println!("  {}  {:18}  {:>6}   {}", i + 1, name, status, path_display);
        }
        println!();
        println!("● = repo running, ○ = stopped");
    }

    Ok(())
}

/// daemon からrepo 一覧を取得（Unison `daemon-control.repos/list`）
///
/// doc 45 段 2: 旧 `GET /api/daemon/repos` から差し替え。daemon 不在は None で、
/// caller が repos.kdl フォールバックに落とす（従来どおり）。
fn fetch_repos_from_thedaemon() -> Option<Vec<(String, String)>> {
    crate::daemon_client::list_repos_blocking()
}

/// daemon から稼働中プロセスのパス一覧を取得（Unison `registry.list`）
///
/// doc 45 段 2: 旧 `GET /api/daemon/processes` から差し替え。daemon 不在は空 Vec
/// （= 全 repo が「停止」表示。表示系なので落とさない）。
fn fetch_running_processes() -> Vec<String> {
    crate::daemon_client::list_processes_blocking()
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p.get("repo_path")?.as_str().map(String::from))
        .collect()
}

// ───────────────────────────────────────────────────────────────────────────
// user 設定（settings.kdl）の読み書き — doc 59 P2
// ───────────────────────────────────────────────────────────────────────────

/// CLI から触れる設定 key（`vp config set <key> <value>`）。
///
/// 表記は **settings.kdl の node 名と同じ kebab-case** に揃える — user が file を開いた
/// ときに CLI で打った語がそのまま見える（wire の snake_case は内部の都合なので隠す）。
const SETTABLE_KEYS: [(&str, KeyKind, &str); 2] = [
    (
        "log-level",
        KeyKind::Str,
        "ログ詳細度（trace|debug|info|warn|error）。空文字で未設定に戻す。反映には daemon 再起動が要る",
    ),
    (
        "idle-timeout-minutes",
        KeyKind::Uint,
        "アイドルとみなすまでの分数（既定 5）。now-line の「⏸N分」と engine 停止の両方に効く",
    ),
];

/// 設定値の型。**CLI は文字列しか受け取れない**ので、wire に載せる前にここで変換する
/// （数値 key に文字列を送ると daemon 側で「不正」と弾かれ、原因が分かりにくい）。
#[derive(Debug, Clone, Copy, PartialEq)]
enum KeyKind {
    Str,
    Uint,
}

/// kebab-case の CLI key → wire の snake_case field 名と型。
fn wire_field_of(key: &str) -> Option<(&'static str, KeyKind)> {
    match key {
        "log-level" => Some(("log_level", KeyKind::Str)),
        "idle-timeout-minutes" => Some(("idle_timeout_minutes", KeyKind::Uint)),
        _ => None,
    }
}

/// CLI の文字列値を wire に載せる JSON へ。**空文字は「未設定に戻す」**なので型に関係なく
/// 空文字のまま送る（daemon 側が消し方として解釈する）。
fn wire_value(kind: KeyKind, raw: &str) -> Result<serde_json::Value> {
    let v = raw.trim();
    if v.is_empty() {
        return Ok(serde_json::Value::String(String::new()));
    }
    match kind {
        KeyKind::Str => Ok(serde_json::Value::String(v.to_string())),
        KeyKind::Uint => v
            .parse::<u64>()
            .map(|n| serde_json::json!(n))
            .map_err(|_| anyhow::anyhow!("数値で指定してください: {raw:?}")),
    }
}

/// `vp config` の subcommand（user 設定 = 「好み」の層、doc 59）。
#[derive(clap::Subcommand, Debug)]
pub enum ConfigCommands {
    /// user 設定（settings.kdl）を表示
    Get,
    /// user 設定を書き換える（daemon が書く）
    Set {
        /// 設定 key（例: log-level）
        key: String,
        /// 値。**空文字を渡すと未設定に戻る**（= 組み込み既定に倒れる）
        value: String,
    },
}

/// CLI key を wire field へ解決する。未知なら使える key を添えて Err。
///
/// ⚠️ **daemon に投げる前に弾く**ため純関数として切ってある。handler は知らない field を
/// 無視するので、打ち間違いをそのまま送ると **成功に見える no-op** になる
/// （「設定したのに効かない」）。
fn resolve_key(key: &str) -> Result<(&'static str, KeyKind)> {
    wire_field_of(key).ok_or_else(|| {
        let known = SETTABLE_KEYS
            .iter()
            .map(|(k, _, _)| *k)
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::anyhow!("未知の設定 key: {key:?}（使えるのは: {known}）")
    })
}

/// `vp config get|set` を実行（daemon に頼む — 書き手は daemon 唯一、doc 59 §3）。
pub async fn execute_settings(cmd: ConfigCommands) -> Result<()> {
    // ⚠️ key の検証は **接続の前**。後ろに置くと、daemon が落ちているときに
    // 「接続できません」しか出ず、typo に気づけない（原因が 2 つあるのに 1 つしか見えない）。
    let target = match &cmd {
        ConfigCommands::Set { key, value } => {
            let (field, kind) = resolve_key(key)?;
            // 値の型変換も接続前に済ませる（数値 key に文字を打った場合も daemon 不要で分かる）。
            Some((field, wire_value(kind, value)?))
        }
        ConfigCommands::Get => None,
    };

    let client = crate::daemon::client::DaemonControlClient::connect(crate::cli::daemon_port(), 3)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "daemon (port {}) に接続できません。 `vp daemon start` で起動してください: {}",
                crate::cli::daemon_port(),
                e
            )
        })?;

    match cmd {
        ConfigCommands::Get => {
            let settings = client.settings_get().await?;
            print_settings(&settings);
        }
        ConfigCommands::Set { .. } => {
            let (field, value) = target.expect("Set なら上で解決済み");
            let settings = client
                .settings_set(serde_json::json!({ field: value }))
                .await?;
            print_settings(&settings);
            println!();
            println!("⚠️ 反映には daemon の再起動が要ります: vp daemon restart");
        }
    }
    Ok(())
}

/// 設定の確定値を表示する。**未設定の key も一覧に出す**（何が設定できるかが分かるように）。
fn print_settings(settings: &serde_json::Value) {
    println!(
        "Settings file: {}",
        crate::settings_file::settings_file_path().display()
    );
    println!();
    for (key, _, help) in SETTABLE_KEYS {
        // 値は型がまちまち（文字列 / 数値）なので、表示は JSON の素の形をそのまま使う。
        let value = wire_field_of(key)
            .and_then(|(f, _)| settings.get(f))
            .map(|v| match v.as_str() {
                Some(s) => s.to_string(),
                None => v.to_string(),
            });
        match value {
            Some(v) => println!("  {key} = {v}"),
            None => println!("  {key} = (未設定)"),
        }
        println!("      {help}");
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    #[test]
    fn known_key_resolves_to_wire_field() {
        // CLI は kebab-case（settings.kdl の node 名と同じ）、wire は snake_case。
        assert_eq!(resolve_key("log-level").unwrap().0, "log_level");
    }

    #[test]
    fn unknown_key_is_rejected_with_the_list_of_valid_keys() {
        // ⚠️ 打ち間違いが no-op にならないことの担保。エラー文に使える key を必ず添える
        // （「何が使えるか」を出さないと、user は次に何を打てばいいか分からない）。
        let err = resolve_key("log_level").unwrap_err().to_string();
        assert!(err.contains("log_level"), "打った key を出す: {err}");
        assert!(err.contains("log-level"), "正しい綴りを出す: {err}");
    }

    #[test]
    fn every_settable_key_resolves() {
        // SETTABLE_KEYS（表示用）と wire_field_of（送信用）が食い違うと、
        // help には出るのに設定できない key が生まれる。両者を突き合わせて固定する。
        for (key, kind, _) in SETTABLE_KEYS {
            let resolved = wire_field_of(key).unwrap_or_else(|| panic!("解決できない key: {key}"));
            // ⚠️ 型も突き合わせる。表 (SETTABLE_KEYS) と変換 (wire_field_of) で型がズレると、
            // 数値 key に文字列を送って daemon 側で弾かれる（CLI 側では気づけない）。
            assert_eq!(resolved.1, kind, "型が食い違う key: {key}");
        }
    }
}
