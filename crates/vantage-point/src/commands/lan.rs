//! LAN address book CLI (VP-148 PR-P3-2)
//!
//! VP-144 Epic Phase 3 sub-PR 2。 mDNS で発見した同 LAN 上の VP world を address book
//! (`~/.config/vp/addresses.toml`) に永続化する CLI。 PR-P3-1 で実装した [`crate::lan_discovery`]
//! を CLI surface として expose、 後続 PR-P3-3 で cross-machine msg dispatch の宛先 lookup
//! source として使う。
//!
//! ## subcommands
//!
//! | command | 動作 |
//! |---------|------|
//! | `vp lan discover [--timeout 3000]` | mDNS で 同 LAN の VP world を列挙 (timeout ms) |
//! | `vp lan add <alias>` | mDNS discover 結果から alias で address book に entry 追加 |
//! | `vp lan list` | address book 内 entries 列挙 |
//! | `vp lan remove <alias>` | address book から削除 |
//!
//! **manual entry mode** (= mDNS なしで host/port 直接入力) は P3-3 以降で必要に応じて
//! 別 subcommand 化 (例: `vp lan add-manual`) する path を予定。 flag-based mode switch は
//! clap の `bool` default_value_t 制約で fragile になるため避ける。
//!
//! ## address book format (`~/.config/vp/addresses.toml`)
//!
//! ```toml
//! [[world]]
//! alias = "macbook-a"
//! hostname = "macbook-a.local."
//! port = 32000
//! pubkey = "pending"
//! discovered_via = "mDNS"
//! last_seen = "2026-05-09T..."
//! ```

use anyhow::{Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::lan_discovery;

/// `vp lan` subcommands
#[derive(Debug, Clone, Subcommand)]
pub enum LanCommands {
    /// mDNS で 同 LAN 上の VP world を列挙
    #[command(alias = "list-lan")]
    Discover {
        /// 列挙の timeout (ms、 default 3000ms)
        #[arg(long, default_value_t = 3000)]
        timeout: u64,
    },
    /// address book に entry を追加 (= mDNS discover 結果から alias 付与で永続化)
    Add {
        /// alias 名 (例: "macbook-a")
        alias: String,
        /// 列挙の timeout (ms、 default 3000ms)
        #[arg(long, default_value_t = 3000)]
        timeout: u64,
    },
    /// address book 内 entries を表示
    List,
    /// address book から entry を削除
    Remove {
        /// 削除対象 alias
        alias: String,
    },
}

/// address book 1 entry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddressEntry {
    pub alias: String,
    pub hostname: String,
    pub port: u16,
    pub pubkey: String,
    /// 検出経路 (例: `"mDNS"` / `"manual"` / `"hub"` (P4+))
    pub discovered_via: String,
    /// 最終 seen 時刻 (ISO 8601、 P3-3 以降で update)
    pub last_seen: String,
}

/// address book file 構造 (`~/.config/vp/addresses.toml`)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AddressBook {
    /// world 配列 (TOML の `[[world]]` array of tables)
    #[serde(default, rename = "world")]
    pub worlds: Vec<AddressEntry>,
}

impl AddressBook {
    /// address book 永続化 path (= `~/.config/vp/addresses.toml`)
    pub fn path() -> PathBuf {
        crate::config::config_dir().join("addresses.toml")
    }

    /// disk から read、 file 不在なら空 book を返す
    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("address book read 失敗: {}", path.display()))?;
        let book: Self = toml::from_str(&raw)
            .with_context(|| format!("address book parse 失敗: {}", path.display()))?;
        Ok(book)
    }

    /// disk に write (config dir を auto create)
    pub fn save(&self) -> Result<()> {
        let dir = crate::config::config_dir();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("config dir create 失敗: {}", dir.display()))?;
        let path = Self::path();
        let raw = toml::to_string_pretty(self)
            .with_context(|| "address book serialize 失敗".to_string())?;
        std::fs::write(&path, raw)
            .with_context(|| format!("address book write 失敗: {}", path.display()))?;
        Ok(())
    }

    /// alias で entry 検索
    pub fn find(&self, alias: &str) -> Option<&AddressEntry> {
        self.worlds.iter().find(|w| w.alias == alias)
    }

    /// hostname で entry 検索 (VP-148 PR-P3-3: cross-machine forward の resolver で使用)
    ///
    /// v3.1 syntax の `Address::Project::world` segment (例 `macbook-a.local`) を
    /// AddressBook の `entry.hostname` と equality match。 末尾 `.` の有無は両側で
    /// 正規化 (= P3-2 の add_entry 時点で `trim_end_matches('.')` 済み、 caller も
    /// 末尾 `.` 抜きで渡す前提)。
    pub fn find_by_host(&self, host: &str) -> Option<&AddressEntry> {
        let normalized = host.trim_end_matches('.');
        self.worlds
            .iter()
            .find(|w| w.hostname.trim_end_matches('.') == normalized)
    }

    /// alias で entry 削除 (戻り値: 削除した entry 数)
    pub fn remove(&mut self, alias: &str) -> usize {
        let before = self.worlds.len();
        self.worlds.retain(|w| w.alias != alias);
        before - self.worlds.len()
    }

    /// entry を upsert (同 alias なら上書き、 さもなくば追加)
    pub fn upsert(&mut self, entry: AddressEntry) {
        if let Some(slot) = self.worlds.iter_mut().find(|w| w.alias == entry.alias) {
            *slot = entry;
        } else {
            self.worlds.push(entry);
        }
    }

    /// VP-149: mDNS [`DiscoveredWorld`] から AddressEntry を導出して auto-upsert
    ///
    /// alias 生成 rule: instance_name (例: `world-mito-mac-4`) の先頭 segment 直後の suffix
    /// (= `mito-mac-4`) を採用。 collision (= 同 alias の別 machine) は last-write-wins。
    /// last_seen は呼び出し時の UTC で更新、 discovered_via = `"mDNS"` 固定。
    ///
    /// VP-154 PR-1 Moody Blues fix #2 (Score 78): TXT record `kind=world` のみ upsert する。
    /// SP entry (= `kind=sp`、 instance `sp-<project>-<host>`) は AddressBook 構造を 2 軸化する
    /// PR-3 (= `project_ports` HashMap 追加) まで filter で除外、 既存 entry の汚染回避。
    pub fn auto_upsert_from_discovered(&mut self, world: &crate::lan_discovery::DiscoveredWorld) {
        // VP-154 PR-1: kind != "world" は filter (= SP entry は AddressBook book に入れない)。
        // TXT record `kind` 不在は v3.1 (VP-148/149) からの旧 entry 互換、 default で world 扱い。
        let kind = world
            .properties
            .get("kind")
            .map(|s| s.as_str())
            .unwrap_or("world");
        if kind != "world" {
            return;
        }
        // instance_name の先頭 segment を取って alias 化:
        // - VP-154+: `world-mito-mac-4._vp._tcp.local.` → segment[0] = `world-mito-mac-4`
        //           ここから `world-` prefix を strip して `mito-mac-4` を alias
        // - VP-149-: `mito-mac-4._vp._tcp.local.` → segment[0] = `mito-mac-4` (= 旧 entry 互換)
        let first_segment = world
            .instance_name
            .split('.')
            .next()
            .unwrap_or(&world.instance_name);
        let alias = first_segment
            .strip_prefix("world-")
            .unwrap_or(first_segment)
            .to_string();
        if alias.is_empty() {
            return;
        }
        let entry = AddressEntry {
            alias,
            hostname: world.hostname.trim_end_matches('.').to_string(),
            port: world.port,
            pubkey: world.pubkey.clone(),
            discovered_via: "mDNS".to_string(),
            last_seen: chrono::Utc::now().to_rfc3339(),
        };
        self.upsert(entry);
    }

    /// VP-149: mDNS instance_name 由来 alias で entry を削除 (= ServiceRemoved 連動)
    ///
    /// 戻り値: 削除した entry 数 (0 or 1)。 alias は instance_name の先頭 segment。
    pub fn auto_remove_by_instance_name(&mut self, instance_name: &str) -> usize {
        let alias = instance_name.split('.').next().unwrap_or(instance_name);
        if alias.is_empty() {
            return 0;
        }
        self.remove(alias)
    }
}

/// `vp lan` subcommand handler
pub fn handle_lan_command(cmd: LanCommands) -> Result<()> {
    match cmd {
        LanCommands::Discover { timeout } => discover_print(timeout),
        LanCommands::Add { alias, timeout } => add_entry(&alias, timeout),
        LanCommands::List => list_book(),
        LanCommands::Remove { alias } => remove_entry(&alias),
    }
}

fn discover_print(timeout_ms: u64) -> Result<()> {
    println!("LAN discover (timeout {}ms)...", timeout_ms);
    let worlds = lan_discovery::discover(timeout_ms)
        .map_err(|e| anyhow::anyhow!("mDNS discover 失敗: {}", e))?;
    if worlds.is_empty() {
        println!("(no VP world found on LAN)");
        return Ok(());
    }
    println!("Found {} VP world(s):", worlds.len());
    for w in &worlds {
        println!(
            "  {} (host={} port={} pubkey={} version={})",
            w.instance_name, w.hostname, w.port, w.pubkey, w.version
        );
    }
    Ok(())
}

fn add_entry(alias: &str, timeout_ms: u64) -> Result<()> {
    if alias.is_empty() {
        anyhow::bail!("alias must not be empty");
    }
    println!(
        "LAN discover (timeout {}ms) で alias={} を解決中...",
        timeout_ms, alias
    );
    let worlds = lan_discovery::discover(timeout_ms)
        .map_err(|e| anyhow::anyhow!("mDNS discover 失敗: {}", e))?;
    // instance_name 先頭 = alias で match (= macbook-a._vp._tcp.local. の prefix が "macbook-a")
    // または hostname で match (= macbook-a.local. が "macbook-a" を含む)
    let matched = worlds.iter().find(|w| {
        w.instance_name.starts_with(alias) || w.hostname.trim_end_matches('.').starts_with(alias)
    });
    let Some(w) = matched else {
        anyhow::bail!(
            "alias '{}' が同 LAN 上で見つからない (mDNS discover 結果 {} 件)",
            alias,
            worlds.len()
        );
    };
    let entry = AddressEntry {
        alias: alias.to_string(),
        hostname: w.hostname.trim_end_matches('.').to_string(),
        port: w.port,
        pubkey: w.pubkey.clone(),
        discovered_via: "mDNS".to_string(),
        last_seen: chrono::Utc::now().to_rfc3339(),
    };

    let mut book = AddressBook::load()?;
    let action = if book.find(alias).is_some() {
        "update"
    } else {
        "add"
    };
    book.upsert(entry.clone());
    book.save()?;
    println!(
        "{} alias={} host={} port={} pubkey={}",
        action, entry.alias, entry.hostname, entry.port, entry.pubkey
    );
    Ok(())
}

fn list_book() -> Result<()> {
    let book = AddressBook::load()?;
    if book.worlds.is_empty() {
        println!("(address book empty — `vp lan add <alias>` で追加)");
        return Ok(());
    }
    println!("Address book ({}):", AddressBook::path().display());
    for w in &book.worlds {
        println!(
            "  {} = {}:{} (pubkey={} via={} last_seen={})",
            w.alias, w.hostname, w.port, w.pubkey, w.discovered_via, w.last_seen
        );
    }
    Ok(())
}

fn remove_entry(alias: &str) -> Result<()> {
    let mut book = AddressBook::load()?;
    let removed = book.remove(alias);
    if removed == 0 {
        anyhow::bail!("alias '{}' は address book に存在しない", alias);
    }
    book.save()?;
    println!("removed alias={}", alias);
    Ok(())
}

/// async caller (= vp daemon 等) から CLI handler を呼びたい場合の wrapper。
/// `lan_discovery::discover` が blocking なので `spawn_blocking` する。
pub async fn handle_lan_command_async(cmd: LanCommands) -> Result<()> {
    tokio::task::spawn_blocking(move || handle_lan_command(cmd))
        .await
        .with_context(|| "spawn_blocking join 失敗")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_book_default_is_empty() {
        let book = AddressBook::default();
        assert!(book.worlds.is_empty());
    }

    #[test]
    fn address_book_upsert_and_find() {
        let mut book = AddressBook::default();
        let entry = AddressEntry {
            alias: "macbook-a".to_string(),
            hostname: "macbook-a.local".to_string(),
            port: 32000,
            pubkey: "pending".to_string(),
            discovered_via: "mDNS".to_string(),
            last_seen: "2026-05-09T00:00:00Z".to_string(),
        };
        book.upsert(entry.clone());
        assert_eq!(book.worlds.len(), 1);
        assert_eq!(book.find("macbook-a"), Some(&entry));
        assert!(book.find("macbook-b").is_none());

        // 同 alias で upsert は上書き
        let updated = AddressEntry {
            port: 33000,
            ..entry.clone()
        };
        book.upsert(updated.clone());
        assert_eq!(book.worlds.len(), 1);
        assert_eq!(book.find("macbook-a").unwrap().port, 33000);
    }

    #[test]
    fn address_book_remove() {
        let mut book = AddressBook::default();
        book.upsert(AddressEntry {
            alias: "a".to_string(),
            hostname: "a.local".to_string(),
            port: 32000,
            pubkey: "pending".to_string(),
            discovered_via: "mDNS".to_string(),
            last_seen: "2026-05-09T00:00:00Z".to_string(),
        });
        assert_eq!(book.remove("a"), 1);
        assert!(book.worlds.is_empty());
        assert_eq!(book.remove("nonexistent"), 0);
    }

    #[test]
    fn address_book_serializes_round_trip() {
        let mut book = AddressBook::default();
        book.upsert(AddressEntry {
            alias: "macbook-a".to_string(),
            hostname: "macbook-a.local".to_string(),
            port: 32000,
            pubkey: "pending".to_string(),
            discovered_via: "mDNS".to_string(),
            last_seen: "2026-05-09T00:00:00Z".to_string(),
        });
        let raw = toml::to_string_pretty(&book).unwrap();
        let parsed: AddressBook = toml::from_str(&raw).unwrap();
        assert_eq!(parsed.worlds, book.worlds);
    }

    #[test]
    fn address_book_path_uses_config_dir() {
        let path = AddressBook::path();
        assert!(path.ends_with("addresses.toml"));
    }

    #[test]
    fn auto_upsert_from_discovered_legacy_no_kind_field() {
        // VP-149 旧 entry 互換: TXT record に kind 不在 → world 扱いで upsert
        let world = crate::lan_discovery::DiscoveredWorld {
            hostname: "macbook-a.local.".to_string(),
            instance_name: "macbook-a._vp._tcp.local.".to_string(),
            port: 32000,
            pubkey: "pending".to_string(),
            version: "v3".to_string(),
            properties: std::collections::HashMap::new(),
        };
        let mut book = AddressBook::default();
        book.auto_upsert_from_discovered(&world);
        assert_eq!(book.worlds.len(), 1);
        assert_eq!(book.worlds[0].alias, "macbook-a");
    }

    #[test]
    fn auto_upsert_from_discovered_world_kind_strips_prefix() {
        // VP-154 PR-1: instance `world-mito-mac-4` の `world-` prefix を strip して alias `mito-mac-4`
        let mut props = std::collections::HashMap::new();
        props.insert("kind".to_string(), "world".to_string());
        let world = crate::lan_discovery::DiscoveredWorld {
            hostname: "mito-mac-4.local.".to_string(),
            instance_name: "world-mito-mac-4._vp._tcp.local.".to_string(),
            port: 32000,
            pubkey: "pending".to_string(),
            version: "v3".to_string(),
            properties: props,
        };
        let mut book = AddressBook::default();
        book.auto_upsert_from_discovered(&world);
        assert_eq!(book.worlds.len(), 1);
        let entry = &book.worlds[0];
        assert_eq!(entry.alias, "mito-mac-4"); // `world-` prefix strip 済
        assert_eq!(entry.hostname, "mito-mac-4.local");
        assert_eq!(entry.port, 32000);
    }

    #[test]
    fn auto_upsert_from_discovered_sp_kind_filtered() {
        // VP-154 PR-1 Moody Blues fix #2 (Score 78): kind=sp は AddressBook に upsert しない
        // (= 「book には world のみ」 invariant、 SP entry は PR-3 で project_ports field に入る path)
        let mut props = std::collections::HashMap::new();
        props.insert("kind".to_string(), "sp".to_string());
        props.insert("project".to_string(), "creo-ui".to_string());
        let world = crate::lan_discovery::DiscoveredWorld {
            hostname: "mito-mac-4.local.".to_string(),
            instance_name: "sp-creo-ui-mito-mac-4._vp._tcp.local.".to_string(),
            port: 33005,
            pubkey: "pending".to_string(),
            version: "v3".to_string(),
            properties: props,
        };
        let mut book = AddressBook::default();
        book.auto_upsert_from_discovered(&world);
        assert!(
            book.worlds.is_empty(),
            "SP entry should not be upserted to book"
        );
    }

    #[test]
    fn auto_upsert_from_discovered_last_write_wins() {
        let mut props = std::collections::HashMap::new();
        props.insert("kind".to_string(), "world".to_string());
        let world = crate::lan_discovery::DiscoveredWorld {
            hostname: "mito-mac-4.local.".to_string(),
            instance_name: "world-mito-mac-4._vp._tcp.local.".to_string(),
            port: 32000,
            pubkey: "pending".to_string(),
            version: "v3".to_string(),
            properties: props,
        };
        let mut book = AddressBook::default();
        book.auto_upsert_from_discovered(&world);
        let mut world2 = world.clone();
        world2.port = 32100;
        book.auto_upsert_from_discovered(&world2);
        assert_eq!(book.worlds.len(), 1);
        assert_eq!(book.worlds[0].port, 32100);
    }

    #[test]
    fn auto_remove_by_instance_name_uses_alias_segment() {
        // VP-149: ServiceRemoved event で instance_name 由来 alias を削除
        let mut book = AddressBook::default();
        book.upsert(AddressEntry {
            alias: "macbook-a".to_string(),
            hostname: "macbook-a.local".to_string(),
            port: 32000,
            pubkey: "pending".to_string(),
            discovered_via: "mDNS".to_string(),
            last_seen: "2026-05-09T00:00:00Z".to_string(),
        });
        let removed = book.auto_remove_by_instance_name("macbook-a._vp._tcp.local.");
        assert_eq!(removed, 1);
        assert!(book.worlds.is_empty());

        // 不在 instance_name は 0 返却
        let removed = book.auto_remove_by_instance_name("nonexistent._vp._tcp.local.");
        assert_eq!(removed, 0);
    }

    #[test]
    fn find_by_host_normalizes_trailing_dot() {
        // VP-148 PR-P3-3: cross-machine forward の resolver path で trailing `.` 揺れを吸収。
        // book.entry.hostname と find_by_host(host) の両方で末尾 `.` を trim して match。
        let mut book = AddressBook::default();
        book.upsert(AddressEntry {
            alias: "macbook-a".to_string(),
            hostname: "macbook-a.local".to_string(), // P3-2 の add で `.` trim 済前提
            port: 32000,
            pubkey: "pending".to_string(),
            discovered_via: "mDNS".to_string(),
            last_seen: "2026-05-09T00:00:00Z".to_string(),
        });

        // 末尾 `.` なしで match
        assert_eq!(
            book.find_by_host("macbook-a.local")
                .map(|e| e.alias.as_str()),
            Some("macbook-a")
        );
        // 末尾 `.` ありでも match (= mDNS form でも OK)
        assert_eq!(
            book.find_by_host("macbook-a.local.")
                .map(|e| e.alias.as_str()),
            Some("macbook-a")
        );
        // 不在 host は None
        assert!(book.find_by_host("macbook-b.local").is_none());
    }
}
