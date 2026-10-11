//! A small declarative rule language over the message elements.
//!
//! A rule is a tag plus a boolean expression over leaf predicates:
//!
//! ```json
//! {"tag": "SUBJ_CREDIT", "when": {"field": "subject", "op": "contains", "value": "free credit"}}
//! {"tag": "BODY_CRYPTO", "when": {"any": [
//!     {"field": "body", "op": "contains", "value": "bitcoin"},
//!     {"field": "subject", "op": "contains", "value": "btc"}]}}
//! ```

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub tag: String,
    #[serde(default)]
    pub score: Option<f32>,
    pub when: Expr,
}

impl Rule {
    pub fn matches(&self, ctx: &RuleContext<'_>) -> bool {
        self.when.eval(ctx)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Expr {
    All { all: Vec<Expr> },
    Any { any: Vec<Expr> },
    Not { not: Box<Expr> },
    Leaf(Leaf),
}

impl Expr {
    pub fn eval(&self, ctx: &RuleContext<'_>) -> bool {
        match self {
            Expr::All { all } => all.iter().all(|expr| expr.eval(ctx)),
            Expr::Any { any } => any.iter().any(|expr| expr.eval(ctx)),
            Expr::Not { not } => !not.eval(ctx),
            Expr::Leaf(leaf) => leaf.eval(ctx),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Leaf {
    pub field: String,
    pub op: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub values: Option<Vec<String>>,
    #[serde(default)]
    pub case_sensitive: bool,
}

impl Leaf {
    fn eval(&self, ctx: &RuleContext<'_>) -> bool {
        let candidates = ctx.field_values(&self.field);
        candidates
            .iter()
            .any(|candidate| self.matches_one(candidate))
    }

    fn matches_one(&self, candidate: &str) -> bool {
        let op = self.op.to_ascii_lowercase();
        let (needle, hay) = if self.case_sensitive {
            (
                self.value.clone().unwrap_or_default(),
                candidate.to_string(),
            )
        } else {
            (
                self.value.clone().unwrap_or_default().to_ascii_lowercase(),
                candidate.to_ascii_lowercase(),
            )
        };
        match op.as_str() {
            "contains" => hay.contains(&needle),
            "equals" | "is" => hay == needle,
            "startswith" => hay.starts_with(&needle),
            "endswith" => hay.ends_with(&needle),
            "glob" => {
                super::config::glob_match(self.value.as_deref().unwrap_or_default(), candidate)
            }
            "regex" => regex::Regex::new(self.value.as_deref().unwrap_or(""))
                .map(|re| re.is_match(candidate))
                .unwrap_or(false),
            "in-list" => self
                .values
                .as_ref()
                .map(|list| {
                    list.iter().any(|item| {
                        if self.case_sensitive {
                            item == candidate
                        } else {
                            item.eq_ignore_ascii_case(candidate)
                        }
                    })
                })
                .unwrap_or(false),
            "cidr" => match candidate.parse::<std::net::IpAddr>() {
                Ok(ip) => parse_net(self.value.as_deref().unwrap_or_default())
                    .map(|net| net.contains(&ip))
                    .unwrap_or(false),
                Err(_) => false,
            },
            _ => false,
        }
    }
}

fn parse_net(value: &str) -> Option<ipnet::IpNet> {
    if let Ok(net) = value.parse::<ipnet::IpNet>() {
        return Some(net);
    }
    value
        .parse::<std::net::IpAddr>()
        .ok()
        .map(ipnet::IpNet::from)
}

/// The message elements a rule can read.
pub struct RuleContext<'a> {
    pub subject: &'a str,
    pub body: &'a str,
    pub header_block: &'a str,
    pub urls: &'a [String],
    pub domains: &'a [String],
    pub emails: &'a [String],
    pub attachments: &'a [String],
    pub peer_ip: &'a str,
}

impl RuleContext<'_> {
    fn field_values(&self, field: &str) -> Vec<String> {
        match field.to_ascii_lowercase().as_str() {
            "subject" => vec![self.subject.to_string()],
            "body" | "text" => vec![self.body.to_string()],
            "header" | "headers" => vec![self.header_block.to_string()],
            "url" | "urls" | "link" => self.urls.to_vec(),
            "domain" | "domains" => self.domains.to_vec(),
            "email" | "emails" | "address" => self.emails.to_vec(),
            "attachment" | "attachments" | "filename" => self.attachments.to_vec(),
            "ip" | "peer" | "remote" => vec![self.peer_ip.to_string()],
            // "any" (and anything unknown) searches every element.
            _ => {
                let mut all = vec![
                    self.subject.to_string(),
                    self.body.to_string(),
                    self.header_block.to_string(),
                    self.peer_ip.to_string(),
                ];
                all.extend(self.urls.iter().cloned());
                all.extend(self.domains.iter().cloned());
                all.extend(self.emails.iter().cloned());
                all.extend(self.attachments.iter().cloned());
                all
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(subject: &'a str, body: &'a str, urls: &'a [String]) -> RuleContext<'a> {
        RuleContext {
            subject,
            body,
            header_block: "",
            urls,
            domains: &[],
            emails: &[],
            attachments: &[],
            peer_ip: "203.0.113.7",
        }
    }

    #[test]
    fn leaf_predicates_match() {
        let urls = vec!["http://bit.ly/x".to_string()];
        let ctx = ctx("Free Credit Now", "hello", &urls);
        let rule: Rule = serde_json::from_value(serde_json::json!({
            "tag": "SUBJ_CREDIT",
            "when": {"field": "subject", "op": "contains", "value": "free credit"}
        }))
        .unwrap();
        assert!(rule.matches(&ctx));
    }

    #[test]
    fn boolean_trees_and_cidr() {
        let ctx = ctx("hi", "pay in bitcoin", &[]);
        let rule: Rule = serde_json::from_value(serde_json::json!({
            "tag": "X",
            "when": {"all": [
                {"field": "body", "op": "contains", "value": "bitcoin"},
                {"field": "ip", "op": "cidr", "value": "203.0.113.0/24"}
            ]}
        }))
        .unwrap();
        assert!(rule.matches(&ctx));

        let negated: Rule = serde_json::from_value(serde_json::json!({
            "tag": "Y",
            "when": {"not": {"field": "subject", "op": "contains", "value": "urgent"}}
        }))
        .unwrap();
        assert!(negated.matches(&ctx));
    }
}
