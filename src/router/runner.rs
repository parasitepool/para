use super::*;

pub(crate) struct OrderRunner {
    settings: Arc<Settings>,
    cashier: Option<Arc<Cashier>>,
    tasks: TaskTracker,
    cancel: CancellationToken,
}

impl OrderRunner {
    pub(crate) fn new(
        settings: Arc<Settings>,
        cashier: Option<Arc<Cashier>>,
        tasks: TaskTracker,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            settings,
            cashier,
            tasks,
            cancel,
        }
    }

    pub(crate) fn spawn(self: &Arc<Self>, order: Arc<Order>) {
        if order.status().is_terminal() {
            return;
        }

        let runner = self.clone();

        self.tasks.spawn(async move {
            if let Err(err) = runner.execute(&order).await {
                error!("Order {} execution error: {err}", order.id);
            }
        });
    }

    pub(crate) async fn execute(self: &Arc<Self>, order: &Arc<Order>) -> RouterResult<()> {
        if !self.await_payment(order).await? {
            return Ok(());
        }

        let check_interval = self.settings.tick_interval();

        loop {
            match retry_with_backoff(&order.cancel, &format!("Order {}", order.id), || {
                order.connect(
                    self.settings.timeout(),
                    self.settings.enonce1_extension_size(),
                    &self.tasks,
                )
            })
            .await
            {
                Ok(()) => {}
                Err(BackoffEnd::Cancelled) => {
                    self.terminate_unless_shutting_down(order);

                    return Ok(());
                }
                Err(BackoffEnd::Exhausted) => {
                    order.terminate(OrderStatus::Disconnected);

                    return Ok(());
                }
            }

            let upstream = order
                .upstream()
                .ok_or(RouterError::MissingActiveUpstream { id: order.id })?;

            tokio::select! {
                biased;
                _ = order.cancel.cancelled() => {
                    self.terminate_unless_shutting_down(order);

                    return Ok(());
                }
                _ = upstream.disconnected() => {
                    warn!(
                        "Upstream {} disconnected, attempting reconnect for order {}",
                        upstream.endpoint(),
                        order.id,
                    );

                    order.cancel_all_sessions();

                    if order.is_fulfilled() {
                        Self::fulfill(order);

                        return Ok(());
                    }

                    continue;
                }
                _ = async {
                    let mut ticker = ticker(check_interval);
                    while !order.is_fulfilled() {
                        ticker.tick().await;
                    }
                } => {
                    Self::fulfill(order);

                    return Ok(());
                }
            }
        }
    }

    async fn await_payment(&self, order: &Arc<Order>) -> RouterResult<bool> {
        let Some(bucket) = &order.bucket else {
            return Ok(true);
        };

        let cashier = self.cashier.as_ref().ok_or(RouterError::WalletRequired)?;

        tokio::select! {
            biased;
            _ = order.cancel.cancelled() => {
                self.terminate_unless_shutting_down(order);
                return Ok(false);
            }
            synced = cashier.wallet().synced() => {
                if !synced {
                    return Ok(false);
                }
            }
        }

        if order.status().awaiting_payment()
            && !cashier.wait_for_payment(order, &bucket.payment).await?
        {
            return Ok(false);
        }

        Ok(true)
    }

    fn terminate_unless_shutting_down(&self, order: &Order) {
        if !self.cancel.is_cancelled() {
            order.terminate(OrderStatus::Cancelled);
        }
    }

    fn fulfill(order: &Order) {
        info!("Order {} fulfilled", order.id);
        order.terminate(OrderStatus::Fulfilled);
    }
}
