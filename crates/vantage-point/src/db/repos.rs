//! 登録 repo の永続層（`repos` table）。
//!
//! ⚠️ **真実源はここではない**。registered repos の SSOT は `~/.config/vp/repos.kdl` で、
//! この table は db/machine 側の写し + 一方向 export（council 2026-05-16「ephemeral な DB でなく
//! 人間可読 file を SSOT に」）。VP-182 の「DB dir 変更で repos 消失」を構造的に解消した形。
//!
//! `export_repos` / `import_repos` / `replace_all_repos` は生 SQL を持たない合成 method で、
//! `crate::repos_file::RepoEntry` を介して kdl 側とやり取りする。
//!
//! unit test はここに無い。実体は `tests/repos_db_poc.rs`（統合 test）にある。
//!
//! 設計は [doc 62](../../../../docs/design/62-db-module-layout.md)。

use anyhow::Result;

use super::VpDb;

impl VpDb {
    // VP-188: Repos CRUD は撤去。 registered repos の SSOT は embedded DB から
    // `~/.config/vp/repos.kdl` に移行 (= VP-182 の「DB dir 変更で repos 消失」
    // regression を構造的に解消、 council 2026-05-16)。 repos 永続化は
    // `crate::repos_file::ReposFile` が担う。

    // =========================================================================
    // Repos CRUD（PoC: VP-188 revert、 db/machine 真実源 + repos.kdl 一方向 export）
    // =========================================================================

    /// 登録 repo を UPSERT（path で一意）。 ord = sidebar 並び順。
    pub async fn upsert_repo(
        &self,
        path: &str,
        name: &str,
        enabled: Option<bool>,
        slot: Option<u16>,
        ord: i64,
    ) -> Result<()> {
        self.db
            .query(
                "INSERT INTO repos {
                    path: $path,
                    name: $name,
                    enabled: $enabled,
                    slot: $slot,
                    ord: $ord
                } ON DUPLICATE KEY UPDATE
                    name = $input.name,
                    enabled = $input.enabled,
                    slot = $input.slot,
                    ord = $input.ord",
            )
            .bind(("path", path.to_string()))
            .bind(("name", name.to_string()))
            .bind(("enabled", enabled))
            .bind(("slot", slot.map(|s| s as i64)))
            .bind(("ord", ord))
            .await
            .map_err(|e| anyhow::anyhow!("repo upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("repo upsert エラー: {}", e))?;
        Ok(())
    }

    /// 登録 repo を削除（path で特定）。
    pub async fn delete_repo(&self, path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM repos WHERE path = $path")
            .bind(("path", path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("repo 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("repo 削除エラー: {}", e))?;
        Ok(())
    }

    /// 登録 repo 一覧を ord 昇順（= sidebar 並び順）で取得。
    pub async fn list_repos(&self) -> Result<Vec<serde_json::Value>> {
        let mut result = self
            .db
            .query("SELECT * FROM repos ORDER BY ord ASC")
            .await
            .map_err(|e| anyhow::anyhow!("repos 取得失敗: {}", e))?;
        let records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records)
    }

    /// DB の repos を RepoEntry 列に export（ord 昇順、 PoC: 一方向 export）。
    pub async fn export_repos(&self) -> Result<Vec<crate::repos_file::RepoEntry>> {
        let rows = self.list_repos().await?;
        Ok(rows
            .iter()
            .map(|v| crate::repos_file::RepoEntry {
                name: v
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string(),
                path: v
                    .get("path")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string(),
                enabled: v.get("enabled").and_then(|x| x.as_bool()),
                slot: v.get("slot").and_then(|x| x.as_u64()).map(|n| n as u16),
            })
            .collect())
    }

    /// RepoEntry 列を DB に import（出現順を ord に焼く、 PoC: 復旧用）。
    pub async fn import_repos(&self, entries: &[crate::repos_file::RepoEntry]) -> Result<()> {
        for (i, e) in entries.iter().enumerate() {
            self.upsert_repo(&e.path, &e.name, e.enabled, e.slot, i as i64)
                .await?;
        }
        Ok(())
    }

    /// repos テーブルを `entries` で全置換する（DELETE → import、 ord = 出現順）。
    ///
    /// `persist_repos` の全置換セマンティクスを 1 メソッドに閉じる。 in-memory を真実源として
    /// DB を上書きするため、 in-memory から消えた repo は DB からも消える (= upsert のみでは
    /// 残ってしまう削除分を確実に反映)。
    ///
    /// DELETE と import の間に空を読む窓が理論上あるが、 daemon は単一プロセスで reload/persist を
    /// 直列実行するため実害なし。 完全な単一トランザクション化は follow-up (epic memory のリスク表)。
    pub async fn replace_all_repos(&self, entries: &[crate::repos_file::RepoEntry]) -> Result<()> {
        self.db
            .query("DELETE FROM repos")
            .await
            .map_err(|e| anyhow::anyhow!("repos 全削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("repos 全削除エラー: {}", e))?;
        self.import_repos(entries).await
    }
}

#[cfg(test)]
mod tests {
    // VP-188: Repos CRUD テストは撤去 (= repos は repos.kdl に移行、
    // crate::repos_file 側の round-trip test でカバー)。
}
