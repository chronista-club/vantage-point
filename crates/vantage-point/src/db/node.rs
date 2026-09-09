//! daemon の node id（`node_identity` table）。
//!
//! federation L2（ADR-020 D2）の home-node を指す**位置独立で安定な id** `nd_xxx`。
//! `db/machine` の singleton row（固定 record id `node_identity:self`）で、daemon が初回起動で
//! 1 度だけ発行して永続し、以降の再起動は復元する。
//!
//! lane 側の `crate::lane::lane_id` とは粒度が違う — lane は (repo, lane) ごとに file 永続、
//! daemon は machine に 1 つなので db に置く。
//!
//! 設計は [doc 62](../../../../docs/design/62-db-module-layout.md)。

use anyhow::Result;

use super::VpDb;

impl VpDb {
    // =========================================================================
    // Daemon identity (federation L2、 ADR-020 D2): home-node の位置独立 安定 id `nd_xxx`。
    // db/machine の singleton row (固定 record id node_identity:self)。daemon が初回起動で
    // 1 度だけ発行し永続、 以降の再起動は復元する。[`crate::lane::lane_id::load_or_create`] の
    // db 版 — lane は (repo,lane) ごと file 永続、 daemon は daemon に 1 つなので db singleton。
    // =========================================================================

    /// home-node の node_id を取得する (無ければ生成して永続)。
    ///
    /// - 既存 singleton row があり非空なら **それを復元** (= 再起動を越えて安定)。
    /// - 無い / 空なら **新規生成して永続** し、 その id を返す。
    ///
    /// daemon は single-writer (db comment 参照) かつ boot で 1 度だけ呼ぶため race は無い。
    /// 書き込みは REMOVE FIELD (旧 catalog 掃除、 下記) + DELETE+CREATE を単一 query に
    /// まとめて行う (空 row が残っていた場合も確実に上書き。 万一 DDL だけ効いて DML が
    /// 失敗しても「制約が外れて row が無い」だけの状態なので、 次回 boot の再発行で自己修復する)。
    pub async fn load_or_create_node_id(&self) -> Result<crate::node::NodeId> {
        // 既存 singleton row の node_id を読む (存在しなければ空配列)。
        let mut result = self
            .db
            .query("SELECT VALUE node_id FROM node_identity:self")
            .await
            .map_err(|e| anyhow::anyhow!("node_id 取得失敗: {}", e))?;
        // 旧 schema 行 (v0.56.0 以前 = `wld_id` field のみ) では node_id が NONE になり、
        // Vec<String> への take は Err を返す。これは「未発行」と同義なので既定値 (空) に
        // 潰して下の再発行経路へ落とす (ADR-021 P5 — field rename 自体が移行機構。 Err の
        // まま返すと呼び出し側が degraded 継続し、 空 node_id で hub に register してしまう)。
        let existing: Vec<String> = result.take(0).unwrap_or_default();
        if let Some(id) = existing.into_iter().find(|s| !s.trim().is_empty()) {
            return Ok(crate::node::NodeId::from(id));
        }

        // 無ければ新規発行して永続する。 先に旧 catalog の残存 field 定義を外す —
        // v0.56.0 以前の DB は SCHEMAFULL に `wld_id: string` (必須) を定義しており、
        // migration DDL (DEFINE IF NOT EXISTS) は既存定義に触らないため rename 後も残る。
        // 残ったままだと node_id のみの新行 CREATE が必須違反で弾かれ、 再発行が Err →
        // 呼び出し側の degraded 継続 = 空 node_id register に化ける (mito-mba 実 live 二段目)。
        let id = crate::node::NodeId::generate();
        self.db
            .query(
                "REMOVE FIELD IF EXISTS wld_id ON node_identity;
                 DELETE node_identity:self;
                 CREATE node_identity:self CONTENT {
                    node_id: $node_id,
                    created_at: time::now()
                 }",
            )
            .bind(("node_id", id.as_str().to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("node_id 永続失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("node_id 永続エラー: {}", e))?;
        tracing::info!("home-node identity 発行: node_id={}", id);
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::make_test_db;

    #[tokio::test]
    async fn test_daemon_id_load_or_create_is_stable() {
        // federation L2: node_id singleton の発行 → 復元 round-trip。
        let db = make_test_db().await;

        // 初回は生成して永続 (EntId 形式 nd_1.. )。
        let first = db.load_or_create_node_id().await.unwrap();
        assert!(
            first.as_str().starts_with("nd_1"),
            "EntId 形式 nd_1.. のはず: {first}"
        );

        // 2 回目以降は同じ id を復元する (= singleton、 再起動越え安定の核)。
        let second = db.load_or_create_node_id().await.unwrap();
        assert_eq!(first, second, "node_id は singleton で安定して復元される");
    }

    /// ADR-021 P5 の実マシン移行経路: v0.56.0 以前の DB の上でも nd_ id が再発行されること。
    /// 実 live で mito-mba が二段構えで踏んだ regression の固定 (2026-07-27):
    /// ① 旧行 (node_id field 無し) の SELECT で take が Err → degraded 継続に化けて
    ///    **空 node_id で hub に register**。
    /// ② ①を直しても、 旧 catalog の残存 field 定義 (SCHEMAFULL `wld_id: string` 必須。
    ///    DEFINE IF NOT EXISTS は既存定義に触らないため rename 後も DB に残る) が
    ///    再発行の CREATE (node_id のみの行) を必須違反で弾き、 結局 Err → 空 register。
    /// fresh test db は旧「行」だけ模しても②を検出できない — 旧 **catalog** ごと模す。
    #[tokio::test]
    async fn test_node_id_reissues_over_legacy_row() {
        let db = make_test_db().await;

        // v0.56.0 の DB を模す: catalog を旧定義 (wld_id 必須・node_id 無し) に巻き戻して
        // 旧行を作り、 その上に v0.57.0 boot の migration DDL (node_id 追加) を適用する。
        db.db
            .query(
                "REMOVE FIELD node_id ON node_identity;
                 DEFINE FIELD wld_id ON node_identity TYPE string;
                 DELETE node_identity:self;
                 CREATE node_identity:self CONTENT { wld_id: 'wld_legacy', created_at: time::now() };
                 DEFINE FIELD IF NOT EXISTS node_id ON node_identity TYPE string;",
            )
            .await
            .unwrap()
            .check()
            .unwrap();

        // Err でも空でもなく、 nd_ を再発行して返す。
        let id = db.load_or_create_node_id().await.unwrap();
        assert!(
            id.as_str().starts_with("nd_1"),
            "旧行の上から nd_ を再発行するはず: {id}"
        );

        // 再発行後は通常の singleton 復元に合流する。
        let again = db.load_or_create_node_id().await.unwrap();
        assert_eq!(id, again, "再発行した id は以降安定して復元される");
    }
}
