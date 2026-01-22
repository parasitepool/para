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

/// Synchronous signal handler for use outside of tokio runtime (e.g., GUI)
pub(crate) fn setup_signal_handler_sync() -> CancellationToken {
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    std::thread::spawn(move || {
        #[cfg(unix)]
        {
            use std::sync::atomic::{AtomicBool, Ordering};
            static SIGNALED: AtomicBool = AtomicBool::new(false);

            unsafe {
                libc::signal(libc::SIGINT, handle_signal as usize);
                libc::signal(libc::SIGTERM, handle_signal as usize);
            }

            extern "C" fn handle_signal(_: i32) {
                SIGNALED.store(true, Ordering::SeqCst);
            }

            while !SIGNALED.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            info!("Received shutdown signal");
        }

        #[cfg(not(unix))]
        {
            // On non-unix, just wait indefinitely (window close will exit)
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }

        cancel_clone.cancel();
    });

    cancel
}
