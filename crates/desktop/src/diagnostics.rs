use crate::controller::View;
use vpn_splitter_core::Error;
pub fn export(view: &View, include_infrastructure: bool) -> Result<String, Error> {
    use serde_json::json;
    let mut report = json!({"version":1,"state":format!("{:?}",view.state),"vpn_connected":view.vpn_connected,"routing_active":view.state==vpn_splitter_core::session::SessionState::Active,"rule_count":view.config.destinations.len(),"helper_status":view.helper_status,"last_error":view.error.map(|e|format!("{e:?}")),"limitations":["IPv4 only; IPv6 may bypass destination routes.","Wildcard domains cannot be expanded completely.","Hostname routes refresh every five minutes while active."],"infrastructure_included":include_infrastructure});
    if include_infrastructure {
        report["configuration"] =
            serde_json::to_value(&view.config).map_err(|_| Error::Transport)?;
        report["profiles"] = serde_json::to_value(&view.profiles).map_err(|_| Error::Transport)?;
    }
    let text = serde_json::to_string_pretty(&report).map_err(|_| Error::Transport)?;
    if text.len() > vpn_splitter_core::protocol::MAX_MESSAGE_BYTES {
        return Err(Error::InvalidInput("Diagnostics exceeds 1 MiB".into()));
    }
    Ok(text)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_is_redacted_by_default_and_infrastructure_is_explicit() {
        let mut view = View::default();
        view.config.profile_id = Some("private-profile-marker".into());
        view.config.destinations =
            vpn_splitter_core::config::import_rules("198.51.100.7\ngit.office.example").unwrap();
        view.profiles.push(crate::platform::macos::Profile {
            id: vpn_splitter_core::session::ProfileId::try_from(
                "private-profile-marker".to_owned(),
            )
            .unwrap(),
            name: "private-vpn-name".into(),
            supported: true,
            split_enabled: true,
            connected: false,
            transitioning: false,
        });
        let redacted = export(&view, false).unwrap();
        for secret in [
            "private-profile-marker",
            "private-vpn-name",
            "198.51.100.7",
            "git.office.example",
        ] {
            assert!(!redacted.contains(secret));
        }
        assert!(redacted.contains("IPv4"));
        let explicit = export(&view, true).unwrap();
        assert!(explicit.contains("198.51.100.7"));
        assert!(explicit.contains("private-vpn-name"));
    }
}
