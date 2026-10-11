//! Durable storage for the spam classifier: the trained models and the
//! reservoir of training samples the periodic retrain replays.

use crate::state::AppState;
use mailcore::spam::SpamEngine;
use rand::seq::SliceRandom;
use sea_orm::{ConnectionTrait, DbBackend, Statement, Value};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// The reservoir keeps at most this many samples; older ones are pruned.
const SAMPLE_CAP: i64 = 20_000;
/// Samples a single scope is rebuilt from.
const MAX_SAMPLES_PER_SCOPE: i64 = 5_000;
/// Shuffled passes over the reservoir during a rebuild.
const RETRAIN_EPOCHS: usize = 3;

/// Load every persisted model into the running engine.
pub async fn load(state: &AppState) {
    let Some(engine) = state.spam.clone() else {
        return;
    };
    let statement = Statement::from_string(
        DbBackend::Postgres,
        "SELECT scope, payload FROM spam_models".to_string(),
    );
    let rows = match state.db.query_all_raw(statement).await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(error = %err, "failed to read the spam models");
            return;
        }
    };
    for row in rows {
        let scope: String = match row.try_get("", "scope") {
            Ok(scope) => scope,
            Err(_) => continue,
        };
        let payload: Vec<u8> = match row.try_get("", "payload") {
            Ok(payload) => payload,
            Err(_) => continue,
        };
        engine.classifier().import(&scope, &payload);
    }
}

/// Persist every trained model on an interval and retrain from the reservoir
/// once every `SPAM_TRAIN_INTERVAL` seconds.
pub fn spawn(state: AppState) {
    let interval = std::env::var("SPAM_TRAIN_INTERVAL")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(3600);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        ticker.tick().await;
        let mut elapsed = 0u64;
        loop {
            ticker.tick().await;
            elapsed += 60;
            persist(&state).await;
            if elapsed >= interval {
                elapsed = 0;
                retrain(&state).await;
            }
        }
    });
}

/// Record one training sample: the classifier's tokens and the label.
pub async fn record_sample(
    db: &sea_orm::DatabaseConnection,
    mailbox_id: Option<Uuid>,
    tokens: &[String],
    is_spam: bool,
) {
    let payload = match serde_json::to_string(tokens) {
        Ok(payload) => payload,
        Err(_) => return,
    };
    let values: Vec<Value> = vec![
        Uuid::now_v7().into(),
        mailbox_id.into(),
        payload.into(),
        is_spam.into(),
    ];
    let statement = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO spam_samples (id, mailbox_id, tokens, is_spam, created_at) \
         VALUES ($1, $2, $3, $4, now())",
        values,
    );
    if let Err(err) = db.execute_raw(statement).await {
        tracing::debug!(error = %err, "failed to record a spam sample");
    }
}

async fn persist(state: &AppState) {
    let Some(engine) = state.spam.clone() else {
        return;
    };
    for scope in engine.classifier().scopes() {
        let trained = engine.classifier().trained(&scope);
        let Some(payload) = engine.classifier().export(&scope) else {
            continue;
        };
        let values: Vec<Value> = vec![
            scope.clone().into(),
            payload.into(),
            (trained as i64).into(),
        ];
        let statement = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO spam_models (scope, payload, trained, updated_at) \
             VALUES ($1, $2, $3, now()) \
             ON CONFLICT (scope) DO UPDATE SET payload = $2, trained = $3, updated_at = now()",
            values,
        );
        if let Err(err) = state.db.execute_raw(statement).await {
            tracing::warn!(error = %err, scope = %scope, "failed to persist a spam model");
        }
    }
}

async fn retrain(state: &AppState) {
    let Some(engine) = state.spam.clone() else {
        return;
    };
    prune(state).await;
    retrain_scope(state, &engine, "global", None).await;
    for mailbox_id in sample_mailboxes(state).await {
        retrain_scope(state, &engine, &mailbox_id.to_string(), Some(mailbox_id)).await;
    }
}

async fn retrain_scope(
    state: &AppState,
    engine: &Arc<SpamEngine>,
    scope: &str,
    mailbox_id: Option<Uuid>,
) {
    let min = engine.classifier().min_samples() as i64;
    let (spam, ham) = sample_counts(state, mailbox_id).await;
    if spam < min || ham < min {
        return;
    }
    let samples = load_samples(state, mailbox_id).await;
    if samples.is_empty() {
        return;
    }
    // FTRL over thousands of samples is CPU work; keep it off the async workers
    // so the SMTP, IMAP and HTTP paths keep responding while it runs.
    let fresh = engine.classifier().fresh();
    let sample_count = samples.len();
    let model = tokio::task::spawn_blocking(move || {
        let mut model = fresh;
        let mut order: Vec<usize> = (0..samples.len()).collect();
        for _ in 0..RETRAIN_EPOCHS {
            order.shuffle(&mut rand::thread_rng());
            for idx in &order {
                let (tokens, is_spam) = &samples[*idx];
                model.learn(tokens, *is_spam);
            }
        }
        model
    })
    .await
    .unwrap_or_else(|_| engine.classifier().fresh());
    engine.classifier().replace(scope, model);
    tracing::info!(scope = %scope, samples = sample_count, "retrained the spam classifier");
}

async fn sample_counts(state: &AppState, mailbox_id: Option<Uuid>) -> (i64, i64) {
    let sql = if mailbox_id.is_some() {
        "SELECT is_spam, count(*)::bigint AS n FROM spam_samples \
         WHERE mailbox_id = $1 GROUP BY is_spam"
    } else {
        "SELECT is_spam, count(*)::bigint AS n FROM spam_samples GROUP BY is_spam"
    };
    let values: Vec<Value> = mailbox_id.map(Into::into).into_iter().collect();
    let statement = Statement::from_sql_and_values(DbBackend::Postgres, sql, values);
    let Ok(rows) = state.db.query_all_raw(statement).await else {
        return (0, 0);
    };
    let (mut spam, mut ham) = (0i64, 0i64);
    for row in rows {
        let is_spam: bool = row.try_get("", "is_spam").unwrap_or(false);
        let count: i64 = row.try_get("", "n").unwrap_or(0);
        if is_spam {
            spam += count;
        } else {
            ham += count;
        }
    }
    (spam, ham)
}

async fn load_samples(state: &AppState, mailbox_id: Option<Uuid>) -> Vec<(Vec<String>, bool)> {
    let sql = if mailbox_id.is_some() {
        "SELECT tokens, is_spam FROM spam_samples WHERE mailbox_id = $1 \
         ORDER BY created_at DESC LIMIT $2"
    } else {
        "SELECT tokens, is_spam FROM spam_samples ORDER BY created_at DESC LIMIT $1"
    };
    let values: Vec<Value> = if mailbox_id.is_some() {
        vec![
            mailbox_id.map(Into::into).unwrap_or(Value::Uuid(None)),
            MAX_SAMPLES_PER_SCOPE.into(),
        ]
    } else {
        vec![MAX_SAMPLES_PER_SCOPE.into()]
    };
    let statement = Statement::from_sql_and_values(DbBackend::Postgres, sql, values);
    let Ok(rows) = state.db.query_all_raw(statement).await else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let tokens: String = row.try_get("", "tokens").ok()?;
            let is_spam: bool = row.try_get("", "is_spam").ok()?;
            let tokens: Vec<String> = serde_json::from_str(&tokens).ok()?;
            Some((tokens, is_spam))
        })
        .collect()
}

async fn sample_mailboxes(state: &AppState) -> Vec<Uuid> {
    let statement = Statement::from_string(
        DbBackend::Postgres,
        "SELECT DISTINCT mailbox_id FROM spam_samples WHERE mailbox_id IS NOT NULL".to_string(),
    );
    let Ok(rows) = state.db.query_all_raw(statement).await else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let id: Option<Uuid> = row.try_get("", "mailbox_id").unwrap_or(None);
            id
        })
        .collect()
}

/// Trim the reservoir to the newest `SAMPLE_CAP` samples.
async fn prune(state: &AppState) {
    let statement = Statement::from_string(
        DbBackend::Postgres,
        format!(
            "DELETE FROM spam_samples WHERE id NOT IN \
             (SELECT id FROM spam_samples ORDER BY created_at DESC LIMIT {SAMPLE_CAP})"
        ),
    );
    if let Err(err) = state.db.execute_raw(statement).await {
        tracing::debug!(error = %err, "failed to prune the spam samples");
    }
}
