use {super::*, tokio::signal::ctrl_c};

pub(crate) fn setup_signal_handler() -> CancellationToken {
    let token = CancellationToken::new();
    let token_clone = token.clone();

    tokio::spawn(async move {
        if ctrl_c().await.is_ok() {
            info!("Received shutdown signal (ctrl-c)");
            token_clone.cancel();
        }
    });

    token
}
