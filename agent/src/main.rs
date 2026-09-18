mod checks;
mod config;
mod docker;
mod host;
mod load;
mod push;

use crate::config::AgentConfig;
use std::time::Duration;
use tracing::{error, info, warn};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cfg = match AgentConfig::load() {
        Ok(c) => c,
        Err(err) => {
            eprintln!("config error: {err}");
            std::process::exit(1);
        }
    };

    info!(
        host = %cfg.host_name,
        system_interval = cfg.system_interval_secs,
        service_interval = cfg.service_interval_secs,
        docker = cfg.docker_enabled,
        "agent_started"
    );

    let cfg = std::sync::Arc::new(cfg);
    let push_client = push::PushClient::new(cfg.clone());

    let system_cfg = cfg.clone();
    let system_push = push_client.clone();
    let system_handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(system_cfg.system_interval_secs));
        loop {
            ticker.tick().await;
            match host::sample(&system_cfg).await {
                Ok(sample) => {
                    if let Err(err) = system_push.send_system(sample).await {
                        warn!(error = format!("{err:#}"), "system_push_failed");
                    }
                }
                Err(err) => warn!(error = %err, "system_sample_failed"),
            }
        }
    });

    let service_cfg = cfg.clone();
    let service_push = push_client;
    let service_handle = tokio::spawn(async move {
        let mut ticker =
            tokio::time::interval(Duration::from_secs(service_cfg.service_interval_secs));
        loop {
            ticker.tick().await;
            let mut services = Vec::new();

            if service_cfg.docker_enabled {
                match docker::collect_checks(&service_cfg).await {
                    Ok(mut items) => services.append(&mut items),
                    Err(err) => warn!(error = %err, "docker_discover_failed"),
                }
            }

            match checks::run_configured(&service_cfg).await {
                Ok(mut items) => services.append(&mut items),
                Err(err) => warn!(error = %err, "bare_metal_checks_failed"),
            }

            if services.is_empty() {
                continue;
            }

            if let Err(err) = service_push.send_services(services).await {
                warn!(error = format!("{err:#}"), "service_push_failed");
            }
        }
    });

    tokio::select! {
        r = system_handle => {
            if let Err(err) = r {
                error!(error = %err, "system_task_crashed");
            }
        }
        r = service_handle => {
            if let Err(err) = r {
                error!(error = %err, "service_task_crashed");
            }
        }
        _ = shutdown_signal() => {
            info!("agent_shutdown");
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
