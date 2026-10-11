//! The in-house spam engine: a pipeline of analyzers that tag a message, a
//! configurable tag -> score map, and thresholds that pick the action.
//!
//! The engine is additive and inert by default: with \`SPAM_MODE=off\` every scan
//! returns an empty verdict, and any Redis, DNS or scanner failure fails open
//! (the message is delivered) rather than quarantining.

pub mod checks;
pub mod classifier;
pub mod config;
pub mod dnsbl;
pub mod pyzor;
pub mod rules;
pub mod scanners;
pub mod state;

pub use classifier::{ClassifierSet, tokenize};
pub use config::{ScoreRule, SpamConfig, SpamMode};

use crate::policy::AuthOutcome;
use mail_auth::MessageAuthenticator;
use mail_auth::hickory_resolver::TokioResolver;
use mail_auth::hickory_resolver::config::{
    ConnectionConfig, NameServerConfig, ResolverConfig, ResolverOpts,
};
use mail_parser::{Message, MessageParser};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Tags collected during analysis, with an optional explicit score override.
pub type Tags = BTreeMap<String, Option<f32>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpamAction {
    Accept,
    /// Recorded but delivered (shadow mode).
    Log,
    Quarantine,
    Discard,
    Reject,
}

impl SpamAction {
    pub fn as_str(self) -> &'static str {
        match self {
            SpamAction::Accept => "accept",
            SpamAction::Log => "log",
            SpamAction::Quarantine => "quarantine",
            SpamAction::Discard => "discard",
            SpamAction::Reject => "reject",
        }
    }

    pub fn is_junk(self) -> bool {
        matches!(
            self,
            SpamAction::Quarantine | SpamAction::Discard | SpamAction::Reject
        )
    }
}

#[derive(Debug, Clone)]
pub struct Verdict {
    pub score: f32,
    pub tags: Vec<(String, f32)>,
    pub action: SpamAction,
    pub malware: bool,
}

impl Verdict {
    pub fn empty() -> Self {
        Self {
            score: 0.0,
            tags: Vec::new(),
            action: SpamAction::Accept,
            malware: false,
        }
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|(name, _)| name == tag)
    }

    pub fn is_junk(&self) -> bool {
        self.action.is_junk()
    }

    /// The \`X-Spam-*\` header block, when the message has anything to report.
    pub fn header_block(&self) -> String {
        if self.tags.is_empty() && self.score == 0.0 {
            return String::new();
        }
        let mut out = String::from("X-Spam-Result: ");
        for (idx, (name, score)) in self.tags.iter().enumerate() {
            if idx > 0 {
                out.push_str(",\r\n\t");
            }
            out.push_str(&format!("{name} ({score:.2})"));
        }
        out.push_str("\r\n");
        let class = if self.is_junk() { "spam" } else { "ham" };
        out.push_str(&format!(
            "X-Spam-Score: {class}, score={:.2}\r\n",
            self.score
        ));
        out
    }

    /// The stored copy with the \`X-Spam-*\` headers prepended.
    /// The tag summary stored on the row, e.g. "RBL_ZEN (6.00), SPF_FAIL (1.50)".
    pub fn tags_summary(&self) -> String {
        self.tags
            .iter()
            .map(|(name, score)| format!("{name} ({score:.2})"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn prepend(&self, raw: &[u8]) -> Vec<u8> {
        let headers = self.header_block();
        if headers.is_empty() {
            return raw.to_vec();
        }
        let mut out = headers.into_bytes();
        out.extend_from_slice(raw);
        out
    }
}

/// Everything one scan needs beyond the engine itself.
pub struct SpamInput<'a> {
    pub raw: &'a [u8],
    pub message: &'a Message<'a>,
    pub peer_ip: IpAddr,
    pub helo: &'a str,
    pub mail_from: &'a str,
    pub hostname: &'a str,
    pub recipients: &'a [String],
    pub auth: &'a AuthOutcome,
}

pub struct SpamEngine {
    pub config: Arc<SpamConfig>,
    resolver: Option<Arc<TokioResolver>>,
    state: state::StateStore,
    classifier: ClassifierSet,
    http: Option<reqwest::Client>,
    /// Rules fetched from the community ruleset, merged with the config's.
    extra_rules: Vec<rules::Rule>,
}

impl SpamEngine {
    pub async fn new(config: Arc<SpamConfig>, resolver: Option<Arc<TokioResolver>>) -> Self {
        let state = state::StateStore::connect(config.redis_url.as_deref()).await;
        let classifier = ClassifierSet::new(config.classifier.clone());
        let needs_http = config.scanners.olefy.is_some()
            || config.scanners.tika.is_some()
            || config.rules_url.is_some();
        let http = if needs_http {
            reqwest::Client::builder()
                .timeout(Duration::from_millis(config.scanners.timeout_ms.max(500)))
                .build()
                .ok()
        } else {
            None
        };
        let extra_rules = fetch_rules(http.as_ref(), config.rules_url.as_deref()).await;
        Self {
            config,
            resolver,
            state,
            classifier,
            http,
            extra_rules,
        }
    }

    pub fn classifier(&self) -> &ClassifierSet {
        &self.classifier
    }

    /// The configured junk threshold.
    pub fn junk_threshold(&self) -> f32 {
        self.config.thresholds.spam
    }

    /// True when the engine may file junk (quarantine, not shadow).
    pub fn is_quarantine_mode(&self) -> bool {
        matches!(self.config.mode, SpamMode::Quarantine)
    }

    /// Run the pipeline and decide an action. Never fails.
    pub async fn scan(&self, input: &SpamInput<'_>, mailbox: Option<Uuid>) -> Verdict {
        if self.config.mode.is_off() {
            return Verdict::empty();
        }

        let elements = checks::build_elements(input.message, input.raw);
        let mut tags: Tags = Tags::new();

        checks::analyze(input, &elements, &self.config, &mut tags);

        dnsbl::check(
            self.resolver.as_deref(),
            input.peer_ip,
            &elements,
            &self.config,
            &mut tags,
            &self.state,
        )
        .await;

        let peer = input.peer_ip.to_string();
        for rule in self.config.rules.iter().chain(self.extra_rules.iter()) {
            if rule.matches(&rules::RuleContext {
                subject: &elements.subject,
                body: &elements.text_body,
                header_block: &elements.header_block,
                urls: &elements.urls,
                domains: &elements.domains,
                emails: &elements.emails,
                attachments: &elements.attachments,
                peer_ip: &peer,
            }) {
                tags.entry(rule.tag.clone()).or_insert(rule.score);
            }
        }

        if let Some(pyzor) = &self.config.pyzor {
            pyzor::check(pyzor, &elements, &mut tags).await;
        }

        let mut sources: Vec<(String, String)> = vec![
            ("s".to_string(), elements.subject.clone()),
            ("b".to_string(), elements.text_body.clone()),
            ("h".to_string(), elements.html_body.clone()),
            ("u".to_string(), elements.urls.join(" ")),
        ];
        if let (Some(http), Some(url)) = (&self.http, self.config.scanners.tika.as_deref())
            && let Some(text) = scanners::extract_text(http, url, input.raw).await
        {
            sources.push(("t".to_string(), text));
        }
        let tokens = tokenize(sources.iter().map(|(p, t)| (p.as_str(), t.as_str())));

        let scope = mailbox.map(|id| id.to_string());
        let probability = scope
            .as_deref()
            .and_then(|scope| self.classifier.predict(scope, &tokens))
            .or_else(|| self.classifier.predict("global", &tokens));
        if let Some(probability) = probability {
            tags.insert(prob_tag(probability).to_string(), None);
        }

        let outcome = scanners::scan(
            self.http.as_ref(),
            &self.config.scanners,
            input.raw,
            &mut tags,
        )
        .await;

        finalize(&self.config, &tags, outcome.malware)
    }

    /// Parse the raw message and scan it; the entry point for the SMTP path.
    #[allow(clippy::too_many_arguments)]
    pub async fn scan_raw(
        &self,
        raw: &[u8],
        peer_ip: IpAddr,
        helo: &str,
        mail_from: &str,
        hostname: &str,
        recipients: &[String],
        auth: &AuthOutcome,
        mailbox: Option<Uuid>,
    ) -> Verdict {
        if self.config.mode.is_off() {
            return Verdict::empty();
        }
        let Some(message) = MessageParser::default().parse(raw) else {
            return Verdict::empty();
        };
        let input = SpamInput {
            raw,
            message: &message,
            peer_ip,
            helo,
            mail_from,
            hostname,
            recipients,
            auth,
        };
        self.scan(&input, mailbox).await
    }

    /// The classifier tokens for a raw message: headers, body, HTML and URLs.
    pub fn tokens_for(&self, raw: &[u8]) -> Vec<String> {
        let Some(message) = MessageParser::default().parse(raw) else {
            return Vec::new();
        };
        let elements = checks::build_elements(&message, raw);
        let sources = elements.token_source();
        tokenize(sources.iter().map(|text| ("b", text.as_str())))
    }

    /// Train the mailbox model and the shared model from one message.
    pub fn learn_raw(&self, mailbox: Option<Uuid>, raw: &[u8], spam: bool) {
        let tokens = self.tokens_for(raw);
        self.learn_tokens(mailbox, &tokens, spam);
    }

    /// Train from tokens already computed for a message.
    pub fn learn_tokens(&self, mailbox: Option<Uuid>, tokens: &[String], spam: bool) {
        if tokens.is_empty() {
            return;
        }
        if let Some(id) = mailbox {
            self.classifier.learn(&id.to_string(), tokens, spam);
        }
        self.classifier.learn("global", tokens, spam);
    }

    /// True when a first-seen sender should be deferred (greylisting).
    pub async fn greylist_defer(&self, ip: IpAddr, from: &str, rcpt: &str) -> bool {
        if !self.config.greylist {
            return false;
        }
        let key = format!(
            "grey:{}:{}:{}",
            ip,
            from.to_ascii_lowercase(),
            rcpt.to_ascii_lowercase()
        );
        self.state
            .greylist_defer(&key, self.config.greylist_delay_secs)
            .await
    }

    /// True when this peer has exceeded the hourly message rate.
    pub async fn rate_exceeded(&self, ip: IpAddr) -> bool {
        if !self.config.ratelimit {
            return false;
        }
        let key = format!("rate:{ip}");
        self.state.counter_incr(&key, 3600).await > self.config.ratelimit_max as u64
    }
}

fn finalize(config: &SpamConfig, tags: &Tags, malware: bool) -> Verdict {
    let mut results: Vec<(String, f32)> = Vec::new();
    let mut score = 0.0f32;
    let mut forced: Option<SpamAction> = None;

    for (name, explicit) in tags {
        if let Some(value) = explicit {
            score += value;
            if *value != 0.0 {
                results.push((name.clone(), *value));
            }
            continue;
        }
        match config.score_for_opt(name) {
            Some(ScoreRule::Score(value)) => {
                score += value;
                if value != 0.0 {
                    results.push((name.clone(), value));
                }
            }
            Some(ScoreRule::Disabled) => {}
            Some(ScoreRule::Discard) => {
                results.push((name.clone(), 0.0));
                forced = Some(SpamAction::Discard);
            }
            Some(ScoreRule::Reject) => {
                results.push((name.clone(), 0.0));
                forced = Some(SpamAction::Reject);
            }
            None => {
                if let Some(value) = default_score(name) {
                    score += value;
                    if value != 0.0 {
                        results.push((name.clone(), value));
                    }
                }
            }
        }
    }

    let junk = malware
        || forced.is_some()
        || (config.thresholds.spam > 0.0 && score >= config.thresholds.spam);

    let action = match config.mode {
        SpamMode::Off => SpamAction::Accept,
        SpamMode::Log => {
            if junk {
                SpamAction::Log
            } else {
                SpamAction::Accept
            }
        }
        SpamMode::Quarantine => {
            if !junk {
                SpamAction::Accept
            } else if config.allow_reject {
                match forced {
                    Some(SpamAction::Reject) => SpamAction::Reject,
                    Some(SpamAction::Discard) => SpamAction::Discard,
                    _ => SpamAction::Quarantine,
                }
            } else {
                SpamAction::Quarantine
            }
        }
    };

    results.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    Verdict {
        score,
        tags: results,
        action,
        malware,
    }
}

/// Built-in scores for tags the operator has not configured.
fn default_score(tag: &str) -> Option<f32> {
    if tag.starts_with("MALWARE_") {
        return Some(10.0);
    }
    Some(match tag {
        "SPAM_TRAP" => 10.0,
        "PROB_SPAM_HIGH" => 6.0,
        "PROB_SPAM_MEDIUM" => 3.5,
        "PROB_SPAM_LOW" => 1.5,
        "PROB_SPAM_UNCERTAIN" => 0.3,
        "PROB_HAM_HIGH" => -4.0,
        "PROB_HAM_MEDIUM" => -2.0,
        "PROB_HAM_LOW" => -0.5,
        "PYZOR_SPAM" => 3.0,
        "PYZOR_LOW" => 0.5,
        "HELO_IS_IP" => 1.0,
        "HELO_NOT_FQDN" => 0.5,
        "HELO_MISSING" => 0.5,
        "NO_RECEIVED" => 1.0,
        "IPREV_FAIL" => 1.0,
        "RECEIVED_MISMATCH" => 1.5,
        "ARC_FAIL" => 1.5,
        _ => return None,
    })
}

/// Whether a message should be filed as junk for one mailbox: the allow-list
/// wins, then the block-list and malware, then the score against the mailbox's
/// threshold (or the global one).
pub fn should_quarantine(
    score: f32,
    malware: bool,
    default_threshold: f64,
    policy: &crate::repo::SpamPolicy,
) -> bool {
    if policy.allowed {
        return false;
    }
    if policy.blocked || malware {
        return true;
    }
    score as f64 >= policy.threshold.unwrap_or(default_threshold)
}

/// Fetch and parse a community ruleset; failures leave the local rules alone.
async fn fetch_rules(client: Option<&reqwest::Client>, url: Option<&str>) -> Vec<rules::Rule> {
    let (Some(client), Some(url)) = (client, url) else {
        return Vec::new();
    };
    let Ok(response) = client.get(url).send().await else {
        tracing::warn!(url = %url, "could not fetch the spam ruleset");
        return Vec::new();
    };
    let Ok(body) = response.text().await else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<rules::Rule>>(&body) {
        Ok(rules) => rules,
        Err(err) => {
            tracing::warn!(url = %url, error = %err, "ignored a malformed spam ruleset");
            Vec::new()
        }
    }
}

/// An authenticator whose resolver is the spec (an ip or ip:port), falling back
/// to the system resolver. The spam engine reuses its resolver for DNSBL.
pub fn build_authenticator(spec: Option<&str>) -> Option<MessageAuthenticator> {
    let Some(spec) = spec.map(str::trim).filter(|spec| !spec.is_empty()) else {
        return MessageAuthenticator::new_system_conf().ok();
    };
    let (host, port) = match spec.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().unwrap_or(53)),
        None => (spec, 53),
    };
    let Ok(ip) = host.parse::<std::net::IpAddr>() else {
        tracing::warn!(spec = %spec, "ignoring an invalid SPAM_DNS_RESOLVER");
        return MessageAuthenticator::new_system_conf().ok();
    };
    let mut udp = ConnectionConfig::udp();
    udp.port = port;
    let mut tcp = ConnectionConfig::tcp();
    tcp.port = port;
    let server = NameServerConfig::new(ip, true, vec![udp, tcp]);
    let config = ResolverConfig::from_name_servers(vec![server]);
    MessageAuthenticator::new(config, ResolverOpts::default()).ok()
}

/// The classifier confidence band, matching rspamd/Stalwart naming.
pub fn prob_tag(probability: f32) -> &'static str {
    match probability {
        p if p < 0.15 => "PROB_HAM_HIGH",
        p if p < 0.25 => "PROB_HAM_MEDIUM",
        p if p < 0.40 => "PROB_HAM_LOW",
        p if p < 0.60 => "PROB_SPAM_UNCERTAIN",
        p if p < 0.75 => "PROB_SPAM_LOW",
        p if p < 0.85 => "PROB_SPAM_MEDIUM",
        _ => "PROB_SPAM_HIGH",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(mode: SpamMode, spam: f32, scores: Vec<(&str, ScoreRule)>) -> SpamConfig {
        SpamConfig::for_mode(mode, spam, scores)
    }

    #[test]
    fn off_mode_accepts_everything() {
        let config = config(SpamMode::Off, 5.0, vec![("*", ScoreRule::Score(9.0))]);
        let mut tags = Tags::new();
        tags.insert("RBL_X".to_string(), None);
        let verdict = finalize(&config, &tags, false);
        assert_eq!(verdict.action, SpamAction::Accept);
        assert_eq!(verdict.score, 9.0);
    }

    #[test]
    fn quarantine_above_threshold_and_log_only_in_shadow() {
        let scores = vec![("RBL_*", ScoreRule::Score(6.0))];
        let mut tags = Tags::new();
        tags.insert("RBL_ZEN".to_string(), None);

        let verdict = finalize(
            &config(SpamMode::Quarantine, 5.0, scores.clone()),
            &tags,
            false,
        );
        assert_eq!(verdict.action, SpamAction::Quarantine);
        assert!(verdict.is_junk());

        let shadow = finalize(&config(SpamMode::Log, 5.0, scores), &tags, false);
        assert_eq!(shadow.action, SpamAction::Log);
        assert!(!shadow.is_junk());
    }

    #[test]
    fn below_threshold_is_accepted() {
        let config = config(
            SpamMode::Quarantine,
            5.0,
            vec![("URL_SHORTENER", ScoreRule::Score(1.0))],
        );
        let mut tags = Tags::new();
        tags.insert("URL_SHORTENER".to_string(), None);
        assert_eq!(finalize(&config, &tags, false).action, SpamAction::Accept);
    }

    #[test]
    fn malware_quarantines_regardless_of_score() {
        let config = config(SpamMode::Quarantine, 50.0, vec![]);
        let mut tags = Tags::new();
        tags.insert("MALWARE_EICAR".to_string(), None);
        let verdict = finalize(&config, &tags, true);
        assert_eq!(verdict.action, SpamAction::Quarantine);
        assert!(verdict.malware);
    }

    fn input<'a>(
        raw: &'a [u8],
        message: &'a Message<'a>,
        auth: &'a AuthOutcome,
        recipients: &'a [String],
    ) -> SpamInput<'a> {
        SpamInput {
            raw,
            message,
            peer_ip: "203.0.113.5".parse().unwrap(),
            helo: "mail.example.com",
            mail_from: "a@example.com",
            hostname: "mx.example.com",
            recipients,
            auth,
        }
    }

    #[tokio::test]
    async fn scan_files_a_shortener_url() {
        let config = SpamConfig::for_mode(
            SpamMode::Quarantine,
            1.0,
            vec![("URL_SHORTENER", ScoreRule::Score(5.0))],
        );
        let engine = SpamEngine::new(Arc::new(config), None).await;
        let raw = b"From: a@example.com\r\nSubject: hi\r\nMessage-ID: <x@y>\r\nDate: today\r\n\r\nsee http://bit.ly/abc";
        let message = MessageParser::default().parse(raw).unwrap();
        let auth = AuthOutcome::skipped();
        let recipients = vec!["me@example.com".to_string()];
        let verdict = engine
            .scan(&input(raw, &message, &auth, &recipients), None)
            .await;
        assert!(verdict.has_tag("URL_SHORTENER"), "tags: {:?}", verdict.tags);
        assert_eq!(verdict.action, SpamAction::Quarantine);
    }

    #[tokio::test]
    async fn scan_files_a_spam_trap() {
        let mut config = SpamConfig::for_mode(SpamMode::Quarantine, 1.0, vec![]);
        config.traps.push("trap@example.com".to_string());
        let engine = SpamEngine::new(Arc::new(config), None).await;
        let raw =
            b"From: a@example.com\r\nSubject: hi\r\nMessage-ID: <x@y>\r\nDate: today\r\n\r\nbody";
        let message = MessageParser::default().parse(raw).unwrap();
        let auth = AuthOutcome::skipped();
        let recipients = vec!["trap@example.com".to_string()];
        let verdict = engine
            .scan(&input(raw, &message, &auth, &recipients), None)
            .await;
        assert!(verdict.has_tag("SPAM_TRAP"));
        assert_eq!(verdict.action, SpamAction::Quarantine);
    }

    #[test]
    fn quarantine_precedence() {
        use crate::repo::SpamPolicy;

        let allowed = SpamPolicy {
            allowed: true,
            blocked: true,
            threshold: Some(1.0),
        };
        assert!(!should_quarantine(100.0, true, 5.0, &allowed));

        let blocked = SpamPolicy {
            blocked: true,
            ..SpamPolicy::default()
        };
        assert!(should_quarantine(0.0, false, 5.0, &blocked));

        let plain = SpamPolicy::default();
        assert!(should_quarantine(6.0, false, 5.0, &plain));
        assert!(!should_quarantine(4.0, false, 5.0, &plain));
        assert!(should_quarantine(0.0, true, 5.0, &plain));

        let lower = SpamPolicy {
            threshold: Some(2.0),
            ..SpamPolicy::default()
        };
        assert!(should_quarantine(2.5, false, 5.0, &lower));
        assert!(!should_quarantine(1.5, false, 5.0, &lower));
    }

    #[test]
    fn headers_report_tags_and_class() {
        let config = config(
            SpamMode::Quarantine,
            1.0,
            vec![("RBL_*", ScoreRule::Score(6.0))],
        );
        let mut tags = Tags::new();
        tags.insert("RBL_ZEN".to_string(), None);
        let verdict = finalize(&config, &tags, false);
        let headers = verdict.header_block();
        assert!(headers.contains("X-Spam-Result: RBL_ZEN (6.00)"));
        assert!(headers.contains("X-Spam-Score: spam, score=6.00"));
    }
}
