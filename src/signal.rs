use {super::*, tokio::signal::ctrl_c};

pub(crate) fn setup_signal_handler() -> CancellationToken {
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};

            let mut sigterm =
                signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
            let mut sighup =
                signal(SignalKind::hangup()).expect("failed to install SIGHUP handler");

            loop {
                tokio::select! {
                    _ = ctrl_c() => {
                        info!("Received shutdown signal (Ctrl-C / SIGINT)");
                        break;
                    }
                    _ = sigterm.recv() => {
                        info!("Received shutdown signal (SIGTERM)");
                        break;
                    }
                    _ = sighup.recv() => {
                        info!("Received SIGHUP, reloading log filter");
                        if let Err(e) = reload_log_filter() {
                            warn!("Failed to reload log filter: {e}");
                        }
                        continue;
                    }
                }
            }
        }

        #[cfg(not(unix))]
        {
            ctrl_c().await.ok();
            info!("Received shutdown signal (Ctrl-C)");
        }

        cancel_clone.cancel();
    });

    cancel
}
