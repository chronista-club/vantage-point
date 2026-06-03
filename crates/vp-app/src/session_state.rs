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
//! ## per-instance file 分離 (= multi-window state の SSOT 化)
//!
//! 各 vp-app instance は **自分専用の session file** を持つ。 共有 1 file 時代の
//! 「2 process が同 file を全体書き戻し → 互いの slot を clobber」 race を根治する。
//!
//! - instance 0 (primary): `~/.local/state/vp/session.json` (= 旧 file、 そのまま primary 用)
//! - instance N (N≥1、 Cmd+N で spawn された secondary): `~/.local/state/vp/session.<N>.json`
//!
//! 「どの window を起動時に開くか」 は **各 instance file の `open` flag** で表す。 primary は
//! 起動時に `session.<N>.json` (N≥1) を走査し、 `open==true` の instance を auto-spawn する。
//! clean close (`CloseRequested`) で `open=false`、 強制 kill (CloseRequested なし) では
//! `open=true` のまま残る ─ つまり「明示的に閉じた window は復活しない / kill された window は
//! 復元される」 という直感的な挙動になる。
//!
//! XDG: session 状態は runtime state なので XDG state zone (`$XDG_STATE_HOME` 優先、
//! default `~/.local/state/vp/`) 配下。

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// instance 0 (primary) の session file 名。 secondary は `session.<N>.json`。
const SESSION_FILE_PRIMARY: &str = "session.json";

/// instance index → file 名。 0 = `session.json`、 N≥1 = `session.<N>.json`。
fn session_file_name(instance_index: usize) -> String {
    if instance_index == 0 {
        SESSION_FILE_PRIMARY.to_string()
    } else {
        format!("session.{instance_index}.json")
    }
}

/// `open` field の serde default。 既存 file (= flag 不在) は「開いていた」 扱いにする
/// (= load できた file は閉じられていない限り開く対象)。
fn default_open() -> bool {
    true
}

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
}

/// vp-app **1 instance** の session UI state。
///
/// 起動時に `load(instance_index)` で自分の file を復元、 IPC mutation 時に `save()` で
/// 自分の file に書き戻す。 他 instance の file は触らない (= clobber free)。
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// **この instance** の window 位置 / サイズ / monitor。
    /// `CloseRequested` / resize / move で自分の file に save、 起動時に復元。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_geometry: Option<WindowGeometry>,
    /// この instance window が「開いている / 開くべき」 か。 primary が起動時に
    /// `open==true` の secondary file を auto-spawn する signal。 clean close で false。
    #[serde(default = "default_open")]
    pub open: bool,

    /// 旧 multi-slot format (PR #459 `window_geometries: Vec`) の **読み込み専用** migration
    /// field。 save では出さない (`skip_serializing`)。 `load()` 内で自 instance slot を
    /// `window_geometry` に移植したら clear する。 1-2 release 後に削除候補。
    #[serde(default, skip_serializing)]
    pub window_geometries: Vec<WindowGeometry>,

    /// 自 instance index (= 0 primary / N≥1 secondary)。 file path 決定に使う。
    /// file 名で表現されるので **永続化しない** (`skip`)。
    #[serde(skip)]
    instance_index: usize,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            projects: HashMap::new(),
            active_lane_address: None,
            currents_order: None,
            window_geometry: None,
            open: true,
            window_geometries: Vec::new(),
            instance_index: 0,
        }
    }
}

impl SessionState {
    /// 指定 instance の永続 file の絶対 path。
    pub fn path(instance_index: usize) -> Option<PathBuf> {
        Some(crate::paths::vp_state_dir().join(session_file_name(instance_index)))
    }

    /// 指定 instance の session file を読み込む。 不在 / 壊れた JSON は default を返す
    /// (= 起動を阻害しない)。 旧 `window_geometries` (Vec) format からは自 slot を移植。
    pub fn load(instance_index: usize) -> Self {
        let fallback = || Self {
            instance_index,
            ..Self::default()
        };
        let Some(p) = Self::path(instance_index) else {
            tracing::warn!("state_dir 取得失敗、SessionState::default() を使用");
            return fallback();
        };
        if !p.exists() {
            tracing::debug!("SessionState file 不在、 default を使用: {}", p.display());
            return fallback();
        }
        match std::fs::read_to_string(&p) {
            Ok(s) => match serde_json::from_str::<SessionState>(&s) {
                Ok(mut state) => {
                    state.instance_index = instance_index;
                    // PR #459 → per-instance migration: 旧 `window_geometries` (Vec) しか
                    // 無い file から、 自 instance slot を `window_geometry` に移植。
                    // 新 file は `window_geometry` を直接持つので migration は no-op。
                    if state.window_geometry.is_none()
                        && let Some(g) = state.window_geometries.get(instance_index)
                        && g.is_valid()
                    {
                        state.window_geometry = Some(g.clone());
                    }
                    state.window_geometries.clear(); // 以後 save では出さない
                    tracing::info!(
                        "SessionState 読込 [instance={}]: {} ({} projects, active_lane={:?}, open={})",
                        instance_index,
                        p.display(),
                        state.projects.len(),
                        state.active_lane_address,
                        state.open
                    );
                    state
                }
                Err(e) => {
                    tracing::warn!(
                        "SessionState JSON パース失敗 ({}): {} - default 使用",
                        p.display(),
                        e
                    );
                    fallback()
                }
            },
            Err(e) => {
                tracing::warn!(
                    "SessionState 読込失敗 ({}): {} - default 使用",
                    p.display(),
                    e
                );
                fallback()
            }
        }
    }

    /// 自 instance の file に atomic write (`tmp file → rename`)。
    /// 失敗は warn (UI 操作は継続させる、 次回 save で書き直し)。
    pub fn save(&self) {
        let Some(p) = Self::path(self.instance_index) else {
            tracing::warn!("state_dir 取得失敗、SessionState save skip");
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

    /// この state が属する instance index。
    pub fn instance_index(&self) -> usize {
        self.instance_index
    }

    /// project の expanded 状態を取得 (未保存なら `None`)。
    pub fn project_expanded(&self, path: &str) -> Option<bool> {
        self.projects.get(path).map(|p| p.expanded)
    }

    /// project の expanded 状態を更新 (entry 無ければ作成)。
    pub fn set_project_expanded(&mut self, path: impl Into<String>, expanded: bool) {
        self.projects.entry(path.into()).or_default().expanded = expanded;
    }

    /// この instance の window geometry を更新する。
    pub fn set_window_geometry(&mut self, geom: WindowGeometry) {
        self.window_geometry = Some(geom);
    }

    /// この instance の valid geometry を取得 (= 起動時 restore 用)。 invalid なら None。
    pub fn window_geometry(&self) -> Option<&WindowGeometry> {
        self.window_geometry.as_ref().filter(|g| g.is_valid())
    }

    /// この instance window の open flag を更新する (clean close で false にして save)。
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    /// primary が auto-spawn すべき secondary instance index (≥1) の一覧。
    ///
    /// state dir 内の `session.<N>.json` (N≥1) を走査し、 `open==true` のものを集める。
    /// 走査失敗 / file 不在は空 Vec (= secondary 無し)。 戻りは昇順。
    pub fn open_secondary_indices() -> Vec<usize> {
        let dir = crate::paths::vp_state_dir();
        let mut out: Vec<usize> = Vec::new();
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return out;
        };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            // `session.<N>.json` (N≥1) のみ対象。 `session.json` (= primary) や
            // `session.json.tmp` (= atomic write 中間) は prefix/suffix strip で弾かれる。
            let Some(rest) = name.strip_prefix("session.") else {
                continue;
            };
            let Some(num) = rest.strip_suffix(".json") else {
                continue;
            };
            let Ok(idx) = num.parse::<usize>() else {
                continue;
            };
            if idx == 0 {
                continue;
            }
            if Self::load(idx).open {
                out.push(idx);
            }
        }
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty_and_open() {
        let s = SessionState::default();
        assert!(s.projects.is_empty());
        assert!(s.active_lane_address.is_none());
        assert!(s.currents_order.is_none());
        assert!(s.window_geometry.is_none());
        // load できた / new instance は「開いている」 が default。
        assert!(s.open);
        assert_eq!(s.instance_index(), 0);
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
        // forward-compat: 空 object でも crash しない (新 field 追加時の back-compat 兼)。
        // `open` は default_open() で true に埋まる。
        let parsed: SessionState = serde_json::from_str("{}").unwrap();
        assert!(parsed.projects.is_empty());
        assert!(parsed.active_lane_address.is_none());
        assert!(parsed.open);
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

    #[test]
    fn file_name_is_per_instance() {
        assert_eq!(session_file_name(0), "session.json");
        assert_eq!(session_file_name(1), "session.1.json");
        assert_eq!(session_file_name(3), "session.3.json");
    }

    #[test]
    fn open_flag_round_trips_and_close_persists() {
        // clean close = open:false が serialize / deserialize される。
        let mut s = SessionState::default();
        s.set_open(false);
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"open\":false"));
        let parsed: SessionState = serde_json::from_str(&json).unwrap();
        assert!(!parsed.open);
    }

    #[test]
    fn legacy_window_geometries_migrates_own_slot() {
        // 旧 Vec format から自 instance slot を window_geometry に移植する migration。
        // instance 1 として読むと slot[1] (= secondary) が採用される。
        let json = r#"{
            "window_geometries": [
                {"width": 1200.0, "height": 800.0, "x": 0.0, "y": 0.0},
                {"width": 1400.0, "height": 900.0, "x": 100.0, "y": 50.0}
            ]
        }"#;
        let mut state: SessionState = serde_json::from_str(json).unwrap();
        state.instance_index = 1;
        // load() の migration を手で再現
        if state.window_geometry.is_none()
            && let Some(g) = state.window_geometries.get(1)
            && g.is_valid()
        {
            state.window_geometry = Some(g.clone());
        }
        let g = state.window_geometry().expect("slot 1 が移植される");
        assert_eq!(g.width, 1400.0);
        assert_eq!(g.height, 900.0);
    }

    #[test]
    fn window_geometry_invalid_is_filtered() {
        let mut s = SessionState::default();
        // MIN 未満 = invalid → window_geometry() は None
        s.set_window_geometry(WindowGeometry {
            width: 10.0,
            height: 10.0,
            x: 0.0,
            y: 0.0,
            monitor: None,
        });
        assert!(s.window_geometry().is_none());
    }
}
