// mem_1Cg5i626zphsJ9iXXXWCTE
use vp_app::daemon_wire::LaneInfo;
use vp_app::pane::{ActiveComponent, RepoPaneState, SidebarState};

#[test]
fn midi_sidebar_wire_fixture() {
    let mut state = SidebarState::default();
    let startup = serde_json::to_value(&state).unwrap();
    let path = "/repos/vantage-point";
    let mut process = RepoPaneState::new(path, "vantage-point");
    process.state = Some("running".into());
    state.processes.push(process);
    let lane: LaneInfo = serde_json::from_value(serde_json::json!({
        "address": {"repo":"vantage-point", "name":"roto-pins", "key":"vantage-point/lane/roto-pins"},
        "state":"running", "agent":"claude", "cwd":"/repos/vantage-point/.vp/lanes/roto-pins"
    })).unwrap();
    state.lanes_by_repo.insert(path.into(), vec![lane]);
    state.active_component = Some(ActiveComponent {
        repo_path: path.into(),
        kind: "devices".into(),
    });
    let devices = serde_json::to_value(&state).unwrap();
    state.active_component = None;
    state.active_lane_address = Some("vantage-point/lane/roto-pins".into());
    let lane = serde_json::to_value(&state).unwrap();
    assert!(lane.get("active_component").is_none());
    let fixture = serde_json::json!({"startup":startup,"devices":devices,"lane":lane});
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("webview/fixtures/midi-sidebar-wire.json");
    if std::env::var_os("UPDATE_MIDI_WIRE_FIXTURE").is_some() {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            format!("{}\n", serde_json::to_string_pretty(&fixture).unwrap()),
        )
        .unwrap();
    }
    let checked_in: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    assert_eq!(
        fixture, checked_in,
        "regenerate with UPDATE_MIDI_WIRE_FIXTURE=1 cargo test -p vp-app --test midi_sidebar_wire"
    );
}
