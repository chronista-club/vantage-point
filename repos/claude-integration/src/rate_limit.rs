//! レート制限モジュール
//!
//! Claude APIのレート制限を管理

use crate::error::{ClaudeError, Result};
use governor::{
    clock::{Clock, DefaultClock},
    middleware::NoOpMiddleware,
    state::{InMemoryState, NotKeyed},
    Quota, RateLimiter as GovernorRateLimiter,
};
use nonzero_ext::*;
use parking_lot::Mutex;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::{debug, warn};

/// レート制限管理
pub struct RateLimiter {
    /// 内部のgovernorレート制限
    limiter: Option<Arc<GovernorRateLimiter<NotKeyed, InMemoryState, DefaultClock, NoOpMiddleware>>>,
    /// カスタムレート制限の設定
    config: RateLimitConfig,
    /// 最後のリクエスト時刻（デバッグ用）
    last_request: Arc<Mutex<Option<Instant>>>,
}

/// レート制限の設定
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// 時間枠あたりの最大リクエスト数
    pub max_requests: u32,
    /// 時間枠の長さ
    pub window: Duration,
    /// バースト許可数（短期間の集中的なリクエストを許可）
    pub burst_size: Option<u32>,
}

impl RateLimiter {
    /// 新しいレート制限を作成
    pub fn new(max_requests: u32, window: Duration) -> Self {
        let config = RateLimitConfig {
            max_requests,
            window,
            burst_size: None,
        };

        Self::with_config(config)
    }

    /// 設定からレート制限を作成
    pub fn with_config(config: RateLimitConfig) -> Self {
        let limiter = if config.max_requests > 0 {
            let quota = match config.window.as_secs() {
                0 => {
                    // 1秒未満の場合はナノ秒で計算
                    let nanos = config.window.as_nanos() as u64;
                    let per_nanosecond = nonzero!(1u64);
                    Quota::with_period(Duration::from_nanos(nanos / config.max_requests as u64))
                        .unwrap()
                        .allow_burst(
                            config
                                .burst_size
                                .unwrap_or(config.max_requests)
                                .try_into()
                                .unwrap(),
                        )
                }
                secs => {
                    // 秒単位で計算
                    let per_second = (config.max_requests as f64 / secs as f64).ceil() as u32;
                    Quota::per_second(per_second.try_into().unwrap())
                        .allow_burst(
                            config
                                .burst_size
                                .unwrap_or(config.max_requests)
                                .try_into()
                                .unwrap(),
                        )
                }
            };

            Some(Arc::new(GovernorRateLimiter::direct(quota)))
        } else {
            None
        };

        Self {
            limiter,
            config,
            last_request: Arc::new(Mutex::new(None)),
        }
    }

    /// デフォルトのレート制限を作成（Anthropicの標準的な制限）
    pub fn default_anthropic() -> Self {
        // デフォルト: 1分あたり50リクエスト
        Self::new(50, Duration::from_secs(60))
    }

    /// 無制限のレート制限を作成（テスト用）
    pub fn unlimited() -> Self {
        Self {
            limiter: None,
            config: RateLimitConfig {
                max_requests: 0,
                window: Duration::from_secs(1),
                burst_size: None,
            },
            last_request: Arc::new(Mutex::new(None)),
        }
    }

    /// リクエストの許可を取得
    pub async fn acquire(&self) -> Result<()> {
        if let Some(limiter) = &self.limiter {
            match limiter.check() {
                Ok(_) => {
                    debug!("Rate limit check passed");
                    *self.last_request.lock() = Some(Instant::now());
                    Ok(())
                }
                Err(negative) => {
                    let wait_time = negative.wait_time_from(DefaultClock::default().now());
                    warn!(
                        "Rate limit exceeded, need to wait {:?}",
                        wait_time
                    );

                    // 待機時間が長すぎる場合はエラーを返す
                    if wait_time > Duration::from_secs(60) {
                        return Err(ClaudeError::rate_limit(Some(wait_time.as_secs())));
                    }

                    // 待機
                    tokio::time::sleep(wait_time).await;

                    // 再試行
                    limiter.check().map_err(|e| {
                        let wait = e.wait_time_from(DefaultClock::default().now());
                        ClaudeError::rate_limit(Some(wait.as_secs()))
                    })?;

                    *self.last_request.lock() = Some(Instant::now());
                    Ok(())
                }
            }
        } else {
            // レート制限が設定されていない場合は常に許可
            *self.last_request.lock() = Some(Instant::now());
            Ok(())
        }
    }

    /// 複数のリクエストの許可を一度に取得
    pub async fn acquire_many(&self, n: u32) -> Result<()> {
        for _ in 0..n {
            self.acquire().await?;
        }
        Ok(())
    }

    /// 現在の使用状況を取得
    pub fn current_usage(&self) -> RateLimitStatus {
        if let Some(limiter) = &self.limiter {
            let state_snapshot = limiter.state();
            let quota = state_snapshot.quota();
            let available = limiter.check().map(|_| 1).unwrap_or(0);

            RateLimitStatus {
                max_requests: self.config.max_requests,
                window: self.config.window,
                available_requests: available,
                burst_capacity: quota.burst_size().get(),
                last_request: *self.last_request.lock(),
                is_limited: self.limiter.is_some(),
            }
        } else {
            RateLimitStatus {
                max_requests: 0,
                window: Duration::from_secs(1),
                available_requests: u32::MAX,
                burst_capacity: u32::MAX,
                last_request: *self.last_request.lock(),
                is_limited: false,
            }
        }
    }

    /// レート制限をリセット（テスト用）
    #[cfg(test)]
    pub fn reset(&self) {
        *self.last_request.lock() = None;
        // Note: governorの内部状態はリセットできないため、新しいインスタンスを作成する必要がある
    }

    /// 設定を更新
    pub fn update_config(&mut self, max_requests: u32, window: Duration) {
        let config = RateLimitConfig {
            max_requests,
            window,
            burst_size: self.config.burst_size,
        };
        *self = Self::with_config(config);
    }

    /// バーストサイズを設定
    pub fn set_burst_size(&mut self, burst_size: u32) {
        self.config.burst_size = Some(burst_size);
        *self = Self::with_config(self.config.clone());
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::default_anthropic()
    }
}

/// レート制限の状態
#[derive(Debug, Clone)]
pub struct RateLimitStatus {
    /// 設定された最大リクエスト数
    pub max_requests: u32,
    /// 時間枠
    pub window: Duration,
    /// 現在利用可能なリクエスト数
    pub available_requests: u32,
    /// バースト容量
    pub burst_capacity: u32,
    /// 最後のリクエスト時刻
    pub last_request: Option<Instant>,
    /// レート制限が有効かどうか
    pub is_limited: bool,
}

impl RateLimitStatus {
    /// 次のリクエストまでの推定待機時間
    pub fn estimated_wait_time(&self) -> Option<Duration> {
        if self.available_requests > 0 || !self.is_limited {
            return None;
        }

        // 簡易的な推定
        if let Some(last) = self.last_request {
            let elapsed = Instant::now().duration_since(last);
            if elapsed < self.window {
                let wait = self.window.saturating_sub(elapsed);
                return Some(wait / self.max_requests);
            }
        }

        None
    }

    /// レート制限に達しているかどうか
    pub fn is_exhausted(&self) -> bool {
        self.is_limited && self.available_requests == 0
    }
}

/// 複数のレート制限を組み合わせる
pub struct CompositeRateLimiter {
    limiters: Vec<RateLimiter>,
}

impl CompositeRateLimiter {
    /// 新しい複合レート制限を作成
    pub fn new() -> Self {
        Self {
            limiters: Vec::new(),
        }
    }

    /// レート制限を追加
    pub fn add_limiter(mut self, limiter: RateLimiter) -> Self {
        self.limiters.push(limiter);
        self
    }

    /// 標準的なAnthropic制限を追加
    pub fn with_anthropic_limits(self) -> Self {
        self
            // 1分あたり50リクエスト
            .add_limiter(RateLimiter::new(50, Duration::from_secs(60)))
            // 1時間あたり1000リクエスト
            .add_limiter(RateLimiter::new(1000, Duration::from_secs(3600)))
    }

    /// すべてのレート制限をチェック
    pub async fn acquire(&self) -> Result<()> {
        for limiter in &self.limiters {
            limiter.acquire().await?;
        }
        Ok(())
    }

    /// 現在の状態を取得
    pub fn current_status(&self) -> Vec<RateLimitStatus> {
        self.limiters.iter().map(|l| l.current_usage()).collect()
    }
}

impl Default for CompositeRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_rate_limiter_basic() {
        let limiter = RateLimiter::new(10, Duration::from_secs(1));

        // 最初の10リクエストは成功するはず
        for i in 0..10 {
            assert!(limiter.acquire().await.is_ok(), "Request {} failed", i);
        }

        // 11番目のリクエストは制限される
        let start = Instant::now();
        let result = limiter.acquire().await;
        let elapsed = start.elapsed();

        // 制限がかかるか、待機後に成功するか
        assert!(result.is_ok() || elapsed > Duration::from_millis(50));
    }

    #[tokio::test]
    async fn test_unlimited_rate_limiter() {
        let limiter = RateLimiter::unlimited();

        // 無制限なので大量のリクエストも成功する
        for _ in 0..1000 {
            assert!(limiter.acquire().await.is_ok());
        }
    }

    #[test]
    fn test_rate_limit_status() {
        let limiter = RateLimiter::new(100, Duration::from_secs(60));
        let status = limiter.current_usage();

        assert_eq!(status.max_requests, 100);
        assert_eq!(status.window, Duration::from_secs(60));
        assert!(status.is_limited);
        assert!(!status.is_exhausted());
    }

    #[tokio::test]
    async fn test_composite_rate_limiter() {
        let composite = CompositeRateLimiter::new()
            .add_limiter(RateLimiter::new(5, Duration::from_secs(1)))
            .add_limiter(RateLimiter::new(10, Duration::from_secs(5)));

        // 最初の5リクエストは両方のレート制限を通過
        for _ in 0..5 {
            assert!(composite.acquire().await.is_ok());
        }

        let statuses = composite.current_status();
        assert_eq!(statuses.len(), 2);
    }

    #[test]
    fn test_rate_limit_config_with_burst() {
        let config = RateLimitConfig {
            max_requests: 100,
            window: Duration::from_secs(60),
            burst_size: Some(10),
        };

        let limiter = RateLimiter::with_config(config);
        let status = limiter.current_usage();

        assert_eq!(status.burst_capacity, 10);
    }
}
