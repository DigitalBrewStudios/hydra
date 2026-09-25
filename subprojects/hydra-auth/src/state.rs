use crate::config::{App, Cli};
use secrecy::ExposeSecret;

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),

    #[error(transparent)]
    Database(#[from] db::Error),
}

#[derive(Debug)]
pub struct State {
    cli: Cli,
    config: App,
    pub db: db::Database,
}

impl State {
    pub async fn new(cli: Cli) -> Result<Self, StateError> {
        let config = App::init(&cli.config_path)?;
        let db =
            db::Database::new(config.db_url.expose_secret(), config.max_db_connections).await?;

        Ok(Self { cli, config, db })
    }

    pub fn print_state(&self) {
        tracing::debug!("state: {self:?}")
    }
}
