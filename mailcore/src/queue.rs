use crate::relay::Relay;
use crate::repo;
use crate::store::MailStore;
use anyhow::{Context, Result};
use chrono::Utc;
use entity::outbound_job::JobStatus;
use sea_orm::{DatabaseConnection, EntityTrait};
use std::sync::Arc;
use std::time::Duration;

/// Background relay worker. It claims due jobs and hands the stored message to
/// the smarthost unchanged (the relay signs), retrying with exponential backoff
/// on transient failures and failing at once on a permanent rejection.
pub struct Queue {
    pub db: DatabaseConnection,
    pub store: MailStore,
    pub relay: Relay,
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
                // A forward the relay refused over the sender's domain is sent
                // again as the mailbox, which the SMTP user may always send from.
                if let Some(envelope) = self.rewrite_refused_forward(&job, &err).await {
                    job.envelope_from = envelope;
                    job.attempts = 0;
                    job.last_error = Some(err.to_string());
                    job.next_attempt_at = Utc::now();
                    job.status = JobStatus::Pending;
                } else {
                    job.attempts += 1;
                    job.last_error = Some(err.to_string());
                    let permanent = err
                        .downcast_ref::<crate::relay::SmtpRejection>()
                        .is_some_and(|rejection| rejection.permanent);
                    if permanent {
                        job.status = JobStatus::Failed;
                        tracing::error!(id = %job.id, error = %err, "outbound message rejected");
                    } else if job.attempts >= self.max_attempts {
                        job.status = JobStatus::Failed;
                        tracing::error!(id = %job.id, error = %err, "giving up on outbound message");
                    } else {
                        job.status = JobStatus::Pending;
                        job.next_attempt_at = Utc::now() + backoff(job.attempts);
                        tracing::warn!(id = %job.id, attempt = job.attempts, error = %err, "outbound attempt failed");
                    }
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

    /// Rewrites and re-queues a forward the relay refused over its sender domain:
    /// the copy goes out again as the mailbox itself. Returns the envelope sender
    /// to retry with, or None when this refusal is something else.
    async fn rewrite_refused_forward(
        &self,
        job: &entity::outbound_job::Model,
        err: &anyhow::Error,
    ) -> Option<String> {
        let rejection = err.downcast_ref::<crate::relay::SmtpRejection>()?;
        if !rejection.permanent || !crate::forward::sender_domain_refused(&rejection.message) {
            return None;
        }
        let mailbox = entity::mailbox::Entity::find_by_id(job.mailbox_id?)
            .one(&self.db)
            .await
            .ok()??;
        // An envelope already on the mailbox is the retry of a rewrite, not a
        // forward that still has to be rewritten.
        if job.envelope_from.eq_ignore_ascii_case(&mailbox.address) {
            return None;
        }
        let payload = self.store.get(&job.storage_key).await.ok()?;
        if !crate::forward::already_forwarded(&payload) {
            return None;
        }
        let rewritten = crate::forward::rewrite_sender(
            &payload,
            &mailbox.address,
            mailbox.display_name.as_deref(),
        );
        self.store.put(&job.storage_key, rewritten).await.ok()?;
        tracing::warn!(
            id = %job.id,
            mailbox = %mailbox.address,
            "the relay refused the sender's domain; resending the forward as the mailbox"
        );
        Some(mailbox.address)
    }

    async fn deliver(&self, job: &entity::outbound_job::Model) -> Result<()> {
        let recipients: Vec<String> = serde_json::from_value(job.recipients.clone())
            .context("failed to decode recipients")?;

        // A scheduled message to a local mailbox has nothing to hand over; the
        // job exists only so the copy leaves the Scheduled folder on time.
        if recipients.is_empty() {
            return Ok(());
        }

        let payload = self.store.get(&job.storage_key).await?;

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
