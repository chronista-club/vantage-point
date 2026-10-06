// mem_1CfkeiUePgFsbYoGtTeDpq
use vp_app::local_urls::*;
fn entry(id: &str, url: &str) -> Entry {
    Entry {
        id: id.into(),
        url: url.into(),
        label: "Editor preview".into(),
    }
}

#[test]
fn loopback_http_only_without_credentials() {
    for url in [
        "http://localhost:12889/",
        "https://127.0.0.2/a?q=1",
        "http://[::1]:8080/",
    ] {
        assert!(validate_url(url).is_ok(), "{url}");
    }
    for url in [
        "https://example.com",
        "http://localhost.evil",
        "file:///tmp/a",
        "http://user:secret@localhost",
        "http://0.0.0.0:1",
        "http://[::]:1",
        "http://192.168.0.1",
        "http://localhost:0",
        "localhost:123",
    ] {
        assert!(validate_url(url).is_err(), "{url}");
    }
}
#[test]
fn multiple_entries_edit_delete_restore_and_lane_repo_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("urls.json");
    let first = vec![
        entry("a", "http://localhost:12889"),
        entry("b", "http://127.0.0.1:12890"),
    ];
    let stored = save(&path, "/repo", "demo", &[], &first).unwrap();
    assert_eq!(load(&path, "/repo", "demo").unwrap(), stored);
    assert!(load(&path, "/repo", "other").unwrap().is_empty());
    assert!(load(&path, "/else", "demo").unwrap().is_empty());
    let mut edited = stored.clone();
    edited[0].label = "Changed".into();
    edited.remove(1);
    save(&path, "/repo", "demo", &stored, &edited).unwrap();
    assert_eq!(load(&path, "/repo", "demo").unwrap(), edited);
    assert!(
        save(&path, "/repo", "demo", &stored, &[]).is_err(),
        "stale window must not overwrite"
    );
    save(&path, "/repo", "demo", &edited, &[]).unwrap();
    assert!(load(&path, "/repo", "demo").unwrap().is_empty());
}
#[test]
fn invalid_or_corrupt_data_is_not_silently_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("urls.json");
    assert!(
        save(
            &path,
            "/repo",
            "demo",
            &[],
            &[entry("a", "https://example.com")]
        )
        .is_err()
    );
    std::fs::write(&path, "broken").unwrap();
    assert!(load(&path, "/repo", "demo").is_err());
    assert!(save(&path, "/repo", "demo", &[], &[]).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken");
    assert!(save(dir.path(), "/repo", "demo", &[], &[]).is_err());
}
#[tokio::test]
async fn http_error_and_redirect_are_responses_and_redirect_is_not_followed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for status in ["503 Service Unavailable", "302 Found"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let serve = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut method = [0; 5];
            stream.read_exact(&mut method).await.unwrap();
            assert_eq!(&method, b"HEAD ");
            stream.write_all(format!("HTTP/1.1 {status}\r\nLocation: http://192.0.2.1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        };
        let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::join!(probe(&url), serve)
        })
        .await
        .expect("probe must send HEAD to the registered loopback URL");
        assert_eq!(
            result,
            Probe::Responding {
                status: if status.starts_with("503") { 503 } else { 302 }
            }
        );
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert_eq!(probe(&url).await, Probe::Refused);
    assert!(matches!(
        probe("https://example.com").await,
        Probe::Failed { .. }
    ));
}

#[test]
fn open_and_probe_resolve_only_registered_ids_in_the_selected_lane() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("urls.json");
    let item = entry("a", "http://localhost:12889/");
    save(&path, "/repo", "demo", &[], std::slice::from_ref(&item)).unwrap();
    assert!(
        matches!(prepare(&path, "/repo", "demo", Action::Load).unwrap(), Prepared::Entries(e) if e == vec![item])
    );
    assert!(
        matches!(prepare(&path, "/repo", "demo", Action::Open{id:"a".into()}).unwrap(), Prepared::OpenUrl(url) if url == "http://localhost:12889/")
    );
    assert!(
        matches!(prepare(&path, "/repo", "demo", Action::Probe{id:"a".into()}).unwrap(), Prepared::ProbeUrl(url) if url == "http://localhost:12889/")
    );
    assert!(prepare(&path, "/repo", "other", Action::Open { id: "a".into() }).is_err());
    assert!(
        prepare(
            &path,
            "/repo",
            "demo",
            Action::Probe {
                id: "unknown".into()
            }
        )
        .is_err()
    );
}
