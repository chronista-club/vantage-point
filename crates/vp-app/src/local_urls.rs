//! Lane に明示登録するローカル URL。ネットワーク状態は保存しない。
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub url: String,
    pub label: String,
}

/// loopback HTTP(S) だけを許可する。localhost は probe で DNS を使わず固定する。
pub fn validate_url(value: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| "URL は http:// または https:// から入力してください".to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port() == Some(0)
    {
        return Err("資格情報のない http(s) のローカル URL を入力してください".into());
    }
    let host = url
        .host_str()
        .unwrap_or("")
        .trim_start_matches('[')
        .trim_end_matches(']');
    if host != "localhost"
        && !host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    {
        return Err("localhost、127.0.0.0/8、::1 の URL だけ登録できます".into());
    }
    Ok(url)
}

type Document = std::collections::BTreeMap<String, Vec<Entry>>;
fn key(repo: &str, lane: &str) -> String {
    serde_json::to_string(&(repo, lane)).expect("string tuple")
}
fn read_document(path: &Path) -> Result<Document, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| format!("URL 保存データを読めません: {e}"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Document::new()),
        Err(e) => Err(format!("URL 保存データを読めません: {e}")),
    }
}
pub fn load(path: &Path, repo: &str, lane: &str) -> Result<Vec<Entry>, String> {
    // 保存は rename なので読取は常に旧版か新版の全体を得る。
    Ok(read_document(path)?
        .remove(&key(repo, lane))
        .unwrap_or_default())
}
pub fn save(
    path: &Path,
    repo: &str,
    lane: &str,
    expected: &[Entry],
    entries: &[Entry],
) -> Result<Vec<Entry>, String> {
    if entries.len() > 16 {
        return Err("1 Lane に登録できる URL は16件までです".into());
    }
    let mut ids = std::collections::HashSet::new();
    for entry in entries {
        validate_url(&entry.url)?;
        if entry.url.len() > 4096
            || entry.label.trim().is_empty()
            || entry.label.chars().count() > 120
            || entry.id.is_empty()
            || entry.id.len() > 128
            || !ids.insert(&entry.id)
        {
            return Err("用途と重複しない ID を指定してください（用途は120文字以内）".into());
        }
    }
    let parent = path.parent().ok_or("保存先がありません")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    // 同じアプリの複数 window と別プロセスの書き手を一つにする。
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    let mut doc = read_document(path)?;
    let key = key(repo, lane);
    if doc.get(&key).map(Vec::as_slice).unwrap_or_default() != expected {
        return Err("別の画面で登録が更新されました。再読み込みしてから編集してください".into());
    }
    if entries.is_empty() {
        doc.remove(&key);
    } else {
        doc.insert(key, entries.to_vec());
    }
    let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, bytes).map_err(|e| format!("URL を保存できません: {e}"))?;
    std::fs::rename(&temporary, path).map_err(|e| format!("URL を保存できません: {e}"))?;
    Ok(entries.to_vec())
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Probe {
    Responding { status: u16 },
    Refused,
    Failed { message: String },
}
pub async fn probe(value: &str) -> Probe {
    let url = match validate_url(value) {
        Ok(url) => url,
        Err(message) => return Probe::Failed { message },
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(3))
        .resolve_to_addrs(
            "localhost",
            &[
                std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
                std::net::SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 0)),
            ],
        )
        .build();
    let client = match client {
        Ok(client) => client,
        Err(e) => {
            return Probe::Failed {
                message: e.to_string(),
            };
        }
    };
    match client.head(url).send().await {
        Ok(response) => Probe::Responding {
            status: response.status().as_u16(),
        },
        Err(error) => {
            // refused 以外（timeout / TLS 等）から停止を推定しない。
            let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
            while let Some(cause) = source {
                if cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused)
                {
                    return Probe::Refused;
                }
                source = cause.source();
            }
            Probe::Failed {
                message: error.without_url().to_string(),
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Load,
    Save {
        expected: Vec<Entry>,
        entries: Vec<Entry>,
    },
    Probe {
        id: String,
    },
    Open {
        id: String,
    },
}
pub enum Prepared {
    Entries(Vec<Entry>),
    ProbeUrl(String),
    OpenUrl(String),
}
pub fn prepare(path: &Path, repo: &str, lane: &str, action: Action) -> Result<Prepared, String> {
    match action {
        Action::Load => load(path, repo, lane).map(Prepared::Entries),
        Action::Save { expected, entries } => {
            save(path, repo, lane, &expected, &entries).map(Prepared::Entries)
        }
        Action::Probe { ref id } | Action::Open { ref id } => {
            let entry = load(path, repo, lane)?
                .into_iter()
                .find(|entry| &entry.id == id)
                .ok_or("登録 URL が見つかりません。再読み込みしてください")?;
            // 保存後に手動編集された場合も、実行時に同じ制約を適用する。
            let url = validate_url(&entry.url)?.to_string();
            Ok(if matches!(action, Action::Probe { .. }) {
                Prepared::ProbeUrl(url)
            } else {
                Prepared::OpenUrl(url)
            })
        }
    }
}
