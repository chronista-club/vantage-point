//! Stand metadata reader — `.mise/tasks/vp/stand/{name}` task ファイル冒頭の
//! `#VP key=value` コメント行を parse して構造化する (VP-108 / doc 11 follow-up #2、
//! VP-110 で `layer` → `tier` rename)。
//!
//! ## なぜ自前 parser か
//!
//! mise は `#MISE description=...` のような `#MISE` prefix のみ解釈する。 VP 固有の
//! metadata (例: `tier`、 `icon`) は VP が責任を持って読む必要があり、 task ファイルを
//! 直接読んで自前 parse する。
//!
//! ## 期待する書式
//!
//! ```text
//! #!/usr/bin/env bash
//! #MISE description="VP Stand: Heaven's Door — Claude TUI auto-launch"
//! #VP icon="📖"
//! #VP tier=2
//! ```
//!
//! → `StandMetadata { tier: 2, icon: Some("📖") }`
//!
//! ## 用語注 (LSCM Layer との衝突回避、 doc 12 §0 Glossary 参照)
//!
//! `tier` は **PTY 階層** を表し、 LSCM の Layer (World/Project/Lane = address-bearing
//! container) とは別概念。 旧 `#VP layer=N` は VP-110 で `#VP tier=N` に rename された。
//!
//! ## forward-compat
//!
//! 不明 key は静かに無視、 値の parse 失敗 (例: `tier=foo`) も静かに無視 (default 維持)。
//! task ファイル作者が将来 `#VP capabilities=...` 等を実験的に書いても、 古い VP バイナリで
//! panic にならない (Postel の法則)。

use std::path::Path;

/// Stand 1 つ分の VP 固有 metadata。
///
/// **tier の意味** (VP-108、 PR #260 後継、 VP-110 で `layer` → `tier` rename):
/// - 0 = bare login shell (tmux 経由なし、 PtySlot scrollback dump を replay する)
/// - 1 = tmux session attached (LLM なし)
/// - 2 = LLM TUI in tmux
///
/// `tier >= 1` で「tmux 経由 lane」 と判定し、 `/ws/terminal` attach 時に
/// PtySlot scrollback dump を skip する (tmux の自動 redraw に委譲)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StandMetadata {
    /// `#VP tier=N` (省略時 0)
    pub tier: u8,
    /// `#VP icon="..."` (省略時 None)
    pub icon: Option<String>,
}

impl StandMetadata {
    /// task script 文字列から `#VP key=value` 行を集めて構築。
    ///
    /// - `#VP ` prefix (空白必須) で始まる行のみ対象 — `#VPx` 等の false-positive を排除
    /// - 値の前後 `"` `'` を strip (シェル風 quote 表現を許容)
    /// - 不明 key は無視、 parse 失敗の値も無視 (forward-compat)
    pub fn from_script(text: &str) -> Self {
        let mut out = Self::default();
        for line in text.lines() {
            let trimmed = line.trim_start();
            let Some(rest) = trimmed.strip_prefix("#VP ") else {
                continue;
            };
            let Some((key, value)) = rest.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim().trim_matches(|c: char| c == '"' || c == '\'');
            match key {
                "tier" => {
                    if let Ok(n) = value.parse::<u8>() {
                        out.tier = n;
                    }
                }
                "icon" => {
                    out.icon = Some(value.to_string());
                }
                _ => {} // 不明 key は無視 (forward-compat)
            }
        }
        out
    }

    /// VP install root + stand 名から task ファイルを読んで parse。
    ///
    /// I/O エラー (file 不在 / 権限なし等) は default (= tier=0、 icon=None) を返し、
    /// caller は graceful degrade する設計。 file が無い stand 名を渡された場合も
    /// default が返るので、 caller 側で「tier=0 として扱う」 = 「scrollback replay する」 が
    /// 安全側の挙動になる。
    pub fn from_install_root(install_root: &Path, name: &str) -> Self {
        let path = install_root
            .join(".mise")
            .join("tasks")
            .join("vp")
            .join("stand")
            .join(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_script(&text),
            Err(e) => {
                tracing::debug!(
                    "stand_metadata: {} 読み込み失敗 ({})、 default を返す",
                    path.display(),
                    e
                );
                Self::default()
            }
        }
    }

    /// 「tmux 経由で render される lane」 か。
    ///
    /// `tier >= 1` を条件にする。 `ws_terminal.rs` の attach 時 scrollback skip 判定 +
    /// 将来の同種分岐 (例: resize 戦略の差し替え) で使う。
    pub fn is_tmux_hosted(&self) -> bool {
        self.tier >= 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_script_hd_tier2_with_icon() {
        let script = r#"#!/usr/bin/env bash
#MISE description="VP Stand: Heaven's Door"
#VP icon="📖"
#VP tier=2

set -euo pipefail
"#;
        let m = StandMetadata::from_script(script);
        assert_eq!(m.tier, 2);
        assert_eq!(m.icon.as_deref(), Some("📖"));
        assert!(m.is_tmux_hosted());
    }

    #[test]
    fn from_script_tmux_tier1() {
        let script = "#VP tier=1\n#VP icon=\"🎭\"\n";
        let m = StandMetadata::from_script(script);
        assert_eq!(m.tier, 1);
        assert_eq!(m.icon.as_deref(), Some("🎭"));
        assert!(m.is_tmux_hosted());
    }

    #[test]
    fn from_script_shell_no_vp_metadata() {
        let script = r#"#!/usr/bin/env bash
#MISE description="VP Stand: bare login shell"

exec ${SHELL:-/bin/zsh} -l
"#;
        let m = StandMetadata::from_script(script);
        assert_eq!(m.tier, 0);
        assert_eq!(m.icon, None);
        assert!(!m.is_tmux_hosted());
    }

    #[test]
    fn from_script_empty() {
        let m = StandMetadata::from_script("");
        assert_eq!(m, StandMetadata::default());
    }

    #[test]
    fn from_script_invalid_tier_value_ignored() {
        // 不正値は静かに無視 (forward-compat)、 default 維持
        let script = "#VP tier=foo\n#VP icon=\"x\"\n";
        let m = StandMetadata::from_script(script);
        assert_eq!(m.tier, 0);
        assert_eq!(m.icon.as_deref(), Some("x"));
    }

    #[test]
    fn from_script_unknown_key_ignored() {
        // 未知 key は無視するが、 既知 key の処理は止めない (forward-compat)
        let script = "#VP capabilities=foo,bar\n#VP tier=2\n";
        let m = StandMetadata::from_script(script);
        assert_eq!(m.tier, 2);
        assert_eq!(m.icon, None);
    }

    #[test]
    fn from_script_order_independent() {
        let a = StandMetadata::from_script("#VP icon=\"a\"\n#VP tier=2\n");
        let b = StandMetadata::from_script("#VP tier=2\n#VP icon=\"a\"\n");
        assert_eq!(a, b);
    }

    #[test]
    fn from_script_quote_variants() {
        // 二重引用符 / 単引用符 / quote なしを全部許容
        let dq = StandMetadata::from_script("#VP icon=\"📖\"\n");
        let sq = StandMetadata::from_script("#VP icon='📖'\n");
        let nq = StandMetadata::from_script("#VP icon=📖\n");
        assert_eq!(dq.icon.as_deref(), Some("📖"));
        assert_eq!(sq.icon.as_deref(), Some("📖"));
        assert_eq!(nq.icon.as_deref(), Some("📖"));
    }

    #[test]
    fn from_script_vp_prefix_strict() {
        // `#VPxxx` のような prefix は対象外 (`#VP ` の空白を要求)
        let m = StandMetadata::from_script("#VPtier=2\n#VPx tier=2\n");
        assert_eq!(m.tier, 0);
    }

    #[test]
    fn from_script_indented_lines_accepted() {
        // 行頭の空白は trim_start で許容 (実用上 indent された script を想定)
        let m = StandMetadata::from_script("    #VP tier=2\n");
        assert_eq!(m.tier, 2);
    }

    #[test]
    fn from_install_root_reads_existing_hd() {
        // CARGO_MANIFEST_DIR の親 2 つ上 = workspace root に
        // .mise/tasks/vp/stand/hd が居る (PR-A で導入済)
        let manifest = env!("CARGO_MANIFEST_DIR");
        let workspace_root = std::path::PathBuf::from(manifest)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let m = StandMetadata::from_install_root(&workspace_root, "echoes");
        assert_eq!(m.tier, 2, "echoes は tier=2 のはず: {:?}", m);
        assert!(m.icon.is_some(), "echoes は icon があるはず: {:?}", m);
    }

    #[test]
    fn from_install_root_reads_existing_tmux() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let workspace_root = std::path::PathBuf::from(manifest)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let m = StandMetadata::from_install_root(&workspace_root, "tmux");
        assert_eq!(m.tier, 1, "tmux は tier=1 のはず: {:?}", m);
    }

    #[test]
    fn from_install_root_reads_existing_shell_no_vp_metadata() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let workspace_root = std::path::PathBuf::from(manifest)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let m = StandMetadata::from_install_root(&workspace_root, "shell");
        assert_eq!(m.tier, 0, "shell は tier 不在 = 0 のはず: {:?}", m);
        assert!(!m.is_tmux_hosted());
    }

    #[test]
    fn from_install_root_missing_file_returns_default() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let workspace_root = std::path::PathBuf::from(manifest)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let m = StandMetadata::from_install_root(&workspace_root, "nonexistent-stand-xyz");
        // graceful degrade: default = tier 0、 = scrollback replay 側 (安全側挙動)
        assert_eq!(m, StandMetadata::default());
        assert!(!m.is_tmux_hosted());
    }
}
