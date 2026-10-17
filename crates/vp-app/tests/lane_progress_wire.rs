// mem_1Cg7MpHWBdf6u9CYjyEAaf
use vp_app::daemon_wire::LaneInfo;

#[test]
fn lane_progress_survives_native_sidebar_wire() {
    let progress = serde_json::json!({
        "phase": "requesting", "now": "Bash の実行を承認してください", "error": false
    });
    let lane: LaneInfo = serde_json::from_value(serde_json::json!({
        "address": {"repo":"vp", "name":"sidebar-lane-rows", "key":"vp/lane/sidebar-lane-rows"},
        "state":"running", "agent":"claude", "cwd":"/repos/vp",
        "sessions": {"root":1,"focused":1,"sessions":[{
            "key":1,"agent":"claude","mode":"gui","progress":progress
        }]}
    }))
    .unwrap();
    let wire = serde_json::to_value(lane).unwrap();
    assert_eq!(wire["sessions"]["sessions"][0]["progress"], progress);
}

#[test]
fn lane_progress_wire_fixture() {
    let mut cases = serde_json::Map::new();
    for phase in ["requesting", "working", "waiting", "stopped"] {
        let lane: LaneInfo = serde_json::from_value(serde_json::json!({
            "address": {"repo":"project", "name":phase, "key":format!("project/lane/{phase}")},
            "state":"running", "agent":"claude", "cwd":format!("/project/.vp/lanes/{phase}"),
            "pid":42,"branch":format!("wip/{phase}"),
            "sub_status":{"branch":format!("wip/{phase}"),"dirty_count":2,"behind":4,"last_commit":"a1b2c3d fixture"},
            "sessions": {"root":1,"focused":1,"sessions":[{
                "key":1,"agent":"claude","mode":"gui","last_activity_at":60000,
                "progress":{"phase":phase,"now":if phase=="requesting" {"Bash の承認をお願いします"} else {"回帰テストを確認中"},"error":false}
            }]}
        })).unwrap();
        cases.insert(phase.into(), serde_json::to_value(lane).unwrap());
    }
    let fixture = serde_json::Value::Object(cases);
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("webview/fixtures/lane-progress-wire.json");
    if std::env::var_os("UPDATE_LANE_PROGRESS_FIXTURE").is_some() {
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
        "UPDATE_LANE_PROGRESS_FIXTURE=1 cargo test -p vp-app --test lane_progress_wire"
    );
}
