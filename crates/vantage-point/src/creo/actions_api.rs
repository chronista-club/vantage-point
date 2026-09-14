//! Current Creo REST boundary. Membership is a label; Atlas is a destination.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

pub(super) fn account_scope(base: &str, token: &str) -> Result<String> {
    use base64::Engine;
    let payload = token
        .split('.')
        .nth(1)
        .context("Creo のアカウントを識別できません")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload)?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes)?;
    let issuer = claims["iss"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("認証元を識別できません")?;
    let subject = claims["sub"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("アカウントを識別できません")?;
    // Partition only. The server validates the token and decides every permission.
    let digest = Sha256::digest(serde_json::to_vec(&(
        base.trim_end_matches('/'),
        issuer,
        subject,
    ))?);
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// Creo catalog uses bare UUIDs; memory responses use EntId. Keep one identity on our wire.
pub(super) fn canonical_atlas_id(id: &str) -> String {
    let raw = id
        .strip_prefix("atlas:")
        .unwrap_or(id)
        .trim_matches(['`', '⟨', '⟩']);
    if let Ok(uuid) = uuid::Uuid::parse_str(raw) {
        return uuid.to_string();
    }
    if let Some(short) = raw.strip_prefix("atl_") {
        let alphabet = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
        let decoded = short.bytes().try_fold(0u128, |n, c| {
            let digit = alphabet.iter().position(|b| *b == c)? as u128;
            n.checked_mul(58)?.checked_add(digit)
        });
        if let Some(value) = decoded.filter(|_| !short.is_empty()) {
            return uuid::Uuid::from_u128(value).to_string();
        }
    }
    raw.to_string()
}

pub(super) struct Api {
    pub base: String,
    pub token: String,
    pub scope: String,
    pub client: reqwest::Client,
}

#[derive(Deserialize)]
struct Label {
    id: String,
    #[serde(rename = "userId")]
    user_id: String,
    name: String,
}

#[derive(Serialize, Deserialize)]
struct CaptureJournal {
    item: CreoAction,
    remote_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct LegacyImport {
    pending: Vec<String>,
    complete: bool,
}

/// Replace within one directory so a restart sees either complete version, never partial JSON.
fn write_journal(path: &Path, journal: &impl Serialize) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("保存先がありません")?;
    std::fs::create_dir_all(parent)?;
    let temp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(&serde_json::to_vec(journal)?)?;
    file.sync_all()?;
    std::fs::rename(temp, path)?;
    Ok(())
}

impl Api {
    pub fn imported(&self, dir: &Path) -> Result<bool> {
        match std::fs::read(dir.join("legacy-import.json")) {
            Ok(bytes) => Ok(serde_json::from_slice::<LegacyImport>(&bytes)?.complete),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    async fn import_legacy(&self, dir: &Path) -> Result<()> {
        let path = dir.join("legacy-import.json");
        let label = self
            .label(true)
            .await?
            .context("ACTIONS ラベルがありません")?;
        let mut migration: LegacyImport = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut seen = std::collections::HashSet::new();
                let mut pending = Vec::new();
                for page in 1usize.. {
                    let mut url = url::Url::parse(&format!("{}/api/memories", self.base))?;
                    url.query_pairs_mut()
                        .append_pair("limit", "100")
                        .append_pair("page", &page.to_string());
                    let value = self.json(self.client.get(url)).await?;
                    let total = value["total"].as_u64().context("件数がありません")?;
                    let rows = value["memories"]
                        .as_array()
                        .context("記憶一覧がありません")?;
                    for row in rows {
                        let id = row["id"].as_str().context("記憶 ID がありません")?;
                        anyhow::ensure!(
                            seen.insert(id.to_string()),
                            "取り込み中に一覧が変わりました。再試行します"
                        );
                        let metadata = &row["metadata"];
                        let tags = metadata["legacy_tags"]
                            .as_array()
                            .or_else(|| metadata["tags"].as_array());
                        if row["userId"].as_str() == Some(&label.user_id)
                            && tags.is_some_and(|tags| {
                                tags.iter().any(|tag| tag.as_str() == Some(ACTIONS_TAG))
                            })
                        {
                            pending.push(id.to_string());
                        }
                    }
                    if seen.len() as u64 >= total {
                        break;
                    }
                    anyhow::ensure!(!rows.is_empty(), "記憶一覧の続きがありません");
                }
                let migration = LegacyImport {
                    pending,
                    complete: false,
                };
                write_journal(&path, &migration)?;
                migration
            }
            Err(e) => return Err(e.into()),
        };
        if migration.complete {
            return Ok(());
        }
        for id in migration.pending.clone() {
            let response = self
                .client
                .post(format!("{}/api/memories/{id}/labels", self.base))
                .bearer_auth(&self.token)
                .json(&serde_json::json!({"labelId": label.id}))
                .send()
                .await?;
            let status = response.status();
            let missing = if status == reqwest::StatusCode::NOT_FOUND {
                self.client
                    .get(format!("{}/api/memories/{id}", self.base))
                    .bearer_auth(&self.token)
                    .send()
                    .await?
                    .status()
                    == reqwest::StatusCode::NOT_FOUND
            } else {
                false
            };
            anyhow::ensure!(
                status.is_success() || missing,
                "Creo が HTTP {status} を返しました"
            );
            migration.pending.retain(|pending| pending != &id);
            write_journal(&path, &migration)?;
        }
        migration.complete = true;
        write_journal(&path, &migration)
    }
    pub fn journal_dir(&self) -> std::path::PathBuf {
        vp_paths::vp_state_dir().join("actions").join(&self.scope)
    }
    pub async fn catalog(&self) -> Result<Vec<CreoAtlas>> {
        let value = self
            .json(self.client.get(format!("{}/api/atlas", self.base)))
            .await?;
        let rows = value["atlas"]
            .as_array()
            .context("Atlas 一覧がありません")?;
        rows.iter()
            .map(|row| {
                let id = row["id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .context("Atlas ID がありません")?;
                let name = row["displayName"]
                    .as_str()
                    .or_else(|| row["name"].as_str())
                    .unwrap_or(id);
                Ok(CreoAtlas {
                    id: canonical_atlas_id(id),
                    name: name.into(),
                    path: row["path"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(name)
                        .into(),
                    writable: row["source"] == "owned"
                        || (row["source"] == "shared"
                            && matches!(row["permission"].as_str(), Some("write" | "admin"))),
                })
            })
            .collect()
    }

    pub async fn flush(
        &self,
        write: &ActionsWrite,
        prev: &[CreoAction],
        dir: &Path,
    ) -> Result<(Vec<CreoAction>, String)> {
        if !write.items.is_empty()
            || !write.removed.is_empty()
            || write.scope.is_some()
            || write.import_legacy
        {
            anyhow::ensure!(
                write.scope.as_deref() == Some(&self.scope),
                "Creo アカウントが変わりました。現在の一覧から再操作してください"
            );
        }
        let path = dir.join("outbox.json");
        let mut pending: ActionsWrite = match std::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).context("保存待ちの記録を読み取れません")?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ActionsWrite {
                scope: Some(self.scope.clone()),
                ..Default::default()
            },
            Err(e) => return Err(e.into()),
        };
        anyhow::ensure!(
            pending.scope.as_deref() == Some(&self.scope),
            "保存待ちのアカウントが一致しません"
        );
        for item in &write.items {
            if write.removed.contains(&item.id) {
                continue;
            }
            if plan_write(item, prev.iter().find(|p| p.id == item.id)) == WritePlan::Unchanged {
                continue;
            }
            pending.items.retain(|p| p.id != item.id);
            pending.items.push(item.clone());
        }
        for id in &write.removed {
            if !pending.removed.contains(id) {
                pending.removed.push(id.clone());
            }
            pending.items.retain(|p| p.id != *id);
        }
        pending.import_legacy |= write.import_legacy;
        if pending.items.is_empty() && pending.removed.is_empty() && !pending.import_legacy {
            return Ok((prev.to_vec(), String::new()));
        }
        write_journal(&path, &pending)?;
        let mut out = prev.to_vec();
        let mut errors = Vec::new();
        if pending.import_legacy {
            match self.import_legacy(dir).await {
                Ok(()) => pending.import_legacy = false,
                Err(error) => errors.push(error.to_string()),
            }
            write_journal(&path, &pending)?;
        }
        for item in pending.items.clone() {
            let result: Result<CreoAction> = async {
                anyhow::ensure!(!item.text.trim().is_empty(), "メモの本文を入力してください");
                if is_local_id(&item.id) {
                    let mut created = self
                        .capture(&item, &dir.join(format!("{}.json", self.slug(&item.id))))
                        .await?;
                    if created.text != item.text
                        || created.done != item.done
                        || created.order != item.order
                    {
                        let desired = CreoAction {
                            id: created.id.clone(),
                            client_id: Some(item.id.clone()),
                            ..item.clone()
                        };
                        update_action(&self.client, &self.base, &self.token, &desired).await?;
                        created = desired;
                    }
                    Ok(created)
                } else {
                    update_action(&self.client, &self.base, &self.token, &item).await?;
                    Ok(item.clone())
                }
            }
            .await;
            let displayed = match result {
                Ok(saved) => {
                    pending.items.retain(|p| p.id != item.id);
                    saved
                }
                Err(error) => {
                    errors.push(error.to_string());
                    item.clone()
                }
            };
            out.retain(|p| {
                p.id != item.id && p.id != displayed.id && p.client_id.as_deref() != Some(&item.id)
            });
            out.push(displayed);
            write_journal(&path, &pending)?;
        }
        for id in pending.removed.clone() {
            let result = if is_local_id(&id) {
                // The UI waits for a native receipt before offering deletion.
                Err(anyhow::anyhow!("保存結果の確認後に削除できます"))
            } else {
                delete_action(&self.client, &self.base, &self.token, &id).await
            };
            match result {
                Ok(()) => {
                    pending.removed.retain(|p| p != &id);
                    out.retain(|p| p.id != id);
                }
                Err(error) => errors.push(error.to_string()),
            }
            write_journal(&path, &pending)?;
        }
        errors.sort();
        errors.dedup();
        Ok((out, errors.join(" / ")))
    }
    async fn json(&self, request: reqwest::RequestBuilder) -> Result<serde_json::Value> {
        let response = request
            .bearer_auth(&self.token)
            .send()
            .await
            .context("Creo に接続できません")?;
        let status = response.status();
        anyhow::ensure!(status.is_success(), "Creo が HTTP {status} を返しました");
        response.json().await.context("Creo の応答を読み取れません")
    }

    async fn label(&self, create: bool) -> Result<Option<Label>> {
        let value = self
            .json(self.client.get(format!("{}/api/labels", self.base)))
            .await?;
        let labels: Vec<Label> = serde_json::from_value(
            value
                .get("labels")
                .context("ラベル一覧がありません")?
                .clone(),
        )?;
        if let Some(label) = labels.into_iter().find(|l| l.name == ACTIONS_TAG) {
            return Ok(Some(label));
        }
        if !create {
            return Ok(None);
        }
        let response = self
            .client
            .post(format!("{}/api/labels", self.base))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({"name":ACTIONS_TAG}))
            .send()
            .await?;
        if response.status().is_success() {
            let value: serde_json::Value = response.json().await?;
            return Ok(Some(serde_json::from_value(value["label"].clone())?));
        }
        // A second client may have created the same label concurrently. Resolve, never fall back to an unfiltered list.
        let value = self
            .json(self.client.get(format!("{}/api/labels", self.base)))
            .await?;
        let labels: Vec<Label> = serde_json::from_value(value["labels"].clone())?;
        labels
            .into_iter()
            .find(|l| l.name == ACTIONS_TAG)
            .map(Some)
            .context("ACTIONS ラベルを作成できません")
    }

    pub async fn read(&self) -> Result<Vec<CreoAction>> {
        let Some(label) = self.label(false).await? else {
            return Ok(Vec::new());
        };
        let mut items = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut page = 1u32;
        loop {
            let mut url = url::Url::parse(&format!("{}/api/memories", self.base))?;
            url.query_pairs_mut()
                .append_pair("labelIds", &label.id)
                .append_pair("limit", &FETCH_LIMIT.to_string())
                .append_pair("page", &page.to_string());
            let value = self.json(self.client.get(url)).await?;
            let total = value["total"]
                .as_u64()
                .context("記憶の総件数がありません")?;
            let count = value["memories"]
                .as_array()
                .context("記憶の一覧がありません")?
                .len();
            let page_items = parse_actions(&value.to_string())?;
            anyhow::ensure!(page_items.len() == count, "記憶の ID がありません");
            for item in page_items {
                anyhow::ensure!(
                    seen.insert(item.id.clone()),
                    "一覧が取得中に変わりました。次の取得で再確認します"
                );
                items.push(item);
            }
            if items.len() as u64 >= total {
                return Ok(items);
            }
            anyhow::ensure!(count > 0, "一覧を最後まで取得できませんでした");
            page = page.checked_add(1).context("ページ数が上限を超えました")?;
        }
    }

    fn slug(&self, id: &str) -> String {
        let hash = Sha256::digest(format!("{}\0{id}", self.scope));
        format!(
            "vp-action-{}",
            hash.iter()
                .take(24)
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        )
    }

    async fn recover(&self, slug: &str, item: &CreoAction, owner: &str) -> Result<Option<String>> {
        let response = self
            .client
            .get(format!("{}/api/memories/{slug}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        anyhow::ensure!(
            response.status().is_success(),
            "作成結果を確認できません: {}",
            response.status()
        );
        let value: serde_json::Value = response.json().await?;
        let memory = &value["memory"];
        anyhow::ensure!(
            memory["slug"] == slug
                && memory["metadata"]["vp"]["client_id"] == item.id
                && memory["atlasId"].as_str().map(canonical_atlas_id)
                    == item.atlas_id.as_deref().map(canonical_atlas_id)
                && memory["userId"] == owner,
            "同じ保存キーの記憶を確認できません。自動作成を停止しました"
        );
        Ok(Some(
            memory["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .context("記憶の ID がありません")?
                .to_string(),
        ))
    }

    pub async fn capture(&self, item: &CreoAction, path: &Path) -> Result<CreoAction> {
        anyhow::ensure!(
            item.atlas_id.as_deref().is_some_and(|s| !s.is_empty()),
            "保存先 Atlas を選択してください"
        );
        let label = self
            .label(true)
            .await?
            .context("ACTIONS ラベルがありません")?;
        let mut journal = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice::<CaptureJournal>(&bytes)
                .context("保存記録を読み取れません")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => CaptureJournal {
                item: item.clone(),
                remote_id: None,
            },
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(
            journal.item.id == item.id && journal.item.atlas_id == item.atlas_id,
            "保存記録の帰属が一致しません"
        );
        // Freeze creation intent until acknowledged. Later edits are a separate update.
        journal.item.client_id = Some(item.id.clone());
        write_journal(path, &journal)?;
        let slug = self.slug(&item.id);
        if journal.remote_id.is_none() {
            journal.remote_id = self.recover(&slug, &journal.item, &label.user_id).await?;
            if journal.remote_id.is_none() {
                let mut body = serde_json::json!({"content":journal.item.text,"atlasId":journal.item.atlas_id,"slug":slug,"metadata":vp_metadata(&journal.item)});
                if let Some(kind) = &journal.item.kind {
                    body["kind"] = serde_json::json!(kind);
                }
                // Even a lost response can only create this globally unique slug once.
                let response = self
                    .json(
                        self.client
                            .post(format!("{}/api/memories", self.base))
                            .json(&body),
                    )
                    .await;
                match response {
                    Ok(value) => {
                        journal.remote_id = Some(
                            value["memory"]["id"]
                                .as_str()
                                .filter(|id| !id.is_empty())
                                .context("記憶の ID がありません")?
                                .to_string(),
                        );
                    }
                    Err(error) => {
                        journal.remote_id =
                            self.recover(&slug, &journal.item, &label.user_id).await?;
                        if journal.remote_id.is_none() {
                            return Err(error);
                        }
                    }
                }
            }
            write_journal(path, &journal)?;
        }
        let id = journal
            .remote_id
            .as_ref()
            .context("記憶の ID がありません")?;
        self.json(
            self.client
                .post(format!("{}/api/memories/{id}/labels", self.base))
                .json(&serde_json::json!({"labelId":label.id})),
        )
        .await?;
        let created = adopt_local_intent(
            CreoAction {
                id: id.clone(),
                ..Default::default()
            },
            &journal.item,
        );
        // Retain the receipt until the cache has incorporated it; stale app snapshots may resend the local ID.
        Ok(created)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json,
        extract::{Request, State},
        response::IntoResponse,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Server {
        posts: AtomicUsize,
        attaches: AtomicUsize,
        lists: AtomicUsize,
        queries: std::sync::Mutex<Vec<String>>,
        memory: std::sync::Mutex<Option<serde_json::Value>>,
        attached_ids: std::sync::Mutex<Vec<String>>,
    }

    async fn route(State(state): State<Arc<Server>>, request: Request) -> axum::response::Response {
        let path = request.uri().path().to_string();
        let method = request.method().clone();
        if path == "/api/atlas" {
            return Json(serde_json::json!({"atlas":[{"id":"atl-owned","name":"Owned","path":"/Owned","source":"owned"},{"id":"atl-shared","name":"Shared","path":"/Shared","source":"shared","permission":"read"}]})).into_response();
        }
        if path == "/api/labels" {
            return Json(serde_json::json!({"labels":[{"id":"lbl-actions","name":"vp-actions","userId":"usr-a"}]})).into_response();
        }
        if path == "/api/memories" && method == reqwest::Method::GET {
            let query = request.uri().query().unwrap_or_default().to_string();
            state.queries.lock().unwrap().push(query);
            let page = state.lists.fetch_add(1, Ordering::SeqCst);
            if !request
                .uri()
                .query()
                .unwrap_or_default()
                .contains("labelIds=")
            {
                return Json(serde_json::json!({"total":3,"memories":[
                    {"id":"mem-old","userId":"usr-a","metadata":{"legacy_tags":["vp-actions"]}},
                    {"id":"mem-unrelated","userId":"usr-a","metadata":{"vp":{"bucket":"ideas"}}},
                    {"id":"mem-shared","userId":"usr-other","metadata":{"legacy_tags":["vp-actions"]}}
                ]})).into_response();
            }
            let memories: Vec<_> = (0..if page == 0 {100} else {1}).map(|i| serde_json::json!({"id":format!("mem-{}", page*100+i),"atlasId":"atl-other","kind":null,"content":"memo"})).collect();
            return Json(serde_json::json!({"memories":memories,"total":101})).into_response();
        }
        if path == "/api/memories" && method == reqwest::Method::POST {
            state.posts.fetch_add(1, Ordering::SeqCst);
            let bytes = axum::body::to_bytes(request.into_body(), 65536)
                .await
                .unwrap();
            let mut memory: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            memory["id"] = serde_json::json!("mem-created");
            memory["userId"] = serde_json::json!("usr-a");
            *state.memory.lock().unwrap() = Some(memory.clone());
            return (
                axum::http::StatusCode::CREATED,
                Json(serde_json::json!({"memory":memory})),
            )
                .into_response();
        }
        if path.ends_with("/labels") {
            if path == "/api/memories/mem-deleted/labels" {
                return axum::http::StatusCode::NOT_FOUND.into_response();
            }
            let bytes = axum::body::to_bytes(request.into_body(), 65536)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if body["labelId"] != "lbl-actions" {
                return axum::http::StatusCode::BAD_REQUEST.into_response();
            }
            state
                .attached_ids
                .lock()
                .unwrap()
                .push(path.split('/').nth(3).unwrap().to_string());
            if state.attaches.fetch_add(1, Ordering::SeqCst) == 0 {
                return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            return Json(serde_json::json!({"ok":true})).into_response();
        }
        if let Some(memory) = state.memory.lock().unwrap().clone() {
            return Json(serde_json::json!({"memory":memory})).into_response();
        }
        axum::http::StatusCode::NOT_FOUND.into_response()
    }

    async fn server() -> (Api, Arc<Server>, tokio::task::JoinHandle<()>) {
        let state = Arc::new(Server::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new()
            .fallback(route)
            .with_state(state.clone());
        let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (
            Api {
                base,
                token: "test".into(),
                scope: "account-a".into(),
                client: reqwest::Client::new(),
            },
            state,
            handle,
        )
    }

    #[tokio::test]
    async fn legacy_import_skips_deleted_candidate_and_continues() {
        let (api, state, handle) = server().await;
        let dir = tempfile::tempdir().unwrap();
        write_journal(
            &dir.path().join("legacy-import.json"),
            &LegacyImport {
                pending: vec!["mem-deleted".into(), "mem-old".into()],
                complete: false,
            },
        )
        .unwrap();
        state.attaches.store(1, Ordering::SeqCst);
        let result = api.import_legacy(dir.path()).await;
        handle.abort();
        result.unwrap();
        assert!(api.imported(dir.path()).unwrap());
        assert_eq!(*state.attached_ids.lock().unwrap(), vec!["mem-old"]);
        assert_eq!(state.lists.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn legacy_import_resumes_only_proven_owned_actions_without_rescanning() {
        let (api, state, handle) = server().await;
        let dir = tempfile::tempdir().unwrap();
        let write = ActionsWrite {
            scope: Some("account-a".into()),
            import_legacy: true,
            ..Default::default()
        };
        let (_, error) = api.flush(&write, &[], dir.path()).await.unwrap();
        assert!(!error.is_empty());
        assert!(!api.imported(dir.path()).unwrap());
        let (_, error) = api
            .flush(&ActionsWrite::default(), &[], dir.path())
            .await
            .unwrap();
        handle.abort();
        assert!(error.is_empty(), "{error}");
        assert!(api.imported(dir.path()).unwrap());
        assert_eq!(state.posts.load(Ordering::SeqCst), 0);
        assert_eq!(state.lists.load(Ordering::SeqCst), 1);
        assert_eq!(
            *state.attached_ids.lock().unwrap(),
            vec!["mem-old", "mem-old"]
        );
    }

    #[tokio::test]
    async fn catalog_includes_destinations_without_project_context() {
        let (api, _, handle) = server().await;
        let catalog = api.catalog().await.unwrap();
        handle.abort();
        assert_eq!(catalog.len(), 2);
        assert!(catalog[0].writable);
        assert!(!catalog[1].writable);
    }

    #[test]
    fn account_scope_survives_refresh_but_separates_users_and_endpoints() {
        use base64::Engine;
        let token = |sub: &str, exp: u32| {
            format!(
                "header.{}.signature",
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(serde_json::json!({"iss":"issuer","sub":sub,"exp":exp}).to_string())
            )
        };
        let a = account_scope("https://creo", &token("a", 1)).unwrap();
        assert_eq!(a, account_scope("https://creo", &token("a", 2)).unwrap());
        assert_ne!(a, account_scope("https://creo", &token("b", 1)).unwrap());
        assert_ne!(a, account_scope("https://other", &token("a", 1)).unwrap());
    }

    #[tokio::test]
    async fn outbox_restores_capture_after_restart_and_rejects_stale_account() {
        let (api, state, handle) = server().await;
        let dir = tempfile::tempdir().unwrap();
        let item = CreoAction {
            id: "act-a".into(),
            text: "memo".into(),
            atlas_id: Some("atl-other".into()),
            bucket: "ideas".into(),
            ..Default::default()
        };
        let write = ActionsWrite {
            items: vec![item],
            scope: Some("account-a".into()),
            ..Default::default()
        };
        let (items, error) = api.flush(&write, &[], dir.path()).await.unwrap();
        assert_eq!(items.len(), 1);
        assert!(!error.is_empty());
        let (items, error) = api
            .flush(&ActionsWrite::default(), &[], dir.path())
            .await
            .unwrap();
        assert!(error.is_empty(), "{error}");
        assert_eq!(items[0].id, "mem-created");
        assert_eq!(state.posts.load(Ordering::SeqCst), 1);
        let stale = ActionsWrite {
            scope: Some("account-b".into()),
            ..write
        };
        assert!(api.flush(&stale, &[], dir.path()).await.is_err());
        handle.abort();
    }

    #[tokio::test]
    async fn label_gate_reads_all_pages_across_atlases() {
        let (api, state, handle) = server().await;
        let items = api.read().await.unwrap();
        handle.abort();
        assert_eq!(items.len(), 101);
        let queries = state.queries.lock().unwrap();
        assert!(queries.iter().all(|q| q.contains("labelIds=lbl-actions")
            && !q.contains("atlasId=")
            && !q.contains("tags=")));
        assert_eq!(items[100].atlas_id.as_deref(), Some("atl-other"));
    }

    #[tokio::test]
    async fn capture_restarts_after_label_failure_without_recreating_memory() {
        let (api, state, handle) = server().await;
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("pending.json");
        let item = CreoAction {
            id: "act-a".into(),
            client_id: Some("act-a".into()),
            atlas_id: Some("atl-other".into()),
            text: "memo".into(),
            bucket: "ideas".into(),
            ..Default::default()
        };
        assert!(api.capture(&item, &journal).await.is_err());
        assert_eq!(state.posts.load(Ordering::SeqCst), 1);
        let created = api.capture(&item, &journal).await.unwrap();
        handle.abort();
        assert_eq!(created.id, "mem-created");
        assert_eq!(state.posts.load(Ordering::SeqCst), 1);
        assert_eq!(state.attaches.load(Ordering::SeqCst), 2);
        let memory = state.memory.lock().unwrap();
        assert_eq!(memory.as_ref().unwrap()["atlasId"], "atl-other");
        assert!(memory.as_ref().unwrap().get("tags").is_none());
    }
}
