//! vp-app の session 状態永続化 ─ 起動を跨いで復元する UI state。
//!
//! `Settings` (vp-app.toml、 ユーザー preference) と分離。 こちらは
//! 「直前の作業文脈」 を残したい ephemeral state ─ どの project が開いていたか、
//! どの Lane が active だったか等。 file 形式は JSON (将来 field 追加に強い)。
//!
//! ## 責務の切り分け (重要)
//!
//! - **Process state** (SSOT): TheWorld daemon が保持 ─ running/dead/port、 SP 起動状態
//! - **UI state** (per-instance preference): この file ─ expanded / active selection / 表示順
//! - **User preference**: `Settings` (vp-app.toml) ─ developer_mode、 default_project_root
//!
//! TheWorld に UI state を載せると secondary vp-app instance (`VP_APP_SECONDARY=1`) が
//! 同 server に向かう時に「私はこの Lane を見る」 「私はあの Lane」 が両立できなくなる。
//! UI state は client ごとに独立であるべき ─ なのでここに置く。
//!
//! ## file path
//!
//! - macOS:  `~/Library/Application Support/vantage/session-state.json`
//! - Linux:  `~/.config/vantage/session-state.json`
//! - Windows: `%APPDATA%\vantage\session-state.json`

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// JSON file 名 (ディレクトリは `dirs::config_dir() + "vantage"`)
const SESSION_FILE: &str = "session-state.json";

/// Per-project UI state ─ project path がキー。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectUiState {
    /// sidebar accordion 開閉状態
    #[serde(default)]
    pub expanded: bool,
    // 将来 field 候補: per-project の Worker form expanded、 lane custom order 等
}

/// vp-app 全体の session UI state。
///
/// 起動時に `load()` で復元、 IPC mutation 時に `save()` で書き戻す。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionState {
    /// project path → UI state (sidebar accordion 等)
    #[serde(default)]
    pub projects: HashMap<String, ProjectUiState>,
    /// 直前 active Lane の address (Display 形 `"<project>/lead"` / `"<project>/worker/<name>"`)。
    /// 起動後の最初の LanesLoaded で実在 lane と照合して復元される (mismatch なら無視)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_lane_address: Option<String>,
    /// Currents セクションの project 表示順 (path の order)。
    /// `None` なら TheWorld の registration 順。 sidebar の DnD で書き込まれる。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currents_order: Option<Vec<String>>,
}

impl SessionState {
    /// 永続 file の絶対 path。 `dirs` crate が config_dir 取得失敗なら `None`。
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("vantage").join(SESSION_FILE))
    }

    /// 設定 file を読み込む。 不在 / 壊れた JSON は default を返す (起動を阻害しない)。
    pub fn load() -> Self {
        let Some(p) = Self::path() else {
            tracing::warn!("config_dir 取得失敗、SessionState::default() を使用");
            return Self::default();
        };
        if !p.exists() {
            tracing::debug!("SessionState file 不在、 default を使用: {}", p.display());
            return Self::default();
        }
        match std::fs::read_to_string(&p) {
            Ok(s) => match serde_json::from_str::<SessionState>(&s) {
                Ok(state) => {
                    tracing::info!(
                        "SessionState 読込: {} ({} projects, active_lane={:?})",
                        p.display(),
                        state.projects.len(),
                        state.active_lane_address
                    );
                    state
                }
                Err(e) => {
                    tracing::warn!(
                        "SessionState JSON パース失敗 ({}): {} - default 使用",
                        p.display(),
                        e
                    );
                    Self::default()
                }
            },
            Err(e) => {
                tracing::warn!(
                    "SessionState 読込失敗 ({}): {} - default 使用",
                    p.display(),
                    e
                );
                Self::default()
            }
        }
    }

    /// 設定 file に atomic write (`tmp file → rename`)。
    /// 失敗は warn (UI 操作は継続させる、 次回 save で書き直し)。
    pub fn save(&self) {
        let Some(p) = Self::path() else {
            tracing::warn!("config_dir 取得失敗、SessionState save skip");
            return;
        };
        if let Err(e) = self.save_inner(&p) {
            tracing::warn!("SessionState save 失敗 ({}): {}", p.display(), e);
        }
    }

    fn save_inner(&self, p: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let s = serde_json::to_string_pretty(self)?;
        // tmp → rename で atomic write。 中途半端な書き込みで file が壊れない。
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, s)?;
        std::fs::rename(&tmp, p)?;
        tracing::debug!("SessionState 保存: {}", p.display());
        Ok(())
    }

    /// project の expanded 状態を取得 (未保存なら `None`)。
    pub fn project_expanded(&self, path: &str) -> Option<bool> {
        self.projects.get(path).map(|p| p.expanded)
    }

    /// project の expanded 状態を更新 (entry 無ければ作成)。
    pub fn set_project_expanded(&mut self, path: impl Into<String>, expanded: bool) {
        self.projects.entry(path.into()).or_default().expanded = expanded;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty() {
        let s = SessionState::default();
        assert!(s.projects.is_empty());
        assert!(s.active_lane_address.is_none());
        assert!(s.currents_order.is_none());
    }

    #[test]
    fn round_trip_json() {
        let mut s = SessionState::default();
        s.set_project_expanded("/path/to/proj", true);
        s.active_lane_address = Some("proj/lead".into());
        s.currents_order = Some(vec!["/proj-a".into(), "/proj-b".into()]);
        let json = serde_json::to_string(&s).unwrap();
        let parsed: SessionState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.project_expanded("/path/to/proj"), Some(true));
        assert_eq!(parsed.active_lane_address.as_deref(), Some("proj/lead"));
        assert_eq!(
            parsed.currents_order.as_deref(),
            Some(&["/proj-a".to_string(), "/proj-b".to_string()][..])
        );
    }

    #[test]
    fn deserialize_empty_object_is_default() {
        // forward-compat: 空 object でも crash しない (新 field 追加時の back-compat 兼)
        let parsed: SessionState = serde_json::from_str("{}").unwrap();
        assert!(parsed.projects.is_empty());
        assert!(parsed.active_lane_address.is_none());
    }

    #[test]
    fn deserialize_partial_only_active_lane() {
        // expanded などの一部 field 欠落でも default で埋まる
        let json = r#"{"active_lane_address":"foo/lead"}"#;
        let parsed: SessionState = serde_json::from_str(json).unwrap();
        assert!(parsed.projects.is_empty());
        assert_eq!(parsed.active_lane_address.as_deref(), Some("foo/lead"));
    }

    #[test]
    fn set_project_expanded_creates_entry() {
        let mut s = SessionState::default();
        s.set_project_expanded("/x", true);
        assert_eq!(s.project_expanded("/x"), Some(true));
        s.set_project_expanded("/x", false);
        assert_eq!(s.project_expanded("/x"), Some(false));
    }

    #[test]
    fn project_expanded_unknown_returns_none() {
        let s = SessionState::default();
        assert_eq!(s.project_expanded("/missing"), None);
    }
}
