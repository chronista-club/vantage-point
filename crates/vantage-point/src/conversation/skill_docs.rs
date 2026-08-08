//! slash command の**説明文**を filesystem から拾う（doc 57 / chat の補完）。
//!
//! ## なぜ filesystem を読むのか
//!
//! `system/init` は `slash_commands[]` を **素の文字列配列**でしか広告しない（実測 2026-08-08:
//! `skills[]` も `["gitbutler", "gitnexus-cli", …]` で、`description` も `argument-hint` も
//! 載っていない）。補完の候補に「何をするコマンドか」を出すには、SKILL.md の frontmatter を
//! 自分で読むしかない。
//!
//! ## ⚠️ 候補の**源**にはしない
//!
//! 一覧の正はあくまで `slash_commands[]`（CLI が「対話端末なしで打てるもの」に絞り込み済み）。
//! ここが返すのは**装飾**で、引けなければ何も出さないだけ。逆にすると「一覧にあるのに打てない」
//! を自分で作ることになる（公式 agent-sdk/slash-commands の保証を捨てる）。
//!
//! ## ⚠️ 外部レイアウトへの依存はこの module に閉じる
//!
//! `~/.claude/plugins/cache/<repo>/<plugin>/<ver>/skills/<name>/SKILL.md` のような**他所の
//! 都合で変わる形**を知っているのはここだけ。実測（2026-08-08）で 273 file・全てに
//! `description` があり、候補 160 個のうち **86 個**が引けた。残りは CLI 組み込みなどで
//! filesystem に実体が無い = **説明が無い候補は普通に混ざる**前提で UI を組むこと。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 説明の最大長。候補行に添えるだけなので 1 行に収まる長さで切る。
const MAX_LEN: usize = 96;

/// frontmatter から `description` を取り出して 1 行に整える。
///
/// ⚠️ **quote を剥がす**。YAML は `description: "..."` も `'...'` も許すので、剥がさないと
/// 説明が全部 `"` で始まる（実測で踏んだ）。改行と連続空白も潰す — 候補行は 1 行だから。
pub fn parse_description(text: &str) -> Option<String> {
    let raw = text
        .lines()
        .take(60) // frontmatter は先頭。本文まで舐めない（本文の `description:` を拾わない）
        .find_map(|l| l.strip_prefix("description:"))?
        .trim();
    let unquoted = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(raw);
    let flat = unquoted.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    Some(truncate(&flat, MAX_LEN))
}

/// 文字境界で切って `…` を付ける。⚠️ `&s[..n]` は日本語で panic するので使わない。
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// SKILL.md / command.md を探す場所（user / repo / plugin cache）。
fn sources(repo: &Path, home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for base in [home.join(".claude"), repo.join(".claude")] {
        push_glob(&mut out, &base.join("skills"), 1, "SKILL.md");
        push_glob(&mut out, &base.join("commands"), 0, "");
    }
    // plugin は cache/<repo>/<plugin>/<ver>/skills/<name>/SKILL.md（実測レイアウト）。
    let cache = home.join(".claude/plugins/cache");
    if let Ok(repos) = std::fs::read_dir(&cache) {
        for r in repos.flatten() {
            if let Ok(plugins) = std::fs::read_dir(r.path()) {
                for p in plugins.flatten() {
                    if let Ok(vers) = std::fs::read_dir(p.path()) {
                        for v in vers.flatten() {
                            push_glob(&mut out, &v.path().join("skills"), 1, "SKILL.md");
                        }
                    }
                }
            }
        }
    }
    out
}

/// `dir` 直下を 1 段舐める。`depth=1` は `dir/<name>/<file>`、`depth=0` は `dir/*.md`。
fn push_glob(out: &mut Vec<PathBuf>, dir: &Path, depth: u8, file: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if depth == 1 {
            let f = p.join(file);
            if f.is_file() {
                out.push(f);
            }
        } else if p.extension().is_some_and(|x| x == "md") {
            out.push(p);
        }
    }
}

/// slash command 名 → 説明。**bare 名と `plugin:name` の両方**を鍵にする。
///
/// ⚠️ `slash_commands[]` には両形が混在する（実測で `code-review` と
/// `code-review:code-review` が両方いた）ので、どちらで引かれても当たるようにする。
///
/// ⚠️ **同期 I/O**（数百 file）。呼び手は engine 起動時に 1 回だけ、`spawn_blocking` の中で使う。
pub fn skill_descriptions(repo: &Path) -> HashMap<String, String> {
    let Some(home) = dirs::home_dir() else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    for path in sources(repo, &home) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(desc) = parse_description(&text) else {
            continue;
        };
        // skills/<name>/SKILL.md は親が名前、commands/<name>.md は stem が名前。
        let name = if path.file_name().is_some_and(|f| f == "SKILL.md") {
            path.parent().and_then(|p| p.file_name())
        } else {
            path.file_stem()
        }
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
        if name.is_empty() {
            continue;
        }
        if let Some(plugin) = plugin_of(&path) {
            map.insert(format!("{plugin}:{name}"), desc.clone());
        }
        map.entry(name).or_insert(desc);
    }
    map
}

/// plugin cache 配下なら plugin 名（`cache/<repo>/<plugin>/<ver>/…` の `<plugin>`）。
fn plugin_of(path: &Path) -> Option<String> {
    let parts: Vec<_> = path.iter().map(|s| s.to_string_lossy()).collect();
    let at = parts.iter().position(|p| p == "cache")?;
    parts.get(at + 2).map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_description_from_frontmatter() {
        let md = "---\nname: foo\ndescription: Do the thing\n---\n\n# body\n";
        assert_eq!(parse_description(md).as_deref(), Some("Do the thing"));
    }

    /// ⚠️ quote を剥がさないと説明が全部 `"` で始まる（実測で踏んだ）。
    #[test]
    fn strips_quotes() {
        assert_eq!(
            parse_description("description: \"Commit, push\"\n").as_deref(),
            Some("Commit, push")
        );
        assert_eq!(
            parse_description("description: 'Commit, push'\n").as_deref(),
            Some("Commit, push")
        );
    }

    /// 候補行は 1 行なので、改行や連続空白は潰す。
    #[test]
    fn flattens_whitespace() {
        assert_eq!(
            parse_description("description:   a    b \n").as_deref(),
            Some("a b")
        );
    }

    /// ⚠️ 日本語で切っても panic しない（`&s[..n]` は文字境界を割る）。
    #[test]
    fn truncates_on_char_boundary() {
        let long = "あ".repeat(200);
        let out = parse_description(&format!("description: {long}\n")).expect("説明");
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), MAX_LEN + 1);
    }

    #[test]
    fn no_description_is_none() {
        assert!(parse_description("---\nname: foo\n---\n").is_none());
        assert!(parse_description("description:   \n").is_none());
    }

    /// 本文中の `description:` を拾わない（frontmatter は先頭にある）。
    #[test]
    fn ignores_body_far_below() {
        let md = format!(
            "---\nname: foo\n---\n{}\ndescription: body one\n",
            "x\n".repeat(80)
        );
        assert!(parse_description(&md).is_none());
    }

    #[test]
    fn plugin_name_comes_from_cache_layout() {
        let p = Path::new("/h/.claude/plugins/cache/myrepo/myplugin/1.0.0/skills/foo/SKILL.md");
        assert_eq!(plugin_of(p).as_deref(), Some("myplugin"));
        assert!(plugin_of(Path::new("/h/.claude/skills/foo/SKILL.md")).is_none());
    }
}
