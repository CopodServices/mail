use crate::relay::Relay;
use crate::repo;
use crate::secrets::SecretBox;
use crate::store::MailStore;
use anyhow::{Context, Result};
use chrono::Utc;
use entity::outbound_job::JobStatus;
use sea_orm::DatabaseConnection;
use std::sync::Arc;
use std::time::Duration;

/// Background relay worker. It claims due jobs, DKIM-signs the stored message
/// with the sender's domain key and hands it to the smarthost, retrying with
/// exponential backoff on transient failures.
pub struct Queue {
    pub db: DatabaseConnection,
    pub store: MailStore,
    pub relay: Relay,
    pub secrets: SecretBox,
    pub max_attempts: i32,
    pub helo: String,
}

impl Queue {
    pub fn spawn(self: Arc<Self>, interval: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match self.run_once().await {
                    Ok(0) => {}
                    Ok(count) => tracing::debug!(count, "relayed outbound messages"),
                    Err(err) => tracing::warn!(error = %err, "outbound queue pass failed"),
                }
                tokio::time::sleep(interval).await;
            }
        })
    }

    pub async fn run_once(&self) -> Result<usize> {
        let jobs = repo::claim_due_jobs(&self.db, 20).await?;
        let count = jobs.len();
        for job in jobs {
            self.process(job).await;
        }
        Ok(count)
    }

    async fn process(&self, mut job: entity::outbound_job::Model) {
        match self.deliver(&job).await {
            Ok(()) => {
                job.status = JobStatus::Sent;
                job.last_error = None;
                tracing::info!(id = %job.id, "relayed outbound message");
                // A scheduled message leaves the Scheduled folder when it goes out.
                if let Some(message_id) = job.scheduled_message_id
                    && let Err(err) = repo::set_message_folder(&self.db, message_id, "sent").await
                {
                    tracing::warn!(error = %err, "failed to move the scheduled copy to sent");
                }
            }
            Err(err) => {
                job.attempts += 1;
                job.last_error = Some(err.to_string());
                if job.attempts >= self.max_attempts {
                    job.status = JobStatus::Failed;
                    tracing::error!(id = %job.id, error = %err, "giving up on outbound message");
                } else {
                    job.status = JobStatus::Pending;
                    job.next_attempt_at = Utc::now() + backoff(job.attempts);
                    tracing::warn!(id = %job.id, attempt = job.attempts, error = %err, "outbound attempt failed");
                }
            }
        }

        if let Err(err) = repo::save_job(
            &self.db,
            job.id,
            job.status,
            job.attempts,
            job.next_attempt_at,
            job.last_error,
        )
        .await
        {
            tracing::error!(error = %err, "failed to record queue job state");
        }
    }

    async fn deliver(&self, job: &entity::outbound_job::Model) -> Result<()> {
        let recipients: Vec<String> = serde_json::from_value(job.recipients.clone())
            .context("failed to decode recipients")?;

        // A scheduled message to a local mailbox has nothing to hand over; the
        // job exists only so the copy leaves the Scheduled folder on time.
        if recipients.is_empty() {
            return Ok(());
        }

        let mut payload = self.store.get(&job.storage_key).await?;

        if let Some(address) = crate::address::parse(&job.envelope_from)
            && let Some(domain) = repo::find_domain(&self.db, &address.domain).await?
        {
            let private_key = self.secrets.decrypt_bytes(&domain.dkim_private_key)?;
            payload =
                crate::dkim::sign(&private_key, &domain.name, &domain.dkim_selector, &payload)?;
        }

        self.relay
            .deliver(&self.helo, &job.envelope_from, &recipients, &payload)
            .await
    }
}

/// Exponential backoff capped at six hours.
pub fn backoff(attempts: i32) -> chrono::Duration {
    let exponent = (attempts - 1).clamp(0, 10) as u32;
    let seconds = 30i64.saturating_mul(2i64.saturating_pow(exponent));
    chrono::Duration::seconds(seconds.min(6 * 3600))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_is_capped() {
        assert_eq!(backoff(1), chrono::Duration::seconds(30));
        assert_eq!(backoff(2), chrono::Duration::seconds(60));
        assert_eq!(backoff(3), chrono::Duration::seconds(120));
        assert_eq!(backoff(30), chrono::Duration::seconds(6 * 3600));
    }
}
