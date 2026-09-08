//! **復元と保存の 1 箇所**（doc 60 §6 6-2b / §8、Codex 再レビュー ⑤「永続は投影 — 復元の優先順位を決めてから
//! 変換を 1 か所に」）。`SessionState`（この instance の session file）の所有者。
//!
//! 6-2a までは `session_state.save()` が boot / on_window ×4 / lane_view / on_sidebar に散っていた。
//! ここに集めることで「いつ・何が・どの順で file に書かれるか」が 1 file で読める。
//! b-1（2026-09-08）は **現行挙動のまま**移した段階で、危険 A〜F（doc 60 §8）は以降の PR で直す。
//!
//! ## 復元 cursor
//! 起動直後の `SidebarState` は空。session file の active lane は [`Persist::pending_active_lane`] に
//! 退避し、その lane を含む lanes snapshot が届いた時に **1 度だけ**消費する（[`Persist::restore_active_lane`]）。
//! daemon 側の canonical（初回 ReposLoaded の `active_lane`）は [`Persist::observe_daemon_active_lane`] で
//! 受けるが、**pending 未消費の間は session を書かない**（b-3、危険 B / C）。sidebar の表示は呼び手が
//! 別に更新するので、file に残るのは「user が最後に選んだ lane」だけになる。
//!
//! ## 保存の契機
//! boot（open=true）/ close（open=false + geometry）/ resize・move（500 ms throttle + geometry）/
//! lane activate / shell layout / sidebar IPC の `session_save`（toggle・reorder）。

use std::time::{Duration, Instant};

use crate::daemon_wire::LaneInfo;
use crate::session_state::{DisplayMode, SessionState, ShellLayout, WindowGeometry};

/// PR #459 throttled save: window resize / move 中も 500ms throttle で session save。
/// CloseRequested の force save に依存しない (= `ge app:stop` の SIGTERM kill や crash
/// でも直近 state が persistent)。 dogfood で「ge app で再起動すると save 走らない」
/// bug を解消。
pub(super) const GEOMETRY_SAVE_THROTTLE: Duration = Duration::from_millis(500);

/// session file の所有者。復元 cursor と保存の throttle を抱える。
pub(super) struct Persist {
    /// この instance の session state。読みは自由、**書きと save は本 struct の method 経由**
    /// （例外: `handle_sidebar_ipc` が in-memory を更新し `session_save` で本 struct に save を頼む）。
    pub(super) session: SessionState,
    /// 直前 active Lane を初回 LanesLoaded で復元するための pending 値。
    /// 1 度復元したら None にして、 後続 LanesLoaded で再復元しないように。
    pending_active_lane: Option<String>,
    /// 直近の geometry 保存時刻（[`GEOMETRY_SAVE_THROTTLE`] の起点）。
    last_geometry_save: Instant,
}

impl Persist {
    /// 起動: 自 instance の file を load し、「開いている」印を **即 save** する
    /// （= 次回 primary 起動時の auto-spawn signal。clean close で false に戻す）。
    pub(super) fn boot(instance_index: usize) -> Self {
        let mut session = SessionState::load(instance_index);
        session.set_open(true);
        session.save();
        Self::from_session(session)
    }

    /// load 済の state から（test 用 / boot の後段）。file は書かない。
    pub(super) fn from_session(session: SessionState) -> Self {
        Self {
            pending_active_lane: session.active_lane_address.clone(),
            session,
            // 起動直後の最初の Resized / Moved で throttle に引っかからないよう 1 秒前に置く
            last_geometry_save: Instant::now() - Duration::from_secs(1),
        }
    }

    /// 復元待ちの lane（消費前だけ Some）。今は test の観測用。
    #[cfg(test)]
    pub(super) fn pending_active_lane(&self) -> Option<&str> {
        self.pending_active_lane.as_deref()
    }

    /// session 復元: `lanes` に pending の lane が含まれていればそれを返し、pending を消費する
    /// （1 度限り）。含まれなければ None で pending は残る（別 repo の snapshot だった等）。
    pub(super) fn restore_active_lane(&mut self, lanes: &[LaneInfo]) -> Option<String> {
        let matched = self
            .pending_active_lane
            .as_ref()
            .filter(|saved| lanes.iter().any(|l| &l.address.key() == *saved))
            .cloned()?;
        self.pending_active_lane = None;
        tracing::info!("session 復元: active_lane = {}", matched);
        Some(matched)
    }

    /// Model Q: 初回 load で daemon canonical の active lane を受ける（session.json でなく daemon が源）。
    ///
    /// **pending 未消費の間は session を書かない**（危険 B / C の根治、doc 60 §8）: 旧実装は in-memory を
    /// 書き換えていたので、後続の resize / move の throttle save や boot 窓の close が daemon 値を file に
    /// 流し、instance 別の保存値（user が最後に選んだ lane）が消えていた。保存値が無い（pending = None）
    /// 時だけ daemon 値を採る（次回 boot は daemon の選択から始まる = 従来の Model Q）。
    /// sidebar の表示は呼び手（on_lanes）が別に更新するので、画面は従来どおり daemon 値になる。
    pub(super) fn observe_daemon_active_lane(&mut self, addr: String) {
        if self.pending_active_lane.is_some() {
            tracing::debug!(
                "daemon の active lane ({addr}) は session に書かない（復元待ち {:?} を優先）",
                self.pending_active_lane
            );
            return;
        }
        self.session.active_lane_address = Some(addr);
    }

    /// user 操作 / 復元で lane を activate した: session に鏡して save。
    pub(super) fn activate(&mut self, address: &str) {
        self.session.active_lane_address = Some(address.to_string());
        self.session.save();
    }

    /// throttle 窓（500 ms）を抜けたら true を返し、起点を `now` に進める。
    fn geometry_save_due(&mut self, now: Instant) -> bool {
        if now.duration_since(self.last_geometry_save) > GEOMETRY_SAVE_THROTTLE {
            self.last_geometry_save = now;
            true
        } else {
            false
        }
    }

    /// PR #459 throttled save: resize / move 中も 500ms throttle で geometry + 表示モードを save。
    /// 全画面 enter/exit も Resized を撃つので、 helper 内の fullscreen 判定で mode が追従する。
    pub(super) fn save_geometry_throttled(&mut self, window: &tao::window::Window) {
        if self.geometry_save_due(Instant::now()) {
            self.record_window(window);
            self.session.save();
        }
    }

    /// clean close: open=false + geometry を確実に書き出す (outer_position 失敗でも open は残す)。
    /// 戻り値は log 用の保存 geometry。
    pub(super) fn save_on_close(
        &mut self,
        window: &tao::window::Window,
    ) -> Option<&WindowGeometry> {
        self.record_window(window);
        self.finish_close();
        self.session.window_geometry()
    }

    /// `save_on_close` の window に依らない部分（test 用に分離）。
    fn finish_close(&mut self) {
        self.session.set_open(false);
        self.session.save();
    }

    /// shell の形（幅 / full-slim / R 開閉）を **この instance の** session file に保存。
    /// 「window をどう開いていたか」なので window_geometry と同じ箱に入れる。
    /// ⚠️ 値の検証は `set_shell_layout` の clamp が持つ（webview の値を信用しない）。
    pub(super) fn save_shell_layout(&mut self, layout: ShellLayout) {
        self.session.set_shell_layout(layout);
        self.session.save();
    }

    /// `handle_sidebar_ipc` が in-memory を更新した後の保存要求（`SidebarIpcOutcome::session_save`）。
    pub(super) fn save(&mut self) {
        self.session.save();
    }

    /// window の現在の geometry + 表示モードを SessionState に write-through する (doc 30 §3.4a / §6.1)。
    ///
    /// - **通常ウィンドウ**: 位置・サイズ・monitor・`display_mode=Windowed` を `set_window_geometry`。
    /// - **全画面**: `inner_size()` は fullscreen frame を返し windowed 座標を潰すため、 `set_display_mode`
    ///   で mode + monitor のみ更新し、 直前の windowed 座標を保持する (全画面解除で元の窓サイズに戻せる)。
    ///
    /// `save()` は呼ばない (caller が open flag 等とまとめて save する)。 outer_position 取得失敗時は
    /// windowed 座標を更新できないので geometry を触らず返る (mode 更新は全画面時のみで別経路)。
    fn record_window(&mut self, window: &tao::window::Window) {
        let monitor_name = window.current_monitor().and_then(|m| m.name());
        if window.fullscreen().is_some() {
            self.session
                .set_display_mode(DisplayMode::Fullscreen, monitor_name);
            return;
        }
        let scale = window.scale_factor();
        match window.outer_position() {
            Ok(pos) => {
                let inner = window.inner_size().to_logical::<f64>(scale);
                let logical_pos = pos.to_logical::<f64>(scale);
                self.session.set_window_geometry(WindowGeometry {
                    width: inner.width,
                    height: inner.height,
                    x: logical_pos.x,
                    y: logical_pos.y,
                    monitor: monitor_name,
                    display_mode: DisplayMode::Windowed,
                });
            }
            Err(e) => {
                tracing::warn!("outer_position() 取得失敗 (geometry save skip): {}", e);
            }
        }
    }
}

/// 現行挙動を固定する characterization test（doc 60 §6 6-2b b-1）。危険 A〜F を直す前の姿。
/// file を書く test は `test_env::state_dir()` で `$XDG_STATE_HOME` を tempdir に向ける。
#[cfg(test)]
mod tests {
    use super::*;

    /// daemon が発行する現行形の key（`<repo>/lane/<name>`）を持つ lane。key 無しの fixture は
    /// 旧 2 分節形に落ちて `SessionState::load` が「旧世代」と見なす形なので、ここでは canonical を書く。
    fn lane(repo: &str, name: &str) -> LaneInfo {
        serde_json::from_value(serde_json::json!({
            "address": {"repo": repo, "name": name, "key": format!("{repo}/lane/{name}")}
        }))
        .expect("LaneInfo deserialize")
    }

    fn saved(instance: usize) -> Option<SessionState> {
        let p = SessionState::path(instance).expect("state dir");
        let s = std::fs::read_to_string(p).ok()?;
        Some(serde_json::from_str(&s).expect("session json"))
    }

    fn with_active(addr: &str) -> SessionState {
        let mut s = SessionState::default();
        s.active_lane_address = Some(addr.to_string());
        s
    }

    #[test]
    fn boot_marks_open_and_saves_immediately() {
        let _env = crate::test_env::state_dir();
        let p = Persist::boot(0);
        assert!(p.session.open);
        assert!(saved(0).expect("saved at boot").open);
        assert_eq!(p.pending_active_lane(), None);
    }

    #[test]
    fn boot_parks_saved_lane_as_pending() {
        let _env = crate::test_env::state_dir();
        with_active("vp/lane/sub-a").save();
        let p = Persist::boot(0);
        assert_eq!(p.pending_active_lane(), Some("vp/lane/sub-a"));
        assert_eq!(
            p.session.active_lane_address.as_deref(),
            Some("vp/lane/sub-a")
        );
    }

    #[test]
    fn restore_consumes_pending_once_when_lanes_contain_it() {
        let mut p = Persist::from_session(with_active("vp/lane/sub-a"));
        let lanes = [lane("vp", "root"), lane("vp", "sub-a")];
        assert_eq!(
            p.restore_active_lane(&lanes).as_deref(),
            Some("vp/lane/sub-a")
        );
        assert_eq!(p.pending_active_lane(), None);
        // 2 回目は復元しない（後続の LanesLoaded で再復元して user の選択を上書きしない）
        assert_eq!(p.restore_active_lane(&lanes), None);
    }

    #[test]
    fn restore_keeps_pending_when_snapshot_is_another_repo() {
        let mut p = Persist::from_session(with_active("vp/lane/sub-a"));
        assert_eq!(p.restore_active_lane(&[lane("other", "root")]), None);
        assert_eq!(p.pending_active_lane(), Some("vp/lane/sub-a"));
        // 空の snapshot でも同じ
        assert_eq!(p.restore_active_lane(&[]), None);
        assert_eq!(p.pending_active_lane(), Some("vp/lane/sub-a"));
    }

    /// 危険 B / C（b-3）: pending 未消費の間、daemon 値は session に入らない。
    /// その後の無関係な save（resize / move の throttle、boot 窓の close）でも保存値は残る。
    #[test]
    fn observe_daemon_active_lane_keeps_saved_value_while_pending() {
        let _env = crate::test_env::state_dir();
        let mut p = Persist::from_session(with_active("vp/lane/sub-a"));
        p.observe_daemon_active_lane("vp/lane/root".to_string());
        assert_eq!(
            p.session.active_lane_address.as_deref(),
            Some("vp/lane/sub-a"),
            "復元待ちの間は daemon 値で上書きしない"
        );
        assert_eq!(p.pending_active_lane(), Some("vp/lane/sub-a"));
        assert!(saved(0).is_none(), "observe だけでは書かない");
        // 危険 C: boot 窓で閉じても保存値は残る
        p.finish_close();
        assert_eq!(
            saved(0).expect("saved").active_lane_address.as_deref(),
            Some("vp/lane/sub-a")
        );
    }

    /// 保存値が無ければ daemon 値を採る（従来の Model Q: 次回 boot は daemon の選択から）。
    #[test]
    fn observe_daemon_active_lane_adopts_when_nothing_pending() {
        let _env = crate::test_env::state_dir();
        let mut p = Persist::from_session(SessionState::default());
        assert_eq!(p.pending_active_lane(), None);
        p.observe_daemon_active_lane("vp/lane/root".to_string());
        assert_eq!(
            p.session.active_lane_address.as_deref(),
            Some("vp/lane/root")
        );
        assert!(saved(0).is_none(), "observe だけでは書かない");
        p.finish_close();
        assert_eq!(
            saved(0).expect("saved").active_lane_address.as_deref(),
            Some("vp/lane/root")
        );
    }

    /// on_lanes の実際の流れ: daemon 値を観測 → その lane を含まない snapshot → 含む snapshot で復元 →
    /// activate で file が復元値に確定する。daemon 値は一度も file に出ない。
    #[test]
    fn restore_then_activate_persists_saved_lane_not_daemon_lane() {
        let _env = crate::test_env::state_dir();
        let mut p = Persist::from_session(with_active("vp/lane/sub-a"));
        p.observe_daemon_active_lane("other/lane/root".to_string());
        assert_eq!(p.restore_active_lane(&[lane("other", "root")]), None);
        p.finish_close(); // 途中で閉じても
        assert_eq!(
            saved(0).expect("saved").active_lane_address.as_deref(),
            Some("vp/lane/sub-a")
        );
        let restored = p
            .restore_active_lane(&[lane("vp", "root"), lane("vp", "sub-a")])
            .expect("restored");
        p.activate(&restored);
        assert_eq!(
            saved(0).expect("saved").active_lane_address.as_deref(),
            Some("vp/lane/sub-a")
        );
        // 復元後は pending が無いので、次の daemon 観測は従来どおり採る
        p.observe_daemon_active_lane("vp/lane/root".to_string());
        assert_eq!(
            p.session.active_lane_address.as_deref(),
            Some("vp/lane/root")
        );
    }

    #[test]
    fn activate_mirrors_and_saves() {
        let _env = crate::test_env::state_dir();
        let mut p = Persist::from_session(SessionState::default());
        p.activate("vp/lane/sub-b");
        assert_eq!(
            p.session.active_lane_address.as_deref(),
            Some("vp/lane/sub-b")
        );
        assert_eq!(
            saved(0).expect("saved").active_lane_address.as_deref(),
            Some("vp/lane/sub-b")
        );
    }

    #[test]
    fn geometry_save_throttle_window() {
        let mut p = Persist::from_session(SessionState::default());
        let t0 = Instant::now();
        // from_session は 1 秒前を起点にするので最初は通る
        assert!(p.geometry_save_due(t0));
        assert!(!p.geometry_save_due(t0 + Duration::from_millis(100)));
        assert!(!p.geometry_save_due(t0 + Duration::from_millis(500)));
        assert!(p.geometry_save_due(t0 + Duration::from_millis(501)));
        // 起点が進んでいる
        assert!(!p.geometry_save_due(t0 + Duration::from_millis(900)));
    }

    #[test]
    fn close_writes_open_false() {
        let _env = crate::test_env::state_dir();
        let mut p = Persist::boot(0);
        assert!(saved(0).expect("boot").open);
        p.finish_close();
        assert!(!saved(0).expect("closed").open);
    }

    #[test]
    fn shell_layout_saves() {
        let _env = crate::test_env::state_dir();
        let mut p = Persist::from_session(SessionState::default());
        p.save_shell_layout(ShellLayout {
            sidebar_width: 300.0,
            right_sidebar_width: 320.0,
            sidebar_form: crate::session_state::SidebarForm::Slim,
            right_sidebar_open: true,
        });
        let s = saved(0).expect("saved");
        let l = s.shell_layout().expect("layout");
        assert_eq!(l.sidebar_width, 300.0);
        assert!(l.right_sidebar_open);
    }

    #[test]
    fn save_flushes_sidebar_ipc_mutations() {
        let _env = crate::test_env::state_dir();
        let mut p = Persist::from_session(SessionState::default());
        p.session.set_repo_expanded("/w/vp", true);
        p.session.currents_order = Some(vec!["/w/b".into(), "/w/a".into()]);
        assert!(saved(0).is_none());
        p.save();
        let s = saved(0).expect("saved");
        assert_eq!(s.repo_expanded("/w/vp"), Some(true));
        assert_eq!(
            s.currents_order.as_deref(),
            Some(&["/w/b".to_string(), "/w/a".to_string()][..])
        );
    }
}
