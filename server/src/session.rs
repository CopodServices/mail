use anyhow::Result;
use chrono::{Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Session cookie names shared by the API and the web client.
pub const SESSION_COOKIE: &str = "cpod_mail_session";
pub const CSRF_COOKIE: &str = "cpod_mail_csrf";
const SESSION_TTL_HOURS: i64 = 12;

/// Claims carried by the session cookie. The mailbox id is the only
/// thing the API trusts; the email and domain are labels for the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub email: String,
    pub domain: String,
    pub iat: i64,
    pub exp: i64,
}

pub fn max_age_secs() -> i64 {
    SESSION_TTL_HOURS * 3600
}

/// Mints a signed (HS256) session token for one mailbox.
pub fn mint(secret: &str, mailbox_id: Uuid, email: &str, domain: &str) -> Result<String> {
    let now = Utc::now();
    let claims = Claims {
        sub: mailbox_id.to_string(),
        email: email.to_string(),
        domain: domain.to_string(),
        iat: now.timestamp(),
        exp: (now + Duration::hours(SESSION_TTL_HOURS)).timestamp(),
    };
    Ok(encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )?)
}

/// Verifies a session token, returning its claims when valid and unexpired.
pub fn verify(secret: &str, token: &str) -> Option<Claims> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 30;
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .ok()
    .map(|data| data.claims)
}

/// A fresh double-submit CSRF token.
pub fn csrf_token() -> String {
    use base64::Engine;
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn session_cookie(token: &str, secure: bool) -> String {
    cookie(SESSION_COOKIE, token, secure, true)
}

pub fn csrf_cookie(token: &str, secure: bool) -> String {
    cookie(CSRF_COOKIE, token, secure, false)
}

/// Expires a cookie by clearing its value and setting Max-Age to zero.
pub fn cleared_cookie(name: &str) -> String {
    format!("{name}=; Path=/; SameSite=Lax; Max-Age=0")
}

fn cookie(name: &str, value: &str, secure: bool, http_only: bool) -> String {
    let mut out = format!(
        "{name}={value}; Path=/; SameSite=Lax; Max-Age={}",
        max_age_secs()
    );
    if http_only {
        out.push_str("; HttpOnly");
    }
    if secure {
        out.push_str("; Secure");
    }
    out
}

/// Reads one cookie value out of a Cookie header.
pub fn cookie_value(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key.trim() == name).then(|| value.trim().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_token() {
        let secret = "dev-session-secret";
        let id = Uuid::now_v7();
        let token = mint(secret, id, "user@example.test", "example.test").unwrap();
        let claims = verify(secret, &token).expect("valid token");
        assert_eq!(claims.sub, id.to_string());
        assert_eq!(claims.email, "user@example.test");
    }

    #[test]
    fn rejects_a_token_signed_with_another_key() {
        let token = mint("one", Uuid::now_v7(), "a@b.test", "b.test").unwrap();
        assert!(verify("two", &token).is_none());
    }

    #[test]
    fn reads_one_cookie_among_many() {
        let header = "a=1; cpod_mail_session=abc.def; cpod_mail_csrf=xyz";
        assert_eq!(
            cookie_value(header, SESSION_COOKIE).as_deref(),
            Some("abc.def")
        );
        assert_eq!(cookie_value(header, CSRF_COOKIE).as_deref(), Some("xyz"));
        assert_eq!(cookie_value(header, "missing"), None);
    }

    #[test]
    fn marks_the_session_cookie_http_only_and_same_site() {
        let value = session_cookie("tok", true);
        assert!(value.starts_with("cpod_mail_session=tok"));
        assert!(value.contains("HttpOnly"));
        assert!(value.contains("SameSite=Lax"));
        assert!(value.contains("; Secure"));

        // The CSRF cookie must stay readable for a same-origin client.
        assert!(!csrf_cookie("tok", true).contains("HttpOnly"));
    }
}
