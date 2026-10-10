/// A parsed, normalized email address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailAddress {
    pub local_part: String,
    pub domain: String,
}

impl MailAddress {
    pub fn normalized(&self) -> String {
        format!("{}@{}", self.local_part, self.domain)
    }
}

/// Extracts the address out of an SMTP command argument, tolerating
/// angle brackets and surrounding whitespace. Returns None for the null
/// sender or anything without exactly one at-sign.
pub fn parse(input: &str) -> Option<MailAddress> {
    let trimmed = input
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = trimmed.split_whitespace().next().unwrap_or(trimmed);
    let (local_part, domain) = candidate.rsplit_once('@')?;
    if local_part.is_empty() || domain.is_empty() || local_part.contains(['<', '>']) {
        return None;
    }
    Some(MailAddress {
        local_part: local_part.to_string(),
        domain: domain.to_ascii_lowercase(),
    })
}

/// Domain part of a MAIL FROM argument, falling back to the supplied hostname
/// for the null sender (used as the DMARC RFC5321.MailFrom domain).
pub fn domain_or(input: &str, fallback: &str) -> String {
    parse(input)
        .map(|address| address.domain)
        .unwrap_or_else(|| fallback.to_string())
}

/// Splits a header value into its display name and its address.
///
/// A mail client sends `"Info" <info@example.com>` or a bare address; the two
/// parts belong in different places, because a name written as part of the
/// address is not what a header wants.
pub fn split_display_name(value: &str) -> (Option<&str>, &str) {
    let trimmed = value.trim();
    if let (Some(start), Some(end)) = (trimmed.find('<'), trimmed.rfind('>'))
        && start < end
    {
        let address = trimmed[start + 1..end].trim();
        let name = trimmed[..start].trim().trim_matches('"').trim();
        if !address.is_empty() {
            return ((!name.is_empty()).then_some(name), address);
        }
    }
    (None, trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        assert_eq!(
            parse("<Alice@Example.COM>").unwrap(),
            MailAddress {
                local_part: "Alice".into(),
                domain: "example.com".into()
            }
        );
        assert_eq!(parse("bob@example.com").unwrap().local_part, "bob");
        assert_eq!(
            parse("carol@example.com SIZE=100").unwrap().normalized(),
            "carol@example.com"
        );
    }

    #[test]
    fn rejects_null_sender_and_garbage() {
        assert!(parse("<>").is_none());
        assert!(parse("").is_none());
        assert!(parse("no-at-sign").is_none());
        assert!(parse("@example.com").is_none());
    }

    #[test]
    fn domain_falls_back_to_hostname() {
        assert_eq!(domain_or("<>", "mx.example.test"), "mx.example.test");
        assert_eq!(domain_or("<a@b.test>", "mx"), "b.test");
    }

    #[test]
    fn splits_a_display_name_from_its_address() {
        assert_eq!(
            split_display_name("\"Info\" <info@example.test>"),
            (Some("Info"), "info@example.test")
        );
        assert_eq!(
            split_display_name("Info <info@example.test>"),
            (Some("Info"), "info@example.test")
        );
        assert_eq!(
            split_display_name("<info@example.test>"),
            (None, "info@example.test")
        );
        assert_eq!(
            split_display_name("info@example.test"),
            (None, "info@example.test")
        );
        assert_eq!(split_display_name("  "), (None, ""));
    }
}
