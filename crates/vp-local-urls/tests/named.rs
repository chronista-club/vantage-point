use vp_local_urls::*;
#[test]
fn named_upsert_is_persistent_and_does_not_copy_between_lanes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("urls.json");
    let first = set_named(
        &path,
        "/repo",
        "main",
        "preview",
        "http://localhost:5173",
        Some("UI"),
    )
    .unwrap();
    assert_eq!(first[0].id, "preview");
    let next = set_named(
        &path,
        "/repo",
        "main",
        "preview",
        "http://localhost:5174",
        None,
    )
    .unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].label, "UI");
    assert_eq!(load(&path, "/repo", "main").unwrap(), next);
    assert!(load(&path, "/repo", "new-lane").unwrap().is_empty());
    assert!(load(&path, "/other", "main").unwrap().is_empty());
    assert!(save(&path, "/repo", "main", &first, &[]).is_err());
    assert!(
        remove_named(&path, "/repo", "main", "preview")
            .unwrap()
            .is_empty()
    );
    assert!(
        remove_named(&path, "/repo", "main", "preview")
            .unwrap()
            .is_empty()
    );
}
#[test]
fn concurrent_named_writers_preserve_each_others_entries_and_reject_invalid_names() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("urls.json");
    std::thread::scope(|scope| {
        for i in 0..8 {
            let path = &path;
            scope.spawn(move || {
                set_named(
                    path,
                    "/repo",
                    "demo",
                    &format!("url-{i}"),
                    "http://localhost:1234",
                    None,
                )
                .unwrap()
            });
        }
    });
    assert_eq!(load(&path, "/repo", "demo").unwrap().len(), 8);
    for name in ["", "../other", "two words", "UPPER", "a/b"] {
        assert!(set_named(&path, "/repo", "demo", name, "http://localhost:1", None).is_err());
    }
    assert!(set_named(&path, "/repo", "demo", "bad", "https://example.com", None).is_err());
    assert_eq!(load(&path, "/repo", "demo").unwrap().len(), 8);
}

#[test]
fn existing_ui_uuid_remains_addressable_without_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("urls.json");
    let id = "dd7c6093-4d40-4238-9342-d73c770c72a5";
    let old = Entry {
        id: id.into(),
        url: "http://localhost:5173/".into(),
        label: "Existing UI".into(),
    };
    save(&path, "/repo", "demo", &[], &[old]).unwrap();
    let updated = set_named(&path, "/repo", "demo", id, "http://localhost:5174", None).unwrap();
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].id, id);
    assert_eq!(updated[0].label, "Existing UI");
    assert!(remove_named(&path, "/repo", "demo", id).unwrap().is_empty());
}
