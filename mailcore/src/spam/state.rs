//! Redis-backed ephemeral state: greylisting, negative caches and rate limits.
//! Every operation fails open, so a missing or broken Redis never blocks mail.

use redis::aio::ConnectionManager;

/// Whether a first sighting is still inside the greylisting delay window.
pub fn within_delay(now: u64, seen: u64, delay_secs: u64) -> bool {
    now.saturating_sub(seen) < delay_secs
}

#[derive(Clone)]
pub struct StateStore {
    conn: Option<ConnectionManager>,
}

impl StateStore {
    pub async fn connect(redis_url: Option<&str>) -> Self {
        let Some(url) = redis_url else {
            return Self { conn: None };
        };
        match redis::Client::open(url.to_string()) {
            Ok(client) => match ConnectionManager::new(client).await {
                Ok(conn) => Self { conn: Some(conn) },
                Err(err) => {
                    tracing::warn!(error = %err, "redis unavailable; spam state disabled");
                    Self { conn: None }
                }
            },
            Err(err) => {
                tracing::warn!(error = %err, "invalid REDIS_URL; spam state disabled");
                Self { conn: None }
            }
        }
    }

    pub fn enabled(&self) -> bool {
        self.conn.is_some()
    }

    fn connection(&self) -> Option<ConnectionManager> {
        self.conn.clone()
    }

    /// True when the key did not exist and was created (first sighting).
    pub async fn first_seen(&self, key: &str, ttl_secs: u64) -> bool {
        let Some(mut conn) = self.connection() else {
            return false;
        };
        let result: redis::RedisResult<Option<String>> = redis::cmd("SET")
            .arg(key)
            .arg(1)
            .arg("NX")
            .arg("EX")
            .arg(ttl_secs)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(value) => value.is_some(),
            Err(err) => {
                tracing::debug!(error = %err, "greylist set failed");
                false
            }
        }
    }

    /// Read a cached value, used for DNSBL answers.
    pub async fn cache_get(&self, key: &str) -> Option<String> {
        let mut conn = self.connection()?;
        match redis::cmd("GET")
            .arg(key)
            .query_async::<Option<String>>(&mut conn)
            .await
        {
            Ok(value) => value,
            Err(err) => {
                tracing::debug!(error = %err, "cache get failed");
                None
            }
        }
    }

    /// Store a value with a TTL.
    pub async fn cache_set(&self, key: &str, value: &str, ttl_secs: u64) {
        let Some(mut conn) = self.connection() else {
            return;
        };
        let _: redis::RedisResult<()> = redis::cmd("SET")
            .arg(key)
            .arg(value)
            .arg("EX")
            .arg(ttl_secs)
            .query_async(&mut conn)
            .await;
    }

    /// Greylisting: true while the triple is inside its delay window (defer).
    pub async fn greylist_defer(&self, key: &str, delay_secs: u64) -> bool {
        let Some(mut conn) = self.connection() else {
            return false;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let set: redis::RedisResult<Option<String>> = redis::cmd("SET")
            .arg(key)
            .arg(now)
            .arg("NX")
            .arg("EX")
            .arg(delay_secs.max(1) * 4)
            .query_async(&mut conn)
            .await;
        match set {
            Ok(Some(_)) => true,
            Ok(None) => {
                let get: redis::RedisResult<Option<u64>> =
                    redis::cmd("GET").arg(key).query_async(&mut conn).await;
                match get {
                    Ok(Some(seen)) => within_delay(now, seen, delay_secs),
                    _ => false,
                }
            }
            Err(err) => {
                tracing::debug!(error = %err, "greylist check failed");
                false
            }
        }
    }

    /// Increment a counter within a rolling window and return the new value.
    pub async fn counter_incr(&self, key: &str, ttl_secs: u64) -> u64 {
        let Some(mut conn) = self.connection() else {
            return 0;
        };
        let count: redis::RedisResult<u64> =
            redis::cmd("INCR").arg(key).query_async(&mut conn).await;
        let count = match count {
            Ok(count) => count,
            Err(err) => {
                tracing::debug!(error = %err, "rate counter failed");
                return 0;
            }
        };
        if count == 1 {
            let _: redis::RedisResult<i64> = redis::cmd("EXPIRE")
                .arg(key)
                .arg(ttl_secs)
                .query_async(&mut conn)
                .await;
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_delay_window_gates_a_first_sighting() {
        assert!(within_delay(1000, 990, 300));
        assert!(!within_delay(1000, 600, 300));
        assert!(within_delay(1000, 1000, 1));
        assert!(!within_delay(1000, 1000, 0));
    }
}
