//! usage の集約（machine scope）— いつ問い合わせるかの方針、値の重ね方、配信。
//!
//! - calculations: [`due_engines`]（どの engine をいつ probe するか）/ [`merge_poll`] /
//!   [`merge_partial`] / [`drop_expired`]（純関数、単体 test 対象）
//! - actions: [`UsageService`]（probe の実行・`watch` での配信・state zone への保存）と
//!   [`UsageService::run`]（daemon 起動時に 1 本 spawn する周期 loop）
//!
//! ## 問い合わせの方針（spec の副作用ガード）
//!
//! | engine | 起動時 | 定期（GUI が見ている間） | 手動 Refresh |
//! |---|---|---|---|
//! | claude | file を読む | 60 秒ごとに file を読む（mtime が同じなら読まない） | file を読む |
//! | codex | spawn | 15 分ごと | 30 秒以内の連打は無視 |
//! | agy | spawn | 10 分ごと | 30 秒以内の連打は無視 |
//! | grok | **しない** | **しない** | 60 秒以内の連打は無視 |
//!
//! - **claude は一度も spawn しない**（[`super::probe::CliProbe`] は file を読むだけ）
//! - grok は MCP server まで立ち上がるので、人が押した時だけ。値は state zone に保存して
//!   daemon 再起動後も cache として出す
//! - 定期の spawn は **購読者（vp-app）がいる間だけ**。誰も見ていないのに 15 分ごとに
//!   codex を起こさない
//! - 受動の通知（Claude の `rate_limit_event` / Codex の `account/rateLimits/updated`）が来たら、
//!   その engine の定期 poll の時計を進める（通知で足りている間は spawn しない）

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use tokio::sync::watch;

use super::model::{EngineUsage, UsageEngine, UsageSnapshot, UsageSource, UsageWindow};
use super::probe::{InertProbe, UsageProbe};

/// 周期 loop の刻み。claude の file 読みの間隔でもある。
pub const TICK: Duration = Duration::from_secs(60);

/// 定期 poll の間隔に持たせる余裕（tick の揺れの吸収、[`due_engines`]）。
const PERIODIC_SLACK: Duration = Duration::from_secs(30);

/// 何がきっかけの refresh か。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// daemon の起動直後に 1 回。
    Startup,
    /// [`TICK`] ごと。`watched` = 購読者がいるか。
    Periodic { watched: bool },
    /// 人が Refresh を押した（MachineStrip / tray）。
    Manual,
}

/// engine ごとの間隔。
struct Cadence {
    startup: bool,
    /// None = 定期では問い合わせない。
    periodic: Option<Duration>,
    /// spawn を伴わない（file を読むだけ）か。購読者がいなくても定期で回してよい。
    cheap: bool,
    /// 手動の連打をまとめる間隔。
    manual_gap: Duration,
}

fn cadence(engine: UsageEngine) -> Cadence {
    match engine {
        UsageEngine::Claude => Cadence {
            startup: true,
            periodic: Some(TICK),
            cheap: true,
            manual_gap: Duration::ZERO,
        },
        UsageEngine::Codex => Cadence {
            startup: true,
            periodic: Some(Duration::from_secs(15 * 60)),
            cheap: false,
            manual_gap: Duration::from_secs(30),
        },
        UsageEngine::Agy => Cadence {
            startup: true,
            periodic: Some(Duration::from_secs(10 * 60)),
            cheap: false,
            manual_gap: Duration::from_secs(30),
        },
        UsageEngine::Grok => Cadence {
            startup: false,
            periodic: None,
            cheap: false,
            manual_gap: Duration::from_secs(60),
        },
    }
}

/// `trigger` の時に問い合わせる engine。`last` = engine ごとの最後に問い合わせた（または受動で
/// 値を得た）時刻 epoch ms。
pub fn due_engines(
    trigger: Trigger,
    now_ms: u64,
    last: &HashMap<UsageEngine, u64>,
) -> Vec<UsageEngine> {
    UsageEngine::ALL
        .into_iter()
        .filter(|engine| {
            let c = cadence(*engine);
            let since = last.get(engine).map(|t| now_ms.saturating_sub(*t));
            let elapsed = |gap: Duration| since.is_none_or(|s| s >= gap.as_millis() as u64);
            match trigger {
                Trigger::Startup => c.startup,
                // 周期 loop の tick は数 ms 揺れるので、間隔に半 tick の余裕を持たせる
                // （ちょうど 1 ms 足りないだけで次の tick = 60 秒後まで遅れないように）。
                Trigger::Periodic { watched } => {
                    (watched || c.cheap)
                        && c.periodic
                            .is_some_and(|p| elapsed(p.saturating_sub(PERIODIC_SLACK)))
                }
                Trigger::Manual => elapsed(c.manual_gap),
            }
        })
        .collect()
}

/// 能動の結果（全量）を今の値に重ねる。
///
/// - 失敗（窓なし + error）: **前回の窓を残して** error だけ付ける（古い値でも無いよりよい）。
///   plan / tier は取れていれば更新する（Claude は cache が無くても plan は読める）
/// - 取得時刻が今の値より古い（Claude の file cache が、受動 event で得た値より古い）: 今の値を保つ
/// - それ以外: 置き換える。取得時刻が無ければ `now_ms`
///
/// どの場合も、incoming のうち**リセット時刻を過ぎた窓は先に落とす**。CC が cache を書き直すまで
/// `~/.claude.json` は同じ値を返し続けるので、落とさないと expire → 再読で復活 → expire … と
/// 毎 tick チラつき、古い % が出続ける。
pub fn merge_poll(current: &EngineUsage, incoming: EngineUsage, now_ms: u64) -> EngineUsage {
    let incoming = drop_expired(&incoming, now_ms);
    if incoming.windows.is_empty() && incoming.error.is_some() {
        return EngineUsage {
            plan: incoming.plan.or_else(|| current.plan.clone()),
            tier: incoming.tier.or_else(|| current.tier.clone()),
            error: incoming.error,
            ..current.clone()
        };
    }
    if let (Some(new_t), Some(old_t)) = (incoming.fetched_at_ms, current.fetched_at_ms)
        && new_t < old_t
    {
        return current.clone();
    }
    EngineUsage {
        fetched_at_ms: incoming.fetched_at_ms.or(Some(now_ms)),
        ..incoming
    }
}

/// 受動の差分（窓の一部 / plan）を今の値に重ねる。窓は id で置き換え、無ければ足す。
pub fn merge_partial(
    current: &EngineUsage,
    windows: Vec<UsageWindow>,
    plan: Option<String>,
    now_ms: u64,
) -> EngineUsage {
    let mut next = current.clone();
    for window in windows {
        match next.windows.iter_mut().find(|w| w.id == window.id) {
            Some(slot) => *slot = window,
            None => next.windows.push(window),
        }
    }
    if plan.is_some() {
        next.plan = plan;
    }
    next.source = UsageSource::Event;
    next.fetched_at_ms = Some(now_ms);
    next.error = None;
    next
}

/// リセット時刻を過ぎた窓を落とす。過ぎた後の使用率は分からない（その値はもう事実でない）ので、
/// 次に取れるまで「—」に倒す。
pub fn drop_expired(usage: &EngineUsage, now_ms: u64) -> EngineUsage {
    let mut next = usage.clone();
    next.windows
        .retain(|w| w.resets_at_ms.is_none_or(|t| t > now_ms));
    next
}

/// 保存してあった snapshot を「cache 由来」として読み直す（error は持ち越さない）。
pub fn restore_cached(mut snapshot: UsageSnapshot, now_ms: u64) -> UsageSnapshot {
    let mut restored = UsageSnapshot::default();
    let mut seen = HashSet::new();
    for usage in snapshot.engines.drain(..) {
        // 未知の engine と重複は捨てる（file は人が触りうる — 最初の 1 件を正とする）
        let Some(engine) = UsageEngine::from_name(&usage.engine) else {
            continue;
        };
        if !seen.insert(engine) {
            continue;
        }
        let mut usage = drop_expired(&usage, now_ms);
        usage.error = None;
        if !usage.windows.is_empty() {
            usage.source = UsageSource::Cache;
        }
        restored.put(usage);
    }
    restored.updated_at_ms = snapshot.updated_at_ms;
    restored
}

pub fn now_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
}

/// usage の集約点。daemon に 1 つ（[`super::passive::install`] で受動の通知もここへ集まる）。
pub struct UsageService {
    probe: Arc<dyn UsageProbe>,
    tx: watch::Sender<UsageSnapshot>,
    last_probe: Mutex<HashMap<UsageEngine, u64>>,
    in_flight: Mutex<HashSet<UsageEngine>>,
    /// 保存先（state zone）。None = 保存しない（test）。
    persist_path: Option<PathBuf>,
    /// 保存の直列化。受動通知（chat の stdout ポンプ）と poll は別 thread から同時に来るので、
    /// lock の中で**その時点の最新**を書く（古い snapshot が後から書かれて勝つことを防ぐ）。
    persist_lock: Mutex<()>,
}

impl UsageService {
    /// `persist_path` に前回の snapshot があれば、cache として読み直して始める。
    pub fn new(probe: Arc<dyn UsageProbe>, persist_path: Option<PathBuf>) -> Self {
        let initial = persist_path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice::<UsageSnapshot>(&bytes).ok())
            .map(|s| restore_cached(s, now_ms()))
            .unwrap_or_default();
        Self {
            probe,
            tx: watch::channel(initial).0,
            last_probe: Mutex::new(HashMap::new()),
            in_flight: Mutex::new(HashSet::new()),
            persist_path,
            persist_lock: Mutex::new(()),
        }
    }

    /// 何も起動しない service（test 用の `DaemonState`）。
    pub fn inert() -> Self {
        Self::new(Arc::new(InertProbe), None)
    }

    /// 保存先の既定（`~/.local/state/vp/usage-snapshot.json`、dev は `vp-dev`）。
    pub fn default_persist_path() -> PathBuf {
        crate::config::vp_state_dir().join("usage-snapshot.json")
    }

    pub fn subscribe(&self) -> watch::Receiver<UsageSnapshot> {
        self.tx.subscribe()
    }

    pub fn snapshot(&self) -> UsageSnapshot {
        self.tx.borrow().clone()
    }

    /// 購読者（vp-app）がいるか。
    pub fn watched(&self) -> bool {
        self.tx.receiver_count() > 0
    }

    /// `trigger` に従って問い合わせる。終わった engine から順に反映して配る。
    pub async fn refresh(&self, trigger: Trigger) {
        let now = now_ms();
        let due: Vec<UsageEngine> = {
            let mut last = self.last_probe.lock().expect("last_probe");
            let mut in_flight = self.in_flight.lock().expect("in_flight");
            let due: Vec<UsageEngine> = due_engines(trigger, now, &last)
                .into_iter()
                .filter(|e| in_flight.insert(*e))
                .collect();
            for engine in &due {
                last.insert(*engine, now);
            }
            due
        };
        if due.is_empty() {
            return;
        }
        tracing::debug!(?trigger, ?due, "usage: probe");
        let mut running: FuturesUnordered<_> = due
            .into_iter()
            .map(|engine| {
                let probe = self.probe.clone();
                let guard = InFlight {
                    service: self,
                    engine,
                };
                async move {
                    let usage = probe.probe(engine).await;
                    (guard, usage)
                }
            })
            .collect();
        while let Some((guard, usage)) = running.next().await {
            if let Some(error) = &usage.error {
                tracing::debug!(engine = guard.engine.name(), %error, "usage: probe 失敗");
            }
            self.apply_poll(usage);
            drop(guard);
        }
    }

    /// 能動の結果を反映する。
    pub fn apply_poll(&self, usage: EngineUsage) {
        let Some(engine) = UsageEngine::from_name(&usage.engine) else {
            return;
        };
        let now = now_ms();
        self.update(engine, |current| merge_poll(current, usage, now));
    }

    /// 受動の差分を反映する（その engine の定期 poll の時計も進める）。
    pub fn apply_partial(
        &self,
        engine: UsageEngine,
        windows: Vec<UsageWindow>,
        plan: Option<String>,
    ) {
        let now = now_ms();
        self.last_probe
            .lock()
            .expect("last_probe")
            .insert(engine, now);
        self.update(engine, |current| merge_partial(current, windows, plan, now));
    }

    /// リセット時刻を過ぎた窓を落とす（周期 loop が毎 tick 呼ぶ）。
    pub fn expire(&self) {
        let now = now_ms();
        for engine in UsageEngine::ALL {
            self.update(engine, |current| drop_expired(current, now));
        }
    }

    fn update(&self, engine: UsageEngine, f: impl FnOnce(&EngineUsage) -> EngineUsage) {
        let changed = self.tx.send_if_modified(|snapshot| {
            let current = snapshot
                .engine(engine)
                .cloned()
                .unwrap_or_else(|| EngineUsage::empty(engine));
            let next = f(&current);
            if next == current {
                return false;
            }
            snapshot.put(next);
            snapshot.updated_at_ms = now_ms();
            true
        });
        if let (true, Some(path)) = (changed, self.persist_path.as_ref()) {
            let _guard = self.persist_lock.lock().expect("persist_lock");
            let latest = self.tx.borrow().clone();
            persist(path, &latest);
        }
    }

    /// daemon 起動時に 1 本 spawn する周期 loop。`shutdown` で畳む。
    pub async fn run(self: Arc<Self>, shutdown: tokio_util::sync::CancellationToken) {
        // 起動時の probe（codex / agy は最大 25 秒）の途中でも shutdown を待たせない。
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = self.refresh(Trigger::Startup) => {}
        }
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await; // interval の初回は即時なので捨てる（Startup で済んでいる）
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tick.tick() => {
                    self.expire();
                    // probe（codex は最大 25 秒）の途中でも shutdown を待たせない。捨てた probe の
                    // 子は kill_on_drop / process group の kill で道連れになる。
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = self.refresh(Trigger::Periodic { watched: self.watched() }) => {}
                    }
                }
            }
        }
    }
}

/// probe 中の印。future が捨てられても（daemon 停止など）drop で必ず外れる。
struct InFlight<'a> {
    service: &'a UsageService,
    engine: UsageEngine,
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        if let Ok(mut set) = self.service.in_flight.lock() {
            set.remove(&self.engine);
        }
    }
}

/// tmp に書いて rename（読み手が書きかけを掴まない）。失敗は warn だけ（表示には響かない）。
fn persist(path: &std::path::Path, snapshot: &UsageSnapshot) {
    let result = (|| -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(snapshot)?)?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        tracing::warn!("usage snapshot の保存に失敗（{}）: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::model::Severity;
    use async_trait::async_trait;

    fn usage_with(engine: UsageEngine, pct: f64, fetched: Option<u64>) -> EngineUsage {
        EngineUsage {
            windows: vec![UsageWindow::new("weekly", "weekly", pct, Some(9_000))],
            fetched_at_ms: fetched,
            source: UsageSource::Poll,
            ..EngineUsage::empty(engine)
        }
    }

    fn last(entries: &[(UsageEngine, u64)]) -> HashMap<UsageEngine, u64> {
        entries.iter().copied().collect()
    }

    const MIN: u64 = 60_000;

    #[test]
    fn startup_probes_everything_but_grok() {
        let due = due_engines(Trigger::Startup, 0, &HashMap::new());
        assert_eq!(
            due,
            vec![UsageEngine::Claude, UsageEngine::Codex, UsageEngine::Agy],
            "grok は MCP server まで立つので起動時に spawn しない"
        );
    }

    #[test]
    fn periodic_never_spawns_grok_and_spawns_nothing_when_unwatched() {
        let never = HashMap::new();
        let watched = due_engines(Trigger::Periodic { watched: true }, 0, &never);
        assert!(!watched.contains(&UsageEngine::Grok));
        assert_eq!(
            due_engines(Trigger::Periodic { watched: false }, 0, &never),
            vec![UsageEngine::Claude],
            "誰も見ていない間は file を読む claude だけ（spawn しない）"
        );
    }

    #[test]
    fn periodic_respects_per_engine_intervals() {
        let now = 100 * MIN;
        let recent = last(&[
            (UsageEngine::Claude, now - MIN),
            (UsageEngine::Codex, now - 14 * MIN),
            (UsageEngine::Agy, now - 10 * MIN),
        ]);
        assert_eq!(
            due_engines(Trigger::Periodic { watched: true }, now, &recent),
            vec![UsageEngine::Claude, UsageEngine::Agy],
            "codex は 15 分、agy は 10 分、claude は 1 分"
        );
    }

    #[test]
    fn manual_refresh_debounces_spawning_engines() {
        let now = 100 * MIN;
        let just_now = last(&[
            (UsageEngine::Claude, now - 1_000),
            (UsageEngine::Codex, now - 10_000),
            (UsageEngine::Agy, now - 31_000),
            (UsageEngine::Grok, now - 59_000),
        ]);
        assert_eq!(
            due_engines(Trigger::Manual, now, &just_now),
            vec![UsageEngine::Claude, UsageEngine::Agy],
            "連打は codex 30 秒 / grok 60 秒の間まとめる（claude は file なので毎回）"
        );
        assert!(due_engines(Trigger::Manual, now, &HashMap::new()).contains(&UsageEngine::Grok));
    }

    #[test]
    fn failed_poll_keeps_previous_windows() {
        let current = usage_with(UsageEngine::Codex, 40.0, Some(1_000));
        let failed = EngineUsage::failed(UsageEngine::Codex, "codex: 時間内に応答がありません");
        let merged = merge_poll(&current, failed, 5_000);
        assert_eq!(merged.windows, current.windows, "古い値でも残す");
        assert_eq!(merged.fetched_at_ms, Some(1_000), "取得時刻は前回のまま");
        assert_eq!(
            merged.error.as_deref(),
            Some("codex: 時間内に応答がありません")
        );
    }

    #[test]
    fn older_cache_does_not_overwrite_newer_event() {
        let from_event = usage_with(UsageEngine::Claude, 91.0, Some(2_000));
        let stale_cache = usage_with(UsageEngine::Claude, 88.0, Some(1_000));
        assert_eq!(merge_poll(&from_event, stale_cache, 3_000), from_event);

        let fresh_cache = usage_with(UsageEngine::Claude, 93.0, Some(2_500));
        assert_eq!(
            merge_poll(&from_event, fresh_cache.clone(), 3_000),
            fresh_cache
        );
    }

    #[test]
    fn successful_poll_replaces_and_stamps_time() {
        let current = usage_with(UsageEngine::Agy, 10.0, Some(1_000));
        let mut incoming = usage_with(UsageEngine::Agy, 20.0, None);
        incoming.error = None;
        let merged = merge_poll(&current, incoming, 7_000);
        assert_eq!(merged.windows[0].used_pct, 20.0);
        assert_eq!(merged.fetched_at_ms, Some(7_000));
    }

    #[test]
    fn partial_update_merges_windows_by_id() {
        let mut current = usage_with(UsageEngine::Claude, 88.0, Some(1_000));
        current
            .windows
            .push(UsageWindow::new("model:fable", "weekly · Fable", 100.0, None).scoped("Fable"));
        current.error = Some("前回の失敗".into());
        let merged = merge_partial(
            &current,
            vec![
                UsageWindow::new("weekly", "weekly", 90.0, Some(9_000)),
                UsageWindow::new("5h", "5h", 12.0, Some(8_000)),
            ],
            None,
            5_000,
        );
        let ids: Vec<&str> = merged.windows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, vec!["weekly", "model:fable", "5h"]);
        assert_eq!(merged.windows[0].used_pct, 90.0);
        assert_eq!(merged.windows[0].severity, Severity::Warning);
        assert_eq!(merged.source, UsageSource::Event);
        assert_eq!(merged.fetched_at_ms, Some(5_000));
        assert_eq!(merged.error, None);
    }

    /// moody-blues #2: リセット時刻を過ぎた窓は、同じ cache を読み直しても戻らない
    /// （CC が cache を書き直すまで、expire → 再読で復活 → expire … とチラつかない）。
    #[test]
    fn expired_windows_in_a_poll_result_do_not_come_back() {
        let cache = usage_with(UsageEngine::Claude, 100.0, Some(1_000)); // resets_at 9_000
        let after_expire = drop_expired(&cache, 10_000);
        let merged = merge_poll(&after_expire, cache, 10_000);
        assert!(merged.windows.is_empty(), "{:?}", merged.windows);
        assert_eq!(merged, after_expire, "変化なし = 配らない・保存しない");
    }

    /// moody-blues #4: 周期 loop の tick は数 ms 揺れる。間隔ちょうどに 1 ms 足りないだけで
    /// 1 tick（60 秒）遅れないこと。
    #[test]
    fn periodic_tolerates_tick_jitter() {
        let now = 100 * MIN;
        let recent = last(&[
            (UsageEngine::Claude, now - MIN + 10),
            (UsageEngine::Codex, now - 15 * MIN + 10),
            (UsageEngine::Agy, now - 10 * MIN + 10),
        ]);
        assert_eq!(
            due_engines(Trigger::Periodic { watched: true }, now, &recent),
            vec![UsageEngine::Claude, UsageEngine::Codex, UsageEngine::Agy]
        );
    }

    /// moody-blues #5: 受動通知（chat の stdout ポンプ）と poll が同時に保存しても、file は
    /// 壊れず最後の値になる。⚠️ 競合の再現は確率的（lock を外しても毎回は赤くならない）—
    /// 守っているのは「lock の中でその時点の最新を書く」構造で、この test はその smoke。
    #[test]
    fn concurrent_saves_leave_the_latest_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage-snapshot.json");
        let service = Arc::new(UsageService::new(Arc::new(InertProbe), Some(path.clone())));
        let workers: Vec<_> = (0..8)
            .map(|n| {
                let service = service.clone();
                std::thread::spawn(move || {
                    for i in 0..40 {
                        service.apply_partial(
                            UsageEngine::Codex,
                            vec![UsageWindow::new(format!("w{n}"), "x", f64::from(i), None)],
                            None,
                        );
                    }
                })
            })
            .collect();
        for w in workers {
            w.join().unwrap();
        }
        let saved: UsageSnapshot =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).expect("壊れていない");
        assert_eq!(saved, service.snapshot(), "最後に書かれたのが最新");
    }

    #[test]
    fn expired_windows_are_dropped() {
        let mut usage = usage_with(UsageEngine::Grok, 12.0, Some(1_000));
        usage
            .windows
            .push(UsageWindow::new("no-reset", "x", 5.0, None));
        let after_reset = drop_expired(&usage, 9_000);
        let ids: Vec<&str> = after_reset.windows.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, vec!["no-reset"], "リセット時刻を過ぎた値は事実でない");
        assert_eq!(drop_expired(&usage, 8_999).windows.len(), 2);
    }

    #[test]
    fn restored_snapshot_is_marked_as_cache() {
        let mut saved = UsageSnapshot::default();
        let mut grok = usage_with(UsageEngine::Grok, 12.0, Some(1_000));
        grok.error = Some("古い失敗".into());
        saved.put(grok);
        saved.put(usage_with(UsageEngine::Codex, 50.0, Some(1_000)));
        saved.engines[1].windows[0].resets_at_ms = Some(10); // 期限切れ
        saved.engines.push(EngineUsage::empty(UsageEngine::Grok)); // 重複は最初の 1 件を正とする
        saved.engines.push(EngineUsage {
            engine: "someday-new".into(), // 未知の engine は捨てる
            ..usage_with(UsageEngine::Claude, 1.0, None)
        });
        let restored = restore_cached(saved, 100);
        assert_eq!(restored.engines.len(), 4);
        let grok = restored.engine(UsageEngine::Grok).unwrap();
        assert_eq!(grok.source, UsageSource::Cache);
        assert_eq!(grok.error, None);
        let codex = restored.engine(UsageEngine::Codex).unwrap();
        assert!(codex.windows.is_empty(), "期限切れの窓は戻さない");
    }

    /// 呼ばれた engine を数える偽 probe（本物の CLI は起動しない）。
    struct CountingProbe {
        calls: Mutex<Vec<UsageEngine>>,
    }

    #[async_trait]
    impl UsageProbe for CountingProbe {
        async fn probe(&self, engine: UsageEngine) -> EngineUsage {
            self.calls.lock().unwrap().push(engine);
            // リセット時刻なし（`usage_with` の 9_000 = 1970 年は実時刻では期限切れで落ちる）
            EngineUsage {
                windows: vec![UsageWindow::new("weekly", "weekly", 42.0, None)],
                ..EngineUsage::empty(engine)
            }
        }
    }

    #[tokio::test]
    async fn refresh_publishes_each_result_and_debounces_manual() {
        let probe = Arc::new(CountingProbe {
            calls: Mutex::new(Vec::new()),
        });
        let service = UsageService::new(probe.clone(), None);
        let mut rx = service.subscribe();
        service.refresh(Trigger::Manual).await;
        assert!(rx.has_changed().unwrap());
        let snapshot = rx.borrow_and_update().clone();
        assert!(snapshot.updated_at_ms > 0);
        for engine in UsageEngine::ALL {
            let usage = snapshot.engine(engine).unwrap();
            assert_eq!(usage.windows[0].used_pct, 42.0, "{engine:?}");
        }
        assert_eq!(probe.calls.lock().unwrap().len(), 4);

        // すぐ押し直しても spawn する engine は問い合わせない（claude の file 読みだけ）
        service.refresh(Trigger::Manual).await;
        assert_eq!(
            probe.calls.lock().unwrap()[4..],
            [UsageEngine::Claude],
            "連打で codex / agy / grok を起こさない"
        );
    }

    #[tokio::test]
    async fn passive_update_postpones_periodic_poll() {
        let probe = Arc::new(CountingProbe {
            calls: Mutex::new(Vec::new()),
        });
        let service = UsageService::new(probe.clone(), None);
        service.apply_partial(
            UsageEngine::Codex,
            vec![UsageWindow::new("weekly", "weekly", 96.0, None)],
            None,
        );
        let codex = service
            .snapshot()
            .engine(UsageEngine::Codex)
            .cloned()
            .unwrap();
        assert_eq!(codex.source, UsageSource::Event);
        service.refresh(Trigger::Periodic { watched: true }).await;
        assert!(
            !probe.calls.lock().unwrap().contains(&UsageEngine::Codex),
            "通知で足りている間は codex を spawn しない"
        );
    }

    #[tokio::test]
    async fn unchanged_values_do_not_notify() {
        let service = UsageService::inert();
        let mut rx = service.subscribe();
        service.expire();
        assert!(!rx.has_changed().unwrap(), "変化が無ければ配らない");
        // inert probe は失敗するが、窓が無いまま error が付くので 1 度は変わる
        service.refresh(Trigger::Manual).await;
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();
        service.apply_poll(EngineUsage::failed(
            UsageEngine::Grok,
            "usage の取得は無効です",
        ));
        assert!(!rx.has_changed().unwrap());
    }

    /// 返ってこない probe（codex が固まった時を写す）。
    struct HangingProbe;

    #[async_trait]
    impl UsageProbe for HangingProbe {
        async fn probe(&self, _engine: UsageEngine) -> EngineUsage {
            std::future::pending().await
        }
    }

    /// sticky-fingers: 起動時の refresh（codex / agy は最大 25 秒）の途中でも shutdown で畳める。
    #[tokio::test]
    async fn shutdown_does_not_wait_for_a_hanging_probe() {
        let service = Arc::new(UsageService::new(Arc::new(HangingProbe), None));
        let shutdown = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn(service.clone().run(shutdown.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("shutdown を待たせている")
            .unwrap();
        assert!(
            service.in_flight.lock().unwrap().is_empty(),
            "捨てた probe の印は外れる"
        );
    }

    #[tokio::test]
    async fn persisted_snapshot_survives_a_new_service() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage-snapshot.json");
        let probe = Arc::new(CountingProbe {
            calls: Mutex::new(Vec::new()),
        });
        {
            let service = UsageService::new(probe.clone(), Some(path.clone()));
            let mut grok = usage_with(UsageEngine::Grok, 12.0, None);
            grok.windows[0].resets_at_ms = Some(now_ms() + 3_600_000);
            service.apply_poll(grok);
        }
        let reborn = UsageService::new(probe, Some(path));
        let grok = reborn
            .snapshot()
            .engine(UsageEngine::Grok)
            .cloned()
            .unwrap();
        assert_eq!(grok.windows[0].used_pct, 12.0);
        assert_eq!(grok.source, UsageSource::Cache);
    }
}
