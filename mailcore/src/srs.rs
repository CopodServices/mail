//! Sender Rewriting Scheme for auto-forwarded mail.
//!
//! Every outbound message goes to one smarthost, and that relay only accepts a
//! forward whose envelope sender has been rewritten with SRS: the envelope must
//! belong to one of our verified domains, with the original sender encoded so a
//! bounce can be routed back. The forwarded message itself is left untouched, so
//! the sender's own DKIM signature stays valid.
//!
//! Only the SRS0 form is produced, in the shape other forwarders use: a short
//! hash, a short stamp, then the original sender split into domain and local
//! part, so a relay recognises the address as forwarded mail. The hash is an HMAC
//! over the forwarding domain, the original sender and the stamp, so an address
//! cannot be forged into an open relay, and the stamp bounds how long a rewritten
//! address stays usable.
//!
//! Shape:
//!
//! ```text
//! SRS0=<hash>=<stamp>=<sender-domain>=<sender-local>@<forwarding-domain>
//! ```
//!
//! Both short fields are upper-case base32, so the address survives a relay that
//! folds the local part to lower case. The stamp is the day it was written,
//! counted through the last 1024 days; an address older than MAX_AGE_DAYS stops
//! working.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Marker and prefix of a rewritten local part.
const PREFIX: &str = "SRS0";
/// Characters of the HMAC digest carried in the address.
const HASH_CHARS: usize = 4;
/// Characters of the stamp carried in the address.
const STAMP_CHARS: usize = 2;
/// Days the stamp counts through before it rolls over.
const STAMP_ROLLOVER: i64 = 1 << (5 * STAMP_CHARS);
/// How long a rewritten address stays usable, in days.
const MAX_AGE_DAYS: i64 = 21;
/// The base32 alphabet both short fields use.
const BASE32: &[u8; 32] = b"0123456789ABCDEFGHIJKLMNOPQRSTUV";

/// Derives the SRS key from the one secret the service already has, so the two
/// uses do not share a key.
pub fn key(secret: &str) -> Vec<u8> {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac accepts a key of any length");
    mac.update(b"copod-mail-srs-v1");
    mac.finalize().into_bytes().to_vec()
}

/// Rewrites `sender` into an address on `domain`, for use as the envelope sender
/// of a forward. Returns None for the null sender or an address that cannot be
/// rewritten.
pub fn encode(sender: &str, domain: &str, key: &[u8], now: i64) -> Option<String> {
    let parsed = crate::address::parse(sender)?;
    if parsed.domain.contains('=') || domain.is_empty() {
        return None;
    }
    let day = now.div_euclid(86_400);
    if day < 0 {
        return None;
    }
    let stamp = base32_encode(day.rem_euclid(STAMP_ROLLOVER) as u64, STAMP_CHARS);
    let hash = hash(domain, &parsed.domain, &parsed.local_part, &stamp, key);
    Some(format!(
        "{PREFIX}={hash}={stamp}={}={}@{domain}",
        parsed.domain, parsed.local_part
    ))
}

/// Reads a rewritten local part back to the original sender. Returns None when
/// it was not written here, does not carry a matching hash, or has expired.
pub fn decode(local_part: &str, domain: &str, key: &[u8], now: i64) -> Option<String> {
    let mut parts = local_part.splitn(5, '=');
    if !parts.next()?.eq_ignore_ascii_case(PREFIX) {
        return None;
    }
    let carried = parts.next()?.to_ascii_uppercase();
    let stamp = parts.next()?.to_ascii_uppercase();
    let sender_domain = parts.next()?;
    let sender_local = parts.next()?;
    if sender_domain.is_empty() || sender_local.is_empty() {
        return None;
    }
    let stamped = base32_decode(&stamp)?;
    let today = now.div_euclid(86_400);
    if today < 0 {
        return None;
    }
    // The stamp rolls over every 1024 days, so its age is counted in the round.
    let age = (today.rem_euclid(STAMP_ROLLOVER) as u64)
        .wrapping_sub(stamped)
        .rem_euclid(STAMP_ROLLOVER as u64);
    if age > MAX_AGE_DAYS as u64 {
        return None;
    }
    let expected = hash(domain, sender_domain, sender_local, &stamp, key);
    if !constant_eq(expected.as_bytes(), carried.as_bytes()) {
        return None;
    }
    Some(format!("{sender_local}@{sender_domain}"))
}

/// The truncated HMAC carried in the address, in base32.
fn hash(
    forward_domain: &str,
    sender_domain: &str,
    sender_local: &str,
    stamp: &str,
    key: &[u8],
) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts a key of any length");
    mac.update(forward_domain.as_bytes());
    mac.update(b"\0");
    mac.update(sender_domain.as_bytes());
    mac.update(b"\0");
    mac.update(sender_local.as_bytes());
    mac.update(b"\0");
    mac.update(stamp.as_bytes());
    let digest = mac.finalize().into_bytes();
    let value = u32::from_be_bytes([digest[0], digest[1], digest[2], 0]) as u64;
    base32_encode(value, HASH_CHARS)
}

/// The last chars characters of value, in base32.
fn base32_encode(mut value: u64, chars: usize) -> String {
    let mut out = vec![b'0'; chars];
    for slot in out.iter_mut().rev() {
        *slot = BASE32[(value % 32) as usize];
        value /= 32;
    }
    String::from_utf8(out).expect("base32 is ascii")
}

/// Reads a base32 field written here, in whatever case it arrived.
fn base32_decode(text: &str) -> Option<u64> {
    let mut value = 0u64;
    for byte in text.bytes() {
        let digit = BASE32
            .iter()
            .position(|ch| ch.eq_ignore_ascii_case(&byte))?;
        value = value * 32 + digit as u64;
    }
    Some(value)
}

/// Compares two byte strings without an early exit.
fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in a.iter().zip(b) {
        diff |= left ^ right;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::{decode, encode, key};

    const NOW: i64 = 1_800_000_000;
    const DOMAIN: &str = "example.test";

    fn secret_key() -> Vec<u8> {
        key("session-secret")
    }

    #[test]
    fn a_rewritten_sender_round_trips() {
        let rewritten = encode("alice@remote.test", DOMAIN, &secret_key(), NOW).unwrap();
        assert!(rewritten.starts_with("SRS0="), "{rewritten}");
        assert!(rewritten.ends_with("@example.test"), "{rewritten}");
        // The shape a relay recognises as forwarded mail.
        let (fields, _) = rewritten.split_once('@').unwrap();
        let parts: Vec<&str> = fields.split('=').collect();
        assert_eq!(parts.len(), 5, "{rewritten}");
        assert_eq!(parts[0], "SRS0");
        assert_eq!(parts[1].len(), 4, "{rewritten}");
        assert_eq!(parts[2].len(), 2, "{rewritten}");
        assert_eq!(parts[3], "remote.test", "{rewritten}");
        assert_eq!(parts[4], "alice", "{rewritten}");
        let (local, domain) = rewritten.split_once('@').unwrap();
        assert_eq!(
            decode(local, domain, &secret_key(), NOW).as_deref(),
            Some("alice@remote.test")
        );
    }

    #[test]
    fn a_local_part_survives_lower_casing_by_a_relay() {
        let rewritten = encode("alice@remote.test", DOMAIN, &secret_key(), NOW).unwrap();
        let lowercased = rewritten.to_ascii_lowercase();
        let (local, domain) = lowercased.split_once('@').unwrap();
        assert_eq!(
            decode(local, domain, &secret_key(), NOW).as_deref(),
            Some("alice@remote.test")
        );
    }

    #[test]
    fn a_sender_with_a_separator_still_round_trips() {
        let rewritten = encode("odd=name@remote.test", DOMAIN, &secret_key(), NOW).unwrap();
        let (local, domain) = rewritten.split_once('@').unwrap();
        assert_eq!(
            decode(local, domain, &secret_key(), NOW).as_deref(),
            Some("odd=name@remote.test")
        );
    }

    #[test]
    fn a_forged_or_stale_address_is_refused() {
        let srs_key = secret_key();
        let rewritten = encode("alice@remote.test", DOMAIN, &srs_key, NOW).unwrap();
        let (local, domain) = rewritten.split_once('@').unwrap();
        // Another key cannot have written it.
        assert!(decode(local, domain, &key("other-secret"), NOW).is_none());
        // Another domain cannot claim it.
        assert!(decode(local, "elsewhere.test", &srs_key, NOW).is_none());
        // A swapped sender fails the hash.
        let forged = local.replace("alice", "mallory");
        assert!(decode(&forged, domain, &srs_key, NOW).is_none());
        // And it expires.
        let later = NOW + 30 * 86_400;
        assert!(decode(local, domain, &srs_key, later).is_none());
        assert!(decode(local, domain, &srs_key, NOW + 2 * 86_400).is_some());
    }

    #[test]
    fn the_null_sender_and_nonsense_are_not_rewritten() {
        assert!(encode("", DOMAIN, &secret_key(), NOW).is_none());
        assert!(encode("not-an-address", DOMAIN, &secret_key(), NOW).is_none());
        assert!(decode("SRS0=short", DOMAIN, &secret_key(), NOW).is_none());
        assert!(decode("not-srs", DOMAIN, &secret_key(), NOW).is_none());
    }
}
