//! Repo Host 帳簿の永続層 — 開発起点 / lane の並び / 見送りの記録。
//!
//! 3 つとも **key が `lane_id`**（address ではない）。rename 耐性のためで、lane の名前が
//! 変わっても帳簿は追従する。`db/lane.rs` の 3 table とは key の種類が違うので分けている。
//!
//! | table | 帳簿 | 何を持つか | 出典 |
//! |---|---|---|---|
//! | `host_origin` | ① | repo ごとの開発起点ポインタ | doc 44 D4 |
//! | `host_lane_order` | ② | sidebar の lane 並び順 | doc 44 |
//! | `host_farewell` | ③ | 見送りの記録（いつ何を見送ったか / AskHuman の滞留） | doc 44 §7.5 / §8.5 |
//!
//! ⚠️ 帳簿① の `host_origin` は `active_lane`（注視）と形が同じ 1-repo-1-row だが**意味が違う**。
//! doc 44 D5 が「注視の切替」と「起点の再指定」を分けた結果で、片方をもう片方で代用しない。
//!
//! ③ は「survey では復元できないもの」だけを持つ。lane を消した後に「いつ何を見送ったか」を
//! 答えられるのはここだけ。
//!
//! logic 側の owner は `host/ledger.rs`（`FarewellEntry` / `FarewellKind` もそこ）。
//! 設計は [doc 62](../../../../docs/design/62-db-module-layout.md)。

use anyhow::Result;

use super::VpDb;

impl VpDb {
    // =========================================================================
    // Repo Host 帳簿①: 開発起点ポインタ (doc 44 D4)。
    //
    // active_lane (注視) と形は同じ 1-repo-1-row だが意味が違う (D5 が分けた
    // 「注視の切替」と「起点の再指定」)。値が **lane_id** なのは rename 耐性のため。
    // =========================================================================

    /// 開発起点ポインタを upsert する (repo_path → lane_id)。
    pub async fn upsert_host_origin(&self, repo_path: &str, lane_id: &str) -> Result<()> {
        self.db
            .query(
                "INSERT INTO host_origin {
                    repo_path: $repo_path,
                    lane_id: $lane_id,
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    lane_id = $input.lane_id,
                    updated_at = time::now()",
            )
            .bind(("repo_path", repo_path.to_string()))
            .bind(("lane_id", lane_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_origin upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("host_origin upsert エラー: {}", e))?;
        Ok(())
    }

    /// 開発起点ポインタを引く。
    ///
    /// `None` は「未指定」= 予約名フォールバック（[`crate::host::ledger::resolve_origin_name`]）。
    /// 指す lane が既に消えている場合も呼び出し側の解決で予約名に落ちるので、ここでは
    /// 実在検証をしない（DB は lane の生死を知らない）。
    pub async fn get_host_origin(&self, repo_path: &str) -> Result<Option<String>> {
        let mut result = self
            .db
            .query("SELECT lane_id FROM host_origin WHERE repo_path = $path")
            .bind(("path", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_origin 取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        Ok(rows
            .first()
            .and_then(|v| v.get("lane_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()))
    }

    /// lane の並び順を repo 単位で**全置換**する（doc 44 D5）。
    ///
    /// 並び順は集合なので replace 型（`replace_lanes` と同じ考え方）。差分 upsert にすると
    /// 「並びから外れた lane の古い ord」が残り、次に現れた時に意図しない位置に挿さる。
    pub async fn replace_lane_order(&self, repo_path: &str, order: &[String]) -> Result<()> {
        self.db
            .query("DELETE host_lane_order WHERE repo_path = $p")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("lane 並び順の削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane 並び順の削除エラー: {}", e))?;
        for (i, lane_id) in order.iter().enumerate() {
            self.db
                .query(
                    "CREATE host_lane_order SET
                        repo_path = $p, lane_id = $id, ord = $ord, updated_at = time::now()",
                )
                .bind(("p", repo_path.to_string()))
                .bind(("id", lane_id.clone()))
                .bind(("ord", i as i64))
                .await
                .map_err(|e| anyhow::anyhow!("lane 並び順の永続失敗: {}", e))?
                .check()
                .map_err(|e| anyhow::anyhow!("lane 並び順の永続エラー: {}", e))?;
        }
        Ok(())
    }

    /// lane の並び順を引く（`lane_id` → `ord`）。未指定 repo は空。
    pub async fn list_lane_order(
        &self,
        repo_path: &str,
    ) -> Result<std::collections::HashMap<String, i64>> {
        let mut result = self
            .db
            .query("SELECT lane_id, ord FROM host_lane_order WHERE repo_path = $p")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("lane 並び順の取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        Ok(rows
            .into_iter()
            .filter_map(|v| {
                let id = v.get("lane_id")?.as_str()?.to_string();
                let ord = v.get("ord")?.as_i64()?;
                Some((id, ord))
            })
            .collect())
    }

    /// lane の並び順を repo ごと回収する（`delete_host_origin` と対、§4.6 含有=所有=寿命）。
    pub async fn delete_lane_order_for_repo(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE host_lane_order WHERE repo_path = $p")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("lane 並び順の全削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane 並び順の全削除エラー: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Repo Host 帳簿③: 見送りの記録 (doc 44 §7.5 / §8.5)。
    //
    // 「いつ何を見送ったか」(= lane を消したので survey では復元できない) と
    // 「AskHuman がいつから何回続いているか」(= 観測の履歴なので計算できない) の 2 つ。
    // key は host_origin / host_lane_order と同じ **lane_id**。
    // =========================================================================

    /// DB の 1 行を [`crate::host::ledger::FarewellEntry`] に写す (壊れた行は `None`)。
    ///
    /// 1 行の欠損で一覧全体を落とさない (帳簿は best-effort read)。
    fn farewell_row(v: &serde_json::Value) -> Option<crate::host::ledger::FarewellEntry> {
        Some(crate::host::ledger::FarewellEntry {
            lane_id: v.get("lane_id")?.as_str()?.to_string(),
            lane_name: v
                .get("lane_name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string(),
            kind: crate::host::ledger::FarewellKind::from_label(v.get("kind")?.as_str()?)?,
            reason: v
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or_default()
                .to_string(),
            streak: v.get("streak").and_then(|s| s.as_u64()).unwrap_or(1) as u32,
            first_seen_at: v
                .get("first_seen_at")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string(),
            last_seen_at: v
                .get("last_seen_at")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string(),
            ongoing: v.get("ongoing").and_then(|o| o.as_bool()).unwrap_or(false),
        })
    }

    /// 継続中の滞留を引く (repo × lane に高々 1 行)。
    pub async fn get_open_farewell(
        &self,
        repo_path: &str,
        lane_id: &str,
    ) -> Result<Option<crate::host::ledger::FarewellEntry>> {
        let mut result = self
            .db
            .query(
                "SELECT * FROM host_farewell
                 WHERE repo_path = $p AND lane_id = $id AND ongoing = true LIMIT 1",
            )
            .bind(("p", repo_path.to_string()))
            .bind(("id", lane_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_farewell 取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        Ok(rows.first().and_then(Self::farewell_row))
    }

    /// 帳簿に 1 行足す (滞留の起票 / 見送りの記録)。
    pub async fn create_farewell_entry(
        &self,
        repo_path: &str,
        entry: &crate::host::ledger::FarewellEntry,
    ) -> Result<()> {
        self.db
            .query(
                "CREATE host_farewell SET
                    repo_path = $p, lane_id = $id, lane_name = $name, kind = $kind,
                    reason = $reason, streak = $streak,
                    first_seen_at = $first, last_seen_at = $last, ongoing = $ongoing",
            )
            .bind(("p", repo_path.to_string()))
            .bind(("id", entry.lane_id.clone()))
            .bind(("name", entry.lane_name.clone()))
            .bind(("kind", entry.kind.as_str().to_string()))
            .bind(("reason", entry.reason.clone()))
            .bind(("streak", entry.streak as i64))
            .bind(("first", entry.first_seen_at.clone()))
            .bind(("last", entry.last_seen_at.clone()))
            .bind(("ongoing", entry.ongoing))
            .await
            .map_err(|e| anyhow::anyhow!("host_farewell 追加失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("host_farewell 追加エラー: {}", e))?;
        Ok(())
    }

    /// 継続中の滞留を伸ばす (連続回数と直近観測時刻の更新)。
    ///
    /// `lane_name` は**更新しない** — 記録時点のスナップショットなので rename で動かさない
    /// (doc 44 §8.5)。`first_seen_at` も同じ理由で不変。
    pub async fn extend_open_farewell(
        &self,
        repo_path: &str,
        lane_id: &str,
        streak: u32,
        reason: &str,
        last_seen_at: &str,
    ) -> Result<()> {
        self.db
            .query(
                "UPDATE host_farewell SET streak = $streak, reason = $reason, last_seen_at = $last
                 WHERE repo_path = $p AND lane_id = $id AND ongoing = true",
            )
            .bind(("p", repo_path.to_string()))
            .bind(("id", lane_id.to_string()))
            .bind(("streak", streak as i64))
            .bind(("reason", reason.to_string()))
            .bind(("last", last_seen_at.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_farewell 更新失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("host_farewell 更新エラー: {}", e))?;
        Ok(())
    }

    /// 継続中の滞留を閉じる (判定が判断待ちから外れた / lane を見送った)。
    ///
    /// 行は消さない — 「いつからいつまで判断待ちだったか」は履歴として残す。
    pub async fn close_open_farewell(&self, repo_path: &str, lane_id: &str) -> Result<()> {
        self.db
            .query(
                "UPDATE host_farewell SET ongoing = false
                 WHERE repo_path = $p AND lane_id = $id AND ongoing = true",
            )
            .bind(("p", repo_path.to_string()))
            .bind(("id", lane_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_farewell 終端失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("host_farewell 終端エラー: {}", e))?;
        Ok(())
    }

    /// 継続中の滞留を repo 単位で列挙する (`vp lane cleanup` の滞留表示)。
    pub async fn list_open_farewells(
        &self,
        repo_path: &str,
    ) -> Result<Vec<crate::host::ledger::FarewellEntry>> {
        let mut result = self
            .db
            .query("SELECT * FROM host_farewell WHERE repo_path = $p AND ongoing = true")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_farewell 滞留取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        Ok(rows.iter().filter_map(Self::farewell_row).collect())
    }

    /// 帳簿を新しい順に読む (`vp lane history`)。`limit` 0 は無制限。
    pub async fn list_farewell_entries(
        &self,
        repo_path: &str,
        limit: usize,
    ) -> Result<Vec<crate::host::ledger::FarewellEntry>> {
        let mut result = self
            .db
            .query("SELECT * FROM host_farewell WHERE repo_path = $p ORDER BY last_seen_at DESC")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_farewell 履歴取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        let mut entries: Vec<crate::host::ledger::FarewellEntry> =
            rows.iter().filter_map(Self::farewell_row).collect();
        if limit > 0 {
            entries.truncate(limit);
        }
        Ok(entries)
    }

    /// 見送りの記録を repo ごと回収する (`delete_host_origin` と対、§4.6 含有=所有=寿命)。
    pub async fn delete_farewell_entries_for_repo(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE host_farewell WHERE repo_path = $p")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_farewell 全削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("host_farewell 全削除エラー: {}", e))?;
        Ok(())
    }

    /// 開発起点ポインタを削除する (repo remove 時の回収、`delete_active_lane` と対)。
    pub async fn delete_host_origin(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM host_origin WHERE repo_path = $path")
            .bind(("path", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("host_origin 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("host_origin 削除エラー: {}", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::make_test_db;

    /// doc 44 D4: 開発起点ポインタの round-trip（upsert → get → 上書き → 削除）。
    ///
    /// **削除まで見る**のは、`remove_repo` が repo namespace を倒す時にこの行を
    /// 回収する契約だから（§4.6 含有=所有=寿命）。残ると同 path で再登録した時に旧 lane の
    /// UUID を指す孤児ポインタが復活し、起点が `Dangling` に落ちる。
    #[tokio::test]
    async fn test_host_origin_round_trip() {
        let db = make_test_db().await;

        // 未設定は None = 予約名フォールバック（`ledger::resolve_origin_name` が受ける形）
        assert!(db.get_host_origin("/repos/vp").await.unwrap().is_none());

        db.upsert_host_origin("/repos/vp", "id-alpha")
            .await
            .unwrap();
        assert_eq!(
            db.get_host_origin("/repos/vp").await.unwrap().as_deref(),
            Some("id-alpha")
        );

        // repo ごとに独立（1 repo 1 ポインタ）
        db.upsert_host_origin("/repos/nexus", "id-beta")
            .await
            .unwrap();
        assert_eq!(
            db.get_host_origin("/repos/vp").await.unwrap().as_deref(),
            Some("id-alpha"),
            "他 repo の指定に引きずられない"
        );

        // 起点の移動は行の上書き（増えない）
        db.upsert_host_origin("/repos/vp", "id-gamma")
            .await
            .unwrap();
        assert_eq!(
            db.get_host_origin("/repos/vp").await.unwrap().as_deref(),
            Some("id-gamma"),
            "UNIQUE index により上書きされる"
        );

        // repo 回収でポインタも消える
        db.delete_host_origin("/repos/vp").await.unwrap();
        assert!(db.get_host_origin("/repos/vp").await.unwrap().is_none());
        assert_eq!(
            db.get_host_origin("/repos/nexus").await.unwrap().as_deref(),
            Some("id-beta"),
            "削除は repo scope に閉じる"
        );
    }
}
