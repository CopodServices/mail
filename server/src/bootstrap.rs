use crate::state::AppState;
use anyhow::Result;
use mailcore::repo;
use tracing::info;

/// Ensures every configured domain exists and seeds a postmaster mailbox when a
/// bootstrap password is set.
pub async fn seed(state: &AppState) -> Result<()> {
    for name in &state.config.bootstrap_domains {
        let domain = repo::ensure_domain(&state.db, name).await?;
        info!(domain = %domain.name, "domain ready");

        if let Some(password) = &state.config.bootstrap_password {
            let address = format!("postmaster@{}", domain.name);
            if repo::find_mailbox(&state.db, &address).await?.is_none() {
                repo::create_mailbox(&state.db, &address, password).await?;
                info!(%address, "seeded bootstrap mailbox");
            }
        }
    }
    Ok(())
}
