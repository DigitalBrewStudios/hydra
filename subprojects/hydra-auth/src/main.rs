#![forbid(unsafe_code)]
#![deny(
    clippy::all,
    clippy::pedantic,
    clippy::expect_used,
    clippy::unwrap_used,
    future_incompatible,
    missing_debug_implementations,
    nonstandard_style,
    missing_copy_implementations,
    unused_qualifications
)]
#![allow(clippy::missing_errors_doc)]

use crate::state::State;

mod config;
mod state;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let _tracing_guard = hydra_tracing::init();
    let cli = config::Cli::new();
    let state = std::sync::Arc::new(State::new(cli).await?);

    loop {
        state.print_state();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    Ok(())
}
