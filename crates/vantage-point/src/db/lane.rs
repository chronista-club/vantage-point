//! lane の永続層 — descriptor / lifecycle / presence の 3 table。
//!
//! この 3 つは **key が lane の address**（文字列）で揃っているので同居させる。
//! `db/schema.rs` の起動時 migration が旧形 address を正規化するのも、ちょうどこの 3 table。
//!
//! | table | 何を持つか | 出典 |
//! |---|---|---|
//! | `lane` | descriptor。repo push の cache ではなく **daemon-canonical な durable truth** | doc 24 §10 Phase 2 |
//! | `lane_lifecycle` | `provisioning` / `ready` / `dead` の **軽量 WAL**。descriptor と別 table なので repo push に clobber されない | doc 24 §4.6 |
//! | `active_lane` | repo ごとの注視（presence、Model Q） | doc 44 |
//!
//! ## 触ってはいけないもの
//!
//! - [`VpDb::upsert_lane`] / [`VpDb::upsert_lane_lifecycle`] は **DELETE + CREATE**（UPDATE ではない）。
//!   旧形 address の行が残っていると WHERE が当たらず重複するので、migration と対で効く
//! - 列名が table ごとに違う（`lane.address` / `lane_lifecycle.address` / `active_lane.lane_address`）。
//!   address を持つ列を数えるときは table 名だけの列挙で足りない（doc 44 P2 で実際に漏れた）
//!
//! 値側の owner は `repo/lane/`（address / info）、runtime は `repo/lane/pool.rs` 他。
//! 設計は [doc 62](../../../../docs/design/62-db-module-layout.md)。

use anyhow::Result;

use super::VpDb;

impl VpDb {
    // =========================================================================
    // Active lane (presence、 Model Q): repo ごとの選択中 lane を daemon-canonical に
    // =========================================================================

    /// active lane を upsert (repo_path → lane_address)。
    pub async fn upsert_active_lane(&self, repo_path: &str, lane_address: &str) -> Result<()> {
        self.db
            .query(
                "INSERT INTO active_lane {
                    repo_path: $repo_path,
                    lane_address: $lane_address,
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    lane_address = $input.lane_address,
                    updated_at = time::now()",
            )
            .bind(("repo_path", repo_path.to_string()))
            .bind(("lane_address", lane_address.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("active_lane upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("active_lane upsert エラー: {}", e))?;
        Ok(())
    }

    /// 全 active lane を (repo_path, lane_address) で返す (boot 時の load 用)。
    pub async fn list_active_lanes(&self) -> Result<Vec<(String, String)>> {
        // list_processes と同じく serde_json::Value で受ける (surrealdb の SurrealValue 制約回避)。
        let mut result = self
            .db
            .query("SELECT repo_path, lane_address FROM active_lane")
            .await
            .map_err(|e| anyhow::anyhow!("active_lane 取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        Ok(rows
            .into_iter()
            .filter_map(|v| {
                let path = v.get("repo_path")?.as_str()?.to_string();
                let addr = v.get("lane_address")?.as_str()?.to_string();
                Some((path, addr))
            })
            .collect())
    }

    /// active lane を削除する (repo remove 時の presence 回収、 §4.6 含有=所有=寿命)。
    pub async fn delete_active_lane(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM active_lane WHERE repo_path = $path")
            .bind(("path", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("active_lane 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("active_lane 削除エラー: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Lane descriptor (doc 24 §10 Phase 2: LanePool authority 反転 repo→daemon)。
    // repo push の cache だった lane_registry を daemon-canonical な durable truth に。
    // repo disconnect では drop せず、 daemon 再起動は db から re-animate する (§3.3 / §4.1)。
    // =========================================================================

    /// 1 lane descriptor を upsert する (repo の Diff::Add / Diff::Update 反映)。
    ///
    /// (repo_path, address) 複合 key で一意。 ON DUPLICATE の composite 挙動に依存せず、
    /// DELETE→CREATE を 1 query (= 中間状態を他読みに晒さない) で行う。 info は LaneInfo を
    /// 丸ごと JSON object 化して持つ (descriptor truth)。
    pub async fn upsert_lane(
        &self,
        repo_path: &str,
        lane: &crate::repo::lane::LaneInfo,
    ) -> Result<()> {
        let address = lane.address.to_string();
        let descriptor = serde_json::to_value(lane)
            .map_err(|e| anyhow::anyhow!("lane descriptor serialize 失敗: {}", e))?;
        self.db
            .query(
                "DELETE lane WHERE repo_path = $p AND address = $a;
                 CREATE lane CONTENT {
                    repo_path: $p,
                    address: $a,
                    descriptor: $descriptor,
                    updated_at: time::now()
                 }",
            )
            .bind(("p", repo_path.to_string()))
            .bind(("a", address))
            .bind(("descriptor", descriptor))
            .await
            .map_err(|e| anyhow::anyhow!("lane upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane upsert エラー: {}", e))?;
        Ok(())
    }

    /// 1 lane descriptor を削除する (repo の Diff::Remove 反映 / 単一 lane の destroy)。
    pub async fn delete_lane(&self, repo_path: &str, address: &str) -> Result<()> {
        self.db
            .query("DELETE lane WHERE repo_path = $p AND address = $a")
            .bind(("p", repo_path.to_string()))
            .bind(("a", address.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("lane 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane 削除エラー: {}", e))?;
        Ok(())
    }

    /// 1 repo の lane descriptor を全削除する (repo remove 時の回収、 §4.6 含有=所有=寿命)。
    pub async fn delete_lanes_for_repo(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE lane WHERE repo_path = $p")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("repo lane 全削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("repo lane 全削除エラー: {}", e))?;
        Ok(())
    }

    /// 1 repo の lane descriptor を snapshot で全置換する (repo register snapshot 反映)。
    ///
    /// snapshot は「その時点の repo の全 lane」なので、 既存を消してから入れ直す repo 単位
    /// replace 型 (active_lane の高頻度 1 行 upsert と違い、 lane は集合なので全置換が自然)。
    pub async fn replace_lanes_for_repo(
        &self,
        repo_path: &str,
        lanes: &[crate::repo::lane::LaneInfo],
    ) -> Result<()> {
        self.delete_lanes_for_repo(repo_path).await?;
        for lane in lanes {
            self.upsert_lane(repo_path, lane).await?;
        }
        Ok(())
    }

    /// 全 lane descriptor を (repo_path, LaneInfo) で返す (boot 時の load 用)。
    ///
    /// list_processes と同じく serde_json::Value で受け、 info object を LaneInfo に
    /// deserialize する。 壊れた行は warn して skip (boot を止めない、 §4.6 ゆるやか統治)。
    pub async fn list_lanes(&self) -> Result<Vec<(String, crate::repo::lane::LaneInfo)>> {
        let mut result = self
            .db
            .query("SELECT repo_path, descriptor FROM lane")
            .await
            .map_err(|e| anyhow::anyhow!("lane 取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        let mut out = Vec::with_capacity(rows.len());
        for v in rows {
            let Some(path) = v.get("repo_path").and_then(|x| x.as_str()) else {
                continue;
            };
            let Some(desc_val) = v.get("descriptor") else {
                continue;
            };
            match serde_json::from_value::<crate::repo::lane::LaneInfo>(desc_val.clone()) {
                Ok(info) => out.push((path.to_string(), info)),
                Err(e) => tracing::warn!("lane descriptor deserialize 失敗 (skip): {}", e),
            }
        }
        Ok(out)
    }

    // =========================================================================
    // Lane lifecycle (doc 24 §4.6: durable lifecycle state machine、 軽量 WAL)。
    // descriptor (lane table) とは別 table — repo push に clobber されない daemon-internal。
    // =========================================================================

    /// lane の lifecycle を upsert する (provisioning / ready / dead)。
    ///
    /// team-b #2: active_lane が `INSERT ON DUPLICATE KEY UPDATE` (単一 key) なのに対し、 lane 系は
    /// **複合 key (repo_path, address)** で ON DUPLICATE の発火が不確実なため DELETE+CREATE を使う
    /// (upsert_lane / lane table と同方針)。 2 statement は単一 `query()` = 1 transaction で
    /// atomic に走る (DELETE 後 CREATE 前に row が消える窓は無い)。
    pub async fn upsert_lane_lifecycle(
        &self,
        repo_path: &str,
        address: &str,
        lifecycle: &str,
    ) -> Result<()> {
        self.db
            .query(
                "DELETE lane_lifecycle WHERE repo_path = $p AND address = $a;
                 CREATE lane_lifecycle CONTENT {
                    repo_path: $p,
                    address: $a,
                    lifecycle: $lc,
                    updated_at: time::now()
                 }",
            )
            .bind(("p", repo_path.to_string()))
            .bind(("a", address.to_string()))
            .bind(("lc", lifecycle.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("lane_lifecycle upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane_lifecycle upsert エラー: {}", e))?;
        Ok(())
    }

    /// 全 lane lifecycle を (repo_path, address, lifecycle) で返す (boot reconcile 用)。
    pub async fn list_lane_lifecycles(&self) -> Result<Vec<(String, String, String)>> {
        let mut result = self
            .db
            .query("SELECT repo_path, address, lifecycle FROM lane_lifecycle")
            .await
            .map_err(|e| anyhow::anyhow!("lane_lifecycle 取得失敗: {}", e))?;
        let rows: Vec<serde_json::Value> = result.take(0)?;
        Ok(rows
            .into_iter()
            .filter_map(|v| {
                let p = v.get("repo_path")?.as_str()?.to_string();
                let a = v.get("address")?.as_str()?.to_string();
                let lc = v.get("lifecycle")?.as_str()?.to_string();
                Some((p, a, lc))
            })
            .collect())
    }

    /// 1 lane の lifecycle を削除 (lane destroy / lifecycle 回収)。
    pub async fn delete_lane_lifecycle(&self, repo_path: &str, address: &str) -> Result<()> {
        self.db
            .query("DELETE lane_lifecycle WHERE repo_path = $p AND address = $a")
            .bind(("p", repo_path.to_string()))
            .bind(("a", address.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("lane_lifecycle 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("lane_lifecycle 削除エラー: {}", e))?;
        Ok(())
    }

    /// 1 repo の lane lifecycle を全削除 (repo remove 時の回収、 §4.6 含有=所有=寿命)。
    pub async fn delete_lane_lifecycles_for_repo(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE lane_lifecycle WHERE repo_path = $p")
            .bind(("p", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("repo lane_lifecycle 全削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("repo lane_lifecycle 全削除エラー: {}", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::make_test_db;

    #[tokio::test]
    async fn test_active_lane_upsert_and_list() {
        // Model Q: active lane (presence) の daemon-canonical round-trip。
        let db = make_test_db().await;

        // 初期は空
        assert!(db.list_active_lanes().await.unwrap().is_empty());

        // repo ごとに upsert
        db.upsert_active_lane("/repos/vp", "vp/root").await.unwrap();
        db.upsert_active_lane("/repos/nexus", "nexus/sub/foo")
            .await
            .unwrap();

        let mut rows = db.list_active_lanes().await.unwrap();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                ("/repos/nexus".to_string(), "nexus/sub/foo".to_string()),
                ("/repos/vp".to_string(), "vp/root".to_string()),
            ]
        );

        // 同 repo の upsert は置換 (UNIQUE index、 per-repo に 1 つ)
        db.upsert_active_lane("/repos/vp", "vp/sub/bar")
            .await
            .unwrap();
        let rows = db.list_active_lanes().await.unwrap();
        assert_eq!(rows.len(), 2, "同 repo は置換、 件数は増えない");
        assert!(rows.contains(&("/repos/vp".to_string(), "vp/sub/bar".to_string())));

        // §4.6 含有=所有=寿命: repo remove 時の presence 回収 (delete_active_lane)。
        db.delete_active_lane("/repos/vp").await.unwrap();
        let rows = db.list_active_lanes().await.unwrap();
        assert_eq!(rows.len(), 1, "削除した repo の active_lane は消える");
        assert_eq!(rows[0].0, "/repos/nexus", "他 repo は残る");
        // 不在 repo の削除は no-op (冪等)
        db.delete_active_lane("/repos/absent").await.unwrap();
        assert_eq!(db.list_active_lanes().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_lane_upsert_list_and_delete() {
        use crate::repo::lane::{LaneAddress, LaneInfo, LaneState};
        // doc 24 §10 Phase 2: lane descriptor の daemon-canonical durable round-trip。
        let db = make_test_db().await;

        // 初期は空
        assert!(db.list_lanes().await.unwrap().is_empty());

        // テスト用 LaneInfo builder (live 値 pid は埋めるが、 検証は descriptor 中心)。
        let mk = |repo: &str, name: &str| LaneInfo {
            id: Default::default(),
            address: LaneAddress::new(repo, name),
            state: LaneState::Running,
            agent: "claude".to_string(),
            created_at: "2026-06-20T00:00:00Z".to_string(),
            pid: Some(1234),
            cwd: "/tmp".to_string(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
            flow_state: None,
        };

        // 2 repo に lane を入れる
        db.upsert_lane("/repos/vp", &mk("vp", "main"))
            .await
            .unwrap();
        db.upsert_lane("/repos/vp", &mk("vp", "foo")).await.unwrap();
        db.upsert_lane("/repos/nexus", &mk("nexus", "main"))
            .await
            .unwrap();

        let rows = db.list_lanes().await.unwrap();
        assert_eq!(rows.len(), 3, "3 lane descriptor が round-trip する");

        // descriptor が round-trip する (address / agent)
        let vp_main = rows
            .iter()
            .find(|(p, l)| p == "/repos/vp" && l.address.is_root())
            .expect("vp root が読める");
        assert_eq!(vp_main.1.address.to_string(), "vp/lane/main");
        assert_eq!(vp_main.1.agent, "claude");

        // 同 address の upsert は置換 (複合 UNIQUE、 件数は増えない)
        db.upsert_lane("/repos/vp", &mk("vp", "main"))
            .await
            .unwrap();
        assert_eq!(
            db.list_lanes().await.unwrap().len(),
            3,
            "同 address の upsert は置換"
        );

        // 単一 lane の削除 (Diff::Remove)
        db.delete_lane("/repos/vp", "vp/lane/foo").await.unwrap();
        let rows = db.list_lanes().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            !rows
                .iter()
                .any(|(_, l)| l.address.to_string() == "vp/lane/foo"),
            "削除した lane は消える"
        );

        // snapshot 全置換 (register snapshot): /repos/vp を sub 2 つに置換
        db.replace_lanes_for_repo("/repos/vp", &[mk("vp", "a"), mk("vp", "b")])
            .await
            .unwrap();
        let vp_lanes: Vec<_> = db
            .list_lanes()
            .await
            .unwrap()
            .into_iter()
            .filter(|(p, _)| p == "/repos/vp")
            .collect();
        assert_eq!(
            vp_lanes.len(),
            2,
            "snapshot で /repos/vp は 2 lane に全置換"
        );
        assert!(
            vp_lanes.iter().all(|(_, l)| !l.address.is_root()),
            "snapshot 後は root が消え sub のみ"
        );

        // §4.6 含有=所有=寿命: repo remove 時の回収 (delete_lanes_for_repo)。
        db.delete_lanes_for_repo("/repos/vp").await.unwrap();
        let rows = db.list_lanes().await.unwrap();
        assert_eq!(rows.len(), 1, "削除した repo の lane は消える");
        assert_eq!(rows[0].0, "/repos/nexus", "他 repo は残る");
    }

    #[tokio::test]
    async fn test_lane_lifecycle_upsert_list_delete() {
        // doc 24 §4.6: lane lifecycle (別 table) の round-trip。
        let db = make_test_db().await;
        assert!(db.list_lane_lifecycles().await.unwrap().is_empty());

        db.upsert_lane_lifecycle("/repos/vp", "vp/foo", "provisioning")
            .await
            .unwrap();
        db.upsert_lane_lifecycle("/repos/vp", "vp/sub/bar", "ready")
            .await
            .unwrap();
        db.upsert_lane_lifecycle("/repos/nexus", "nexus/sub/x", "ready")
            .await
            .unwrap();
        assert_eq!(db.list_lane_lifecycles().await.unwrap().len(), 3);

        // 同 (repo, address) の upsert は置換 (複合 UNIQUE)。
        db.upsert_lane_lifecycle("/repos/vp", "vp/foo", "ready")
            .await
            .unwrap();
        let rows = db.list_lane_lifecycles().await.unwrap();
        assert_eq!(rows.len(), 3, "同 address は置換、 件数は増えない");
        assert!(
            rows.iter()
                .any(|(p, a, lc)| p == "/repos/vp" && a == "vp/foo" && lc == "ready"),
            "provisioning → ready に置換される"
        );

        // 単一削除。
        db.delete_lane_lifecycle("/repos/vp", "vp/foo")
            .await
            .unwrap();
        assert_eq!(db.list_lane_lifecycles().await.unwrap().len(), 2);

        // repo 単位削除 (§4.6 含有=所有=寿命)。
        db.delete_lane_lifecycles_for_repo("/repos/vp")
            .await
            .unwrap();
        let rows = db.list_lane_lifecycles().await.unwrap();
        assert_eq!(rows.len(), 1, "削除した repo の lifecycle は消える");
        assert_eq!(rows[0].0, "/repos/nexus", "他 repo は残る");
    }
}
