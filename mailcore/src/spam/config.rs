//! Spam filter configuration.
//!
//! Thresholds and the tag -> score map live in an optional JSON file
//! (`SPAM_CONFIG_PATH`, default `/etc/copod-mail/spam.json`); switches and
//! service endpoints may also be overridden from the environment. Anything
//! unset keeps a safe default that disables the feature.

use crate::spam::rules::Rule;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpamMode {
    Off,
    Log,
    Quarantine,
}

impl SpamMode {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "log" | "shadow" => SpamMode::Log,
            "quarantine" | "on" | "true" | "1" | "yes" => SpamMode::Quarantine,
            _ => SpamMode::Off,
        }
    }

    pub fn is_off(self) -> bool {
        matches!(self, SpamMode::Off)
    }
}

/// What a matching tag does: add a score, or force discard/reject/ignore.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScoreRule {
    Score(f32),
    Discard,
    Reject,
    Disabled,
}

impl ScoreRule {
    /// The score to add when the tag only contributes a number.
    pub fn score(self, fallback: f32) -> f32 {
        match self {
            ScoreRule::Score(score) => score,
            ScoreRule::Disabled => 0.0,
            ScoreRule::Discard | ScoreRule::Reject => fallback,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RawRule {
    Score(f32),
    Action(String),
}

impl RawRule {
    fn resolve(self) -> ScoreRule {
        match self {
            RawRule::Score(score) => ScoreRule::Score(score),
            RawRule::Action(action) => match action.trim().to_ascii_lowercase().as_str() {
                "discard" => ScoreRule::Discard,
                "reject" => ScoreRule::Reject,
                "disabled" | "ignore" | "off" => ScoreRule::Disabled,
                other => other
                    .parse()
                    .map(ScoreRule::Score)
                    .unwrap_or(ScoreRule::Disabled),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Thresholds {
    #[serde(default)]
    pub spam: f32,
    #[serde(default)]
    pub discard: f32,
    #[serde(default)]
    pub reject: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DnsblServer {
    pub zone: String,
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default = "default_max_checks")]
    pub max_checks: u32,
}

fn default_scope() -> String {
    "ip".to_string()
}

fn default_max_checks() -> u32 {
    5
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClassifierConfig {
    #[serde(default = "default_hash_size")]
    pub feature_hash_size: usize,
    #[serde(default = "default_alpha")]
    pub alpha: f32,
    #[serde(default = "default_beta")]
    pub beta: f32,
    #[serde(default)]
    pub l1: f32,
    #[serde(default)]
    pub l2: f32,
    #[serde(default = "default_min_samples")]
    pub min_samples: u64,
    #[serde(default = "default_true")]
    pub auto_learn_trap: bool,
    #[serde(default)]
    pub auto_learn_rbl: u32,
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        Self {
            feature_hash_size: default_hash_size(),
            alpha: default_alpha(),
            beta: default_beta(),
            l1: 0.0,
            l2: 1.0,
            min_samples: default_min_samples(),
            auto_learn_trap: true,
            auto_learn_rbl: 2,
        }
    }
}

fn default_hash_size() -> usize {
    1 << 20
}
fn default_alpha() -> f32 {
    0.1
}
fn default_beta() -> f32 {
    1.0
}
fn default_min_samples() -> u64 {
    200
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScannersConfig {
    #[serde(default)]
    pub clamav: Option<String>,
    #[serde(default)]
    pub olefy: Option<String>,
    #[serde(default)]
    pub tika: Option<String>,
    #[serde(default = "default_scan_timeout")]
    pub timeout_ms: u64,
}

fn default_scan_timeout() -> u64 {
    5000
}

#[derive(Debug, Clone, Deserialize)]
pub struct PyzorConfig {
    pub address: String,
    #[serde(default = "default_pyzor_timeout")]
    pub timeout_ms: u64,
}

fn default_pyzor_timeout() -> u64 {
    2000
}

#[derive(Debug, Clone, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    thresholds: Thresholds,
    #[serde(default)]
    scores: BTreeMap<String, RawRule>,
    #[serde(default)]
    dnsbl: Vec<DnsblServer>,
    #[serde(default)]
    traps: Vec<String>,
    #[serde(default)]
    dangerous_extensions: Vec<String>,
    #[serde(default)]
    spam_words: Vec<String>,
    #[serde(default)]
    rules: Vec<serde_json::Value>,
    classifier: Option<ClassifierConfig>,
    pyzor: Option<PyzorConfig>,
    scanners: Option<ScannersConfig>,
}

/// The resolved filter configuration.
#[derive(Debug, Clone)]
pub struct SpamConfig {
    pub mode: SpamMode,
    pub thresholds: Thresholds,
    /// Ordered tag -> score patterns; the first glob match wins.
    scores: Vec<(String, ScoreRule)>,
    pub dnsbl: Vec<DnsblServer>,
    pub traps: Vec<String>,
    pub dangerous_extensions: Vec<String>,
    pub spam_words: Vec<String>,
    pub rules: Vec<Rule>,
    pub classifier: ClassifierConfig,
    pub pyzor: Option<PyzorConfig>,
    pub scanners: ScannersConfig,
    pub dns_resolver: Option<String>,
    /// Optional community ruleset fetched at startup.
    pub rules_url: Option<String>,
    pub greylist: bool,
    pub greylist_delay_secs: u64,
    pub ratelimit: bool,
    pub ratelimit_max: u32,
    pub redis_url: Option<String>,
    pub allow_reject: bool,
    pub debug: bool,
}

impl SpamConfig {
    pub fn load() -> Self {
        let mode = env("SPAM_MODE")
            .map(|value| SpamMode::parse(&value))
            .unwrap_or(SpamMode::Off);

        let path =
            env("SPAM_CONFIG_PATH").unwrap_or_else(|| "/etc/copod-mail/spam.json".to_string());
        let file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| match serde_json::from_str::<FileConfig>(&text) {
                Ok(config) => Some(config),
                Err(err) => {
                    tracing::warn!(path = %path, error = %err, "ignoring an unreadable spam config");
                    None
                }
            })
            .unwrap_or_default();

        let mut thresholds = file.thresholds;
        if thresholds.spam <= 0.0 {
            thresholds.spam = number("SPAM_JUNK_SCORE", 5.0);
        }

        let scores = sort_patterns(
            file.scores
                .into_iter()
                .map(|(pattern, rule)| (pattern, rule.resolve()))
                .collect(),
        );

        let scanners = merge_scanners(file.scanners);
        let mut rules: Vec<Rule> = file
            .rules
            .into_iter()
            .filter_map(|value| match serde_json::from_value::<Rule>(value) {
                Ok(rule) => Some(rule),
                Err(err) => {
                    tracing::warn!(error = %err, "ignoring a malformed spam rule");
                    None
                }
            })
            .collect();
        for rule in &mut rules {
            rule.compile();
        }

        let dnsbl = merge_zones(file.dnsbl);

        Self {
            mode,
            thresholds,
            scores,
            dnsbl,
            traps: file.traps,
            dangerous_extensions: file.dangerous_extensions,
            spam_words: file.spam_words,
            rules,
            classifier: file.classifier.unwrap_or_default(),
            pyzor: file.pyzor,
            scanners,
            dns_resolver: env("SPAM_DNS_RESOLVER"),
            rules_url: env("SPAM_RULES_URL"),
            greylist: flag("SPAM_GREYLIST", false),
            greylist_delay_secs: number("SPAM_GREYLIST_DELAY_SECS", 300),
            ratelimit: flag("SPAM_RATELIMIT", false),
            ratelimit_max: number("SPAM_RATELIMIT_MAX", 100),
            redis_url: env("REDIS_URL"),
            allow_reject: flag("SPAM_ALLOW_REJECT", false),
            debug: flag("SPAM_DEBUG", false),
        }
    }

    /// The rule for a tag: the first glob match, or a zero score.
    pub fn score_for(&self, tag: &str) -> ScoreRule {
        self.score_for_opt(tag).unwrap_or(ScoreRule::Score(0.0))
    }

    /// The rule for a tag if any pattern matches, else `None`.
    pub fn score_for_opt(&self, tag: &str) -> Option<ScoreRule> {
        for (pattern, rule) in &self.scores {
            if glob_match(pattern, tag) {
                return Some(*rule);
            }
        }
        None
    }

    /// A minimal configuration: a mode, a junk threshold and a tag -> score
    /// list; every other check and the scanners stay off.
    pub fn for_mode(
        mode: SpamMode,
        spam_threshold: f32,
        scores: Vec<(&str, ScoreRule)>,
    ) -> SpamConfig {
        SpamConfig {
            mode,
            thresholds: Thresholds {
                spam: spam_threshold,
                discard: 0.0,
                reject: 0.0,
            },
            scores: sort_patterns(
                scores
                    .into_iter()
                    .map(|(pattern, rule)| (pattern.to_string(), rule))
                    .collect(),
            ),
            dnsbl: Vec::new(),
            traps: Vec::new(),
            dangerous_extensions: Vec::new(),
            spam_words: Vec::new(),
            rules: Vec::new(),
            classifier: ClassifierConfig::default(),
            pyzor: None,
            scanners: ScannersConfig::default(),
            dns_resolver: None,
            rules_url: None,
            greylist: false,
            greylist_delay_secs: 300,
            ratelimit: false,
            ratelimit_max: 100,
            redis_url: None,
            allow_reject: false,
            debug: false,
        }
    }
}

/// Appends the zone lists from the environment to the file's DNSBL servers.
fn merge_zones(mut servers: Vec<DnsblServer>) -> Vec<DnsblServer> {
    let add = |key: &str, scope: &str, servers: &mut Vec<DnsblServer>| {
        for zone in env(key).unwrap_or_default().split(',').map(str::trim) {
            if zone.is_empty() || servers.iter().any(|server| server.zone == zone) {
                continue;
            }
            servers.push(DnsblServer {
                zone: zone.to_string(),
                scope: scope.to_string(),
                max_checks: default_max_checks(),
            });
        }
    };
    add("SPAM_DNSBL_ZONES", "ip", &mut servers);
    add("SPAM_URIBL_ZONES", "domain", &mut servers);
    servers
}

fn merge_scanners(from_file: Option<ScannersConfig>) -> ScannersConfig {
    let mut scanners = from_file.unwrap_or_default();
    if let Some(value) = env("CLAMAV_ADDR") {
        scanners.clamav = Some(value);
    }
    if let Some(value) = env("OLEFY_URL") {
        scanners.olefy = Some(value);
    }
    if let Some(value) = env("TIKA_URL") {
        scanners.tika = Some(value);
    }
    scanners
}

/// Exact patterns first, then wildcards by decreasing literal length, so a
/// specific entry always beats a broad `*` default.
fn sort_patterns(mut patterns: Vec<(String, ScoreRule)>) -> Vec<(String, ScoreRule)> {
    patterns.sort_by(|a, b| {
        let exact_a = !a.0.contains('*');
        let exact_b = !b.0.contains('*');
        exact_b
            .cmp(&exact_a)
            .then_with(|| literal_len(&b.0).cmp(&literal_len(&a.0)))
            .then_with(|| a.0.cmp(&b.0))
    });
    patterns
}

fn literal_len(pattern: &str) -> usize {
    pattern.chars().filter(|c| *c != '*').count()
}

/// A case-sensitive glob with `*` matching any run of characters.
pub fn glob_match(pattern: &str, value: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let v: Vec<char> = value.chars().collect();
    let (mut pi, mut vi) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while vi < v.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == v[vi]) {
            pi += 1;
            vi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = vi;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            vi = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

fn flag(key: &str, default: bool) -> bool {
    std::env::var(key)
        .map(|value| !matches!(value.as_str(), "" | "0" | "false" | "no" | "off"))
        .unwrap_or(default)
}

fn number<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_prefixes_and_anything() {
        assert!(glob_match("RBL_*", "RBL_SPAMHAUS"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("PROB_SPAM_*", "PROB_SPAM_HIGH"));
        assert!(!glob_match("RBL_*", "BAYES_SPAM"));
        assert!(glob_match("SPF_FA?L", "SPF_FAIL"));
    }

    #[test]
    fn specific_patterns_win_over_broad_ones() {
        let patterns = sort_patterns(vec![
            ("*".to_string(), ScoreRule::Score(1.0)),
            ("RBL_SPAMHAUS".to_string(), ScoreRule::Score(6.0)),
            ("RBL_*".to_string(), ScoreRule::Score(4.0)),
        ]);
        let config = SpamConfig {
            mode: SpamMode::Off,
            thresholds: Thresholds::default(),
            scores: patterns,
            dnsbl: Vec::new(),
            traps: Vec::new(),
            dangerous_extensions: Vec::new(),
            spam_words: Vec::new(),
            rules: Vec::new(),
            classifier: ClassifierConfig::default(),
            pyzor: None,
            scanners: ScannersConfig::default(),
            dns_resolver: None,
            rules_url: None,
            greylist: false,
            greylist_delay_secs: 300,
            ratelimit: false,
            ratelimit_max: 100,
            redis_url: None,
            allow_reject: false,
            debug: false,
        };
        assert_eq!(config.score_for("RBL_SPAMHAUS"), ScoreRule::Score(6.0));
        assert_eq!(config.score_for("RBL_OTHER"), ScoreRule::Score(4.0));
        assert_eq!(config.score_for("UNKNOWN"), ScoreRule::Score(1.0));
    }
}
