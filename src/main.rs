use std::sync::Arc;

use shortener::config::Config;
use shortener::state::{connect_db, AppState};
use shortener::web::app_router;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load optional local .env (production uses real environment/secrets).
    let _ = dotenvy::dotenv();

    let config = Config::from_env()?;

    init_tracing(&config);

    let db = connect_db(&config.database_url).await?;
    tracing::info!(
        env = ?config.env,
        bind = %config.bind,
        base_url = %config.base_url,
        "shortener starting"
    );

    let state = AppState {
        db: db.clone(),
        config: Arc::clone(&config),
    };
    let app = app_router(state);

    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("shortener stopped");
    Ok(())
}

fn init_tracing(config: &Config) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let env_filter = EnvFilter::try_new(filter).unwrap_or_else(|_| EnvFilter::new("info"));
    // NOTE: request middleware logs method/path/status only. Authorization,
    // Cookie, CSRF tokens, and destination URLs are never logged.
    if config.env.is_production() {
        fmt()
            .json()
            .with_env_filter(env_filter)
            .with_target(false)
            .init();
    } else {
        fmt()
            .pretty()
            .with_env_filter(env_filter)
            .with_target(false)
            .init();
    }
}

async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    #[cfg(unix)]
    {
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler");
        tokio::select! {
            _ = term.recv() => {},
            _ = int.recv() => {},
            _ = tokio::signal::ctrl_c() => {},
        }
        tracing::info!("shutdown signal received");
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
