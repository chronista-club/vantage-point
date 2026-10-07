//! Owned virtual ports only. No physical-name fallback when the service is absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnedPorts {
    pub device_id: String,
    pub lease: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}
#[derive(Default)]
pub(crate) struct MidiAccess {
    pub owned: Vec<OwnedPorts>,
}
impl MidiAccess {
    pub fn replace(&mut self, next: Vec<OwnedPorts>) -> Vec<String> {
        let released = self
            .owned
            .iter()
            .filter(|old| !next.contains(old))
            .map(|p| p.device_id.clone())
            .collect();
        self.owned = next;
        released
    }
    pub fn input(&self, name: &str) -> Option<String> {
        self.port(name, true)
    }
    pub fn output(&self, name: &str) -> Option<String> {
        self.port(name, false)
    }
    fn port(&self, name: &str, input: bool) -> Option<String> {
        self.owned
            .iter()
            .flat_map(|p| if input { &p.inputs } else { &p.outputs })
            .find(|port| {
                port.as_str() == name
                    || port
                        .split_once(" | ")
                        .is_some_and(|(_, physical)| physical == name)
            })
            .cloned()
    }
    pub fn roto_prefix(&self) -> Option<String> {
        let ports = self.owned.iter().find(|p| p.device_id == "roto")?;
        let prefix = format!("{}/", ports.inputs.first()?.rsplit_once('/')?.0);
        ports
            .outputs
            .iter()
            .any(|name| name.starts_with(&prefix))
            .then_some(prefix)
    }
    pub fn permits_input(&self, name: &str) -> bool {
        self.owned
            .iter()
            .any(|p| p.inputs.iter().any(|n| n == name))
    }
    pub fn permits_output(&self, name: &str) -> bool {
        self.owned
            .iter()
            .any(|p| p.outputs.iter().any(|n| n == name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ownership_is_exact_and_release_does_not_affect_other_devices() {
        let mut access = MidiAccess::default();
        assert_eq!(access.input("LPD8"), None);
        access.replace(vec![
            OwnedPorts {
                device_id: "lpd8".into(),
                lease: "a".into(),
                inputs: vec!["Midistage/vp/a/1 | LPD8".into()],
                outputs: vec!["Midistage/vp/a/2 | LPD8".into()],
            },
            OwnedPorts {
                device_id: "xtouch".into(),
                lease: "b".into(),
                inputs: vec!["Midistage/vp/b/3 | X-Touch INT".into()],
                outputs: vec![],
            },
        ]);
        assert_eq!(
            access.input("LPD8").as_deref(),
            Some("Midistage/vp/a/1 | LPD8")
        );
        assert_eq!(access.input("Midistage/ladyland/c/1 | LPD8"), None);
        assert_eq!(access.input("LPD"), None);
        let mut next = access.owned.clone();
        next.retain(|p| p.device_id != "lpd8");
        let released = access.replace(next);
        assert_eq!(released, vec!["lpd8"]);
        assert_eq!(access.input("LPD8"), None);
        assert!(access.input("X-Touch INT").is_some());
    }
    #[test]
    fn changed_generation_or_ports_requires_releasing_old_connections() {
        let old = OwnedPorts {
            device_id: "roto".into(),
            lease: "a".into(),
            inputs: vec!["Midistage/vp/a/1 | Roto".into()],
            outputs: vec!["Midistage/vp/a/2 | Roto".into()],
        };
        let mut access = MidiAccess::default();
        assert!(access.replace(vec![old.clone()]).is_empty());
        assert!(access.replace(vec![old.clone()]).is_empty());
        let mut new = old;
        new.lease = "b".into();
        assert_eq!(access.replace(vec![new]), vec!["roto"]);
        assert_eq!(access.roto_prefix().as_deref(), Some("Midistage/vp/a/"));
        access.replace(vec![]);
        assert_eq!(access.roto_prefix(), None);
    }
}
