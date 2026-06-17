//! ターゲット解決モジュール
//!
//! 全コマンド共通のプロジェクト解決ロジックを提供する。
//! CLI 引数（target）から実行対象のプロジェクトを特定し、
//! ポート番号やプロジェクト名を解決する。

use anyhow::{Result, bail};

use crate::config::Config;

/// ターゲット解決結果
pub enum ResolvedTarget {
    /// 実行中の Process
    Running {
        port: u16,
        name: String,
        project_dir: String,
    },
    /// 設定済みだが未起動
    Configured {
        name: String,
        path: String,
        index: usize,
    },
    /// 未登録ディレクトリ（cwd から検出）
    Cwd { path: String },
}

/// target 解決
///
/// 優先順位:
/// 1. None → cwd から running.json/config を検索
/// 2. 数値文字列 → プロジェクトインデックス（後方互換、1始まり）
/// 3. 文字列 → プロジェクト名検索
pub fn resolve_target(target: Option<&str>, config: &Config) -> Result<ResolvedTarget> {
    match target {
        None => resolve_from_cwd(config),
        Some(s) => {
            if let Ok(idx) = s.parse::<usize>() {
                resolve_by_index(idx, config)
            } else {
                resolve_by_name(s, config)
            }
        }
    }
}

/// cwd からプロジェクトを解決
fn resolve_from_cwd(config: &Config) -> Result<ResolvedTarget> {
    let cwd = std::env::current_dir()?;
    let cwd_str = Config::normalize_path(&cwd);

    // 1. 稼働中 Process を検索（TheWorld API → HTTP スキャンフォールバック）
    if let Some(running) = crate::discovery::find_for_cwd_blocking() {
        let name = project_name_from_path(&running.project_dir, config);
        return Ok(ResolvedTarget::Running {
            port: running.port,
            name,
            project_dir: running.project_dir,
        });
    }

    // 2. config でプロジェクトを検索（完全一致）
    if let Some(idx) = config.find_project_index(&cwd_str) {
        let project = &config.projects[idx];
        return Ok(ResolvedTarget::Configured {
            name: project.name.clone(),
            path: cwd_str,
            index: idx,
        });
    }

    // 3. config でサブディレクトリマッチ（最も具体的なパスを優先）
    let best_match = config
        .projects
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            let normalized = Config::normalize_path(std::path::Path::new(&p.path));
            cwd_str.starts_with(&format!("{}/", normalized))
        })
        .max_by_key(|(_, p)| Config::normalize_path(std::path::Path::new(&p.path)).len());

    if let Some((idx, project)) = best_match {
        let path = Config::normalize_path(std::path::Path::new(&project.path));
        return Ok(ResolvedTarget::Configured {
            name: project.name.clone(),
            path,
            index: idx,
        });
    }

    // 4. 未登録ディレクトリ
    Ok(ResolvedTarget::Cwd { path: cwd_str })
}

/// プロジェクトインデックスから解決（1始まり、後方互換）
fn resolve_by_index(index: usize, config: &Config) -> Result<ResolvedTarget> {
    if index == 0 || index > config.projects.len() {
        bail!(
            "Invalid project index {}. Use `vp config` to list projects (1\u{2013}{}).",
            index,
            config.projects.len()
        );
    }

    let project = &config.projects[index - 1];
    let path = Config::normalize_path(std::path::Path::new(&project.path));

    // 実行中かチェック
    if let Some(running) = crate::discovery::find_by_project_blocking(&path) {
        return Ok(ResolvedTarget::Running {
            port: running.port,
            name: project.name.clone(),
            project_dir: path,
        });
    }

    Ok(ResolvedTarget::Configured {
        name: project.name.clone(),
        path,
        index: index - 1,
    })
}

/// プロジェクト名から解決
fn resolve_by_name(name: &str, config: &Config) -> Result<ResolvedTarget> {
    let found = config
        .projects
        .iter()
        .enumerate()
        .find(|(_, p)| p.name == name);

    match found {
        Some((idx, project)) => {
            let path = Config::normalize_path(std::path::Path::new(&project.path));

            // 実行中かチェック
            if let Some(running) = crate::discovery::find_by_project_blocking(&path) {
                return Ok(ResolvedTarget::Running {
                    port: running.port,
                    name: project.name.clone(),
                    project_dir: path,
                });
            }

            Ok(ResolvedTarget::Configured {
                name: project.name.clone(),
                path,
                index: idx,
            })
        }
        None => bail!(
            "Project '{}' not found. Use `vp config` to list registered projects.",
            name
        ),
    }
}

/// パスからプロジェクト名を取得（config になければディレクトリ名）
pub fn project_name_from_path(project_dir: &str, config: &Config) -> String {
    for project in &config.projects {
        let normalized = Config::normalize_path(std::path::Path::new(&project.path));
        if normalized == project_dir {
            return project.name.clone();
        }
    }

    // ディレクトリ名をフォールバック
    std::path::Path::new(project_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// 正規化済み path から登録 project 名を **SP 非依存 (config のみ)** に解決する純関数
/// (wire identity SSOT)。完全一致 → longest-prefix サブディレクトリ一致 → `None`。
///
/// `resolve_from_cwd` の config 経路だけを抜き出した純 config lookup。discovery / TheWorld を
/// 引かないので、自 project の SP が落ちていても conductor の canonical address
/// (`agent@<project>`) を確定できる。I/O なし (cwd は呼び出し側で正規化して渡す) で単体 test 可能。
///
/// 戻り値は登録 project の `name` (= [`project_name_from_path`] が登録 path に返すのと同じ値、
/// SSOT は `config.projects[].name`)。未登録 path は `None` → 呼び出し側で fail-closed する
/// (誤 identity で wire を送らない)。`project_name_from_path` と違い **basename fallback しない**
/// — basename 衝突による silent-wrong-identity を避けるため。
pub(crate) fn match_project_name_for_path(
    normalized_path: &str,
    config: &Config,
) -> Option<String> {
    // 1. 完全一致
    if let Some(idx) = config.find_project_index(normalized_path) {
        return Some(config.projects[idx].name.clone());
    }

    // 2. longest-prefix サブディレクトリ一致 (最も具体的な path を優先)
    config
        .projects
        .iter()
        .filter(|p| {
            let normalized = Config::normalize_path(std::path::Path::new(&p.path));
            normalized_path.starts_with(&format!("{}/", normalized))
        })
        .max_by_key(|p| Config::normalize_path(std::path::Path::new(&p.path)).len())
        .map(|p| p.name.clone())
}

/// FNV-1a 64-bit hash。
///
/// VP-182: `project_slug` の非 ASCII fallback で使う。 標準 `DefaultHasher` は
/// 「no stability guarantees on future Rust versions」 (std doc 明記) のため、
/// Rust バージョン更新で出力が変わると DB ディレクトリ名 (`db/sp_{slug}/`) が
/// ズレて msgbox 履歴が孤立する。 FNV-1a は仕様固定の決定的アルゴリズムなので
/// 永続データ key の安定性を保証できる。
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325; // FNV offset basis
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3); // FNV prime
    }
    hash
}

/// project を Whitesnake のディレクトリ等で使える安定 slug に変換する（VP-165 / doc 17 決定B）。
///
/// `project_name_from_path` の結果を `[a-zA-Z0-9_-]` のみに sanitize（それ以外は `_`）。
/// 安定 alnum 文字が 1 つも残らない場合（= 名前が全部非 ASCII 等）は、正規化パスの
/// hash を fallback に使う（`h{16進}`）。これで「日本語名のみの project が複数」でも
/// slug 衝突しない。port と違い reshuffle しないので、永続データの key として安全。
pub fn project_slug(project_dir: &str, config: &Config) -> String {
    let name = project_name_from_path(project_dir, config);
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // 安定 alnum が無い（空 or 全部 '_'/'-'）なら path の hash を fallback に。
    // VP-182: DefaultHasher は Rust バージョン間で出力不安定のため FNV-1a に変更。
    if !sanitized.chars().any(|c| c.is_ascii_alphanumeric()) {
        let normalized = Config::normalize_path(std::path::Path::new(project_dir));
        format!("h{:016x}", fnv1a_64(normalized.as_bytes()))
    } else {
        sanitized
    }
}

/// 空きポートを検索（バインドテストのみ）
pub fn find_available_port() -> Option<u16> {
    crate::discovery::find_available_port()
}

/// VP-165 (doc 17 決定C): project 名から SP port を解決し、新規 slot 割当なら永続化。
///
/// `Config::load()` → [`Config::resolve_sp_port`]（`port` override or `ensure_slot` → flat slot）
/// → 新規 slot 割当があれば永続化（失敗は warn のみ — port 自体は正しい）。slot は
/// 永続なので、project リスト変更でも既存 project の port は不変。project が
/// 未登録なら `Err`（caller 側で `find_available_port` 等に fallback）。
///
/// PR-D (control plane 一元化): slot の永続化先は db/world。 新規割当を
/// `world_client::notify_world_set_slot` で World daemon に通知し、 daemon が DB に書く。
/// projects.kdl は World が DB から吐く読み取り専用ミラー。 daemon 不在時は warn のみ
/// (port は正しく、 次回 SP 起動 / reconcile で同期)。
pub fn sp_port_for_project(name: &str) -> Result<u16> {
    let mut config = Config::load().unwrap_or_default();
    let had_slot = config.resolve_slot_by_name(name).is_some();
    let port = config.resolve_sp_port(name)?;
    // PR-D: 新規 slot 割当を daemon (db/world 真実源) に永続化通知する。 slot 計算は config ミラーで
    // 完結し、 永続化のみ daemon 経由 (HTTP best-effort)。 daemon 不在は warn のみ (port は正しい)。
    if !had_slot && let Some(slot) = config.resolve_slot_by_name(name) {
        let key = config
            .projects
            .iter()
            .find(|p| p.name == name)
            .map(|p| Config::normalize_path(std::path::Path::new(&p.path)));
        if let Some(k) = key
            && !crate::world_client::notify_world_set_slot(&k, slot)
        {
            tracing::warn!("VP-165: slot の daemon 永続化に失敗 (port={port} は正しい)");
        }
    }
    Ok(port)
}

/// Configured ターゲットのポートを決定（VP-165 (doc 17 決定C): flat stable slot 方式）
///
/// `index` から project 名を引いて [`sp_port_for_project`] に委譲。slot は config 永続なので
/// project リスト変更でも既存 project の port は不変（旧: `PORT_RANGE_START + index` の位置依存
/// で、 project 追加/削除のたびに全 SP の port が雪崩シフトしていた = VP-165 の根）。
pub fn port_for_configured(index: usize, config: &Config) -> Result<u16> {
    let name = config
        .projects
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("project index {} out of range", index))?
        .name
        .clone();
    sp_port_for_project(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_slug_ascii_name_passthrough() {
        let cfg = Config::default();
        assert_eq!(
            project_slug("/Users/x/repos/vantage-point", &cfg),
            "vantage-point"
        );
        assert_eq!(
            project_slug("/Users/x/repos/creo_memories", &cfg),
            "creo_memories"
        );
    }

    #[test]
    fn project_slug_sanitizes_non_alnum() {
        let cfg = Config::default();
        // basename "my project!" → '_' 置換
        assert_eq!(project_slug("/tmp/my project!", &cfg), "my_project_");
        // ドット等も '_' に
        assert_eq!(project_slug("/tmp/a.b.c", &cfg), "a_b_c");
    }

    #[test]
    fn project_slug_all_non_ascii_falls_back_to_path_hash() {
        let cfg = Config::default();
        let s1 = project_slug("/tmp/日本語", &cfg);
        let s2 = project_slug("/tmp/にほんご", &cfg);
        // hash fallback: "h" + 16 hex
        assert!(s1.starts_with('h') && s1.len() == 17, "got {s1}");
        assert!(s2.starts_with('h') && s2.len() == 17, "got {s2}");
        // 別 path なら別 slug（衝突しない）
        assert_ne!(s1, s2);
        // 同 path は安定
        assert_eq!(project_slug("/tmp/日本語", &cfg), s1);
    }

    // --- match_project_name_for_path (wiremsg identity SSOT) ---

    fn cfg_with(projects: &[(&str, &str)]) -> Config {
        use crate::config::ProjectConfig;
        Config {
            projects: projects
                .iter()
                .map(|(name, path)| ProjectConfig {
                    name: name.to_string(),
                    path: path.to_string(),
                    port: None,
                    enabled: true,
                    slot: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn match_project_exact_root() {
        // 存在しない絶対 path は normalize_path が lexical fallback するので test 安定。
        let cfg = cfg_with(&[("club-unison", "/Users/x/repos/club-unison")]);
        assert_eq!(
            match_project_name_for_path("/Users/x/repos/club-unison", &cfg).as_deref(),
            Some("club-unison")
        );
    }

    #[test]
    fn match_project_longest_prefix_subdir() {
        // conductor は subdir から動く → longest-prefix で親 project を引く
        let cfg = cfg_with(&[("club-unison", "/Users/x/repos/club-unison")]);
        assert_eq!(
            match_project_name_for_path("/Users/x/repos/club-unison/clients/typescript", &cfg)
                .as_deref(),
            Some("club-unison")
        );
    }

    #[test]
    fn match_project_picks_most_specific() {
        // nested 登録: より深い (具体的) path を優先
        let cfg = cfg_with(&[
            ("outer", "/Users/x/repos/outer"),
            ("inner", "/Users/x/repos/outer/inner"),
        ]);
        assert_eq!(
            match_project_name_for_path("/Users/x/repos/outer/inner/sub", &cfg).as_deref(),
            Some("inner")
        );
    }

    #[test]
    fn match_project_unregistered_is_none() {
        // 未登録 cwd → None (= conductor fail-closed)。sibling 名前共有 (basename 衝突) でも誤マッチしない
        let cfg = cfg_with(&[("vp", "/Users/x/repos/vp")]);
        assert_eq!(
            match_project_name_for_path("/Users/x/repos/other", &cfg),
            None
        );
        // basename だけ一致する別 root は prefix マッチしない (basename fallback しない証明)
        assert_eq!(match_project_name_for_path("/tmp/elsewhere/vp", &cfg), None);
    }

    #[test]
    fn match_project_uses_config_name_not_basename() {
        // name != basename (vp projects rename 済) でも config 名を返す
        let cfg = cfg_with(&[("renamed-name", "/Users/x/repos/orig-dir")]);
        assert_eq!(
            match_project_name_for_path("/Users/x/repos/orig-dir", &cfg).as_deref(),
            Some("renamed-name")
        );
    }
}
