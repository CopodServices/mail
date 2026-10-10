use crate::address;
use mail_auth::dmarc::Policy;
use mail_auth::dmarc::verify::DmarcParameters;
use mail_auth::spf::verify::SpfParameters;
use mail_auth::{AuthenticatedMessage, DkimResult, DmarcResult, MessageAuthenticator};
use std::net::IpAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthAction {
    Accept,
    Quarantine,
    Reject,
}

#[derive(Debug, Clone)]
pub struct AuthOutcome {
    pub spf: Option<String>,
    pub dkim: Option<String>,
    pub dmarc: Option<String>,
    pub dmarc_policy: Option<String>,
    pub action: AuthAction,
}

impl AuthOutcome {
    /// Used when verification is disabled or the resolver is unavailable.
    pub fn skipped() -> Self {
        Self {
            spf: None,
            dkim: None,
            dmarc: None,
            dmarc_policy: None,
            action: AuthAction::Accept,
        }
    }

    /// Maps a DMARC result and published policy onto an SMTP action.
    pub fn action_for(policy: Policy, result: &DmarcResult) -> AuthAction {
        if matches!(result, DmarcResult::Fail(_)) {
            return match policy {
                Policy::Reject => AuthAction::Reject,
                Policy::Quarantine => AuthAction::Quarantine,
                _ => AuthAction::Accept,
            };
        }
        AuthAction::Accept
    }
}

/// Runs SPF, DKIM and DMARC against the raw message and returns the results
/// plus the action the SMTP server should take.
pub async fn evaluate(
    authenticator: &MessageAuthenticator,
    raw: &[u8],
    peer_ip: IpAddr,
    helo: &str,
    mail_from: &str,
    hostname: &str,
) -> AuthOutcome {
    let Some(message) = AuthenticatedMessage::parse(raw) else {
        return AuthOutcome::skipped();
    };

    let spf = authenticator
        .verify_spf(SpfParameters::verify_mail_from(
            peer_ip, helo, hostname, mail_from,
        ))
        .await;
    let spf_label = spf.result().to_string().to_ascii_lowercase();

    let dkim_output = authenticator.verify_dkim(&message).await;
    let dkim_label = if dkim_output.is_empty() {
        "none".to_string()
    } else if dkim_output
        .iter()
        .all(|output| matches!(output.result(), DkimResult::Pass))
    {
        "pass".to_string()
    } else {
        "fail".to_string()
    };

    let mail_from_domain = address::domain_or(mail_from, hostname);
    let dmarc = authenticator
        .verify_dmarc(DmarcParameters::new(
            &message,
            &dkim_output,
            &mail_from_domain,
            &spf,
        ))
        .await;

    let policy = dmarc.policy();
    let result = dmarc.result();
    let dmarc_label = format!("{result:?}").to_ascii_lowercase();
    let policy_label = format!("{policy:?}").to_ascii_lowercase();
    let action = AuthOutcome::action_for(policy, &result);

    AuthOutcome {
        spf: Some(spf_label),
        dkim: Some(dkim_label),
        dmarc: Some(dmarc_label),
        dmarc_policy: Some(policy_label),
        action,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail_auth::Error;

    fn failure() -> DmarcResult {
        DmarcResult::Fail(Error::NotAligned)
    }

    #[test]
    fn reject_policy_rejects_failures() {
        assert_eq!(
            AuthOutcome::action_for(Policy::Reject, &failure()),
            AuthAction::Reject
        );
        assert_eq!(
            AuthOutcome::action_for(Policy::Quarantine, &failure()),
            AuthAction::Quarantine
        );
        assert_eq!(
            AuthOutcome::action_for(Policy::None, &failure()),
            AuthAction::Accept
        );
    }

    #[test]
    fn passes_and_temp_errors_are_accepted() {
        assert_eq!(
            AuthOutcome::action_for(Policy::Reject, &DmarcResult::Pass),
            AuthAction::Accept
        );
        assert_eq!(
            AuthOutcome::action_for(Policy::Reject, &DmarcResult::TempError(Error::NotAligned)),
            AuthAction::Accept
        );
        assert_eq!(
            AuthOutcome::action_for(Policy::Reject, &DmarcResult::None),
            AuthAction::Accept
        );
    }
}
