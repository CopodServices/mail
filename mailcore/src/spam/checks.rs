//! The message analyzers that need no network: header, body, URL and
//! attachment heuristics. Each match adds a tag; the score comes later from
//! the configured tag -> score map.

use crate::spam::config::SpamConfig;
use crate::spam::{SpamInput, Tags};
use mail_parser::{Message, MimeHeaders};
use regex::Regex;
use std::collections::BTreeSet;
use std::sync::OnceLock;

const SHORTENERS: &[&str] = &[
    "bit.ly",
    "tinyurl.com",
    "t.co",
    "goo.gl",
    "ow.ly",
    "is.gd",
    "buff.ly",
    "rebrand.ly",
    "cutt.ly",
    "shorturl.at",
    "t.ly",
    "rb.gy",
    "tiny.cc",
    "surl.li",
    "shorte.st",
];

const SUSPICIOUS_TLDS: &[&str] = &[
    "zip", "mov", "xyz", "top", "gq", "cf", "tk", "ml", "work", "click", "link", "loan", "review",
    "country", "kim", "men", "party", "quest", "rest", "win",
];

/// Everything once parsed out of the message and reused by every analyzer.
pub struct Elements {
    pub subject: String,
    pub header_block: String,
    pub text_body: String,
    pub html_body: String,
    pub urls: Vec<String>,
    pub url_hosts: Vec<String>,
    pub domains: Vec<String>,
    pub emails: Vec<String>,
    pub attachments: Vec<String>,
    pub attachment_exts: Vec<String>,
}

impl Elements {
    pub fn token_source(&self) -> Vec<String> {
        vec![
            self.subject.clone(),
            self.text_body.clone(),
            self.html_body.clone(),
            self.header_block.clone(),
            self.urls.join(" "),
        ]
    }
}

pub fn build_elements(message: &Message<'_>, raw: &[u8]) -> Elements {
    let header_block = header_block(raw);
    let text_body = message
        .body_text(0)
        .map(|c| c.into_owned())
        .unwrap_or_default();
    let html_body = message
        .body_html(0)
        .map(|c| c.into_owned())
        .unwrap_or_default();

    let mut urls = BTreeSet::new();
    for text in [&text_body, &html_body] {
        for m in url_regex().find_iter(text) {
            urls.insert(m.as_str().trim_end_matches(['.', ',', ')']).to_string());
        }
    }
    let urls: Vec<String> = urls.into_iter().collect();
    let url_hosts: Vec<String> = urls.iter().filter_map(|url| host_of(url)).collect();

    let mut emails = BTreeSet::new();
    for text in [&header_block, &text_body] {
        for m in email_regex().find_iter(text) {
            emails.insert(m.as_str().to_ascii_lowercase());
        }
    }
    let emails: Vec<String> = emails.into_iter().collect();

    let mut domains = BTreeSet::new();
    for email in &emails {
        if let Some((_, domain)) = email.rsplit_once('@') {
            domains.insert(domain.to_string());
        }
    }
    for host in &url_hosts {
        domains.insert(host.clone());
    }
    let domains: Vec<String> = domains.into_iter().collect();

    let mut attachments = Vec::new();
    let mut attachment_exts = Vec::new();
    for part in message.attachments() {
        if let Some(name) = part.attachment_name() {
            let name = name.to_string();
            if let Some((_, ext)) = name.rsplit_once('.') {
                attachment_exts.push(ext.to_ascii_lowercase());
            }
            attachments.push(name);
        }
    }

    Elements {
        subject: message.subject().unwrap_or("").to_string(),
        header_block,
        text_body,
        html_body,
        urls,
        url_hosts,
        domains,
        emails,
        attachments,
        attachment_exts,
    }
}

pub fn analyze(input: &SpamInput<'_>, elements: &Elements, config: &SpamConfig, tags: &mut Tags) {
    add_auth_tags(input, tags);
    add_helo_tags(input, tags);
    add_recipient_tags(input, config, tags);
    add_header_tags(input, elements, tags);
    add_body_tags(elements, config, tags);
    add_url_tags(elements, tags);
    add_attachment_tags(elements, config, tags);
    add_received_tags(input, elements, tags);
}

/// HELO/EHLO sanity: a missing, non-FQDN or literal-IP name.
fn add_helo_tags(input: &SpamInput<'_>, tags: &mut Tags) {
    let helo = input.helo.trim();
    if helo.is_empty() || helo.eq_ignore_ascii_case("unknown") {
        tag(tags, "HELO_MISSING");
        return;
    }
    if helo.parse::<std::net::IpAddr>().is_ok() {
        tag(tags, "HELO_IS_IP");
    }
    if !helo.contains('.') {
        tag(tags, "HELO_NOT_FQDN");
    }
}

/// A message with no Received header was injected locally; a hop that names an
/// IP other than the peer is forged.
fn add_received_tags(input: &SpamInput<'_>, elements: &Elements, tags: &mut Tags) {
    let received = received_headers(&elements.header_block);
    let Some(top) = received.first() else {
        tag(tags, "NO_RECEIVED");
        return;
    };
    let peer = input.peer_ip.to_string();
    let mut named = false;
    for found in ipv4_regex().find_iter(top) {
        named = true;
        if found.as_str() == peer {
            return;
        }
    }
    if named {
        tag(tags, "RECEIVED_MISMATCH");
    }
}

/// The Received headers, in order, with folded continuations joined.
fn received_headers(block: &str) -> Vec<String> {
    let mut headers: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for line in block.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(cur) = current.as_mut() {
                cur.push(' ');
                cur.push_str(line.trim());
            }
            continue;
        }
        if let Some(cur) = current.take() {
            headers.push(cur);
        }
        if line.to_ascii_lowercase().starts_with("received:") {
            current = Some(line.to_string());
        }
    }
    if let Some(cur) = current {
        headers.push(cur);
    }
    headers
}

fn ipv4_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b\d{1,3}(?:\.\d{1,3}){3}\b").unwrap())
}

fn add_auth_tags(input: &SpamInput<'_>, tags: &mut Tags) {
    match input.auth.spf.as_deref() {
        Some("fail") => {
            tag(tags, "SPF_FAIL");
        }
        Some("softfail") => {
            tag(tags, "SPF_SOFTFAIL");
        }
        _ => {}
    }
    if input.auth.dkim.as_deref() == Some("fail") {
        tag(tags, "DKIM_FAIL");
    }
    if input.auth.dmarc.as_deref() == Some("fail") {
        tag(tags, "DMARC_FAIL");
    }
    if input.auth.iprev.as_deref() == Some("fail") {
        tag(tags, "IPREV_FAIL");
    }
    if input.auth.arc.as_deref() == Some("fail") {
        tag(tags, "ARC_FAIL");
    }
}

fn add_recipient_tags(input: &SpamInput<'_>, config: &SpamConfig, tags: &mut Tags) {
    if !config.traps.is_empty()
        && input.recipients.iter().any(|rcpt| {
            config
                .traps
                .iter()
                .any(|trap| trap.eq_ignore_ascii_case(rcpt))
        })
    {
        tag(tags, "SPAM_TRAP");
    }
}

fn add_header_tags(input: &SpamInput<'_>, elements: &Elements, tags: &mut Tags) {
    if input.message.message_id().is_none() {
        tag(tags, "MISSING_MESSAGE_ID");
    }
    if elements.subject.trim().is_empty() {
        tag(tags, "MISSING_SUBJECT");
    }
    if input.message.date().is_none() {
        tag(tags, "MISSING_DATE");
    }
    if is_all_caps(&elements.subject) {
        tag(tags, "SUBJECT_ALL_CAPS");
    }
    if excessive_punctuation(&elements.subject) {
        tag(tags, "SUBJECT_EXCESSIVE_PUNCT");
    }
    if let (Some(from), Some(reply)) = (
        header_domain(&elements.header_block, "from"),
        header_domain(&elements.header_block, "reply-to"),
    ) && from != reply
        && !from.is_empty()
    {
        tag(tags, "FROM_REPLYTO_MISMATCH");
    }
}

fn add_body_tags(elements: &Elements, config: &SpamConfig, tags: &mut Tags) {
    if !elements.html_body.is_empty() && elements.text_body.trim().is_empty() {
        tag(tags, "HTML_ONLY");
    }
    let html = elements.html_body.to_ascii_lowercase();
    if html.contains("<script") {
        tag(tags, "HTML_SCRIPT");
    }
    if html.contains("<iframe") {
        tag(tags, "HTML_IFRAME");
    }
    if html.contains("<form") {
        tag(tags, "HTML_FORM");
    }
    if html.contains("<img") && elements.text_body.trim().len() < 40 {
        tag(tags, "HTML_IMAGE_ONLY");
    }
    let haystack = format!("{} {}", elements.subject, elements.text_body).to_ascii_lowercase();
    for word in &config.spam_words {
        if !word.is_empty() && haystack.contains(&word.to_ascii_lowercase()) {
            tags.entry(format!("BODY_SPAM_WORD_{}", sanitize(word)))
                .or_insert(None);
        }
    }
}

fn add_url_tags(elements: &Elements, tags: &mut Tags) {
    if elements.urls.len() >= 10 {
        tag(tags, "URL_COUNT_MANY");
    }
    let mut shortener = false;
    let mut ip_literal = false;
    for host in &elements.url_hosts {
        let host = host.to_ascii_lowercase();
        if SHORTENERS
            .iter()
            .any(|s| host == *s || host.ends_with(&format!(".{s}")))
        {
            shortener = true;
        }
        if host.parse::<std::net::IpAddr>().is_ok() {
            ip_literal = true;
        }
    }
    if shortener {
        tag(tags, "URL_SHORTENER");
    }
    if ip_literal {
        tag(tags, "URL_IP_LITERAL");
    }
    if elements.url_hosts.iter().any(|host| {
        host.rsplit_once('.')
            .map(|(_, tld)| SUSPICIOUS_TLDS.contains(&tld.to_ascii_lowercase().as_str()))
            .unwrap_or(false)
    }) {
        tag(tags, "SUSPICIOUS_TLD");
    }
}

fn add_attachment_tags(elements: &Elements, config: &SpamConfig, tags: &mut Tags) {
    if elements.attachments.len() >= 5 {
        tag(tags, "ATTACH_COUNT_MANY");
    }
    for ext in &elements.attachment_exts {
        if config
            .dangerous_extensions
            .iter()
            .any(|d| d.eq_ignore_ascii_case(ext))
        {
            tag(tags, "ATTACH_DANGEROUS");
        }
    }
}

pub fn tag(tags: &mut Tags, name: impl Into<String>) {
    tags.entry(name.into()).or_insert(None);
}

fn header_block(raw: &[u8]) -> String {
    let end = raw
        .windows(2)
        .position(|w| w == b"\n\n")
        .or_else(|| raw.windows(4).position(|w| w == b"\r\n\r\n"))
        .unwrap_or_else(|| raw.len().min(64 * 1024));
    String::from_utf8_lossy(&raw[..end]).to_string()
}

fn header_domain(block: &str, name: &str) -> Option<String> {
    for line in block.lines() {
        if let Some((key, value)) = line.split_once(':')
            && key.trim().eq_ignore_ascii_case(name)
        {
            return email_regex().find(value).and_then(|m| {
                m.as_str()
                    .rsplit_once('@')
                    .map(|(_, d)| d.to_ascii_lowercase())
            });
        }
    }
    None
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit('@').next()?;
    let host = host.split(':').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

fn is_all_caps(subject: &str) -> bool {
    let letters: Vec<char> = subject.chars().filter(|c| c.is_alphabetic()).collect();
    letters.len() >= 10 && letters.iter().all(|c| c.is_uppercase())
}

fn excessive_punctuation(subject: &str) -> bool {
    let marks = subject
        .chars()
        .filter(|c| *c == '!' || *c == '$' || *c == '?')
        .count();
    marks >= 4 || subject.to_ascii_uppercase().contains("!!!")
}

fn sanitize(word: &str) -> String {
    word.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn url_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new("(?i)https?://[^\\s<>\\]\\)\x22']+").unwrap())
}

fn email_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new("[A-Za-z0-9._%+\\-]+@[A-Za-z0-9.\\-]+\\.[A-Za-z]{2,}").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail_parser::MessageParser;

    fn parse(raw: &str) -> Elements {
        let bytes = raw.as_bytes().to_vec();
        let message = MessageParser::default().parse(&bytes).unwrap();
        build_elements(&message, &bytes)
    }

    #[test]
    fn flags_all_caps_subject_and_shorteners() {
        let elements = parse(
            "From: a@example.com\r\nSubject: WIN MONEY NOW PLEASE\r\n\r\nsee http://bit.ly/x",
        );
        assert!(is_all_caps(&elements.subject));
        assert!(elements.url_hosts.contains(&"bit.ly".to_string()));
    }

    #[test]
    fn flags_a_forged_received() {
        let raw: &[u8] = b"Received: from evil ([10.0.0.9]) by mx.example.com\r\nFrom: a@b.test\r\nSubject: hi\r\nMessage-ID: <x@y>\r\nDate: today\r\n\r\nbody";
        let message = MessageParser::default().parse(raw).unwrap();
        let elements = build_elements(&message, raw);
        let auth = crate::policy::AuthOutcome::skipped();
        let recipients = vec!["me@example.com".to_string()];
        let input = SpamInput {
            raw,
            message: &message,
            peer_ip: "203.0.113.5".parse().unwrap(),
            helo: "mail.example.com",
            mail_from: "a@b.test",
            hostname: "mx.example.com",
            recipients: &recipients,
            auth: &auth,
        };
        let mut tags = Tags::new();
        add_received_tags(&input, &elements, &mut tags);
        assert!(tags.contains_key("RECEIVED_MISMATCH"), "tags: {tags:?}");
    }

    #[test]
    fn extracts_emails_and_domains() {
        let elements = parse("From: Alice <alice@example.org>\r\nSubject: hi\r\n\r\nbody");
        assert!(elements.emails.contains(&"alice@example.org".to_string()));
        assert!(elements.domains.contains(&"example.org".to_string()));
    }
}
