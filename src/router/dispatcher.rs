use {super::*, crate::event_sink::Event};

pub(crate) struct Dispatcher {
    settings: Arc<Settings>,
    metatron: Arc<Metatron>,
    tasks: TaskTracker,
}

impl Dispatcher {
    pub(crate) fn new(
        settings: Arc<Settings>,
        metatron: Arc<Metatron>,
        tasks: TaskTracker,
    ) -> Self {
        Self {
            settings,
            metatron,
            tasks,
        }
    }

    pub(crate) async fn serve(
        self: &Arc<Self>,
        listener: TcpListener,
        event_tx: Option<mpsc::Sender<Event>>,
        select: impl Fn(SocketAddr, &Prelude) -> Option<Arc<Order>> + Send + Sync + 'static,
        on_shutdown: impl Fn() -> Result + Send + Sync + 'static,
        cancel_token: CancellationToken,
    ) -> Result {
        let select = Arc::new(select);

        loop {
            let (stream, addr) = tokio::select! {
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, addr)) => (stream, addr),
                        Err(err) => {
                            error!("Accept error: {err}");
                            continue;
                        }
                    }
                }
                _ = cancel_token.cancelled() => {
                    info!("Shutting down stratum listener");

                    self.tasks.close();
                    let _ = timeout(Duration::from_secs(2), self.tasks.wait()).await;

                    if let Err(err) = on_shutdown() {
                        warn!("Final persistence error: {err}");
                    }

                    info!("All listener tasks stopped");

                    return Ok(());
                }
            };

            let _ = stream.set_nodelay(true);

            let event_tx = event_tx.clone();
            let select = select.clone();
            let dispatcher = self.clone();

            self.tasks.spawn(async move {
                let (read_half, write_half) = stream.into_split();

                let reader =
                    FramedRead::new(read_half, LinesCodec::new_with_max_length(MAX_MESSAGE_SIZE));

                let writer = FramedWrite::new(write_half, LinesCodec::new());

                let Some((reader, prelude)) = greet(reader, addr).await else {
                    return;
                };

                let Some(order) = select(addr, &prelude) else {
                    warn!("No order to match with available, dropping connection from {addr}");
                    return;
                };

                let order_type = if order.is_sink() { "sink" } else { "bucket" };

                info!(
                    "Routing {addr} to {order_type} order {} at {}",
                    order.id, order.upstream_target,
                );

                let settings = dispatcher.settings.clone();
                let metatron = dispatcher.metatron.clone();
                let start_diff = settings.start_diff();
                let cancel = order.cancel.child_token();

                let Some((upstream, allocator)) = order.upstream_route() else {
                    error!("Dropping {addr}: order {} has no upstream route", order.id);
                    order.release_placement(&addr);
                    return;
                };

                let stratifier: Stratifier<Notify> = Stratifier::new(
                    addr,
                    settings,
                    allocator,
                    metatron,
                    Some(upstream.clone()),
                    reader,
                    writer,
                    prelude.inbox,
                    upstream.workbase_rx(),
                    cancel,
                    event_tx,
                    start_diff,
                    Some(order.clone()),
                );

                if let Err(err) = stratifier.serve().await {
                    error!("Stratifier error for {addr} on order {}: {err}", order.id);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::router::testkit::*, tokio::io::AsyncBufReadExt};

    #[tokio::test]
    async fn serve_drops_connection_and_releases_placement_when_order_has_no_upstream_route() {
        let router = test_router();
        let order = test_order(0, None, OrderStatus::Active, &router.metatron);
        *order.upstream.lock() = None;
        add_orders(router.as_ref(), [order.clone()]);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let dispatcher = Arc::new(Dispatcher::new(
            router.settings.clone(),
            router.metatron.clone(),
            TaskTracker::new(),
        ));

        let cancel = CancellationToken::new();
        let server_cancel = cancel.clone();

        let select_router = router.router.clone();
        let server = tokio::spawn(async move {
            dispatcher
                .serve(
                    listener,
                    None,
                    move |addr, prelude| select_router.next_order(addr, prelude),
                    || Ok(()),
                    server_cancel,
                )
                .await
                .unwrap();
        });

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(
            &mut stream,
            b"{\"id\":1,\"method\":\"mining.subscribe\",\"params\":[\"foo\"]}\n",
        )
        .await
        .unwrap();
        let mut reader = tokio::io::BufReader::new(stream);

        timeout(Duration::from_secs(5), async {
            loop {
                if order.placements.lock().is_empty() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("placement should be released after connection is dropped");

        let mut line = String::new();
        let read = timeout(Duration::from_secs(5), reader.read_line(&mut line))
            .await
            .expect("client should observe the drop")
            .unwrap();
        assert_eq!(
            (read, line.as_str()),
            (0, ""),
            "expected EOF after dropped connection"
        );

        router.cancel.cancel();
        cancel.cancel();
        timeout(Duration::from_secs(5), server)
            .await
            .expect("dispatcher should shut down")
            .unwrap();
    }
}
