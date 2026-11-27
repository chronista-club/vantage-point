// mem_1ChSjZ8945Y4MsvU4F2qEf
use super::*;

fn stat(path: &str, insertions: usize, deletions: usize) -> DiffFileStat {
    DiffFileStat {
        path: path.into(),
        insertions,
        deletions,
        binary: false,
    }
}

#[test]
fn empty_diff_is_idle() {
    assert!(classify_diff(&[]).is_none());
}

#[test]
fn docs_tests_and_small_local_change_can_downgrade_to_a() {
    for files in [
        vec![
            stat("docs/design/thing.md", 200, 50),
            stat("README.md", 2, 0),
        ],
        vec![
            stat("src/tests/check.rs", 200, 0),
            stat("ui/foo.test.tsx", 100, 0),
            stat("tests/a.rs", 1, 0),
        ],
        vec![stat("src/lib.rs", 20, 10), stat("fixtures/input", 300, 0)],
        vec![stat("src/lib.rs", 40, 40), stat("testdata/input", 300, 0)],
    ] {
        let c = classify_diff(&files).unwrap();
        assert_eq!(c.tier, DiffTier::A);
        assert_eq!(
            (
                c.review_required.as_str(),
                c.pr_mode.as_str(),
                c.merge_authority.as_str()
            ),
            ("none", "ready", "lead")
        );
        assert_eq!(c.changed_files, files.len());
        assert_eq!(
            c.insertions,
            files.iter().map(|f| f.insertions).sum::<usize>()
        );
        assert!(!c.reasons.is_empty());
    }
}

#[test]
fn multi_file_and_large_implementation_require_b() {
    for files in [
        vec![stat("src/a.rs", 1, 0), stat("src/b.rs", 1, 0)],
        vec![stat("src/a.rs", 120, 0)],
        vec![stat("src/a.rs", 40, 41)],
    ] {
        let c = classify_diff(&files).unwrap();
        assert_eq!(c.tier, DiffTier::B);
        assert_eq!(
            (
                c.review_required.as_str(),
                c.pr_mode.as_str(),
                c.merge_authority.as_str()
            ),
            ("moody-blues", "ready", "lead")
        );
    }
}

#[test]
fn every_risky_boundary_requires_c_even_for_one_line() {
    for path in [
        "crates/vantage-point/src/daemon/server.rs",
        "crates/vantage-point/src/protocol/messages.rs",
        "crates/vp-app/src/daemon_wire.rs",
        "crates/vantage-point/src/capability/wiremsg_store.rs",
        "vendor/agent-chat",
        "Cargo.toml",
        ".mise/tasks/app/swap",
        ".mise/tasks/app/swap-detached",
        ".mise/tasks/release/mac",
        ".github/workflows/check.yml",
        "AGENTS.md",
        "CLAUDE.md",
        "docs/AGENTS.md",
        "crates/vantage-point/src/lane/session_registry.rs",
        "crates/vantage-point/src/settings_file.rs",
        "crates/vantage-point/src/roto_setup_sync.rs",
        "crates/vantage-point/src/commands/midi.rs",
        "crates/vantage-point/src/roto_midi.rs",
    ] {
        let c = classify_diff(&[stat(path, 1, 0), stat("docs/a.md", 1, 0)]).unwrap();
        assert_eq!(c.tier, DiffTier::C, "{path}");
        assert!(c.reasons.iter().any(|r| r.contains(path)), "{path}");
        assert_eq!(
            (
                c.review_required.as_str(),
                c.pr_mode.as_str(),
                c.merge_authority.as_str()
            ),
            ("santa", "draft", "user")
        );
        assert_eq!(serde_json::to_value(c.tier).unwrap(), "C");
    }
}

#[test]
fn binary_implementation_cannot_claim_small_line_change() {
    let mut file = stat("assets/image.png", 0, 0);
    file.binary = true;
    assert_eq!(classify_diff(&[file]).unwrap().tier, DiffTier::B);
}

#[test]
fn midi_feedback_output_changes_require_c() {
    let c = classify_diff(&[stat("crates/vantage-point/src/devices.rs", 1, 0)]).unwrap();
    assert_eq!(c.tier, DiffTier::C);
    assert!(c.reasons[0].contains("devices.rs"));
}
