//! wire cross-process delivery (R3 — 設計 `mem_1CbDLrECNZiNEZqjySLfSB`)
//!
//! `wire_send` の宛先 (`to`) に `agent@<other-project>` のように **自 SP 以外の project**
//! を指すアドレスが含まれるとき、 その message を受信側 SP に HTTP forward する。
//!
//! ## 失敗モデル — best-effort 確定 (決定 `mem_1CbDYnc7GjZkXXWgm9PAfK`)
//!
//! R3 は **best-effort**。 retry / reconciler / pending queue は **作らない**。
//!
//! - ローカル INSERT (= 送信側 accumulation への記録) は常に成功させる。
//! - remote forward が失敗 (受信 SP が落ちている等) しても log するだけで、 `wire_send`
//!   全体は成功扱いで返す。
//! - no-loss 保証は same-process inbox のみ。 cross-process は best-effort。
//!
//! 既存 msgbox の `msgbox_remote.rs` (`RemoteRoutingClient::forward` + `/api/msgbox/remote_deliver`)
//! が mirror 元のパターン。 ただし msgbox は exponential-backoff の 5 回 retry を持つのに対し、
//! wire R3 は best-effort 確定なので **retry を持たない** (1 回 POST して失敗なら log のみ)。
//!
//! ## data / calculations / actions の分離
//!
//! - **calculations** (純粋): [`classify_recipients`] — `to` の各アドレスを self-project と
//!   突き合わせて local / remote に振り分ける。 [`parse_project`] — アドレスから project
//!   segment を抽出。 いずれも I/O を持たず unit test 可能。
//! - **actions** (I/O): [`forward_to_remote`] / [`lookup_sp_port`] — TheWorld への port
//!   lookup と受信 SP への HTTP POST。

use std::collections::BTreeSet;
use std::time::Duration;

use crate::capability::WireMessage;

/// cross-process forward / port lookup の HTTP timeout
///
/// best-effort なので短め。 受信 SP が落ちていれば早く諦めて log する。
const FORWARD_TIMEOUT: Duration = Duration::from_secs(3);

/// world segment 予約語。 `agent@world` 等は SP project ではなく World 階層宛なので、
/// wire の cross-process SP forward 対象にしない (= remote SP として扱わない)。
const WORLD_SEGMENT: &str = "world";

// =============================================================================
// calculations — 宛先分類 (純粋関数、 unit test 可能)
// =============================================================================

/// wire アドレスから project segment を抽出する (`@` の直後、 `/` の手前)
///
/// wire アドレス syntax (doc 16 / `mailbox_addresses`):
/// - bare `agent` — 自 SP の lead (project segment 無し → `None`)
/// - `agent@<project>` — `<project>` の lead
/// - `agent@<project>/<wing>` — `<project>` の wing
///
/// `@` が無ければ `None` (= bare local アドレス)。 `@` 以降の `/` より前を project とみなす。
pub fn parse_project(addr: &str) -> Option<&str> {
    let after_at = addr.split_once('@')?.1;
    // project segment は `/` の手前まで (wing suffix を除く)
    Some(after_at.split('/').next().unwrap_or(after_at))
}

/// `to` の各アドレスを local / remote に振り分ける (R3 宛先分類、 純粋)
///
/// - **remote** — `@<project>` を持ち、 `<project>` が `self_project` とも `world` とも
///   異なるアドレス。 受信側 SP への forward 対象。
/// - **local** — bare アドレス (`@` 無し) / `@<self_project>` / `@<self_project>/<wing>` /
///   `@world` 系。 forward 不要 (= ローカル INSERT で完結、 World 宛は wire の対象外)。
///
/// 戻り値 `WireRecipients` の `remote_projects` は **重複排除済の安定順序** (BTreeSet)。
/// 同一 remote project 宛が複数アドレスあっても forward は project 単位で 1 回でよい
/// (受信側 SP が `to_addrs` 全体を保持するため)。
pub fn classify_recipients(to: &[String], self_project: &str) -> WireRecipients {
    let mut remote_projects: BTreeSet<String> = BTreeSet::new();
    let mut has_local = false;

    for addr in to {
        match parse_project(addr) {
            // project segment あり: self / world 以外なら remote
            Some(proj) if proj != self_project && proj != WORLD_SEGMENT => {
                remote_projects.insert(proj.to_string());
            }
            // self project / world / bare はすべて local 扱い (forward 不要)
            _ => has_local = true,
        }
    }

    WireRecipients {
        has_local,
        remote_projects: remote_projects.into_iter().collect(),
    }
}

/// [`classify_recipients`] の結果 — local 宛の有無と remote project 集合
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireRecipients {
    /// local (自 SP / world / bare) 宛が `to` に 1 つでもあれば true
    pub has_local: bool,
    /// forward 対象の remote project 名 (重複排除済・安定順序)
    pub remote_projects: Vec<String>,
}

impl WireRecipients {
    /// remote forward 対象が 1 つでもあるか
    pub fn has_remote(&self) -> bool {
        !self.remote_projects.is_empty()
    }
}

// =============================================================================
// actions — TheWorld port lookup + 受信 SP への HTTP forward (best-effort)
// =============================================================================

/// TheWorld (`:32000`) に project 名 → SP port を問い合わせる
///
/// `GET /api/world/port_for?project=<name>` を叩く。 msgbox の TheWorld lookup と同じく
/// TheWorld を single source of truth として port を解決する。 TheWorld 側は config の
/// slot から port を決めるため、 対象 SP が稼働中かどうかは問わない (= 稼働判定は後続の
/// HTTP POST 失敗で best-effort に吸収される)。
///
/// best-effort: lookup 失敗 (TheWorld 不在 / project 未登録) は `Err` を返し、 caller が
/// log のみ行う。
pub async fn lookup_sp_port(world_port: u16, project: &str) -> anyhow::Result<u16> {
    let url = format!(
        "http://[::1]:{}/api/world/port_for?project={}",
        world_port, project
    );
    let resp = reqwest::Client::builder()
        .timeout(FORWARD_TIMEOUT)
        .build()?
        .get(&url)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        anyhow::bail!("port_for lookup failed for project '{project}': HTTP {status}");
    }
    #[derive(serde::Deserialize)]
    struct PortForResponse {
        port: u16,
    }
    let body: PortForResponse = resp.json().await?;
    Ok(body.port)
}

/// forward された WireMessage を受信 SP の `/api/wire/remote-deliver` に POST する
///
/// 送るのは [`WireMessage`] 全体 (`id` / `prev` / `from` / `to` / `body` / `created_at`)。
/// `local_seq` も serialize されるが、 受信側は無視して自分の accumulation で採番し直す
/// ([`WiremsgStore::receive_forwarded`](crate::capability::WiremsgStore::receive_forwarded))。
///
/// best-effort: HTTP 失敗は `Err`、 caller が log のみ行う。
async fn http_remote_deliver(sp_port: u16, msg: &WireMessage) -> anyhow::Result<()> {
    let url = format!("http://[::1]:{}/api/wire/remote-deliver", sp_port);
    let resp = reqwest::Client::builder()
        .timeout(FORWARD_TIMEOUT)
        .build()?
        .post(&url)
        .json(msg)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("remote-deliver to port {sp_port} failed: HTTP {status} - {body}");
    }
    Ok(())
}

/// message を remote project 群に best-effort で forward する (R3 の forward 送出)
///
/// `handle_wire_send` がローカル INSERT (`send_root` / `send_reply`) の **後** に呼ぶ。
/// 各 remote project について:
///
/// 1. TheWorld で SP port を lookup ([`lookup_sp_port`])
/// 2. 受信 SP の `/api/wire/remote-deliver` に [`WireMessage`] を POST ([`http_remote_deliver`])
///
/// **best-effort 確定** — どの段階で失敗しても `tracing::warn!` で log するだけで、 関数
/// 全体としては成功で返す (`wire_send` を失敗にしない)。 retry はしない (決定
/// `mem_1CbDYnc7GjZkXXWgm9PAfK`)。 1 つの remote project への forward 失敗が他の remote
/// project への forward を妨げないよう、 各 project を独立に処理する。
pub async fn forward_to_remote(world_port: u16, remote_projects: &[String], msg: &WireMessage) {
    for project in remote_projects {
        match lookup_sp_port(world_port, project).await {
            Ok(sp_port) => {
                if let Err(e) = http_remote_deliver(sp_port, msg).await {
                    // best-effort: forward 失敗は log のみ。 wire_send は成功扱い。
                    tracing::warn!(
                        "wire R3: remote forward 失敗 (best-effort) project={} port={} id={} err={}",
                        project,
                        sp_port,
                        msg.id,
                        e
                    );
                } else {
                    tracing::debug!(
                        "wire R3: remote forward 成功 project={} port={} id={}",
                        project,
                        sp_port,
                        msg.id
                    );
                }
            }
            Err(e) => {
                // best-effort: port lookup 失敗も log のみ。
                tracing::warn!(
                    "wire R3: remote SP port lookup 失敗 (best-effort) project={} id={} err={}",
                    project,
                    msg.id,
                    e
                );
            }
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ----- parse_project -----

    /// parse_project: bare アドレス (`@` 無し) は project 無し
    #[test]
    fn parse_project_bare_is_none() {
        assert_eq!(parse_project("agent"), None);
    }

    /// parse_project: `agent@<project>` から project を抽出
    #[test]
    fn parse_project_with_project_segment() {
        assert_eq!(parse_project("agent@vantage-point"), Some("vantage-point"));
    }

    /// parse_project: wing suffix (`/<wing>`) は project に含めない
    #[test]
    fn parse_project_strips_wing_suffix() {
        assert_eq!(
            parse_project("agent@vantage-point/chore"),
            Some("vantage-point")
        );
    }

    /// parse_project: world 宛も project segment としては "world" を返す
    /// (remote/local 振り分けは classify_recipients 側の責務)
    #[test]
    fn parse_project_world_segment() {
        assert_eq!(parse_project("hermit_purple@world"), Some("world"));
    }

    // ----- classify_recipients: local 判定 -----

    /// bare アドレスは local (forward 不要)
    #[test]
    fn classify_bare_address_is_local() {
        let r = classify_recipients(&["agent".to_string()], "vantage-point");
        assert!(r.has_local, "bare は local");
        assert!(!r.has_remote(), "remote なし");
    }

    /// 自 project 宛 (`@<self>`) は local
    #[test]
    fn classify_self_project_is_local() {
        let r = classify_recipients(&["agent@vantage-point".to_string()], "vantage-point");
        assert!(r.has_local, "自 project 宛は local");
        assert!(!r.has_remote());
    }

    /// 自 project の wing 宛 (`@<self>/<wing>`) も local
    #[test]
    fn classify_self_project_wing_is_local() {
        let r = classify_recipients(&["agent@vantage-point/chore".to_string()], "vantage-point");
        assert!(r.has_local, "自 project の wing 宛は local");
        assert!(!r.has_remote());
    }

    /// world 宛 (`@world`) は remote SP 扱いしない (= local 側に倒す)
    #[test]
    fn classify_world_address_is_not_remote() {
        let r = classify_recipients(&["hermit_purple@world".to_string()], "vantage-point");
        assert!(
            !r.has_remote(),
            "world 宛は SP cross-process forward の対象外"
        );
        assert!(r.has_local, "world 宛は local 側に分類 (forward しない)");
    }

    // ----- classify_recipients: remote 判定 -----

    /// 他 project 宛 (`@<other>`) は remote
    #[test]
    fn classify_other_project_is_remote() {
        let r = classify_recipients(&["agent@creo-memories".to_string()], "vantage-point");
        assert!(r.has_remote(), "他 project 宛は remote");
        assert_eq!(r.remote_projects, vec!["creo-memories".to_string()]);
        assert!(!r.has_local, "local 宛なし");
    }

    /// 他 project の wing 宛も remote、 project segment のみ抽出
    #[test]
    fn classify_other_project_wing_is_remote() {
        let r = classify_recipients(&["agent@creo-memories/sub".to_string()], "vantage-point");
        assert_eq!(
            r.remote_projects,
            vec!["creo-memories".to_string()],
            "wing 宛でも remote_projects は project 名のみ"
        );
    }

    /// local 宛と remote 宛が混在する `to` を正しく振り分ける
    #[test]
    fn classify_mixed_local_and_remote() {
        let to = vec![
            "agent".to_string(),                     // local (bare)
            "agent@vantage-point/chore".to_string(), // local (self wing)
            "agent@creo-memories".to_string(),       // remote
        ];
        let r = classify_recipients(&to, "vantage-point");
        assert!(r.has_local, "local 宛あり");
        assert!(r.has_remote(), "remote 宛あり");
        assert_eq!(r.remote_projects, vec!["creo-memories".to_string()]);
    }

    /// 同一 remote project 宛が複数あっても remote_projects は 1 件に重複排除
    #[test]
    fn classify_dedups_same_remote_project() {
        let to = vec![
            "agent@creo-memories".to_string(),     // lead
            "agent@creo-memories/sub".to_string(), // wing — 同 project
            "canvas@creo-memories".to_string(),    // 別 stand — 同 project
        ];
        let r = classify_recipients(&to, "vantage-point");
        assert_eq!(
            r.remote_projects,
            vec!["creo-memories".to_string()],
            "同 project は forward 1 回でよいので重複排除"
        );
    }

    /// 複数の異なる remote project は安定順序 (sorted) で列挙される
    #[test]
    fn classify_multiple_remote_projects_sorted() {
        let to = vec![
            "agent@zzz-project".to_string(),
            "agent@aaa-project".to_string(),
            "agent@mmm-project".to_string(),
        ];
        let r = classify_recipients(&to, "vantage-point");
        assert_eq!(
            r.remote_projects,
            vec![
                "aaa-project".to_string(),
                "mmm-project".to_string(),
                "zzz-project".to_string(),
            ],
            "remote_projects は安定順序 (BTreeSet 由来)"
        );
    }

    /// 空の `to` は local も remote も無し
    #[test]
    fn classify_empty_to() {
        let r = classify_recipients(&[], "vantage-point");
        assert!(!r.has_local);
        assert!(!r.has_remote());
    }

    // ----- best-effort 失敗パス -----

    /// best-effort: TheWorld が居ない (接続不能) と forward_to_remote は panic せず
    /// 静かに戻る (= caller の wire_send を失敗にしない)
    ///
    /// port 0 は接続できない (= lookup_sp_port が即 Err)。 forward_to_remote は log のみで
    /// 正常終了することを確認する (返り値が無い = 失敗しても呼び元に伝播しない契約)。
    #[tokio::test]
    async fn forward_to_remote_swallows_lookup_failure() {
        let msg = WireMessage::new_root(
            "agent@vantage-point",
            vec!["agent@creo-memories".to_string()],
            serde_json::json!({ "text": "hi" }),
        );
        // world_port = 0 は接続不能 → lookup_sp_port が Err。
        // forward_to_remote は best-effort なので panic せず戻る。
        forward_to_remote(0, &["creo-memories".to_string()], &msg).await;
        // ここに到達 = 失敗が呼び元に伝播していない (best-effort 契約を満たす)
    }

    /// best-effort: lookup_sp_port 自体も到達不能 world に対し Err を返す (panic しない)
    #[tokio::test]
    async fn lookup_sp_port_unreachable_world_returns_err() {
        let result = lookup_sp_port(0, "creo-memories").await;
        assert!(result.is_err(), "接続不能な world への lookup は Err");
    }
}
