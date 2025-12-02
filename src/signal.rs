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

            tokio::select! {
                _ = ctrl_c() => {
                    info!("Received shutdown signal (Ctrl-C / SIGINT)");
                }
                _ = sigterm.recv() => {
                    info!("Received shutdown signal (SIGTERM)");
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
