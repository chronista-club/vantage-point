//! 「見送り」— Project Host の第一の振る舞い（doc 44 D3 / §7）。
//!
//! merge 済 lane の残骸（worktree / branch / lane state）を掃除する判断と実行。
//!
//! ## なぜ Host の第一号がこれなのか
//!
//! D3 が挙げた 5 つの振る舞い（迎え入れ / 場の維持 / 交通整理 / 見送り / 帳簿）のうち、
//! 見送りは **判定基準が言語化済み・失敗の被害が小さい・毎週確実に価値が出る**。
//! 2026-07-20 の手作業 lane 掃除（19 本中 18 本を機械判定、1 本のみ人間へ）が実演で、
//! その判定基準（PR MERGED? 独自 commit 0? リモート有無?）が本 module の仕様の元になった。
//!
//! ## 層の分離（CLAUDE.md: data / calculations / actions）
//!
//! - **data**: [`LaneFacts`] — 判定に要る事実だけを持つ。git も DB も知らない
//! - **calculations**: [`judge_farewell`] — 純関数。I/O ゼロなのでテストで全分岐を固定できる
//! - **actions**: [`collect_facts`] — git subprocess で事実を集めるだけ。判定は一切しない
//!
//! 既存の `lane::commands::classify_performer_for_cleanup` はこの 3 つが 1 関数に混ざっており、
//! `run_git_in(fetch)` を内部で呼ぶためテストできなかった。本 module はそれを解く。
//!
//! ## Host は推測しない（D3 のアーキテクチャ原則）
//!
//! 判定は 3 値。事実だけで決まらないものは **`AskHuman` として帳簿に積む**（黙って消さない・
//! 黙って残さない）。旧実装は 2 値（削除 / 保持）で、`merged` なら **未コミットの変更を見ずに
//! 削除候補**へ入れていた — これは「推測」に当たる。

use serde::{Deserialize, Serialize};

/// branch が既定 branch に取り込まれているかの状態。
///
/// 3 通りある理由は git の merge 方式ごとに検出手段が違うため（squash merge は元 commit を
/// 歴史に残さないので ancestry では検出できず、PR state を引く必要がある）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergedState {
    /// 未 merge（どの手段でも取り込みを検出できない）
    NotMerged,
    /// ancestry で検出（merge commit / fast-forward）
    Ancestry,
    /// squash / rebase merge（patch-id 一致 or PR が merged state）
    Squash,
}

impl MergedState {
    /// 取り込み済みか（判定の分岐で使う）。
    pub fn is_merged(self) -> bool {
        !matches!(self, MergedState::NotMerged)
    }
}

/// 見送り判定の入力 — **I/O 済みの事実だけ**を持つ data（git も DB も知らない）。
///
/// この型が「Host が何を見て決めているか」の全量になる。増やすときは
/// 「その事実が無いと判定を誤るか？」を通す（Host は推測しないので、
/// 判定に使わない情報は持たない）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneFacts {
    /// lane 名（`LaneAddress.name`）
    pub name: String,
    /// 開発起点 lane か（doc 44 D4 — Host の帳簿が持つポインタが指す lane）。
    ///
    /// P3 初版は「予約名 `conductor` か」で判定していた。D4 で起点が **project の指定**に
    /// なり任意の lane へ移せるようになったため、判定材料は名前ではなく
    /// [`crate::host::ledger`] が解決した起点名との一致になる。
    pub is_origin: bool,
    /// ground（worktree ディレクトリ）が実在するか
    pub has_ground: bool,
    /// 未コミットの変更ファイル数（`git status --short` の行数）
    pub dirty_count: usize,
    /// 既定 branch から見た独自 commit 数（= この lane で積んだ仕事の量）
    pub own_commits: u32,
    /// lane の branch 名（detached HEAD なら `None`）。
    ///
    /// 判定には使わないが、**見送りの実行**（共有 `.git` からの `git branch -d`）に要る。
    /// 実行時に引き直せないため事実として持つ — worktree を消した後では `git` を回す
    /// cwd が消えており取得不能になる（P3 初版はこれで `branch -d` が never-fire だった）。
    pub branch: Option<String>,
    /// 既定 branch への取り込み状態
    pub merged: MergedState,
    /// remote に同名 branch が在るか（push 済みの仕事が失われないかの判断材料）
    pub has_remote_branch: bool,
    /// lane が今動いているか（PtySlot / engine が生きている）
    pub is_running: bool,
}

/// 見送りの判定結果（D3 の 3 層モデルの射影）。
///
/// - [`Reclaim`](Self::Reclaim) = 層 1（決定的判定）で「見送ってよい」と決まったもの
/// - [`Keep`](Self::Keep) = 層 1 で「まだ残す」と決まったもの
/// - [`AskHuman`](Self::AskHuman) = 事実だけで決まらず**層 3（人間へのエスカレーション）**に回すもの
///
/// 層 2（LLM への発注）がここに無いのは、見送りが**決定的に決まる**振る舞いだから。
/// D3 の「Host は決定論。LLM にしない」がこの enum の形に出ている。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum FarewellVerdict {
    /// 見送ってよい（残骸として回収可能）
    Reclaim { reason: String },
    /// 残す（まだ仕事が終わっていない / 構造上消せない）
    Keep { reason: String },
    /// 人間に聞く（事実だけでは決まらない）
    AskHuman { reason: String },
}

impl FarewellVerdict {
    /// 表示・集計用の短いラベル。
    pub fn label(&self) -> &'static str {
        match self {
            FarewellVerdict::Reclaim { .. } => "reclaim",
            FarewellVerdict::Keep { .. } => "keep",
            FarewellVerdict::AskHuman { .. } => "ask_human",
        }
    }

    /// 理由の本文。
    pub fn reason(&self) -> &str {
        match self {
            FarewellVerdict::Reclaim { reason }
            | FarewellVerdict::Keep { reason }
            | FarewellVerdict::AskHuman { reason } => reason,
        }
    }
}

/// 見送ってよいかを決める **純関数**（Host の層 1 = calculations）。
///
/// I/O を一切しないので、全分岐をテストで固定できる。判定順序そのものが仕様なので、
/// 分岐を足すときは「なぜその順に見るのか」を必ず書くこと。
///
/// # 判定順序と根拠
///
/// 1. **開発起点**は消せない（project lifetime に紐づく。P2 の予約名がそのまま境界）
/// 2. **ground 不在**は掃除するものが無い（既に消えている / まだ作られていない）
/// 3. **稼働中**は触らない（engine が生きている lane の足元を外さない）
/// 4. **未コミットの変更**があれば人間へ — **merged かどうかより先に見る**。
///    旧実装は merged なら dirty を見ずに削除候補へ入れており、取り込み済みの branch 上に
///    残った作業を黙って捨てうる形だった（Host は推測しない、の違反）
/// 5. **取り込み済み**なら見送る（本命の枝）
/// 6. **独自 commit 0** は空 lane なので見送ってよい（未 merge でも失うものが無い）。
///    ただし remote に branch があるなら人間へ — こちらが知らない履歴が push 済みでありうる
/// 7. 残りは仕事が生きているので保持
pub fn judge_farewell(facts: &LaneFacts) -> FarewellVerdict {
    if facts.is_origin {
        return FarewellVerdict::Keep {
            reason: "開発起点 lane は project lifetime に紐づく".to_string(),
        };
    }
    if !facts.has_ground {
        return FarewellVerdict::Keep {
            reason: "ground（worktree）が無いので掃除対象が無い".to_string(),
        };
    }
    if facts.is_running {
        return FarewellVerdict::Keep {
            reason: "稼働中（engine が生きている）".to_string(),
        };
    }
    if facts.dirty_count > 0 {
        return FarewellVerdict::AskHuman {
            reason: format!(
                "未コミットの変更が {} 件ある（{}）",
                facts.dirty_count,
                integration_label(facts)
            ),
        };
    }
    if facts.merged.is_merged() {
        return FarewellVerdict::Reclaim {
            reason: format!("{}で作業残なし", integration_label(facts)),
        };
    }
    if facts.own_commits == 0 {
        if facts.has_remote_branch {
            return FarewellVerdict::AskHuman {
                reason: "独自 commit は無いが remote branch が在る（push 済みの履歴がありうる）"
                    .to_string(),
            };
        }
        return FarewellVerdict::Reclaim {
            reason: "独自 commit が無く remote branch も無い（空 lane）".to_string(),
        };
    }
    FarewellVerdict::Keep {
        reason: format!("未 merge の commit が {} 件ある", facts.own_commits),
    }
}

/// 取り込み状態を**断定できる範囲で**言葉にする（理由文の組み立て用）。
///
/// `MergedState` をそのまま出さないのは、`NotMerged` が 2 つの状態を畳んでいるため:
///
/// - lane を作ったばかりで何もしていない（fresh）
/// - lane の作業が **fast-forward merge** で取り込まれ、既定 branch が追いついた
///
/// どちらも `HEAD == origin/<default>` になり、`lane::commands::is_branch_merged` は
/// 「fresh は消さない」安全弁として `false` を返す（ancestry 判定より前で早期 return する）。
/// この 2 つは git のこの情報だけでは区別できないので、**区別できないことを区別できると
/// 書かない** — 実機検証で「merge 済みの lane に『未 merge』と表示される」形で露見した。
fn integration_label(facts: &LaneFacts) -> String {
    match facts.merged {
        MergedState::Ancestry => "merge 済み".to_string(),
        MergedState::Squash => "squash merge 済み".to_string(),
        // 独自 commit 0 = 既定 branch と同位置。fresh か fast-forward 取り込み済かは判らない
        MergedState::NotMerged if facts.own_commits == 0 => {
            "既定 branch と同位置（未着手 or fast-forward で取り込み済）".to_string()
        }
        MergedState::NotMerged => {
            format!("未 merge の commit が {} 件", facts.own_commits)
        }
    }
}

// =============================================================================
// actions — 事実の収集（git subprocess）。ここでは**判定を一切しない**
// =============================================================================

/// 1 lane 分の事実を集める（Host の層 1 に渡す入力を作るだけ）。
///
/// 判定を混ぜないのが要点。旧 `classify_performer_for_cleanup` は収集と判定と分類を
/// 1 関数でやっていたため、git が要る = テストできない状態だった。ここで境界を引くことで
/// [`judge_farewell`] 側は I/O ゼロの純関数として全分岐をテストできる。
///
/// `is_running` は呼び出し側（LanePool を持つ層）が渡す — Host は lane の生死を
/// git からは知れないため。
pub fn collect_facts(
    repo_root: &std::path::Path,
    lane_name: &str,
    is_origin: bool,
    is_running: bool,
    default_branch: &str,
) -> LaneFacts {
    let ground = crate::lane::config::project_lanes_dir(repo_root).join(lane_name);
    // `.git` は clone なら dir / worktree なら file。どちらも `exists()` は true。
    let has_ground = ground.join(".git").exists();

    if !has_ground {
        // ground が無いなら git を引く先が無い。事実として「無い」だけを返す
        // （ここで Keep を返したくなるが、それは判定なので calculations の仕事）。
        return LaneFacts {
            name: lane_name.to_string(),
            is_origin,
            has_ground: false,
            dirty_count: 0,
            own_commits: 0,
            branch: None,
            merged: MergedState::NotMerged,
            has_remote_branch: false,
            is_running,
        };
    }

    // remote の取り込み状況を最新化してから判定材料を集める（fetch 失敗は無視 —
    // オフラインでも「ローカルに見える範囲の事実」で判定できる方が良い）。
    //
    // `--prune` が要る: 素の fetch は **消えた remote branch の tracking ref を残す**ため、
    // GitHub 側で merge 後に head branch が自動削除されていても
    // `refs/remotes/origin/<branch>` が stale に生き残り、`has_remote_branch` が誤って
    // true になる（= 存在しない「push 済みの履歴」を理由に AskHuman へ過剰に倒れる）。
    let _ = crate::lane::commands::run_git_in(&ground, &["fetch", "--quiet", "--prune"]);

    let merged = if crate::lane::commands::is_branch_merged(&ground, default_branch) {
        MergedState::Ancestry
    } else if crate::lane::commands::is_branch_squash_merged(&ground, default_branch) {
        MergedState::Squash
    } else {
        MergedState::NotMerged
    };

    let branch = crate::lane::commands::get_branch(&ground);
    LaneFacts {
        name: lane_name.to_string(),
        is_origin,
        has_ground: true,
        dirty_count: crate::lane::commands::count_changes(&ground),
        own_commits: count_own_commits(&ground, default_branch),
        has_remote_branch: branch
            .as_deref()
            .is_some_and(|b| has_remote_branch(&ground, b)),
        branch,
        merged,
        is_running,
    }
}

/// project の全 lane を見て、見送り判定を並べる（Host の入口）。
///
/// `running` は「今動いている lane 名」— 呼び出し側（LanePool を持つ層）が渡す。
/// Host は lane の生死を git からは知れないため、ここだけ外から供給される。
///
/// `origin` は帳簿が解決した**開発起点 lane 名**（[`crate::host::ledger::resolve_origin_name`]）。
/// D4 で起点が予約名から project の指定になったため、外から供給される事実になった。
/// **worktree を持つ lane も起点になりうる**ので、この供給を怠ると起点を見送りうる
/// （P3 初版は予約名を直接見ており、起点が動かせない前提に依存していた）。
///
/// **実行はしない。** 判定を並べて返すだけで、実際に消すかは呼び出し側（人間の承認 or
/// 明示コマンド）が決める。D3 の「決定的判定 → 帳簿 → エスカレーション」のうち、
/// 本関数は判定の集約にあたる。
pub fn survey_project(
    repo_root: &std::path::Path,
    running: &[String],
    origin: &str,
) -> Vec<FarewellReport> {
    let default_branch = crate::lane::commands::resolve_default_branch(repo_root)
        .unwrap_or_else(|| "main".to_string());

    crate::lane::commands::list_performers_for_repo(repo_root)
        .into_iter()
        .map(|entry| {
            let facts = collect_facts(
                repo_root,
                &entry.name,
                entry.name == origin,
                running.iter().any(|r| r == &entry.name),
                &default_branch,
            );
            let verdict = judge_farewell(&facts);
            FarewellReport { facts, verdict }
        })
        .collect()
}

/// 1 lane 分の見送り判定（事実 + 判定）。帳簿・API に載る単位。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FarewellReport {
    pub facts: LaneFacts,
    #[serde(flatten)]
    pub verdict: FarewellVerdict,
}

/// 既定 branch から見た独自 commit 数（`git rev-list --count origin/<default>..HEAD`）。
///
/// 「この lane で積んだ仕事の量」。0 なら空 lane（見送っても失うものが無い）。
/// 取得失敗は 0 ではなく **1 を返す** — 数えられないものを「仕事が無い」と見なすと
/// 見送り側に倒れてしまうため、安全側（保持）に寄せる。
fn count_own_commits(ground: &std::path::Path, default_branch: &str) -> u32 {
    let range = format!("origin/{default_branch}..HEAD");
    let out = std::process::Command::new("git")
        .args(["rev-list", "--count", &range])
        .current_dir(ground)
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse::<u32>()
            .unwrap_or(1),
        _ => 1,
    }
}

/// remote に同名 branch が在るか（`origin/<branch>` ref の存在で見る）。
///
/// `git ls-remote` は network を叩くので使わない — `collect_facts` 冒頭の
/// `fetch --prune` で remote-tracking ref が最新化済み（**削除も反映済み**）という前提に乗る。
/// prune 無しだと消えた branch の ref が残り、この関数が stale な true を返す。
/// fetch 自体が失敗した場合（オフライン等）は最後に成功した fetch 時点の事実になる。
fn has_remote_branch(ground: &std::path::Path, branch: &str) -> bool {
    let refname = format!("refs/remotes/origin/{branch}");
    std::process::Command::new("git")
        .args(["show-ref", "--verify", "--quiet", &refname])
        .current_dir(ground)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 判定の基準となる「見送ってよい」状態（merged / clean / 停止中）。
    /// 各テストはここから 1 つだけ崩して分岐を確かめる。
    fn reclaimable() -> LaneFacts {
        LaneFacts {
            name: "feat-x".to_string(),
            is_origin: false,
            has_ground: true,
            dirty_count: 0,
            own_commits: 3,
            branch: Some("feat-x".to_string()),
            merged: MergedState::Ancestry,
            has_remote_branch: true,
            is_running: false,
        }
    }

    #[test]
    fn merged_and_clean_is_reclaimed() {
        assert!(matches!(
            judge_farewell(&reclaimable()),
            FarewellVerdict::Reclaim { .. }
        ));
        // squash merge も同じ枝（検出手段が違うだけで意味は同じ）
        let squashed = LaneFacts {
            merged: MergedState::Squash,
            ..reclaimable()
        };
        assert!(matches!(
            judge_farewell(&squashed),
            FarewellVerdict::Reclaim { .. }
        ));
    }

    /// 回帰固定: **dirty は merged より先に見る**。
    ///
    /// 旧 `classify_performer_for_cleanup` は merged なら `count_changes` を見ずに削除候補へ
    /// 入れており、取り込み済み branch 上に残った未コミット作業を黙って捨てうる形だった。
    /// D3「Host は推測しない」に反するので、ここは人間へ回す。
    #[test]
    fn dirty_beats_merged_and_asks_human() {
        let dirty = LaneFacts {
            dirty_count: 4,
            ..reclaimable()
        };
        let verdict = judge_farewell(&dirty);
        assert!(
            matches!(verdict, FarewellVerdict::AskHuman { .. }),
            "merged でも未コミット変更があれば人間へ: {verdict:?}"
        );
        assert!(
            verdict.reason().contains('4'),
            "理由に件数が出る: {}",
            verdict.reason()
        );
    }

    /// 理由文は **断定できることだけ**を言う（facts-over-narrative）。
    ///
    /// `MergedState::NotMerged` かつ独自 commit 0 は 2 状態を畳んでいる:
    /// ①作ったばかり（fresh）②fast-forward merge で既定 branch が追いついた。
    /// どちらも `HEAD == origin/<default>` になり git のこの情報では区別できないので、
    /// 「未 merge」と断定してはいけない。
    ///
    /// 回帰の出所: 実機検証で **merge 済みの lane に「取り込み状態: 未 merge」と
    /// 表示された**（fast-forward merge されたケース）。判定自体は正しかったが理由が嘘だった。
    #[test]
    fn reason_does_not_claim_unmerged_when_indistinguishable() {
        let ff_or_fresh = LaneFacts {
            merged: MergedState::NotMerged,
            own_commits: 0,
            dirty_count: 2,
            ..reclaimable()
        };
        let reason = judge_farewell(&ff_or_fresh).reason().to_string();
        assert!(
            !reason.contains("未 merge"),
            "区別できない状態を「未 merge」と断定しない: {reason}"
        );
        assert!(
            reason.contains("同位置"),
            "同位置であることは断定できるので言う: {reason}"
        );

        // 独自 commit がある = 確実に未 merge。こちらは断定してよい
        let really_unmerged = LaneFacts {
            merged: MergedState::NotMerged,
            own_commits: 3,
            dirty_count: 1,
            ..reclaimable()
        };
        assert!(
            judge_farewell(&really_unmerged)
                .reason()
                .contains("未 merge"),
            "独自 commit があれば未 merge と断定できる"
        );
    }

    /// 開発起点は見送り条件を全て満たしていても消さない。
    ///
    /// D4 で起点は予約名から**帳簿のポインタ**になったので、この分岐が守るのは
    /// 「`conductor` という名前の lane」ではなく「今起点に指定されている lane」。
    #[test]
    fn origin_lane_is_never_reclaimed() {
        let conductor = LaneFacts {
            is_origin: true,
            // 見送り条件を全て満たしていても消さない
            merged: MergedState::Ancestry,
            dirty_count: 0,
            ..reclaimable()
        };
        assert!(matches!(
            judge_farewell(&conductor),
            FarewellVerdict::Keep { .. }
        ));
    }

    #[test]
    fn running_lane_is_kept() {
        let running = LaneFacts {
            is_running: true,
            ..reclaimable()
        };
        assert!(matches!(
            judge_farewell(&running),
            FarewellVerdict::Keep { .. }
        ));
    }

    #[test]
    fn empty_lane_without_remote_is_reclaimed() {
        let empty = LaneFacts {
            own_commits: 0,
            merged: MergedState::NotMerged,
            has_remote_branch: false,
            ..reclaimable()
        };
        assert!(matches!(
            judge_farewell(&empty),
            FarewellVerdict::Reclaim { .. }
        ));
    }

    /// 独自 commit 0 でも remote branch があれば人間へ（こちらが知らない履歴がありうる）。
    #[test]
    fn empty_lane_with_remote_asks_human() {
        let empty_but_pushed = LaneFacts {
            own_commits: 0,
            merged: MergedState::NotMerged,
            has_remote_branch: true,
            ..reclaimable()
        };
        assert!(matches!(
            judge_farewell(&empty_but_pushed),
            FarewellVerdict::AskHuman { .. }
        ));
    }

    #[test]
    fn unmerged_work_is_kept() {
        let working = LaneFacts {
            merged: MergedState::NotMerged,
            own_commits: 5,
            ..reclaimable()
        };
        let verdict = judge_farewell(&working);
        assert!(matches!(verdict, FarewellVerdict::Keep { .. }));
        assert!(verdict.reason().contains('5'), "理由に commit 数が出る");
    }

    #[test]
    fn missing_ground_is_kept_not_reclaimed() {
        // ground が無い = 掃除するものが無い。Reclaim にすると「消す対象が無いのに消す」
        // という無意味な action を生むので Keep に倒す。
        let no_ground = LaneFacts {
            has_ground: false,
            ..reclaimable()
        };
        assert!(matches!(
            judge_farewell(&no_ground),
            FarewellVerdict::Keep { .. }
        ));
    }

    // =========================================================================
    // actions 側 — 実 git repo を作って「事実が正しく取れるか」だけを見る
    // （判定の正しさは上の純関数テストが担当。ここで両方を混ぜない）
    // =========================================================================

    /// ground 不在の lane は git を引かずに「無い」という事実を返す。
    ///
    /// ここで Keep を返さないのが設計の要（それは判定 = calculations の仕事）。
    /// actions が判定を先取りすると、旧実装と同じ「収集と判定の混在」に戻る。
    #[test]
    fn collect_facts_reports_missing_ground_without_judging() {
        let tmp = std::env::temp_dir().join(format!("vp-farewell-noground-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);

        let facts = collect_facts(&tmp, "nonexistent", false, false, "main");

        assert!(!facts.has_ground, "ground 不在が事実として出る");
        assert_eq!(facts.name, "nonexistent");
        assert_eq!(facts.dirty_count, 0);
        assert_eq!(facts.own_commits, 0);
        assert_eq!(facts.merged, MergedState::NotMerged);
        // 判定は calculations が別途行う（ここでは Keep と決めない）
        assert!(matches!(
            judge_farewell(&facts),
            FarewellVerdict::Keep { .. }
        ));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 実 git worktree で dirty_count が拾えること（未コミット変更 → AskHuman の入力）。
    #[test]
    fn collect_facts_sees_dirty_worktree() {
        let root =
            std::env::temp_dir().join(format!("vp-farewell-dirty-{}-{}", std::process::id(), 1));
        let _ = std::fs::remove_dir_all(&root);
        let lane_dir = crate::lane::config::project_lanes_dir(&root).join("w1");
        std::fs::create_dir_all(&lane_dir).unwrap();

        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&lane_dir)
                .output()
                .expect("git 実行");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(lane_dir.join("a.txt"), "one").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        // 未コミットの変更を作る
        std::fs::write(lane_dir.join("a.txt"), "two").unwrap();

        let facts = collect_facts(&root, "w1", false, false, "main");

        assert!(facts.has_ground, "ground が見つかる");
        assert_eq!(facts.dirty_count, 1, "未コミット変更 1 件を拾う");
        assert!(!facts.has_remote_branch, "remote は無い");
        assert!(
            facts.branch.is_some(),
            "branch 名は facts に載る（見送り実行時に引き直せないため）"
        );
        // dirty は merged より優先されるので人間へ回る
        assert!(matches!(
            judge_farewell(&facts),
            FarewellVerdict::AskHuman { .. }
        ));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 回帰固定: **起点が worktree lane に移っていたら見送らない**（doc 44 D4）。
    ///
    /// P3 初版の `survey_project` は予約名 `conductor` を直接見ていた。起点が動かせない
    /// 前提に依存しており、D4 でポインタが入ると **移動済みの起点を掃除対象に入れうる**。
    ///
    /// ここで固定するのは「`origin` 引数が判定まで届くこと」— 純関数側のテスト
    /// （[`origin_lane_is_never_reclaimed`]）だけだと、`survey_project` が実際にその事実を
    /// 供給しているかは分からない（P3 の `branch -d` never-fire と同じ穴の作り方）。
    /// 同じ lane に対し `origin` だけを変えて、**判定理由が変わること**まで見る。
    #[test]
    fn survey_marks_pointed_lane_as_origin() {
        let root = std::env::temp_dir().join(format!("vp-farewell-origin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let lane_dir = crate::lane::config::project_lanes_dir(&root).join("w1");
        std::fs::create_dir_all(&lane_dir).unwrap();

        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&lane_dir)
                .output()
                .expect("git 実行");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(lane_dir.join("a.txt"), "one").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);

        let find = |reports: Vec<FarewellReport>| {
            reports
                .into_iter()
                .find(|r| r.facts.name == "w1")
                .expect("w1 が一覧に出る")
        };

        // 起点が別 lane（予約名）なら、w1 はただの performer として判定される
        let as_normal = find(survey_project(
            &root,
            &[],
            crate::process::lanes_state::CONDUCTOR_LANE_NAME,
        ));
        assert!(!as_normal.facts.is_origin, "起点ではない");
        assert!(
            !as_normal.verdict.reason().contains("開発起点"),
            "起点以外の理由で判定される: {}",
            as_normal.verdict.reason()
        );

        // 帳簿が w1 を指していれば、同じ lane が起点として保持される
        let as_origin = find(survey_project(&root, &[], "w1"));
        assert!(as_origin.facts.is_origin, "帳簿のポインタが事実として届く");
        assert!(
            matches!(as_origin.verdict, FarewellVerdict::Keep { .. }),
            "起点は保持: {:?}",
            as_origin.verdict
        );
        assert!(
            as_origin.verdict.reason().contains("開発起点"),
            "起点であることが判定理由になる: {}",
            as_origin.verdict.reason()
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 回帰固定: **worktree を消した後に branch 名は引けない**。
    ///
    /// だから `LaneFacts.branch` を削除前に捕捉して持ち回る。P3 初版はこれを持たず、
    /// 削除ループ内で `get_branch(&path)` を呼び直していたため、`git` の cwd が既に
    /// 存在せず `output()` が Err → 常に None → **`git branch -d` が一度も実行されない**
    /// （merge 済 branch が共有 `.git` に溜まり続ける）状態だった。
    ///
    /// ここでは「消えた dir に対する `get_branch` は None」という前提そのものを固定する。
    /// これが崩れる（= 引けるようになる）ことは無いが、崩れたら設計の前提が変わる。
    #[test]
    fn branch_cannot_be_read_after_ground_is_removed() {
        let root = std::env::temp_dir().join(format!("vp-farewell-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let lane_dir = crate::lane::config::project_lanes_dir(&root).join("w1");
        std::fs::create_dir_all(&lane_dir).unwrap();

        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&lane_dir)
                .output()
                .expect("git 実行");
        };
        git(&["init", "-q", "-b", "feat-gone"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(lane_dir.join("a.txt"), "one").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);

        // 削除**前**なら引ける = facts に載せられる
        let facts = collect_facts(&root, "w1", false, false, "main");
        assert_eq!(
            facts.branch.as_deref(),
            Some("feat-gone"),
            "削除前は branch を引ける"
        );

        // ground を消すと引けなくなる（`git` の cwd が無い）
        std::fs::remove_dir_all(&lane_dir).unwrap();
        assert!(
            crate::lane::commands::get_branch(&lane_dir).is_none(),
            "消えた dir からは branch を引けない — だから facts に持つ"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// verdict は wire に載る（帳簿 / API）ので serde 形を固定する。
    #[test]
    fn verdict_serde_shape() {
        let v = FarewellVerdict::Reclaim {
            reason: "merge済みで作業残なし".to_string(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["verdict"], "reclaim");
        assert_eq!(json["reason"], "merge済みで作業残なし");
        assert_eq!(v.label(), "reclaim");
    }
}
