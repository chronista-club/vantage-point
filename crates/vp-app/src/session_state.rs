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
//! VP-192: session 状態は生成データなので `vp_data_dir()` 配下。
//! - macOS:  `~/Library/Application Support/vp/session-state.json`
//! - Linux:  `~/.local/share/vp/session-state.json`
//! - Windows: `%LOCALAPPDATA%\vp\session-state.json`

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// JSON file 名 (ディレクトリは `vp_data_dir()`)
const SESSION_FILE: &str = "session-state.json";

/// Per-project UI state ─ project path がキー。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectUiState {
    /// sidebar accordion 開閉状態
    #[serde(default)]
    pub expanded: bool,
    // 将来 field 候補: per-project の Wing form expanded、 lane custom order 等
}

/// 保存 geometry が valid 判定の閾値 (LogicalPixel)。 これ未満は無視して default に
/// fallback する (= 破損 / race / 異常 close 由来の極小値ガード)。
/// `app.rs` 側の `MIN_WINDOW_WIDTH` / `MIN_WINDOW_HEIGHT` と整合させる (= 720x480)。
pub const GEOMETRY_MIN_WIDTH: f64 = 720.0;
pub const GEOMETRY_MIN_HEIGHT: f64 = 480.0;

/// 起動時に main window の位置 / サイズ / monitor を復元するための snapshot。
///
/// 単位は **LogicalPixel** (= scale_factor 込みの DPI 補正後座標)。 保存時に
/// `outer_position().to_logical(scale_factor)` で取得し、 復元時に `with_position`
/// + `with_inner_size` に渡す。 raw physical pixel だと HiDPI 切替で破綻する。
///
/// `monitor` は tao の `MonitorHandle::name()` (= OS が提供する display 名、 macOS なら
/// e.g. "Built-in Retina Display" / "DELL U3415W")。 multi-screen 切断 → 再接続で
/// 保存 monitor が消失した場合は、 primary monitor 内に clamp して復元する。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowGeometry {
    /// Window inner size width (LogicalPixel)
    pub width: f64,
    /// Window inner size height (LogicalPixel)
    pub height: f64,
    /// Window outer position x (LogicalPixel、 OS screen 全体での top-left 座標)
    pub x: f64,
    /// Window outer position y (LogicalPixel)
    pub y: f64,
    /// 保存時 window が居た monitor の name (= tao `MonitorHandle::name()` の戻り値)。
    /// 復元時に同名 monitor が available なら geometry を尊重、 消失していれば
    /// primary monitor 内に clamp する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
}

impl WindowGeometry {
    /// 保存 geometry が valid か (= 破損 / 異常極小値ガード)。
    /// 起動時 clamp と被るため、 invalid なら caller 側で None 扱いに fallback。
    pub fn is_valid(&self) -> bool {
        self.width >= GEOMETRY_MIN_WIDTH
            && self.height >= GEOMETRY_MIN_HEIGHT
            && self.width.is_finite()
            && self.height.is_finite()
            && self.x.is_finite()
            && self.y.is_finite()
    }

    /// Vec を resize する時の placeholder。 invalid な値 (= 0x0) で、 `is_valid()` で reject
    /// される。 instance index 0 だけ存在する状態で index 2 を save する場合 (= 中間 slot
    /// が未保存) に slot 1 を埋めるのに使う。
    pub fn placeholder() -> Self {
        Self {
            width: 0.0,
            height: 0.0,
            x: 0.0,
            y: 0.0,
            monitor: None,
        }
    }
}

/// vp-app 全体の session UI state。
///
/// 起動時に `load()` で復元、 IPC mutation 時に `save()` で書き戻す。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionState {
    /// project path → UI state (sidebar accordion 等)
    #[serde(default)]
    pub projects: HashMap<String, ProjectUiState>,
    /// 直前 active Lane の address (Display 形 `"<project>/lead"` / `"<project>/wing/<name>"`)。
    /// 起動後の最初の LanesLoaded で実在 lane と照合して復元される (mismatch なら無視)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_lane_address: Option<String>,
    /// Currents セクションの project 表示順 (path の order)。
    /// `None` なら TheWorld の registration 順。 sidebar の DnD で書き込まれる。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currents_order: Option<Vec<String>>,
    /// 直前の main window 位置 / サイズ / monitor (= **legacy 単一 slot**)。
    ///
    /// PR #459 で `window_geometries` (= Vec) に多 instance 対応として置換予定だが、
    /// 旧 file format の backward compat のため `Option` field を keep。 `load()` 内で
    /// `window_geometries` が空 + `window_geometry` Some の時に slot 0 に migrate。
    ///
    /// 1-2 release 後に削除候補 (= 既存 save file が新 format に rewrite された後)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_geometry: Option<WindowGeometry>,

    /// 各 vp-app instance の window geometry。 配列 index = instance index (= `VP_APP_INSTANCE`
    /// env)。 0 番目 = primary、 1+ = secondary (= Cmd+N で spawn された window)。
    ///
    /// 起動時に self instance index で slot 取得して WindowBuilder に apply。 primary 起動時
    /// `len() > 1` なら **未起動の secondary を auto-spawn** (= child process で
    /// `VP_APP_INSTANCE=1..N` 起動)、 これで再起動時に全 window が復元される。
    ///
    /// `CloseRequested` で自分の slot を update + save。 Vec の resize は `slot_or_grow()` で
    /// instance index が範囲外なら自動拡張。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub window_geometries: Vec<WindowGeometry>,
}

impl SessionState {
    /// 永続 file の絶対 path。
    ///
    /// VP-192: session 状態は生成データなので `vp_data_dir()` 配下。
    /// `Option` を維持するのは既存 caller (`load`/`save`) との互換のため。
    pub fn path() -> Option<PathBuf> {
        Some(crate::paths::vp_data_dir().join(SESSION_FILE))
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
                Ok(mut state) => {
                    // PR #459 migration: 旧 `window_geometry` (Option) → 新
                    // `window_geometries` (Vec) に移植。 新 file format で save された
                    // 場合は `window_geometries` 側が priority、 旧 field は ignore。
                    if state.window_geometries.is_empty()
                        && let Some(legacy) = state.window_geometry.take()
                    {
                        state.window_geometries.push(legacy);
                    }
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

    /// 指定 instance index (= `VP_APP_INSTANCE` env 値) の geometry を更新する。
    ///
    /// Vec が短い場合は **手前の slot を default WindowGeometry で埋めて拡張**。
    /// 例: index=2 で len()=0 → `[default, default, geom]` で len()=3 に。
    /// default placeholder は valid 判定で reject されるので、 起動時には ignored。
    pub fn set_window_geometry(&mut self, instance_index: usize, geom: WindowGeometry) {
        if instance_index >= self.window_geometries.len() {
            self.window_geometries
                .resize_with(instance_index + 1, WindowGeometry::placeholder);
        }
        self.window_geometries[instance_index] = geom;
    }

    /// 指定 instance index の valid geometry を取得 (= 起動時 restore 用)。
    /// placeholder / invalid / 範囲外なら None。
    pub fn window_geometry_for(&self, instance_index: usize) -> Option<&WindowGeometry> {
        self.window_geometries
            .get(instance_index)
            .filter(|g| g.is_valid())
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
