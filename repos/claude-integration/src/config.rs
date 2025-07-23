//! 設定モジュール
//!
//! Claude統合ライブラリの設定管理

use crate::{
    error::{ClaudeError, Result},
    model::Model,
};
use serde::{Deserialize, Serialize};
use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

/// Claude統合の設定
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeConfig {
    /// APIキー
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// ベースURL
    #[serde(default = "default_base_url")]
    pub base_url: String,

    /// APIバージョン
    #[serde(default = "default_api_version")]
    pub api_version: String,

    /// デフォルトモデル
    #[serde(default)]
    pub default_model: Model,

    /// 最大リトライ回数
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    /// タイムアウト
    #[serde(with = "duration_serde", default = "default_timeout")]
    pub timeout: Duration,

    /// レート制限 - リクエスト数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_requests: Option<u32>,

    /// レート制限 - 時間枠
    #[serde(
        with = "optional_duration_serde",
        skip_serializing_if = "Option::is_none"
    )]
    pub rate_limit_window: Option<Duration>,
}

impl ClaudeConfig {
    /// 新しい設定を作成
    pub fn new() -> Self {
        Self::default()
    }

    /// APIキーを設定
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// 環境変数から設定を読み込み
    pub fn from_env() -> Result<Self> {
        let mut config = Self::default();

        // APIキー
        if let Ok(api_key) = env::var("ANTHROPIC_API_KEY") {
            config.api_key = Some(api_key);
        } else if let Ok(api_key) = env::var("CLAUDE_API_KEY") {
            config.api_key = Some(api_key);
        }

        // ベースURL
        if let Ok(base_url) = env::var("ANTHROPIC_BASE_URL") {
            config.base_url = base_url;
        }

        // APIバージョン
        if let Ok(api_version) = env::var("ANTHROPIC_API_VERSION") {
            config.api_version = api_version;
        }

        // デフォルトモデル
        if let Ok(model_str) = env::var("CLAUDE_DEFAULT_MODEL") {
            if let Some(model) = Model::from_str(&model_str) {
                config.default_model = model;
            } else {
                return Err(ClaudeError::Configuration(format!(
                    "Invalid model in CLAUDE_DEFAULT_MODEL: {}",
                    model_str
                )));
            }
        }

        // 最大リトライ回数
        if let Ok(max_retries_str) = env::var("CLAUDE_MAX_RETRIES") {
            config.max_retries = max_retries_str.parse().map_err(|_| {
                ClaudeError::Configuration(format!(
                    "Invalid value for CLAUDE_MAX_RETRIES: {}",
                    max_retries_str
                ))
            })?;
        }

        // タイムアウト
        if let Ok(timeout_str) = env::var("CLAUDE_TIMEOUT_SECONDS") {
            let timeout_secs: u64 = timeout_str.parse().map_err(|_| {
                ClaudeError::Configuration(format!(
                    "Invalid value for CLAUDE_TIMEOUT_SECONDS: {}",
                    timeout_str
                ))
            })?;
            config.timeout = Duration::from_secs(timeout_secs);
        }

        // レート制限
        if let Ok(rate_limit_str) = env::var("CLAUDE_RATE_LIMIT_REQUESTS") {
            config.rate_limit_requests = Some(rate_limit_str.parse().map_err(|_| {
                ClaudeError::Configuration(format!(
                    "Invalid value for CLAUDE_RATE_LIMIT_REQUESTS: {}",
                    rate_limit_str
                ))
            })?);
        }

        if let Ok(window_str) = env::var("CLAUDE_RATE_LIMIT_WINDOW_SECONDS") {
            let window_secs: u64 = window_str.parse().map_err(|_| {
                ClaudeError::Configuration(format!(
                    "Invalid value for CLAUDE_RATE_LIMIT_WINDOW_SECONDS: {}",
                    window_str
                ))
            })?;
            config.rate_limit_window = Some(Duration::from_secs(window_secs));
        }

        Ok(config)
    }

    /// ファイルから設定を読み込み
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .map_err(|e| ClaudeError::FileRead(format!("Failed to read config file: {}", e)))?;

        let config = match path.extension().and_then(|ext| ext.to_str()) {
            Some("json") => serde_json::from_str(&content)
                .map_err(|e| ClaudeError::Deserialization(e.to_string()))?,
            Some("toml") => {
                toml::from_str(&content).map_err(|e| ClaudeError::Deserialization(e.to_string()))?
            }
            Some("yaml") | Some("yml") => serde_yaml::from_str(&content)
                .map_err(|e| ClaudeError::Deserialization(e.to_string()))?,
            _ => {
                return Err(ClaudeError::Configuration(
                    "Unsupported config file format. Use .json, .toml, .yaml, or .yml".to_string(),
                ))
            }
        };

        Ok(config)
    }

    /// デフォルトの設定ファイルパスから読み込み
    pub fn from_default_locations() -> Result<Self> {
        let possible_paths = [
            PathBuf::from(".claude.toml"),
            PathBuf::from("claude.toml"),
            PathBuf::from(".claude/config.toml"),
            dirs::config_dir()
                .map(|d| d.join("claude-integration/config.toml"))
                .unwrap_or_default(),
        ];

        for path in &possible_paths {
            if path.exists() {
                match Self::from_file(path) {
                    Ok(mut config) => {
                        // 環境変数で上書き
                        let env_config = Self::from_env()?;
                        config.merge(env_config);
                        return Ok(config);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to load config from {:?}: {}", path, e);
                    }
                }
            }
        }

        // 設定ファイルが見つからない場合は環境変数のみから読み込み
        Self::from_env()
    }

    /// 別の設定をマージ（上書き）
    pub fn merge(&mut self, other: Self) {
        if other.api_key.is_some() {
            self.api_key = other.api_key;
        }
        if other.base_url != default_base_url() {
            self.base_url = other.base_url;
        }
        if other.api_version != default_api_version() {
            self.api_version = other.api_version;
        }
        if other.default_model != Model::default() {
            self.default_model = other.default_model;
        }
        if other.max_retries != default_max_retries() {
            self.max_retries = other.max_retries;
        }
        if other.timeout != default_timeout() {
            self.timeout = other.timeout;
        }
        if other.rate_limit_requests.is_some() {
            self.rate_limit_requests = other.rate_limit_requests;
        }
        if other.rate_limit_window.is_some() {
            self.rate_limit_window = other.rate_limit_window;
        }
    }

    /// 設定を検証
    pub fn validate(&self) -> Result<()> {
        if self.api_key.is_none() {
            return Err(ClaudeError::Configuration(
                "API key is required. Set ANTHROPIC_API_KEY or CLAUDE_API_KEY environment variable"
                    .to_string(),
            ));
        }

        if self.max_retries > 10 {
            tracing::warn!(
                "Max retries is set to {}, which is quite high",
                self.max_retries
            );
        }

        if self.timeout.as_secs() < 5 {
            return Err(ClaudeError::Configuration(
                "Timeout must be at least 5 seconds".to_string(),
            ));
        }

        if let (Some(requests), Some(window)) = (self.rate_limit_requests, self.rate_limit_window) {
            if requests == 0 {
                return Err(ClaudeError::Configuration(
                    "Rate limit requests must be greater than 0".to_string(),
                ));
            }
            if window.as_secs() == 0 {
                return Err(ClaudeError::Configuration(
                    "Rate limit window must be greater than 0 seconds".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// 設定をファイルに保存
    pub fn save_to_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();

        // ディレクトリが存在しない場合は作成
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ClaudeError::Io(e))?;
        }

        let content = match path.extension().and_then(|ext| ext.to_str()) {
            Some("json") => serde_json::to_string_pretty(self)
                .map_err(|e| ClaudeError::Serialization(e.to_string()))?,
            Some("toml") => toml::to_string_pretty(self)
                .map_err(|e| ClaudeError::Serialization(e.to_string()))?,
            Some("yaml") | Some("yml") => serde_yaml::to_string(self)
                .map_err(|e| ClaudeError::Serialization(e.to_string()))?,
            _ => {
                return Err(ClaudeError::Configuration(
                    "Unsupported config file format. Use .json, .toml, .yaml, or .yml".to_string(),
                ))
            }
        };

        std::fs::write(path, content).map_err(|e| ClaudeError::Io(e))?;

        Ok(())
    }
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: default_base_url(),
            api_version: default_api_version(),
            default_model: Model::default(),
            max_retries: default_max_retries(),
            timeout: default_timeout(),
            rate_limit_requests: None,
            rate_limit_window: None,
        }
    }
}

// デフォルト値を返す関数

fn default_base_url() -> String {
    crate::DEFAULT_BASE_URL.to_string()
}

fn default_api_version() -> String {
    crate::DEFAULT_API_VERSION.to_string()
}

fn default_max_retries() -> u32 {
    crate::DEFAULT_MAX_RETRIES
}

fn default_timeout() -> Duration {
    Duration::from_secs(crate::DEFAULT_TIMEOUT_SECS)
}

// Duration用のシリアライゼーション/デシリアライゼーション

mod duration_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(duration.as_secs())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs = u64::deserialize(deserializer)?;
        Ok(Duration::from_secs(secs))
    }
}

mod optional_duration_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match duration {
            Some(d) => serializer.serialize_some(&d.as_secs()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt_secs = Option::<u64>::deserialize(deserializer)?;
        Ok(opt_secs.map(Duration::from_secs))
    }
}

// 外部crateが必要
use dirs;
use serde_yaml;
use toml;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ClaudeConfig::default();
        assert_eq!(config.base_url, crate::DEFAULT_BASE_URL);
        assert_eq!(config.api_version, crate::DEFAULT_API_VERSION);
        assert_eq!(config.default_model, Model::default());
        assert_eq!(config.max_retries, crate::DEFAULT_MAX_RETRIES);
        assert_eq!(
            config.timeout,
            Duration::from_secs(crate::DEFAULT_TIMEOUT_SECS)
        );
    }

    #[test]
    fn test_config_validation() {
        let mut config = ClaudeConfig::default();

        // APIキーなしでは検証失敗
        assert!(config.validate().is_err());

        config.api_key = Some("test-key".to_string());
        assert!(config.validate().is_ok());

        // タイムアウトが短すぎる
        config.timeout = Duration::from_secs(2);
        assert!(config.validate().is_err());

        config.timeout = Duration::from_secs(10);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_merge() {
        let mut config1 = ClaudeConfig::default();
        config1.api_key = Some("key1".to_string());
        config1.max_retries = 3;

        let mut config2 = ClaudeConfig::default();
        config2.api_key = Some("key2".to_string());
        config2.timeout = Duration::from_secs(60);

        config1.merge(config2);

        assert_eq!(config1.api_key, Some("key2".to_string()));
        assert_eq!(config1.max_retries, 3); // 元の値を保持
        assert_eq!(config1.timeout, Duration::from_secs(60)); // 新しい値で上書き
    }

    #[test]
    fn test_config_serialization() {
        let config = ClaudeConfig {
            api_key: Some("test-key".to_string()),
            base_url: "https://api.example.com".to_string(),
            api_version: "2024-01-01".to_string(),
            default_model: Model::Claude3Opus,
            max_retries: 5,
            timeout: Duration::from_secs(45),
            rate_limit_requests: Some(100),
            rate_limit_window: Some(Duration::from_secs(60)),
        };

        // JSON
        let json = serde_json::to_string(&config).unwrap();
        let config2: ClaudeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.api_key, config2.api_key);

        // TOML
        let toml = toml::to_string(&config).unwrap();
        let config3: ClaudeConfig = toml::from_str(&toml).unwrap();
        assert_eq!(config.api_key, config3.api_key);
    }
}
