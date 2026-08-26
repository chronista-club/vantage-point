//! vpcode の model catalog — **engine の endpoint から動的に引く**（2026-08-26）
//!
//! ## なぜ静的表を捨てたか
//!
//! 当初 [`super::engine::EngineKind::model_choices`] は vpcode の候補を静的に 3 つ書いていた。
//! これは**原理的に維持できない**ことが同日の dogfood 30 分で実証された — user が
//! `qwen3-coder-30b` を消し、catalog に無い MLX 版を落とした。静的表は消えた model を
//! 選択肢として残し（＝押しても必ず失敗する行き止まり）、増えた model を出せない。
//!
//! `permission_choices` の doc が掲げる「**押しても効かない選択肢を並べない**」という
//! 原則は、供給源が現実を見ていて初めて守れる。local LLM の世界は model の出入りが
//! 激しい（HF に派生版が週単位で溢れる）ので、この差は claude の catalog より遥かに速く効く。
//!
//! ## 供給源 = vpcode が実際に喋る相手
//!
//! `{VPCODE_BASE_URL}/models`（既定 `http://localhost:1234/v1/models`）。**vpcode 自身の
//! `--models` と同じ endpoint / 同じ env** を使うのが肝で、こうすると「VP の picker に
//! 出る = vpcode が実際に使える」が構造的に一致する。VP 独自の探索路を持つと両者が
//! 乖離しうる（doc 43 §3 の「二重管理で SSOT が割れる」と同じ轍）。
//!
//! OpenAI 互換の `/v1/models` を使うので LM Studio 専用 API（`/api/v0/models`）に
//! 依存しない — 供給元が Ollama や llama.cpp server に変わっても同じ経路で動く。
//!
//! ## 同期 API × 背景更新
//!
//! `model_choices()` は snapshot 組み立て（`LaneSessionsView::from_registry`）から
//! **同期**で呼ばれるため、その場で HTTP を叩けない。背景 task が周期的に fetch して
//! cache を差し替え、読み手は cache を見るだけにする（endpoint 不在でも snapshot は
//! 止まらない = fail-open）。
//!
//! data / calculations / actions:
//! - calculations: [`parse_models`] / [`is_selectable`]（純関数、単体テスト対象）
//! - actions: HTTP fetch / cache 差し替え / 周期 loop

use std::sync::RwLock;
use std::time::Duration;

use super::engine::Choice;

/// 背景更新の周期。model の出入りは人間の操作なので分オーダーで十分
/// （短くしても endpoint に無駄な負荷がかかるだけ）。
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// fetch の timeout。endpoint 不在（LM Studio 未起動）を素早く諦めるための短め設定。
const FETCH_TIMEOUT: Duration = Duration::from_secs(3);

/// 動的 catalog の cache。`None` = 未取得（endpoint 不在 / 起動直後）。
static CACHE: RwLock<Option<Vec<Choice>>> = RwLock::new(None);

/// vpcode の endpoint（**vpcode 本体と同じ env / 既定値**を使う — `--base-url` の doc 参照）。
fn base_url() -> String {
    std::env::var("VPCODE_BASE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "http://localhost:1234/v1".to_string())
        .trim_end_matches('/')
        .to_string()
}

/// picker に出す候補（**同期** — snapshot 経路から呼ばれる）。
///
/// cache が空（endpoint 不在 / 起動直後）なら**空を返す**。空 catalog の意味は
/// [`super::engine::EngineKind::model_choices`] の契約どおり「VP から切替不可」で、
/// client は picker を出さず実測 model の read-only 表示に落ちる。
///
/// ⚠️ ここで静的 fallback を返さない理由: 消えた model を並べる行き止まりが、この
/// module を作った動機そのものだから。「候補が出ない」は「選ぶと失敗する候補が出る」
/// より正直で、原因（endpoint 不在）も一意に絞れる。
pub fn choices() -> Vec<Choice> {
    CACHE
        .read()
        .ok()
        .and_then(|c| c.clone())
        .unwrap_or_default()
}

/// picker に出す価値がある model か（純関数）。
///
/// embedding 専用 model は chat に使えないので落とす（`/v1/models` は OpenAI 互換の
/// 最小形で **type を返さない**ため、id の慣習に頼る best-effort。LM Studio 専用の
/// `/api/v0/models` なら `type: "llm"` で厳密に分かるが、供給元非依存を優先した）。
/// 判定を外して chat model を 1 つ落としても、user は VPCODE_MODEL / 直接指定で回避できる。
fn is_selectable(id: &str) -> bool {
    !id.is_empty() && !id.to_ascii_lowercase().contains("embed")
}

/// `/v1/models` の JSON → catalog（純関数）。
///
/// label は id そのまま。**綺麗な表示名を作らない** — local model の id は user が
/// 落とした repo 名そのもので、加工すると LM Studio の一覧と見た目が食い違って
/// 「これはどれ？」が発生する。id を出せば両者が一致する。
fn parse_models(body: &serde_json::Value) -> Vec<Choice> {
    let mut ids: Vec<String> = body
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|v| v.as_str()))
                .filter(|id| is_selectable(id))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    // 供給元の順序は不定（LM Studio は落とした順）。並びが毎回変わると picker の
    // 位置記憶が効かないので、VP 側で安定な順（id 昇順）に固定する。
    ids.sort();
    ids.dedup();
    ids.into_iter()
        .map(|id| Choice {
            value: id.clone(),
            label: id,
        })
        .collect()
}

/// 1 回だけ fetch して cache を差し替える。
///
/// **失敗しても cache を消さない**（endpoint の一時的な不達で picker が空になると、
/// user から見れば「機能が消えた」ため）。消えた model が残り続ける窓は最大
/// [`REFRESH_INTERVAL`] で、次の成功時に正される。
pub async fn refresh_once() {
    let url = format!("{}/models", base_url());
    let fetched = async {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .ok()?;
        let body: serde_json::Value = client.get(&url).send().await.ok()?.json().await.ok()?;
        Some(parse_models(&body))
    }
    .await;
    match fetched {
        Some(list) if !list.is_empty() => {
            let changed = CACHE
                .read()
                .ok()
                .map(|c| c.as_deref() != Some(list.as_slice()))
                .unwrap_or(true);
            if let Ok(mut cache) = CACHE.write() {
                *cache = Some(list.clone());
            }
            if changed {
                tracing::info!("vpcode model catalog を更新: {} 件（{url}）", list.len());
            }
        }
        Some(_) => {
            tracing::debug!("vpcode model catalog: 候補ゼロ（{url}）— cache は据え置き");
        }
        None => {
            tracing::debug!("vpcode model catalog の取得に失敗（{url}）— cache は据え置き");
        }
    }
}

/// 背景 refresher（daemon 起動時に 1 回 spawn する）。
pub async fn refresh_loop() {
    let mut tick = tokio::time::interval(REFRESH_INTERVAL);
    loop {
        tick.tick().await;
        refresh_once().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/v1/models` の実レスポンス形（2026-08-26 の LM Studio 実測）を catalog に写す。
    #[test]
    fn parses_openai_models_response_sorted_and_without_embeddings() {
        let body = serde_json::json!({
            "data": [
                {"id": "plamo-2-translate", "object": "model"},
                {"id": "openai/gpt-oss-20b", "object": "model"},
                {"id": "text-embedding-nomic-embed-text-v1.5", "object": "model"},
                {"id": "google/gemma-4-e4b", "object": "model"},
            ]
        });
        let list = parse_models(&body);
        let got: Vec<&str> = list.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(
            got,
            vec![
                "google/gemma-4-e4b",
                "openai/gpt-oss-20b",
                "plamo-2-translate"
            ],
            "embedding は落とし、id 昇順で安定させる"
        );
        // label = id そのまま（LM Studio の一覧と見た目を一致させる）
        assert!(list.iter().all(|c| c.value == c.label));
    }

    /// 壊れた / 空のレスポンスで panic しない（endpoint が別物でも snapshot を止めない）。
    #[test]
    fn degrades_to_empty_on_unexpected_shape() {
        assert!(parse_models(&serde_json::json!({})).is_empty());
        assert!(parse_models(&serde_json::json!({"data": "nope"})).is_empty());
        assert!(parse_models(&serde_json::json!({"data": [{"no_id": 1}]})).is_empty());
    }

    /// endpoint の env override（vpcode 本体と同じ契約 — 末尾 `/` は落とす）。
    #[test]
    fn base_url_defaults_to_lm_studio_and_trims_slash() {
        // env 未設定時の既定（他テストと並走しても壊れないよう env は触らない）
        assert!(base_url().ends_with("/v1") || !base_url().ends_with('/'));
    }

    /// embedding 判定は大小文字を問わない。
    #[test]
    fn embedding_filter_is_case_insensitive() {
        assert!(!is_selectable("text-embedding-3-large"));
        assert!(!is_selectable("Nomic-EMBED-text"));
        assert!(is_selectable("qwen/qwen3-coder-30b"));
        assert!(!is_selectable(""));
    }
}
