//! Bonjour Capability - mDNS/DNS-SD Service Advertisement
//!
//! Standをローカルネットワーク上で発見可能にするCapability。
//! `_vantage-point._tcp` サービスとして広告し、macOSアプリやその他の
//! Bonjour対応クライアントから自動発見を可能にする。
//!
//! ## 使用例
//!
//! ```ignore
//! let mut bonjour = BonjourCapability::new(33000, "my-project");
//! bonjour.initialize(&ctx).await?;
//! // Stand終了時
//! bonjour.shutdown().await?;
//! ```

use crate::capability::core::{Capability, CapabilityContext, CapabilityError, CapabilityResult};
use crate::capability::eventbus::EventBus;
use crate::capability::{CapabilityEvent, CapabilityInfo, CapabilityState};
use async_trait::async_trait;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::any::Any;
use std::sync::Arc;

/// Bonjour service type for Vantage Point
const SERVICE_TYPE: &str = "_vantage-point._tcp.local.";

/// Bonjour Capability for mDNS service advertisement
pub struct BonjourCapability {
    /// Current state
    state: CapabilityState,
    /// mDNS service daemon
    daemon: Option<ServiceDaemon>,
    /// Port number to advertise
    port: u16,
    /// Project name for TXT record
    project_name: String,
    /// Instance name (hostname)
    instance_name: String,
    /// Event bus for emitting events
    event_bus: Option<Arc<EventBus>>,
}

impl BonjourCapability {
    /// Create a new BonjourCapability
    pub fn new(port: u16, project_name: impl Into<String>) -> Self {
        let instance_name = hostname::get()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "vantage-point".to_string());

        Self {
            state: CapabilityState::Uninitialized,
            daemon: None,
            port,
            project_name: project_name.into(),
            instance_name,
            event_bus: None,
        }
    }

    /// Set the event bus for emitting events
    pub fn set_event_bus(&mut self, bus: Arc<EventBus>) {
        self.event_bus = Some(bus);
    }

    /// Emit an event if event bus is available
    fn emit_event(&self, event: CapabilityEvent) {
        if let Some(ref bus) = self.event_bus {
            let bus = bus.clone();
            tokio::spawn(async move {
                bus.emit(event).await;
            });
        }
    }

    /// Start advertising the service
    fn start_advertising(&mut self) -> CapabilityResult<()> {
        // Create service daemon
        let daemon = ServiceDaemon::new().map_err(|e| {
            CapabilityError::InitializationFailed(format!("Failed to create mDNS daemon: {}", e))
        })?;

        // Get local hostname for the service
        let host_name = format!("{}.local.", self.instance_name);

        // Create service info with TXT records
        let properties = [
            ("project", self.project_name.as_str()),
            ("version", env!("CARGO_PKG_VERSION")),
        ];

        let service_info = ServiceInfo::new(
            SERVICE_TYPE,
            &self.instance_name,
            &host_name,
            (), // Will be resolved automatically
            self.port,
            &properties[..],
        )
        .map_err(|e| {
            CapabilityError::InitializationFailed(format!("Failed to create service info: {}", e))
        })?;

        // Register the service
        daemon.register(service_info).map_err(|e| {
            CapabilityError::InitializationFailed(format!("Failed to register service: {}", e))
        })?;

        self.daemon = Some(daemon);

        tracing::info!(
            port = self.port,
            project = %self.project_name,
            instance = %self.instance_name,
            "Bonjour service advertised: {}",
            SERVICE_TYPE
        );

        self.emit_event(
            CapabilityEvent::new("bonjour.advertised", "bonjour-capability").with_payload(
                &serde_json::json!({
                    "service_type": SERVICE_TYPE,
                    "port": self.port,
                    "project": self.project_name,
                    "instance": self.instance_name,
                }),
            ),
        );

        Ok(())
    }

    /// Stop advertising the service
    fn stop_advertising(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            // ServiceDaemon will unregister services on drop
            drop(daemon);
            tracing::info!("Bonjour service unregistered");

            self.emit_event(
                CapabilityEvent::new("bonjour.unregistered", "bonjour-capability").with_payload(
                    &serde_json::json!({
                        "port": self.port,
                    }),
                ),
            );
        }
    }
}

#[async_trait]
impl Capability for BonjourCapability {
    fn info(&self) -> CapabilityInfo {
        CapabilityInfo::new(
            "bonjour-capability",
            env!("CARGO_PKG_VERSION"),
            "mDNS/DNS-SD service advertisement for LAN discovery",
        )
    }

    fn state(&self) -> CapabilityState {
        self.state
    }

    async fn initialize(&mut self, _ctx: &CapabilityContext) -> CapabilityResult<()> {
        if self.state != CapabilityState::Uninitialized {
            return Err(CapabilityError::AlreadyInitialized);
        }

        self.state = CapabilityState::Initializing;

        match self.start_advertising() {
            Ok(()) => {
                self.state = CapabilityState::Idle;
                Ok(())
            }
            Err(e) => {
                self.state = CapabilityState::Error;
                Err(e)
            }
        }
    }

    async fn shutdown(&mut self) -> CapabilityResult<()> {
        self.stop_advertising();
        self.state = CapabilityState::Stopped;
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl Drop for BonjourCapability {
    fn drop(&mut self) {
        self.stop_advertising();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bonjour_capability_new() {
        let cap = BonjourCapability::new(33000, "test-project");
        assert_eq!(cap.port, 33000);
        assert_eq!(cap.project_name, "test-project");
        assert_eq!(cap.state(), CapabilityState::Uninitialized);
    }

    #[test]
    fn test_service_type() {
        assert_eq!(SERVICE_TYPE, "_vantage-point._tcp.local.");
    }
}
