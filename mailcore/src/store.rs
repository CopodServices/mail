use crate::config::S3Config;
use anyhow::{Context, Result};
use object_store::aws::AmazonS3Builder;
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt};
use std::sync::Arc;
use uuid::Uuid;

/// Thin wrapper over an S3-compatible object store (MinIO in development,
/// Cloudflare R2 or S3 in production).
#[derive(Clone)]
pub struct MailStore {
    store: Arc<dyn ObjectStore>,
}

impl MailStore {
    pub fn new(config: &S3Config) -> Result<Self> {
        let mut builder = AmazonS3Builder::new()
            .with_bucket_name(&config.bucket)
            .with_region(&config.region)
            .with_access_key_id(&config.access_key)
            .with_secret_access_key(&config.secret_key)
            .with_allow_http(config.allow_http)
            .with_virtual_hosted_style_request(!config.path_style);

        if let Some(endpoint) = &config.endpoint {
            builder = builder.with_endpoint(endpoint);
        }

        let store = builder.build().context("failed to build the S3 client")?;
        Ok(Self {
            store: Arc::new(store),
        })
    }

    pub async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<()> {
        self.store
            .put(&ObjectPath::from(key), bytes.into())
            .await
            .with_context(|| format!("failed to write {key} to object storage"))?;
        Ok(())
    }

    pub async fn get(&self, key: &str) -> Result<Vec<u8>> {
        let result = self
            .store
            .get(&ObjectPath::from(key))
            .await
            .with_context(|| format!("failed to read {key} from object storage"))?;
        let bytes = result.bytes().await.context("failed to read object body")?;
        Ok(bytes.to_vec())
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        self.store
            .delete(&ObjectPath::from(key))
            .await
            .with_context(|| format!("failed to delete {key} from object storage"))?;
        Ok(())
    }
}

/// Storage key for a message delivered into a mailbox.
pub fn inbound_key(
    domain: &str,
    local_part: &str,
    id: &Uuid,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    format!(
        "{}/{}/{:04}/{:02}/{}.eml",
        domain,
        local_part,
        chrono::Datelike::year(&now),
        chrono::Datelike::month(&now),
        id
    )
}

/// Storage key for an outgoing copy (also used for messages awaiting relay).
pub fn outbound_key(id: &Uuid, now: chrono::DateTime<chrono::Utc>) -> String {
    format!(
        "outbound/{:04}/{:02}/{}.eml",
        chrono::Datelike::year(&now),
        chrono::Datelike::month(&now),
        id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_namespaced_by_month() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-03-04T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let id = Uuid::nil();
        assert_eq!(
            inbound_key("example.test", "postmaster", &id, now),
            "example.test/postmaster/2026/03/00000000-0000-0000-0000-000000000000.eml"
        );
        assert_eq!(
            outbound_key(&id, now),
            "outbound/2026/03/00000000-0000-0000-0000-000000000000.eml"
        );
    }
}
