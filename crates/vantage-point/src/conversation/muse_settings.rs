//! Muse Code の Model / Mode 意図。候補は ACP configOptions が所有する。
#[cfg(test)]
use ts_rs::TS;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct MuseSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub mode: Option<String>,
}

const MAX_VALUE_LEN: usize = 64;

impl MuseSettings {
    pub fn config_pairs(&self) -> Vec<(String, String)> {
        [("model", &self.model), ("mode", &self.mode)]
            .into_iter()
            .filter_map(|(id, value)| value.as_ref().map(|v| (id.to_string(), v.clone())))
            .collect()
    }

    pub fn with_value(&self, config_id: &str, value: &str) -> Option<Self> {
        let mut next = self.clone();
        let slot = match config_id {
            "model" => &mut next.model,
            "mode" => &mut next.mode,
            _ => return None,
        };
        *slot = Some(value.to_string());
        Some(next)
    }

    pub fn follow_engine(&self, options: &[super::acp_config::AcpConfigOption]) -> Self {
        let follow = |id: &str, value: &Option<String>| -> Option<String> {
            let value = value.as_ref()?;
            match options.iter().find(|o| o.id == id) {
                Some(o) if !o.current_value.is_empty() => Some(o.current_value.clone()),
                _ => Some(value.clone()),
            }
        };
        Self {
            model: follow("model", &self.model),
            mode: follow("mode", &self.mode),
        }
    }

    pub fn validate_shape(&self) -> Result<(), String> {
        for (id, value) in self.config_pairs() {
            let ok = !value.is_empty()
                && value.len() <= MAX_VALUE_LEN
                && value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            if !ok {
                return Err(format!("Muse の {id} の値が不正: {value:?}"));
            }
        }
        Ok(())
    }
}
