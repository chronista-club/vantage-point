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
use std::collections::HashMap;
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
    /// VP-154 chore: stale entry 一掃 (= 過去 LocalHostName churn 由来の累積掃除)
    ///
    /// `last_seen` が threshold より古い entry を削除。 mDNS goodbye 漏れや LocalHostName
    /// auto-increment 由来の累積に対処する手動 reset。 dogfood で 「book がゴチャゴチャ」 を感じたら
    /// 実行、 PR-3.5 (= advertise_hostname 固定) と組み合わせて使う想定。
    Prune {
        /// 削除 threshold (例: `1h` / `30m` / `1d` / `7d`)、 default `1h`
        #[arg(long, default_value = "1h")]
        older_than: String,
        /// 全 entry 削除 (= `older_than` 無視)
        #[arg(long)]
        all: bool,
        /// 削除対象を表示するだけ (= 実行せず preview)
        #[arg(long)]
        dry_run: bool,
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
    /// VP-154 PR-3: 同 host 上の SP port mapping (= `{ project_name → SP port }`)
    ///
    /// mDNS で `kind=sp` instance を受信したとき、 同 hostname の AddressEntry に対して
    /// `project_ports[project] = sp_port` を upsert する。 cross-machine 1-hop forward (PR-4) で
    /// `(host, project) → SP port` lookup の本体になる field。
    ///
    /// 旧 entry (= v3.1 = PR-3 以前) は本 field 不在、 `#[serde(default)]` で空 HashMap として
    /// load される。 backward compat なので migration 不要 — 旧 entry は SP discover で
    /// 自然に populate される。
    #[serde(default)]
    pub project_ports: HashMap<String, u16>,
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

    /// entry を upsert (= 同 alias なら **world fields 上書き、 project_ports は merge**、
    /// さもなくば追加)
    ///
    /// VP-154 PR-3 changes: 同 alias 上書き時に `project_ports` を保持して merge する。
    /// 旧 (= 単純 replace) では mDNS で kind=world が後着すると SP discover 結果の port 情報が
    /// 飛んでしまう。 PR-3 以降は `project_ports` を最新の SP discover で populate する flow なので、
    /// world 上書きで port 情報を消す挙動は不適。
    pub fn upsert(&mut self, mut entry: AddressEntry) {
        if let Some(slot) = self.worlds.iter_mut().find(|w| w.alias == entry.alias) {
            // 既存 project_ports を新 entry に merge (= 既存 wins for project_ports key 衝突、
            // ただし通常は新 entry 側が空 HashMap なので実質 既存 全保持)。 caller 側で SP port
            // 更新したい場合は `record_sp_port` を使う path にする。
            for (project, port) in slot.project_ports.drain() {
                entry.project_ports.entry(project).or_insert(port);
            }
            *slot = entry;
        } else {
            self.worlds.push(entry);
        }
    }

    /// VP-154 PR-3: SP discover 由来の `(host, project, sp_port)` を該当 world entry に記録
    ///
    /// mDNS で `kind=sp` instance (= `sp-<project>-<host>`) を受信したとき、 hostname で AddressBook
    /// を引いて `project_ports[project] = sp_port` を upsert する。 該当 world entry が不在なら
    /// **stub entry を作る** (= alias=hostname-derived、 world port=0、 pubkey=pending)。 後続で
    /// world advertise が届いたら upsert で world fields が埋まり、 project_ports は merge で保持。
    ///
    /// mDNS event 順序保証なし (= SP advertise が world advertise より先着するケース) への
    /// resilience として、 stub create を許容する設計。
    pub fn record_sp_port(&mut self, hostname: &str, project: &str, sp_port: u16) {
        let normalized_host = hostname.trim_end_matches('.').to_string();
        if let Some(slot) = self
            .worlds
            .iter_mut()
            .find(|w| w.hostname.trim_end_matches('.') == normalized_host)
        {
            slot.project_ports.insert(project.to_string(), sp_port);
            slot.last_seen = chrono::Utc::now().to_rfc3339();
            return;
        }
        // stub entry: alias は hostname の先頭 segment から推定 (= `mito-mac-4.local` → `mito-mac-4`)
        let alias = normalized_host
            .split('.')
            .next()
            .unwrap_or(&normalized_host)
            .to_string();
        if alias.is_empty() {
            return;
        }
        let mut project_ports = HashMap::new();
        project_ports.insert(project.to_string(), sp_port);
        self.worlds.push(AddressEntry {
            alias,
            hostname: normalized_host,
            port: 0,                       // stub: world port 未確定
            pubkey: "pending".to_string(), // stub
            discovered_via: "mDNS".to_string(),
            last_seen: chrono::Utc::now().to_rfc3339(),
            project_ports,
        });
    }

    /// VP-154 PR-3: `(host, project)` lookup で SP port を取得 (= cross-machine forward の resolver)
    pub fn find_sp_port(&self, host: &str, project: &str) -> Option<u16> {
        let normalized = host.trim_end_matches('.');
        self.worlds
            .iter()
            .find(|w| w.hostname.trim_end_matches('.') == normalized)
            .and_then(|w| w.project_ports.get(project).copied())
    }

    /// VP-149 / VP-154 PR-3: mDNS [`DiscoveredWorld`] から auto-upsert (= kind 別 dispatch)
    ///
    /// - `kind=world` (= 旧 default、 v3.1 互換含む): world entry として upsert (alias 生成は
    ///   instance_name 先頭 segment、 `world-` prefix 付与の場合は strip)
    /// - `kind=sp`: hostname 解決して `record_sp_port(host, project, port)` で project_ports に
    ///   反映 (= PR-1 Moody Blues fix で filter してた経路を PR-3 で活性化)
    /// - `kind` その他: 未知 → debug log のみで skip (= 将来 kind 拡張時の forward compat)
    pub fn auto_upsert_from_discovered(&mut self, world: &crate::lan_discovery::DiscoveredWorld) {
        let kind = world
            .properties
            .get("kind")
            .map(|s| s.as_str())
            .unwrap_or("world");

        match kind {
            "world" => {
                // VP-154+: `world-mito-mac-4` → strip prefix → alias `mito-mac-4`
                // VP-149-: `mito-mac-4` (旧 default、 prefix なし) → alias そのまま
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
                    project_ports: HashMap::new(),
                };
                self.upsert(entry);
            }
            "sp" => {
                // SP advertise の TXT record は `project=<name>` を必ず含む (= lan_discovery.rs の
                // AnnounceKind::Sp で seed)。 不在なら不正な advertise なので skip。
                let Some(project) = world.properties.get("project") else {
                    tracing::debug!(
                        "auto_upsert: kind=sp に project field 不在、 skip (instance={})",
                        world.instance_name
                    );
                    return;
                };
                let host = world.hostname.trim_end_matches('.');
                self.record_sp_port(host, project, world.port);
            }
            other => {
                tracing::debug!(
                    "auto_upsert: 未知 kind={} (forward compat、 skip)、 instance={}",
                    other,
                    world.instance_name
                );
            }
        }
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
        LanCommands::Prune {
            older_than,
            all,
            dry_run,
        } => prune_entries(&older_than, all, dry_run),
    }
}

/// VP-154 chore: simple duration parser — `1h` / `30m` / `1d` / `7d` 形式を `chrono::Duration` に
///
/// humantime crate は dep 追加せず inline 実装。 末尾 1 文字を unit、 残りを number として解釈:
/// - `s`: 秒、 `m`: 分、 `h`: 時間、 `d`: 日
fn parse_duration(s: &str) -> anyhow::Result<chrono::Duration> {
    let s = s.trim();
    if s.len() < 2 {
        anyhow::bail!(
            "duration format invalid: `{}` (例: `1h` / `30m` / `1d` / `7d`)",
            s
        );
    }
    let (num_part, unit) = s.split_at(s.len() - 1);
    let n: i64 = num_part
        .parse()
        .map_err(|e| anyhow::anyhow!("duration の数値部 `{}` parse 失敗: {}", num_part, e))?;
    match unit {
        "s" => Ok(chrono::Duration::seconds(n)),
        "m" => Ok(chrono::Duration::minutes(n)),
        "h" => Ok(chrono::Duration::hours(n)),
        "d" => Ok(chrono::Duration::days(n)),
        _ => anyhow::bail!(
            "duration unit `{}` 未対応 (= s/m/h/d のみ、 例: `1h` / `7d`)",
            unit
        ),
    }
}

fn prune_entries(older_than: &str, all: bool, dry_run: bool) -> Result<()> {
    let mut book = AddressBook::load()?;
    let initial_count = book.worlds.len();
    if initial_count == 0 {
        println!("(address book empty、 nothing to prune)");
        return Ok(());
    }

    let cutoff_label;
    let to_remove: Vec<String> = if all {
        cutoff_label = "ALL".to_string();
        book.worlds.iter().map(|w| w.alias.clone()).collect()
    } else {
        let duration = parse_duration(older_than)?;
        let cutoff = chrono::Utc::now() - duration;
        cutoff_label = format!(
            "older than {} (cutoff = {})",
            older_than,
            cutoff.to_rfc3339()
        );
        book.worlds
            .iter()
            .filter(|w| {
                // last_seen が parse 不能なら「不明 = 古いものとして prune 対象」 扱い
                chrono::DateTime::parse_from_rfc3339(&w.last_seen)
                    .map(|t| t < cutoff)
                    .unwrap_or(true)
            })
            .map(|w| w.alias.clone())
            .collect()
    };

    if to_remove.is_empty() {
        println!(
            "({}: 削除対象 entry なし、 全 {} 件 fresh)",
            cutoff_label, initial_count
        );
        return Ok(());
    }

    println!(
        "prune target ({}): {} entries",
        cutoff_label,
        to_remove.len()
    );
    for alias in &to_remove {
        println!("  - {}", alias);
    }

    if dry_run {
        println!("(--dry-run、 実行せず終了。 disk への変更なし)");
        return Ok(());
    }

    let mut removed_count = 0usize;
    for alias in &to_remove {
        removed_count += book.remove(alias);
    }
    book.save()?;
    println!(
        "removed {} entries、 残り {} entries",
        removed_count,
        book.worlds.len()
    );
    Ok(())
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
        project_ports: HashMap::new(),
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
        // VP-154 PR-3: project_ports があれば 1 行ずつインデントして列挙
        if !w.project_ports.is_empty() {
            // alphabetical 順で安定表示 (= HashMap iteration 順は非決定的)
            let mut entries: Vec<(&String, &u16)> = w.project_ports.iter().collect();
            entries.sort_by_key(|(k, _)| k.as_str());
            for (project, port) in entries {
                println!("    └ {} → port {}", project, port);
            }
        }
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

    fn make_entry(alias: &str, hostname: &str, port: u16) -> AddressEntry {
        AddressEntry {
            alias: alias.to_string(),
            hostname: hostname.to_string(),
            port,
            pubkey: "pending".to_string(),
            discovered_via: "mDNS".to_string(),
            last_seen: "2026-05-09T00:00:00Z".to_string(),
            project_ports: HashMap::new(),
        }
    }

    #[test]
    fn address_book_upsert_and_find() {
        let mut book = AddressBook::default();
        let entry = make_entry("macbook-a", "macbook-a.local", 32000);
        book.upsert(entry.clone());
        assert_eq!(book.worlds.len(), 1);
        assert_eq!(book.find("macbook-a"), Some(&entry));
        assert!(book.find("macbook-b").is_none());

        // 同 alias で upsert は world fields 上書き
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
        book.upsert(make_entry("a", "a.local", 32000));
        assert_eq!(book.remove("a"), 1);
        assert!(book.worlds.is_empty());
        assert_eq!(book.remove("nonexistent"), 0);
    }

    #[test]
    fn address_book_serializes_round_trip() {
        let mut book = AddressBook::default();
        book.upsert(make_entry("macbook-a", "macbook-a.local", 32000));
        let raw = toml::to_string_pretty(&book).unwrap();
        let parsed: AddressBook = toml::from_str(&raw).unwrap();
        assert_eq!(parsed.worlds, book.worlds);
    }

    #[test]
    fn address_book_serializes_round_trip_with_project_ports() {
        // VP-154 PR-3 (Moody Blues Issue #2): project_ports populated 状態の round-trip 検証。
        // TOML 1.0 仕様で `[[world]]` array element 内 HashMap は inline table
        // (`project_ports = {creo-ui = 33005}`) として serialize される (= `[world.project_ports]`
        // サブセクション形式は array of tables 制約で使えない)。 toml 1.1 crate の挙動が
        // 期待通りで、 disk persistence の forward / backward が破綻しないことを保証する。
        let mut book = AddressBook::default();
        let mut entry = make_entry("mito-mac-4", "mito-mac-4.local", 32000);
        entry.project_ports.insert("creo-ui".to_string(), 33005);
        entry
            .project_ports
            .insert("vantage-point".to_string(), 33002);
        book.upsert(entry);

        let raw = toml::to_string_pretty(&book).unwrap();
        let parsed: AddressBook = toml::from_str(&raw).unwrap();
        assert_eq!(parsed.worlds, book.worlds);
        assert_eq!(parsed.worlds[0].project_ports.get("creo-ui"), Some(&33005));
        assert_eq!(
            parsed.worlds[0].project_ports.get("vantage-point"),
            Some(&33002)
        );
    }

    #[test]
    fn address_book_loads_old_schema_without_project_ports() {
        // VP-154 PR-3 (Moody Blues Issue #2): 旧 schema (= v3.1 / VP-148/149) で書かれた
        // `addresses.toml` を新 binary で load する際、 `project_ports` field 不在でも
        // `#[serde(default)]` で空 HashMap として deserialize される forward-compat 確認。
        // migration なしで旧 entry が動き続ける invariant の test 化。
        let raw = r#"
[[world]]
alias = "old-entry"
hostname = "old-entry.local"
port = 32000
pubkey = "pending"
discovered_via = "mDNS"
last_seen = "2026-05-09T00:00:00Z"
"#;
        let book: AddressBook = toml::from_str(raw).expect("旧 schema parse 成功");
        assert_eq!(book.worlds.len(), 1);
        assert_eq!(book.worlds[0].alias, "old-entry");
        assert!(
            book.worlds[0].project_ports.is_empty(),
            "旧 entry は project_ports 不在 → 空 HashMap で load"
        );
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
    fn auto_upsert_from_discovered_sp_kind_creates_stub() {
        // VP-154 PR-3: PR-1 では kind=sp 完全 filter だったが、 PR-3 で project_ports に
        // 反映する path を活性化。 world entry 不在時は stub を作る (= mDNS event 順序保証なし
        // への resilience)。
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
        assert_eq!(
            book.worlds.len(),
            1,
            "stub entry を作る (= world advertise 先着保証なし)"
        );
        assert_eq!(book.worlds[0].alias, "mito-mac-4");
        assert_eq!(book.worlds[0].port, 0, "stub world port=0");
        assert_eq!(book.worlds[0].project_ports.get("creo-ui"), Some(&33005));
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
        book.upsert(make_entry("macbook-a", "macbook-a.local", 32000));
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
        book.upsert(make_entry("macbook-a", "macbook-a.local", 32000));

        assert_eq!(
            book.find_by_host("macbook-a.local")
                .map(|e| e.alias.as_str()),
            Some("macbook-a")
        );
        assert_eq!(
            book.find_by_host("macbook-a.local.")
                .map(|e| e.alias.as_str()),
            Some("macbook-a")
        );
        assert!(book.find_by_host("macbook-b.local").is_none());
    }

    // =========================================================================
    // VP-154 PR-3: project_ports + record_sp_port + find_sp_port
    // =========================================================================

    #[test]
    fn record_sp_port_updates_existing_entry() {
        // 既存 world entry がある状態で SP discover → project_ports に upsert
        let mut book = AddressBook::default();
        book.upsert(make_entry("mito-mac-4", "mito-mac-4.local", 32000));
        book.record_sp_port("mito-mac-4.local", "creo-ui", 33005);
        assert_eq!(book.worlds.len(), 1, "stub 作らず既存 entry を update");
        assert_eq!(book.worlds[0].project_ports.get("creo-ui"), Some(&33005));
    }

    #[test]
    fn record_sp_port_creates_stub_when_world_absent() {
        // SP advertise が world advertise より先着するケース → stub entry 作成 (= PR-3 設計)
        let mut book = AddressBook::default();
        book.record_sp_port("mito-mac-4.local", "creo-ui", 33005);
        assert_eq!(book.worlds.len(), 1);
        let stub = &book.worlds[0];
        assert_eq!(stub.alias, "mito-mac-4"); // hostname 先頭 segment derive
        assert_eq!(stub.port, 0); // stub world port
        assert_eq!(stub.pubkey, "pending");
        assert_eq!(stub.project_ports.get("creo-ui"), Some(&33005));
    }

    #[test]
    fn record_sp_port_normalizes_trailing_dot() {
        // mDNS 由来 hostname (例: `mito-mac-4.local.`) と AddressBook entry (= 末尾 `.` trim 済) の
        // 表記揺れを吸収して match
        let mut book = AddressBook::default();
        book.upsert(make_entry("mito-mac-4", "mito-mac-4.local", 32000));
        book.record_sp_port("mito-mac-4.local.", "creo-ui", 33005);
        assert_eq!(book.worlds.len(), 1, "trailing dot 揺れで stub 作らず");
        assert_eq!(book.worlds[0].project_ports.get("creo-ui"), Some(&33005));
    }

    #[test]
    fn find_sp_port_returns_port_for_known_pair() {
        let mut book = AddressBook::default();
        book.upsert(make_entry("mito-mac-4", "mito-mac-4.local", 32000));
        book.record_sp_port("mito-mac-4.local", "creo-ui", 33005);
        book.record_sp_port("mito-mac-4.local", "vantage-point", 33002);

        assert_eq!(
            book.find_sp_port("mito-mac-4.local", "creo-ui"),
            Some(33005)
        );
        assert_eq!(
            book.find_sp_port("mito-mac-4.local", "vantage-point"),
            Some(33002)
        );
        // 不在 project は None
        assert_eq!(book.find_sp_port("mito-mac-4.local", "nonexistent"), None);
        // 不在 host は None
        assert_eq!(book.find_sp_port("other.local", "creo-ui"), None);
    }

    #[test]
    fn upsert_preserves_project_ports_on_world_re_advertise() {
        // SP discover で project_ports を埋めた後、 world advertise が再着 → port 情報を消さない
        let mut book = AddressBook::default();
        book.record_sp_port("mito-mac-4.local", "creo-ui", 33005);
        // ↑ stub entry が project_ports={creo-ui: 33005} で created

        // world entry が後から到着
        book.upsert(make_entry("mito-mac-4", "mito-mac-4.local", 32000));

        let entry = &book.worlds[0];
        assert_eq!(entry.port, 32000, "world port が埋まる");
        assert_eq!(
            entry.project_ports.get("creo-ui"),
            Some(&33005),
            "project_ports は merge で保持"
        );
    }

    #[test]
    fn auto_upsert_kind_sp_populates_project_ports() {
        // VP-154 PR-3: kind=sp instance を受信 → AddressBook の project_ports に反映
        // (PR-1 では filter で skip してた経路を PR-3 で活性化)
        let mut book = AddressBook::default();
        // world entry を予め用意
        book.upsert(make_entry("mito-mac-4", "mito-mac-4.local", 32000));

        let mut props = std::collections::HashMap::new();
        props.insert("kind".to_string(), "sp".to_string());
        props.insert("project".to_string(), "creo-ui".to_string());
        let sp = crate::lan_discovery::DiscoveredWorld {
            hostname: "mito-mac-4.local.".to_string(),
            instance_name: "sp-creo-ui-mito-mac-4._vp._tcp.local.".to_string(),
            port: 33005,
            pubkey: "pending".to_string(),
            version: "v3".to_string(),
            properties: props,
        };
        book.auto_upsert_from_discovered(&sp);

        assert_eq!(
            book.worlds.len(),
            1,
            "SP advertise で stub 作らず既存 entry update"
        );
        assert_eq!(book.worlds[0].project_ports.get("creo-ui"), Some(&33005));
    }

    #[test]
    fn auto_upsert_kind_sp_without_project_field_is_skipped() {
        // 不正な kind=sp advertise (= project field 不在) は skip
        let mut book = AddressBook::default();
        let mut props = std::collections::HashMap::new();
        props.insert("kind".to_string(), "sp".to_string());
        // project 不在
        let sp = crate::lan_discovery::DiscoveredWorld {
            hostname: "mito-mac-4.local.".to_string(),
            instance_name: "sp-malformed._vp._tcp.local.".to_string(),
            port: 33005,
            pubkey: "pending".to_string(),
            version: "v3".to_string(),
            properties: props,
        };
        book.auto_upsert_from_discovered(&sp);
        assert!(
            book.worlds.is_empty(),
            "malformed advertise は entry 作らず"
        );
    }

    #[test]
    fn auto_upsert_unknown_kind_is_skipped() {
        // forward compat: 未知 kind は debug log + skip
        let mut book = AddressBook::default();
        let mut props = std::collections::HashMap::new();
        props.insert("kind".to_string(), "future-kind".to_string());
        let unknown = crate::lan_discovery::DiscoveredWorld {
            hostname: "host.local.".to_string(),
            instance_name: "future-host._vp._tcp.local.".to_string(),
            port: 12345,
            pubkey: "pending".to_string(),
            version: "v3".to_string(),
            properties: props,
        };
        book.auto_upsert_from_discovered(&unknown);
        assert!(book.worlds.is_empty(), "未知 kind は forward compat skip");
    }

    // =========================================================================
    // VP-154 chore: parse_duration + prune logic
    // =========================================================================

    #[test]
    fn parse_duration_supports_smhd_units() {
        assert_eq!(
            parse_duration("30s").unwrap(),
            chrono::Duration::seconds(30)
        );
        assert_eq!(parse_duration("5m").unwrap(), chrono::Duration::minutes(5));
        assert_eq!(parse_duration("2h").unwrap(), chrono::Duration::hours(2));
        assert_eq!(parse_duration("7d").unwrap(), chrono::Duration::days(7));
    }

    #[test]
    fn parse_duration_rejects_invalid_format() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("h").is_err()); // 数値部不在
        assert!(parse_duration("1y").is_err()); // 未対応 unit
        assert!(parse_duration("abc").is_err()); // 数値 parse 失敗
    }

    #[test]
    fn parse_duration_handles_whitespace() {
        // 前後 whitespace は trim される (= user typo 救済)
        assert_eq!(
            parse_duration("  1h  ").unwrap(),
            chrono::Duration::hours(1)
        );
    }
}
