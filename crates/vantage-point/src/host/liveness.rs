//! lane の**生死**という事実の供給（doc 44 §7.5「稼働中 lane の保護」）。
//!
//! [`crate::host::farewell`] の判定は `LaneFacts.is_running` を見て稼働中 lane を保持するが、
//! この事実は git からは知れないので**外から供給される**。P3 第一スライスでは供給側が
//! 未実装で、CLI 経路（`vp lane cleanup`）は常に空配列を渡していた — つまり
//! **judge 側の guard は一度も発火していなかった**（判定は正しいが事実が届いていない、
//! §7.4 と同型の「供給の穴」）。
//!
//! 本 module はその事実を daemon の答えから読む部分（**純関数**）を持つ。
//! transport（daemon への QUIC ask）は CLI surface 側（`lane::commands`）に置く —
//! 帳簿（[`crate::host::ledger`]）が §8.4 で採った分担と同じで、Host は事実の**形**と
//! **読み方**だけを知る。
//!
//! ## 「不明」を「無い」に畳まない
//!
//! [`Liveness`] が 2 値なのは、**Daemon 不達 = 稼働 lane が無い、ではない**から。
//! 畳むと「daemon が落ちている時だけ稼働中 lane が削除可能に見える」という、
//! 一番危ない条件でだけ guard が消える形になる（D3「Host は推測しない」）。

use std::path::Path;

/// lane の稼働状況の照会結果。
///
/// この型の唯一の仕事は **「答えられなかった」を「稼働 lane は無い」に潰さないこと**。
/// `Vec<String>` を直接返す設計だと、呼び出し側が `unwrap_or_default()` を書いた瞬間に
/// 不明が空になる（今回のバグの発生機序そのもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness {
    /// daemon が答えた = この repo で今動いている lane 名。
    ///
    /// **0 件も立派な答え**（repo が daemon の registry に居ない = その repo の lane は
    /// 1 本も動いていない）。fold-in 後、lane の engine は daemon プロセスの中で動くので、
    /// registry に repo が無い ⇒ その repo の engine は生きていない、が成り立つ。
    Known(Vec<String>),
    /// 稼働状況を**確認できなかった**（Daemon 不達 / 応答が読めない）。理由を添える。
    ///
    /// ⚠️ これは「稼働 lane が無い」ではない。
    Unknown(String),
}

impl Liveness {
    /// 見送りの判定に進んでよいか。`Ok` = 判定に渡す稼働 lane 名 / `Err` = 進めない理由。
    ///
    /// [`Liveness::Unknown`] は**必ず** `Err`。ここが「不明 → 空リスト」に落ちる唯一の
    /// 分岐点なので、判断を呼び出し側に散らさず 1 箇所に閉じる。
    ///
    /// `force` を引数に取らないのが要点 — `--force` は「判定結果を**実行する**」意思表示で
    /// あって「事実が無くても構わない」ではない。1 つの flag に 2 つの仕事を兼ねさせると、
    /// 「強制削除したい」人が意図せず稼働中 lane の保護まで外すことになる。
    pub fn lanes_for_survey(&self) -> Result<&[String], &str> {
        match self {
            Liveness::Known(lanes) => Ok(lanes),
            Liveness::Unknown(reason) => Err(reason),
        }
    }
}

/// lane の `state` 文字列が「engine が生きている」と言えるか（純関数）。
///
/// # なぜ pid を見ないか
///
/// gui（chat）の lane は **`pid: None` + `state: running` が正常形**
/// （`lane_spawn_actor` は chat mode で PtySlot を張らず engine-less で登録する）。
/// pid の有無で判定すると chat lane を「停止中」と誤認して見送ってしまう。
///
/// # なぜ `spawning` を含めないか
///
/// `spawning` は主に **disk 由来の placeholder** で、`build_lanes_snapshot` が LanePool に
/// 居ない intended performer を `Spawning(pid=null)` で merge した結果（#683 の手当て）。
/// これを稼働中に数えると、daemon が上がっている限り **disk 上の全 lane が稼働中**になり、
/// 見送りが恒久的に無効化される（守り過ぎて機能が死ぬ = これも無音の失敗）。
///
/// `exiting` は engine の teardown 中 = まだ足元を外してはいけないので稼働側に入れる。
fn state_is_alive(state: &str) -> bool {
    matches!(state, "running" | "exiting")
}

/// daemon の `list_all_lanes` 応答から、**この repo の稼働中 lane 名**を取り出す（純関数）。
///
/// 応答の形は `{"repos": [{"repo_name":.., "repo_path":.., "lanes":[LaneInfo]}]}`
/// （`daemon::server::build_node_lanes`）。repo の同定は `repo_path` を
/// [`crate::capability::repo_manager_capability::normalize_path_key`] で正規化して比較する
/// （symlink / 相対パスで別 repo 扱いにならないよう、両辺を同じ関数に通す）。
///
/// lane 名は `address.name` から読む。**LaneInfo の `kind` は doc 44 P2 で消えた**ので、
/// それを条件にすると全 lane が落ちる（既存 `parse_node_lanes` はこの形のまま残っている）。
pub fn running_lanes_in(snapshot: &serde_json::Value, repo_path: &Path) -> Vec<String> {
    let want = crate::capability::repo_manager_capability::normalize_path_key(repo_path);
    let Some(repos) = snapshot.get("repos").and_then(|p| p.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for repo in repos {
        let Some(path) = repo.get("repo_path").and_then(|p| p.as_str()) else {
            continue;
        };
        if crate::capability::repo_manager_capability::normalize_path_key(Path::new(path)) != want {
            continue;
        }
        let Some(lanes) = repo.get("lanes").and_then(|l| l.as_array()) else {
            continue;
        };
        for lane in lanes {
            let alive = lane
                .get("state")
                .and_then(|s| s.as_str())
                .is_some_and(state_is_alive);
            if !alive {
                continue;
            }
            if let Some(name) = lane
                .get("address")
                .and_then(|a| a.get("name"))
                .and_then(|n| n.as_str())
            {
                out.push(name.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `{repo_path, lanes:[{address:{name}, state, pid}]}` の応答を組む test helper。
    fn snapshot(entries: &[(&str, &[(&str, &str)])]) -> serde_json::Value {
        let repos: Vec<serde_json::Value> = entries
            .iter()
            .map(|(path, lanes)| {
                let lanes: Vec<serde_json::Value> = lanes
                    .iter()
                    .map(|(name, state)| {
                        serde_json::json!({
                            "address": {"repo": "p", "name": name},
                            "state": state,
                            // chat lane の正常形を混ぜる（pid は判定に使わない）
                            "pid": serde_json::Value::Null,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "repo_name": "p",
                    "repo_path": path,
                    "lanes": lanes,
                })
            })
            .collect();
        serde_json::json!({ "repos": repos })
    }

    /// 稼働中の判定は `state` で決まる（pid ではない）。
    ///
    /// `running` / `exiting` = engine が生きている。`spawning` は disk 由来 placeholder、
    /// `dead` は spawn 失敗 / 終了済なので稼働ではない。
    #[test]
    fn only_alive_states_count_as_running() {
        let v = snapshot(&[(
            "/tmp/vp-liveness-a",
            &[
                ("live", "running"),
                ("closing", "exiting"),
                ("placeholder", "spawning"),
                ("gone", "dead"),
            ],
        )]);
        let running = running_lanes_in(&v, Path::new("/tmp/vp-liveness-a"));
        assert_eq!(running, vec!["live".to_string(), "closing".to_string()]);
    }

    /// 回帰固定: **`spawning` を稼働中に数えない**。
    ///
    /// `build_lanes_snapshot` は LanePool に居ない intended performer を `Spawning(pid=null)` で
    /// snapshot に merge する（#683）。これを稼働中に含めると、daemon が上がっている限り
    /// disk 上の全 lane が保護対象になり `vp lane cleanup` が恒久的に何もしなくなる。
    #[test]
    fn disk_placeholder_does_not_block_cleanup() {
        let v = snapshot(&[("/tmp/vp-liveness-b", &[("idle", "spawning")])]);
        assert!(running_lanes_in(&v, Path::new("/tmp/vp-liveness-b")).is_empty());
    }

    /// 他 repo の稼働 lane を巻き込まない（同名 lane は珍しくない）。
    #[test]
    fn other_repos_are_filtered_out() {
        let v = snapshot(&[
            ("/tmp/vp-liveness-mine", &[("shared-name", "running")]),
            ("/tmp/vp-liveness-other", &[("shared-name", "running")]),
        ]);
        assert_eq!(
            running_lanes_in(&v, Path::new("/tmp/vp-liveness-mine")),
            vec!["shared-name".to_string()]
        );
    }

    /// repo が registry に居なければ「稼働 lane 0 件」— これは推測ではなく答え。
    ///
    /// fold-in 後、lane の engine は daemon プロセスの中で動くので、daemon が
    /// 「この repo は動いていない」と答えた = その repo の lane は 1 本も生きていない。
    #[test]
    fn absent_repo_yields_no_running_lanes() {
        let v = snapshot(&[("/tmp/vp-liveness-other", &[("w1", "running")])]);
        assert!(running_lanes_in(&v, Path::new("/tmp/vp-liveness-mine")).is_empty());
    }

    /// 壊れた応答（repos 欠落 / 型違い）で panic しない。
    #[test]
    fn malformed_snapshot_is_empty_not_panic() {
        for v in [
            serde_json::json!({}),
            serde_json::json!({"repos": "not-an-array"}),
            serde_json::json!({"repos": [{"repo_path": 1}]}),
            serde_json::json!({"repos": [{"repo_path": "/tmp/x", "lanes": [{}]}]}),
        ] {
            assert!(running_lanes_in(&v, Path::new("/tmp/x")).is_empty());
        }
    }

    /// 回帰固定: **`Unknown` は空リストに化けない**。
    ///
    /// 今回のバグは「事実を持たない呼び出し側が空配列を渡していた」ことだった。
    /// 型の上で不明と 0 件を区別し、不明では判定へ進めない（`lanes_for_survey` が `Err`）。
    #[test]
    fn unknown_never_degrades_to_empty() {
        let unknown = Liveness::Unknown("Daemon 不達".to_string());
        assert_eq!(
            unknown.lanes_for_survey(),
            Err("Daemon 不達"),
            "不明は「稼働 lane が無い」ではない（理由ごと返る）"
        );
        let known = Liveness::Known(vec!["w1".to_string()]);
        assert_eq!(known.lanes_for_survey(), Ok(&["w1".to_string()][..]));
        let empty = Liveness::Known(Vec::new());
        assert_eq!(
            empty.lanes_for_survey(),
            Ok(&[][..]),
            "0 件は答えなので判定に進める"
        );
    }
}
