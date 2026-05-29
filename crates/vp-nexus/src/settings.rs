//! Phase A3a: VP 個人設定同期 — unison QUIC + session-scoped auth。
//!
//! ## transport 棲み分け
//!
//! auth (= `/v1/auth/*`、 [`crate::auth`]) は HTTP のまま (A1/A2)。 settings sync
//! **だけ** unison QUIC channel で行う。 理由は (i) VP が既に unison を多用、
//! (ii) DynamicProtocol の runtime schema discovery を dogfood、 (iii) QUIC TLS 1.3
//! で wire 暗号化済。
//!
//! ## 認証 = session-scoped verify-once
//!
//! settings channel の最初の `Authenticate` request で JWT を **1 回だけ署名検証**し、
//! 抽出した `sub` を channel handler loop の stack-local 変数に束ねる。 以降の
//! `Get` / `Set` は同 channel session 内でその `sub` を使う (= JWT 再送不要、 RSA
//! 検証は session あたり 1 回)。
//!
//! **重要 (security)**: QUIC の暗号化は wire を守るが client を認証しない。 `sub` を
//! そのまま信じるのは なりすまし可能なので、 最初の署名検証は必須。 検証済 `sub` は
//! per-connection task の stack 上にあり、 QUIC stream は 1 本 = 1 session で splice
//! 不能なため、 一度認証すれば session 終了まで信頼してよい。
//!
//! ## store
//!
//! A3a は in-memory ([`SettingsStore`] = `HashMap<sub, StoredSettings>`)。 file
//! 永続化 + conflict 検出は A3b で [`SettingsStore`] の seam を差し替えて実装する
//! (= `get` / `set` の public API は不変に保つ)。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use serde::Serialize;
use tokio::sync::Mutex;
use unison::network::channel::UnisonChannel;
use unison::network::quic::QuicServer;
use unison::network::{CertSource, MessageType, NetworkError, ProtocolServer};

/// settings channel 名 (= KDL schema の `channel "settings"` と一致)。
pub const SETTINGS_CHANNEL: &str = "settings";

/// QUIC port の HTTP port からの offset。 TCP (HTTP) と UDP (QUIC) は OS で port
/// 名前空間が独立するため、 同一 port 番号で共存できる (= 既存 vantage-point 慣行)。
pub const QUIC_PORT_OFFSET: u16 = 0;

/// `enable_discovery` に渡す + DynamicProtocol client が validate する KDL schema。
///
/// grammar は club-unison の `schema_registry` test の "demo" schema 書式に準拠
/// (`required=#true` は KDL boolean、 `type="int"` は JSON integer)。 version を
/// bump したら DynamicProtocol client は次回 fetch 時に renegotiate する。
pub const SETTINGS_PROTOCOL_KDL: &str = r#"
protocol "vp-settings" version="0.3.0" {
    namespace "vp.settings"
    channel "settings" from="client" lifetime="persistent" {
        request "Authenticate" {
            field "token" type="string" required=#true
            returns "AuthResult" {
                field "sub" type="string" required=#true
            }
        }
        request "Get" {
            returns "Settings" {
                field "kdl" type="string"
                field "version" type="int"
            }
        }
        request "Set" {
            field "kdl" type="string" required=#true
            returns "SetResult" {
                field "version" type="int"
            }
        }
        request "NodeCreate" {
            field "parent" type="string"
            field "name" type="string" required=#true
            field "or_ref" type="string"
            returns "NodeCreated" {
                field "node" type="json"
            }
        }
        request "TreeGet" {
            returns "Tree" {
                field "nodes" type="json"
            }
        }
        request "TreeResolve" {
            returns "ResolvedTree" {
                field "nodes" type="json"
            }
        }
        request "NodeRename" {
            field "id" type="string" required=#true
            field "name" type="string" required=#true
            returns "NodeRenamed" {
                field "ok" type="bool"
            }
        }
        request "NodeMove" {
            field "id" type="string" required=#true
            field "new_parent" type="string"
            returns "NodeMoved" {
                field "ok" type="bool"
            }
        }
        request "NodeDelete" {
            field "id" type="string" required=#true
            returns "NodeDeleted" {
                field "deleted" type="int"
            }
        }
    }
}
"#;

/// 1 user 分の保存済設定 — KDL 文字列 + version (= 楽観 concurrency の素地)。
#[derive(Clone, Default, Debug)]
pub struct StoredSettings {
    /// 設定本体 (= 不透明 KDL 文字列、 server は中身を解釈しない)
    pub kdl: String,
    /// 書き込みごとに +1 する単調 counter。 A3a は last-write-wins、 A3b で
    /// base_version conflict 検出に拡張する。
    pub version: u64,
}

/// `sub` → [`StoredSettings`] の in-memory store。
///
/// `Arc<Mutex<..>>` を wrap した `Clone` 型。 `serve_settings` で 1 個作り、
/// channel handler closure に clone して渡す。 **`get` / `set` の public API が
/// A3b (file 永続化) の差し替え seam**。
#[derive(Clone, Default)]
pub struct SettingsStore {
    inner: Arc<Mutex<HashMap<String, StoredSettings>>>,
}

impl SettingsStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 指定 user の設定を取得。 未保存なら default (= `{kdl: "", version: 0}`)。
    pub async fn get(&self, sub: &str) -> StoredSettings {
        self.inner
            .lock()
            .await
            .get(sub)
            .cloned()
            .unwrap_or_default()
    }

    /// 指定 user の設定を上書きし、 新しい version を返す (= A3a は last-write-wins、
    /// version は previous + 1)。
    pub async fn set(&self, sub: &str, kdl: String) -> u64 {
        let mut map = self.inner.lock().await;
        let entry = map.entry(sub.to_string()).or_default();
        entry.kdl = kdl;
        entry.version += 1;
        entry.version
    }
}

// ============================================================================
// Node tree (= OR file grouping、 dogfood 14)
// ============================================================================

/// VP 層の node tree の 1 node (= uniform model)。
///
/// `or_ref` を持てば OR (ObjectRecords) の cloud file への参照 (= leaf 相当)、
/// なければ folder。 どの node も子を持てる (= 階層化)。 階層は `parent` ポインタで
/// 表現し、 子は `parent == self.id` で filter する。 実ファイルは OR が SSOT、
/// VP は orRef (= uuid 文字列) を grouping する metadata-only。
#[derive(Clone, Debug, Serialize)]
pub struct Node {
    /// node id (= per-user server 生成 counter `"n1"`, `"n2"`…)
    pub id: String,
    /// 親 node id (= `None` は root 直下)
    pub parent: Option<String>,
    /// 表示名
    pub name: String,
    /// OR record uuid (= 参照するクラウドファイル、 folder node は `None`)
    pub or_ref: Option<String>,
}

/// 1 user 分の node tree (= flat map + id counter)。
#[derive(Clone, Default)]
struct UserTree {
    nodes: HashMap<String, Node>,
    next_id: u64,
}

/// `sub` → [`UserTree`] の in-memory store。
///
/// [`SettingsStore`] と同じく `Arc<Mutex<..>>` wrap の `Clone` 型。 A3b で file-backed
/// に差し替える seam は public API (= create/tree/rename/move_node/delete) を保つ。
/// **ミニマム段階では OR API call はしない** (= orRef は uuid 文字列として保持するのみ、
/// 存在検証 / resolve は後続増分)。
#[derive(Clone, Default)]
pub struct NodeStore {
    inner: Arc<Mutex<HashMap<String, UserTree>>>,
}

impl NodeStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// node を作成して返す。 `parent` 指定時は存在検証 (= 不在なら Err)。
    pub async fn create(
        &self,
        sub: &str,
        parent: Option<String>,
        name: String,
        or_ref: Option<String>,
    ) -> Result<Node, String> {
        let mut map = self.inner.lock().await;
        let tree = map.entry(sub.to_string()).or_default();
        if let Some(p) = &parent
            && !tree.nodes.contains_key(p)
        {
            return Err(format!("parent node not found: {p}"));
        }
        tree.next_id += 1;
        let id = format!("n{}", tree.next_id);
        let node = Node {
            id: id.clone(),
            parent,
            name,
            or_ref,
        };
        tree.nodes.insert(id, node.clone());
        Ok(node)
    }

    /// user の全 node を返す (= client が parent ポインタで階層を再構築)。
    /// id 昇順で決定的に並べる (= test 安定)。
    pub async fn tree(&self, sub: &str) -> Vec<Node> {
        let map = self.inner.lock().await;
        match map.get(sub) {
            Some(t) => {
                let mut v: Vec<Node> = t.nodes.values().cloned().collect();
                v.sort_by(|a, b| a.id.cmp(&b.id));
                v
            }
            None => Vec::new(),
        }
    }

    /// node の名前を変更。
    pub async fn rename(&self, sub: &str, id: &str, name: String) -> Result<(), String> {
        let mut map = self.inner.lock().await;
        let tree = map
            .get_mut(sub)
            .ok_or_else(|| "no tree for user".to_string())?;
        let node = tree
            .nodes
            .get_mut(id)
            .ok_or_else(|| format!("node not found: {id}"))?;
        node.name = name;
        Ok(())
    }

    /// node を別の親へ移動 (= reparent)。
    ///
    /// cycle 防止: `new_parent` を node 自身 / その子孫にはできない (= 自分の subtree に
    /// 自分を入れると tree が壊れる)。 `new_parent` から root へ遡って `id` に当たれば reject。
    pub async fn move_node(
        &self,
        sub: &str,
        id: &str,
        new_parent: Option<String>,
    ) -> Result<(), String> {
        let mut map = self.inner.lock().await;
        let tree = map
            .get_mut(sub)
            .ok_or_else(|| "no tree for user".to_string())?;
        if !tree.nodes.contains_key(id) {
            return Err(format!("node not found: {id}"));
        }
        if let Some(p) = &new_parent {
            if !tree.nodes.contains_key(p) {
                return Err(format!("parent node not found: {p}"));
            }
            // cycle 検査: p (= 新親) から root へ遡り id に当たれば自己 / 子孫への移動
            let mut cur = Some(p.clone());
            while let Some(c) = cur {
                if c == id {
                    return Err("cannot move a node into itself or its descendant".to_string());
                }
                cur = tree.nodes.get(&c).and_then(|n| n.parent.clone());
            }
        }
        tree.nodes.get_mut(id).unwrap().parent = new_parent;
        Ok(())
    }

    /// node とその subtree を cascade 削除し、 削除した node 数を返す。
    pub async fn delete(&self, sub: &str, id: &str) -> Result<usize, String> {
        let mut map = self.inner.lock().await;
        let tree = map
            .get_mut(sub)
            .ok_or_else(|| "no tree for user".to_string())?;
        if !tree.nodes.contains_key(id) {
            return Err(format!("node not found: {id}"));
        }
        // BFS で subtree の全 node id を集めてから一括削除
        let mut to_remove = vec![id.to_string()];
        let mut i = 0;
        while i < to_remove.len() {
            let cur = to_remove[i].clone();
            for (nid, n) in tree.nodes.iter() {
                if n.parent.as_deref() == Some(cur.as_str()) {
                    to_remove.push(nid.clone());
                }
            }
            i += 1;
        }
        let count = to_remove.len();
        for nid in &to_remove {
            tree.nodes.remove(nid);
        }
        Ok(count)
    }
}

// ============================================================================
// OR (ObjectRecords) client (= dogfood 15、 or_ref の実在検証)
// ============================================================================

/// OR record `GET /records/<uuid>` レスポンスの検証結果。
#[derive(Debug)]
pub enum OrValidation {
    /// OR が record を返した (= ref 実在)
    Found,
    /// OR が 404 (= ref 不在)
    NotFound,
    /// OR が token を拒否 (= 401/403、 audience/scope 不一致の疑い)
    Unauthorized,
    /// OR 到達不可 / 5xx / その他 (= 検証不能)
    Unreachable(String),
}

/// OR (ObjectRecords) への client。
///
/// 設計: creo-memories backend の `OR_API_BASE_URL` + `OR_FORWARD_USER_TOKEN` pattern
/// を踏襲 ([[mem_1CbMPhsNtXgpw3hnVhP5d4]])。 `base_url` 未設定 (= `None`) なら検証無効
/// (= dogfood 14 の挙動維持)。 設定時は **end user の token を透過 forward** して OR を
/// 叩く (= nexus は自前 credential を持たず confused-deputy にならない、 OR が実 user で authz)。
#[derive(Clone, Default)]
pub struct OrClient {
    /// OR API base URL (= env `NEXUS_OR_API_BASE_URL`、 例 `https://api.objectrecords.io`)。
    /// `None` なら or_ref 検証を skip する。
    base_url: Option<String>,
}

impl OrClient {
    /// 検証無効な client (= base_url なし)。
    pub fn disabled() -> Self {
        Self { base_url: None }
    }

    /// base_url 明示で構築 (= test で mock OR を inject する用)。
    pub fn new(base_url: Option<String>) -> Self {
        Self { base_url }
    }

    /// env `NEXUS_OR_API_BASE_URL` から構築。 未設定なら検証無効。
    pub fn from_env() -> Self {
        Self {
            base_url: std::env::var("NEXUS_OR_API_BASE_URL")
                .ok()
                .filter(|s| !s.is_empty()),
        }
    }

    /// OR 検証が有効か (= base_url 設定済)。
    pub fn is_enabled(&self) -> bool {
        self.base_url.is_some()
    }

    /// or_ref (= OR record uuid) が OR に実在するか、 user token を forward して検証。
    /// base_url 未設定なら検証 skip で [`OrValidation::Found`] 扱い (= 機能 off)。
    pub async fn validate_ref(&self, or_ref: &str, user_token: &str) -> OrValidation {
        let Some(base) = &self.base_url else {
            return OrValidation::Found; // 検証無効 = 素通し
        };
        // defense in depth: URL 構築前に format を再検証 (= SSRF 防止、 不正は不在扱い)
        if !is_valid_or_ref(or_ref) {
            return OrValidation::NotFound;
        }
        let url = format!("{}/records/{}", base.trim_end_matches('/'), or_ref);
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => return OrValidation::Unreachable(format!("client build: {e}")),
        };
        match client
            .get(&url)
            .header("authorization", format!("Bearer {user_token}"))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    OrValidation::Found
                } else if status == reqwest::StatusCode::NOT_FOUND {
                    OrValidation::NotFound
                } else if status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN
                {
                    OrValidation::Unauthorized
                } else {
                    OrValidation::Unreachable(format!("OR returned {status}"))
                }
            }
            Err(e) => OrValidation::Unreachable(format!("request failed: {e}")),
        }
    }

    /// or_ref (= OR record uuid) を OR から取得し、 raw JSON metadata を返す
    /// (= TreeResolve 用)。 base_url 未設定 / 404 / auth-fail / 到達不可 は `None`
    /// (= view は soft failure、 解決できなければ or_meta null)。
    ///
    /// VP は OR の JSON を **解釈せず opaque に pass-through** する (= OR schema 非結合、
    /// metadata-only 哲学)。 client が field を解釈する。
    pub async fn fetch_record(&self, or_ref: &str, user_token: &str) -> Option<serde_json::Value> {
        let base = self.base_url.as_ref()?;
        // defense in depth: URL 構築前に format を再検証 (= SSRF 防止)
        if !is_valid_or_ref(or_ref) {
            return None;
        }
        let url = format!("{}/records/{}", base.trim_end_matches('/'), or_ref);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .ok()?;
        let resp = client
            .get(&url)
            .header("authorization", format!("Bearer {user_token}"))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None; // 404 / auth-fail / 5xx → soft (or_meta null)
        }
        resp.json::<serde_json::Value>().await.ok()
    }
}

/// nexus が channel handler に渡す store 束 (= settings + node tree + OR client)。
///
/// `serve_settings` で 1 個作り、 register_channel closure に clone して渡す。
/// 新しい store を足すときはここに field を追加する。
#[derive(Clone, Default)]
pub struct NexusStores {
    pub settings: SettingsStore,
    pub nodes: NodeStore,
    pub or_client: OrClient,
}

impl NexusStores {
    /// 全 store default (= OR 検証無効)。 test の基本形。
    pub fn new() -> Self {
        Self::default()
    }

    /// production 用 — OR client を env から構築 (= 他 store は default)。
    pub fn from_env() -> Self {
        Self {
            or_client: OrClient::from_env(),
            ..Self::default()
        }
    }
}

/// error response の共通 builder (= `{"error": "..."}`)。
///
/// `MessageType::Error` ではなく通常 response payload に error を載せることで、
/// client (= 生 `ProtocolClient` / `DynamicProtocol` 両方) が普通に受け取って
/// inspect できる (= 既存 unison_server.rs 慣行)。
fn err_value(msg: impl Into<String>) -> serde_json::Value {
    serde_json::json!({ "error": msg.into() })
}

/// or_ref (= OR record uuid) の format を検証する (= SSRF / URL path injection 防止)。
///
/// OR record id は uuid 相当 (= hex + hyphen)。 厳格な allowlist `[A-Za-z0-9_-]{1,64}`
/// で受け、 `/` `..` `?` `#` `&` `:` `@` `.` 等の URL metacharacter を全て排除する。
/// これにより `or_ref` を OR の URL path に interpolate しても path traversal /
/// query 注入 / host 注入ができない (= security review MEDIUM の対処)。
fn is_valid_or_ref(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `Authenticate` request の処理 — JWT を署名検証して `sub` を返す。
///
/// payload に `token` (= access token) が必須。 [`crate::auth::verify_token`] で
/// 署名 / iss / aud / exp を検証する (= HTTP `/v1/auth/me` と同じ verify path)。
async fn authenticate(payload: &serde_json::Value) -> Result<String, String> {
    let token = payload
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Authenticate: missing 'token' field".to_string())?;
    // 署名検証は必須 (= bare sub を信じると なりすまし可能、 暗号化 != 認証)。
    let claims = crate::auth::verify_token(token)
        .await
        .map_err(|e| format!("authentication failed: {e:?}"))?;
    Ok(claims.sub)
}

/// 認証済 session の method dispatch (= settings Get/Set + node tree 操作)。
///
/// 認証は呼び出し元 (= `run_settings_channel`) が済ませ、 ここには検証済 `sub` と
/// raw `token` (= OR forward 用) が渡る。 未知 method は error response を返す
/// (= 認証前と区別される)。
async fn dispatch_authed(
    stores: &NexusStores,
    sub: &str,
    token: &str,
    method: &str,
    payload: &serde_json::Value,
) -> serde_json::Value {
    match method {
        // --- settings (= A3a) ---
        "Get" => {
            let s = stores.settings.get(sub).await;
            serde_json::json!({ "kdl": s.kdl, "version": s.version })
        }
        "Set" => match payload.get("kdl").and_then(|v| v.as_str()) {
            None => err_value("Set: missing 'kdl' field"),
            Some(kdl) => {
                let version = stores.settings.set(sub, kdl.to_string()).await;
                serde_json::json!({ "version": version })
            }
        },

        // --- node tree (= OR file grouping、 dogfood 14 + OR 実連携 dogfood 15) ---
        "NodeCreate" => {
            let name = match payload.get("name").and_then(|v| v.as_str()) {
                Some(n) => n.to_string(),
                None => return err_value("NodeCreate: missing 'name' field"),
            };
            let parent = payload
                .get("parent")
                .and_then(|v| v.as_str())
                .map(String::from);
            let or_ref = payload
                .get("or_ref")
                .and_then(|v| v.as_str())
                .map(String::from);

            // 入口で or_ref の format を検証 (= 不正 ref を tree に入れない + SSRF 防止)。
            // OR 設定の有無に関わらず常に検証する。
            if let Some(r) = &or_ref
                && !is_valid_or_ref(r)
            {
                return err_value("invalid or_ref format (expected [A-Za-z0-9_-], max 64 chars)");
            }

            // or_ref があれば OR に実在検証 (= base_url 未設定なら素通し)。
            // user token を forward して OR を叩く (= confused-deputy 回避、 OR が実 user authz)。
            if let Some(r) = &or_ref {
                match stores.or_client.validate_ref(r, token).await {
                    OrValidation::Found => {}
                    OrValidation::NotFound => {
                        return err_value(format!("or_ref not found in OR: {r}"));
                    }
                    OrValidation::Unauthorized => {
                        return err_value(
                            "OR rejected the token (audience/scope mismatch?)".to_string(),
                        );
                    }
                    OrValidation::Unreachable(e) => {
                        return err_value(format!("OR validation failed: {e}"));
                    }
                }
            }

            match stores.nodes.create(sub, parent, name, or_ref).await {
                Ok(node) => serde_json::json!({ "node": node }),
                Err(e) => err_value(e),
            }
        }
        "TreeGet" => {
            let nodes = stores.nodes.tree(sub).await;
            serde_json::json!({ "nodes": nodes })
        }
        "TreeResolve" => {
            // 各 node の or_ref を OR から resolve して or_meta (opaque) を添付。
            // soft per-node: 解決失敗は or_meta null、 tree 全体は返す。
            let nodes = stores.nodes.tree(sub).await;
            let mut resolved = Vec::with_capacity(nodes.len());
            for node in &nodes {
                let mut v = serde_json::to_value(node).unwrap_or_default();
                let or_meta = match &node.or_ref {
                    Some(r) => stores.or_client.fetch_record(r, token).await,
                    None => None,
                };
                if let Some(obj) = v.as_object_mut() {
                    obj.insert(
                        "or_meta".to_string(),
                        or_meta.unwrap_or(serde_json::Value::Null),
                    );
                }
                resolved.push(v);
            }
            serde_json::json!({ "nodes": resolved })
        }
        "NodeRename" => {
            let id = match payload.get("id").and_then(|v| v.as_str()) {
                Some(i) => i,
                None => return err_value("NodeRename: missing 'id' field"),
            };
            let name = match payload.get("name").and_then(|v| v.as_str()) {
                Some(n) => n.to_string(),
                None => return err_value("NodeRename: missing 'name' field"),
            };
            match stores.nodes.rename(sub, id, name).await {
                Ok(()) => serde_json::json!({ "ok": true }),
                Err(e) => err_value(e),
            }
        }
        "NodeMove" => {
            let id = match payload.get("id").and_then(|v| v.as_str()) {
                Some(i) => i,
                None => return err_value("NodeMove: missing 'id' field"),
            };
            let new_parent = payload
                .get("new_parent")
                .and_then(|v| v.as_str())
                .map(String::from);
            match stores.nodes.move_node(sub, id, new_parent).await {
                Ok(()) => serde_json::json!({ "ok": true }),
                Err(e) => err_value(e),
            }
        }
        "NodeDelete" => {
            let id = match payload.get("id").and_then(|v| v.as_str()) {
                Some(i) => i,
                None => return err_value("NodeDelete: missing 'id' field"),
            };
            match stores.nodes.delete(sub, id).await {
                Ok(n) => serde_json::json!({ "deleted": n }),
                Err(e) => err_value(e),
            }
        }

        _ => err_value(format!("unknown method: {method}")),
    }
}

/// settings channel の handler loop (= 1 connection / channel ごとに 1 回起動)。
///
/// session-scoped auth: `authenticated_sub` が `None` の間は `Get` / `Set` を拒否、
/// `Authenticate` 成功で `Some(sub)` に束ねる。 失敗した `Authenticate` は sub を
/// 束ねない (= session は locked のまま)。 channel 切断 (= `recv` Err) で loop を
/// 抜けて task 終了、 stack-local の sub は自然に破棄される (= cleanup 不要)。
async fn run_settings_channel(
    stores: NexusStores,
    channel: UnisonChannel,
) -> Result<(), NetworkError> {
    // この変数は per-connection task の stack 上にあり、 QUIC stream 1 本 = 1 session
    // なので splice 不能。 一度束ねれば session 終了まで信頼してよい。
    // (sub, raw token) を保持: token は OR への forward (= dogfood 15) に使う。
    let mut session: Option<(String, String)> = None;

    loop {
        let msg = match channel.recv().await {
            Ok(m) => m,
            Err(_) => break, // peer close / transport error → session 終了
        };
        if msg.msg_type != MessageType::Request {
            continue;
        }

        let request_id = msg.id;
        let method = msg.method.clone();
        let payload = msg.payload_as_value().unwrap_or_default();

        let response: serde_json::Value = match method.as_str() {
            "Authenticate" => {
                // raw token も束ねる (= OR forward 用)、 検証は authenticate 内で実施
                let token = payload
                    .get("token")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                match authenticate(&payload).await {
                    Ok(sub) => {
                        session = Some((sub.clone(), token));
                        serde_json::json!({ "sub": sub })
                    }
                    // 失敗時は session を束ねない (= locked 維持)
                    Err(e) => err_value(e),
                }
            }
            // Authenticate 以外は全て認証必須 → dispatch_authed が method dispatch + unknown
            _ => match &session {
                None => err_value("unauthenticated: send Authenticate first"),
                Some((sub, token)) => dispatch_authed(&stores, sub, token, &method, &payload).await,
            },
        };

        if channel
            .send_response(request_id, &method, &response)
            .await
            .is_err()
        {
            break;
        }
    }

    Ok(())
}

/// settings QUIC server を build → bind → run する。 caller (= main.rs) は
/// `tokio::spawn` で axum HTTP server と並走させる。
///
/// - `addr`: bind address (= `"[::]:9200"` 等)。
/// - `shutdown_rx`: 発火で graceful shutdown。
/// - `ready_tx`: bind 完了時に実 `SocketAddr` を、 失敗時に `None` を送る (= test が
///   ephemeral port `[::1]:0` を使って実 port を知るため)。
pub async fn serve_settings(
    stores: NexusStores,
    addr: String,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    ready_tx: tokio::sync::oneshot::Sender<Option<SocketAddr>>,
) -> anyhow::Result<()> {
    let server =
        ProtocolServer::with_identity("vp-settings", env!("CARGO_PKG_VERSION"), "vp.settings");

    // DynamicProtocol client が schema を fetch できるよう discovery を有効化。
    server
        .enable_discovery(SETTINGS_PROTOCOL_KDL)
        .await
        .context("failed to enable unison discovery")?;

    server
        .register_channel(SETTINGS_CHANNEL, {
            let stores = stores.clone();
            move |_ctx, stream| {
                let stores = stores.clone();
                async move { run_settings_channel(stores, UnisonChannel::new(stream)).await }
            }
        })
        .await;

    let server = Arc::new(server);
    // TODO(A3-prod): dev_localhost cert を mesh keypair / 実 CA cert に差し替える
    // (= nexus QUIC を production deploy する時の cross-cutting follow-up)。
    let mut quic = QuicServer::builder(server)
        .cert_source(CertSource::dev_localhost())
        .build();

    if let Err(e) = quic.bind(&addr).await {
        let _ = ready_tx.send(None);
        return Err(e).context("settings QUIC server failed to bind");
    }
    let _ = ready_tx.send(quic.local_addr());

    quic.start_with_shutdown(shutdown_rx)
        .await
        .context("settings QUIC server terminated with error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn store_get_on_empty_returns_default() {
        let store = SettingsStore::new();
        let s = store.get("nobody").await;
        assert_eq!(s.kdl, "");
        assert_eq!(s.version, 0);
    }

    #[tokio::test]
    async fn store_set_bumps_version_monotonically() {
        let store = SettingsStore::new();
        assert_eq!(store.set("u", "a".to_string()).await, 1);
        assert_eq!(store.set("u", "b".to_string()).await, 2);
        let s = store.get("u").await;
        assert_eq!(s.kdl, "b");
        assert_eq!(s.version, 2);
    }

    #[tokio::test]
    async fn store_isolates_per_sub() {
        let store = SettingsStore::new();
        store.set("alice", "alice-kdl".to_string()).await;
        // bob は影響を受けない
        let bob = store.get("bob").await;
        assert_eq!(bob.kdl, "");
        assert_eq!(bob.version, 0);
        let alice = store.get("alice").await;
        assert_eq!(alice.kdl, "alice-kdl");
    }

    #[tokio::test]
    async fn dispatch_get_then_set_then_get() {
        let stores = NexusStores::new();
        // 初期 Get
        let g0 = dispatch_authed(&stores, "u", "tok", "Get", &serde_json::json!({})).await;
        assert_eq!(g0["version"], 0);
        assert_eq!(g0["kdl"], "");
        // Set
        let s1 = dispatch_authed(
            &stores,
            "u",
            "tok",
            "Set",
            &serde_json::json!({"kdl": "theme \"dark\""}),
        )
        .await;
        assert_eq!(s1["version"], 1);
        // 再 Get
        let g1 = dispatch_authed(&stores, "u", "tok", "Get", &serde_json::json!({})).await;
        assert_eq!(g1["kdl"], "theme \"dark\"");
        assert_eq!(g1["version"], 1);
    }

    #[tokio::test]
    async fn dispatch_set_missing_kdl_returns_error() {
        let stores = NexusStores::new();
        let r = dispatch_authed(&stores, "u", "tok", "Set", &serde_json::json!({})).await;
        assert!(r.get("error").is_some());
    }

    #[tokio::test]
    async fn authenticate_missing_token_errors() {
        let r = authenticate(&serde_json::json!({})).await;
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("missing 'token'"));
    }

    #[test]
    fn protocol_kdl_declares_channel_and_methods() {
        // schema 文字列の guard (= channel / method 名が handler dispatch と一致)
        assert!(SETTINGS_PROTOCOL_KDL.contains("version=\"0.3.0\""));
        assert!(SETTINGS_PROTOCOL_KDL.contains("channel \"settings\""));
        for m in [
            "Authenticate",
            "Get",
            "Set",
            "NodeCreate",
            "TreeGet",
            "TreeResolve",
            "NodeRename",
            "NodeMove",
            "NodeDelete",
        ] {
            assert!(
                SETTINGS_PROTOCOL_KDL.contains(&format!("request \"{m}\"")),
                "KDL schema missing request {m}"
            );
        }
    }

    // === node tree (= dogfood 14) ===

    #[tokio::test]
    async fn node_create_and_tree() {
        let store = NodeStore::new();
        let folder = store
            .create("u", None, "docs".to_string(), None)
            .await
            .expect("create folder");
        assert_eq!(folder.id, "n1");
        assert!(folder.parent.is_none());
        assert!(folder.or_ref.is_none());

        let file = store
            .create(
                "u",
                Some(folder.id.clone()),
                "spec.pdf".to_string(),
                Some("or-uuid-123".to_string()),
            )
            .await
            .expect("create file");
        assert_eq!(file.id, "n2");
        assert_eq!(file.parent.as_deref(), Some("n1"));
        assert_eq!(file.or_ref.as_deref(), Some("or-uuid-123"));

        let tree = store.tree("u").await;
        assert_eq!(tree.len(), 2);
        // id 昇順
        assert_eq!(tree[0].id, "n1");
        assert_eq!(tree[1].id, "n2");
    }

    #[tokio::test]
    async fn node_create_rejects_missing_parent() {
        let store = NodeStore::new();
        let r = store
            .create("u", Some("ghost".to_string()), "x".to_string(), None)
            .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn node_rename_and_move() {
        let store = NodeStore::new();
        let a = store
            .create("u", None, "a".to_string(), None)
            .await
            .unwrap();
        let b = store
            .create("u", None, "b".to_string(), None)
            .await
            .unwrap();
        let child = store
            .create("u", Some(a.id.clone()), "c".to_string(), None)
            .await
            .unwrap();

        // rename
        store
            .rename("u", &child.id, "c2".to_string())
            .await
            .unwrap();
        // move child from a → b
        store
            .move_node("u", &child.id, Some(b.id.clone()))
            .await
            .unwrap();

        let tree = store.tree("u").await;
        let moved = tree.iter().find(|n| n.id == child.id).unwrap();
        assert_eq!(moved.name, "c2");
        assert_eq!(moved.parent.as_deref(), Some(b.id.as_str()));
    }

    #[tokio::test]
    async fn node_move_rejects_cycle() {
        let store = NodeStore::new();
        let a = store
            .create("u", None, "a".to_string(), None)
            .await
            .unwrap();
        let b = store
            .create("u", Some(a.id.clone()), "b".to_string(), None)
            .await
            .unwrap();
        // a を自分の子孫 b の下に移そうとする → cycle で拒否
        let r = store.move_node("u", &a.id, Some(b.id.clone())).await;
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("descendant"));
        // 自分自身への移動も拒否
        assert!(
            store
                .move_node("u", &a.id, Some(a.id.clone()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn node_delete_cascades_subtree() {
        let store = NodeStore::new();
        let root = store
            .create("u", None, "root".to_string(), None)
            .await
            .unwrap();
        let mid = store
            .create("u", Some(root.id.clone()), "mid".to_string(), None)
            .await
            .unwrap();
        let _leaf = store
            .create(
                "u",
                Some(mid.id.clone()),
                "leaf".to_string(),
                Some("or-x".to_string()),
            )
            .await
            .unwrap();
        // 別 root も 1 つ (= cascade 対象外を確認)
        let other = store
            .create("u", None, "other".to_string(), None)
            .await
            .unwrap();

        // root を消すと root/mid/leaf の 3 つが消える
        let deleted = store.delete("u", &root.id).await.unwrap();
        assert_eq!(deleted, 3);
        let tree = store.tree("u").await;
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].id, other.id);
    }

    #[tokio::test]
    async fn node_store_isolates_per_sub() {
        let store = NodeStore::new();
        store
            .create("alice", None, "a".to_string(), None)
            .await
            .unwrap();
        // bob の tree は空
        assert!(store.tree("bob").await.is_empty());
        assert_eq!(store.tree("alice").await.len(), 1);
    }

    #[tokio::test]
    async fn dispatch_node_create_and_tree() {
        let stores = NexusStores::new();
        let created = dispatch_authed(
            &stores,
            "u",
            "tok",
            "NodeCreate",
            &serde_json::json!({"name": "docs"}),
        )
        .await;
        assert_eq!(created["node"]["id"], "n1");
        assert_eq!(created["node"]["name"], "docs");

        let tree = dispatch_authed(&stores, "u", "tok", "TreeGet", &serde_json::json!({})).await;
        assert_eq!(tree["nodes"].as_array().unwrap().len(), 1);
    }

    // === OR client (= dogfood 15) ===

    #[test]
    fn is_valid_or_ref_allowlist() {
        // uuid 相当 (= hex + hyphen) は OK
        assert!(is_valid_or_ref("550e8400-e29b-41d4-a716-446655440000"));
        assert!(is_valid_or_ref("or_exists-123"));
        // SSRF / path injection vector は全て reject
        assert!(!is_valid_or_ref("../admin"));
        assert!(!is_valid_or_ref("a/b"));
        assert!(!is_valid_or_ref("x?foo=bar"));
        assert!(!is_valid_or_ref("x#frag"));
        assert!(!is_valid_or_ref("evil.com"));
        assert!(!is_valid_or_ref("a@b"));
        assert!(!is_valid_or_ref("a:b"));
        assert!(!is_valid_or_ref("")); // 空
        assert!(!is_valid_or_ref(&"x".repeat(65))); // 長すぎ
    }

    #[tokio::test]
    async fn dispatch_node_create_rejects_malformed_or_ref() {
        // OR 無効でも format 検証は効く (= 不正 ref を tree に入れない)
        let stores = NexusStores::new();
        let r = dispatch_authed(
            &stores,
            "u",
            "tok",
            "NodeCreate",
            &serde_json::json!({"name": "x", "or_ref": "../etc/passwd"}),
        )
        .await;
        assert!(
            r.get("error").is_some(),
            "malformed or_ref should be rejected: {r}"
        );
        assert!(r["error"].as_str().unwrap().contains("invalid or_ref"));
        // tree は空のまま (= 作られていない)
        assert!(stores.nodes.tree("u").await.is_empty());
    }

    #[tokio::test]
    async fn or_client_disabled_passes_through() {
        // base_url 未設定 = 検証無効 → 常に Found
        let c = OrClient::disabled();
        assert!(!c.is_enabled());
        assert!(matches!(
            c.validate_ref("any-uuid", "tok").await,
            OrValidation::Found
        ));
    }

    #[tokio::test]
    async fn dispatch_node_create_with_or_disabled_skips_validation() {
        // OR 無効な NexusStores では or_ref ありでも create 成功 (= dogfood 14 挙動)
        let stores = NexusStores::new();
        let r = dispatch_authed(
            &stores,
            "u",
            "tok",
            "NodeCreate",
            &serde_json::json!({"name": "f", "or_ref": "or-uuid-x"}),
        )
        .await;
        assert_eq!(r["node"]["or_ref"], "or-uuid-x");
    }

    #[test]
    fn or_client_from_env_respects_unset() {
        // env 未設定なら disabled (= base_url None)。 単一 test で env を触り race 回避 (N4)。
        unsafe {
            std::env::remove_var("NEXUS_OR_API_BASE_URL");
        }
        assert!(!OrClient::from_env().is_enabled());
        unsafe {
            std::env::set_var("NEXUS_OR_API_BASE_URL", "https://api.objectrecords.io");
        }
        assert!(OrClient::from_env().is_enabled());
        unsafe {
            std::env::remove_var("NEXUS_OR_API_BASE_URL");
        }
    }
}
