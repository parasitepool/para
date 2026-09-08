use super::*;

pub(crate) const DEFAULT_REFUND_FEE_RATE: FeeRate = FeeRate::from_sat_per_kwu(250);

pub(crate) struct Refund {
    pub(crate) psbt: Psbt,
    pub(crate) destination: Address,
    pub(crate) amount: Amount,
    pub(crate) outpoints: Vec<OutPoint>,
    pub(crate) fee_rate: FeeRate,
}

pub(crate) struct Cashier {
    wallet: Arc<Wallet>,
    settings: Arc<Settings>,
}

impl Cashier {
    pub(crate) fn new(wallet: Arc<Wallet>, settings: Arc<Settings>) -> Self {
        Self { wallet, settings }
    }

    pub(crate) fn wallet(&self) -> &Wallet {
        &self.wallet
    }

    pub(crate) fn build_refund(
        &self,
        id: u32,
        derivation_index: u32,
        default_destination: &Address<NetworkUnchecked>,
        fee_rate: Option<FeeRate>,
        destination: Option<Address>,
    ) -> RouterResult<Refund> {
        let wallet = &self.wallet;

        if !wallet.is_synced() {
            return Err(RouterError::WalletSyncing);
        }

        let destination = match destination {
            Some(destination) => destination,
            None => default_destination
                .clone()
                .require_network(self.settings.chain().network())
                .map_err(|_| RouterError::InvalidRefundDestination { id })?,
        };

        let fee_rate = fee_rate.unwrap_or(DEFAULT_REFUND_FEE_RATE);

        let psbt = wallet.build_refund_psbt(id, derivation_index, destination.clone(), fee_rate)?;
        let outpoints = psbt
            .unsigned_tx
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect();

        let amount = psbt.unsigned_tx.output[0].value;

        Ok(Refund {
            psbt,
            destination,
            amount,
            outpoints,
            fee_rate,
        })
    }

    pub(crate) async fn wait_for_payment(
        self: &Arc<Self>,
        order: &Arc<Order>,
        payment: &Payment,
    ) -> RouterResult<bool> {
        let wallet = &self.wallet;
        let mut sync_rx = wallet.subscribe_sync();

        loop {
            let snapshot = sync_rx.borrow_and_update().snapshot();
            if let Some(snapshot) = snapshot {
                let total = snapshot.received(payment.derivation_index);

                let timeout = if total >= payment.amount {
                    EXTENDED_PAYMENT_TIMEOUT
                } else {
                    PAYMENT_TIMEOUT
                };

                let deadline_height = payment.created_at_height.saturating_add(timeout);

                let confirmed_by_deadline =
                    snapshot.received_by_deadline(payment.derivation_index, deadline_height);

                let timed_out = snapshot.tip() >= deadline_height;

                {
                    let lifecycle = order.lifecycle.lock();

                    if confirmed_by_deadline >= payment.amount
                        && lifecycle.status.awaiting_payment()
                    {
                        return Ok(true);
                    }

                    if timed_out {
                        drop(lifecycle);
                        order.terminate(OrderStatus::Expired);
                        return Ok(false);
                    }
                }

                order.note_payment_seen(total >= payment.amount);
            }

            tokio::select! {
                biased;
                _ = order.cancel.cancelled() => return Ok(false),
                changed = sync_rx.changed() => {
                    if changed.is_err() {
                        return Ok(false);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::router::testkit::*};

    fn payment_fixture(
        status: OrderStatus,
    ) -> (TestRouter, Arc<Wallet>, bdk_wallet::AddressInfo, Arc<Order>) {
        let router = test_router();
        let wallet = router.wallet.clone().unwrap();
        let address = wallet.test_reveal_address();
        let order = test_order_with_payment(
            0,
            Payment::new(
                address.address.clone(),
                address.index,
                Amount::from_sat(1000),
                wallet.tip(),
            ),
            status,
            &router.metatron,
        );
        (router, wallet, address, order)
    }

    fn spawn_waiter(router: &TestRouter, order: &Arc<Order>) -> task::JoinHandle<bool> {
        let monitored = order.clone();
        let router = router.router.clone();
        tokio::spawn(async move {
            router
                .wait_for_payment(&monitored, payment(&monitored))
                .await
                .unwrap()
        })
    }

    #[test]
    fn build_refund_constructs_unsigned_psbt() {
        let router = test_router();
        let wallet = router.wallet.clone().unwrap();

        wallet.mark_synced();

        let funded = wallet.test_reveal_address();
        let tx = wallet.test_receive_unconfirmed(&funded.address, Amount::from_sat(10_000));

        wallet.test_confirm_tx(tx);
        wallet.mark_synced();

        add_orders(
            router.as_ref(),
            [funded_bucket_order(0, &funded, 10_000, &router.metatron)],
        );

        let refund = router
            .build_refund(0, Some(FeeRate::from_sat_per_vb(1).unwrap()), None)
            .unwrap();

        assert_eq!(refund.amount, refund.psbt.unsigned_tx.output[0].value);
        assert!(refund.amount < Amount::from_sat(10_000));
        assert_eq!(refund.outpoints.len(), 1);
        assert_eq!(
            refund.destination.to_string(),
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
        );
        assert_eq!(refund.psbt.unsigned_tx.input.len(), 1);
        assert_eq!(
            refund.psbt.unsigned_tx.input[0].previous_output,
            refund.outpoints[0],
        );
        assert!(refund.psbt.inputs[0].final_script_witness.is_none());

        let cold = wallet.test_reveal_address();
        let tx = wallet.test_receive_unconfirmed(&cold.address, Amount::from_sat(2000));
        wallet.test_confirm_tx(tx);
        wallet.mark_synced();

        let order = funded_bucket_order(1, &cold, 2000, &router.metatron);
        router.book.add_cold(1, order.to_entry());

        let override_destination = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"
            .parse::<Address<NetworkUnchecked>>()
            .unwrap()
            .require_network(Network::Bitcoin)
            .unwrap();

        let refund = router
            .build_refund(
                1,
                Some(FeeRate::from_sat_per_vb(1).unwrap()),
                Some(override_destination.clone()),
            )
            .unwrap();

        assert_eq!(refund.amount, refund.psbt.unsigned_tx.output[0].value);
        assert!(refund.amount < Amount::from_sat(2000));
        assert_eq!(refund.destination, override_destination);
    }

    #[test]
    fn build_refund_errors() {
        let fee_rate = FeeRate::from_sat_per_vb(1).unwrap();

        let router = test_router();
        let wallet = router.wallet.clone().unwrap();

        assert!(matches!(
            router.build_refund(99, Some(fee_rate), None),
            Err(RouterError::OrderNotFound { id: 99 })
        ));

        add_orders(
            router.as_ref(),
            [test_order(
                0,
                None,
                OrderStatus::Cancelled,
                &router.metatron,
            )],
        );

        assert!(matches!(
            router.build_refund(0, Some(fee_rate), None),
            Err(RouterError::NotABucketOrder { id: 0 })
        ));

        let unfunded = wallet.test_reveal_address();

        add_orders(
            router.as_ref(),
            [test_order_with_payment(
                1,
                Payment::new(
                    unfunded.address.clone(),
                    unfunded.index,
                    Amount::from_sat(1000),
                    0,
                ),
                OrderStatus::Expired,
                &router.metatron,
            )],
        );

        assert!(matches!(
            router.build_refund(1, Some(fee_rate), None),
            Err(RouterError::WalletSyncing)
        ));

        wallet.mark_synced();

        assert!(matches!(
            router.build_refund(1, Some(fee_rate), Some(test_address())),
            Err(RouterError::NoUnspentFunds { id: 1 })
        ));

        let funded = wallet.test_reveal_address();
        let tx = wallet.test_receive_unconfirmed(&funded.address, Amount::from_sat(1000));
        wallet.test_confirm_tx(tx);
        wallet.mark_synced();

        add_orders(
            router.as_ref(),
            [test_order_with_payment(
                2,
                Payment::new(
                    funded.address.clone(),
                    funded.index,
                    Amount::from_sat(1000),
                    0,
                ),
                OrderStatus::Expired,
                &router.metatron,
            )],
        );

        assert!(matches!(
            router.build_refund(2, Some(fee_rate), None),
            Err(RouterError::InvalidRefundDestination { id: 2 })
        ));

        let router = test_router_with_wallet(None);

        add_orders(
            router.as_ref(),
            [test_order_with_payment(
                0,
                Payment::new(test_address(), 0, Amount::from_sat(1000), 0),
                OrderStatus::Expired,
                &router.metatron,
            )],
        );

        assert!(matches!(
            router.build_refund(0, Some(fee_rate), None),
            Err(RouterError::WalletRequired)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_expires_instead_of_activating_after_extended_timeout() {
        let (router, wallet, address, order) = payment_fixture(OrderStatus::Pending);

        wallet.test_receive_unconfirmed(&address.address, payment(&order).amount);
        wallet.test_advance_tip_to(EXTENDED_PAYMENT_TIMEOUT);
        wallet.mark_synced();

        assert!(
            !router
                .wait_for_payment(&order, payment(&order))
                .await
                .unwrap()
        );
        assert_eq!(order.status(), OrderStatus::Expired);
        assert!(order.cancel.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_partial_payment_does_not_extend_deadline() {
        let (router, wallet, address, order) = payment_fixture(OrderStatus::Pending);

        wallet.test_receive_unconfirmed(&address.address, Amount::from_sat(500));
        wallet.test_advance_tip_to(PAYMENT_TIMEOUT);
        wallet.mark_synced();

        assert!(
            !router
                .wait_for_payment(&order, payment(&order))
                .await
                .unwrap()
        );
        assert_eq!(order.status(), OrderStatus::Expired);
        assert!(order.cancel.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_extends_deadline_while_payment_in_mempool() {
        let (router, wallet, address, order) = payment_fixture(OrderStatus::Pending);

        wallet.test_receive_unconfirmed(&address.address, payment(&order).amount);
        wallet.test_advance_tip_to(PAYMENT_TIMEOUT);
        wallet.mark_synced();

        let waiter = spawn_waiter(&router, &order);

        tokio::task::yield_now().await;
        assert_eq!(order.status(), OrderStatus::InMempool);
        assert!(!waiter.is_finished());

        order.cancel.cancel();
        assert!(!waiter.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_expires_promptly_after_payment_disappears() {
        let (router, wallet, address, order) = payment_fixture(OrderStatus::Pending);

        let tx = wallet.test_receive_unconfirmed(&address.address, payment(&order).amount);
        wallet.mark_synced();

        let waiter = spawn_waiter(&router, &order);

        tokio::task::yield_now().await;
        assert_eq!(order.status(), OrderStatus::InMempool);

        wallet.test_evict_tx(&tx);
        wallet.test_advance_tip_to(PAYMENT_TIMEOUT);
        wallet.mark_synced();

        assert!(!waiter.await.unwrap());
        assert_eq!(order.status(), OrderStatus::Expired);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_requires_wallet_for_bucket_order() {
        let router = test_router_with_wallet(None);
        let order = test_order_with_payment(
            0,
            Payment::new(test_address(), 0, Amount::from_sat(1000), 0),
            OrderStatus::Pending,
            &router.metatron,
        );

        assert!(matches!(
            router.wait_for_payment(&order, payment(&order)).await,
            Err(RouterError::WalletRequired),
        ));
        assert_eq!(order.status(), OrderStatus::Pending);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_marks_pending_order_in_mempool_before_confirmation() {
        let (router, wallet, address, order) = payment_fixture(OrderStatus::Pending);

        wallet.test_receive_unconfirmed(&address.address, payment(&order).amount);
        wallet.mark_synced();

        let waiter = spawn_waiter(&router, &order);

        tokio::task::yield_now().await;
        assert_eq!(order.status(), OrderStatus::InMempool);

        order.cancel.cancel();
        assert!(!waiter.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_returns_in_mempool_to_pending_when_total_is_below_amount() {
        let (router, wallet, _address, order) = payment_fixture(OrderStatus::InMempool);
        wallet.mark_synced();

        let waiter = spawn_waiter(&router, &order);

        tokio::task::yield_now().await;
        assert_eq!(order.status(), OrderStatus::Pending);

        order.cancel.cancel();
        assert!(!waiter.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_activates_when_confirmed_at_timeout() {
        let (router, wallet, address, order) = payment_fixture(OrderStatus::Pending);

        let tx = wallet.test_receive_unconfirmed(&address.address, payment(&order).amount);
        wallet.mark_synced();

        let waiter = spawn_waiter(&router, &order);

        tokio::task::yield_now().await;
        assert_eq!(order.status(), OrderStatus::InMempool);

        wallet.test_confirm_tx(tx);
        wallet.test_advance_tip_to(PAYMENT_TIMEOUT);
        wallet.mark_synced();

        assert!(waiter.await.unwrap());
        assert_eq!(order.status(), OrderStatus::InMempool);
        assert!(!order.cancel.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_expires_when_payment_confirms_after_extended_timeout() {
        let (router, wallet, address, order) = payment_fixture(OrderStatus::Pending);

        let tx = wallet.test_receive_unconfirmed(&address.address, payment(&order).amount);
        wallet.test_advance_tip_to(EXTENDED_PAYMENT_TIMEOUT);
        wallet.test_confirm_tx(tx);
        wallet.mark_synced();

        assert!(
            !router
                .wait_for_payment(&order, payment(&order))
                .await
                .unwrap()
        );
        assert_eq!(order.status(), OrderStatus::Expired);
        assert!(order.cancel.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_payment_activates_when_confirmed_within_extended_deadline() {
        let (router, wallet, address, order) = payment_fixture(OrderStatus::Pending);

        let tx = wallet.test_receive_unconfirmed(&address.address, payment(&order).amount);
        wallet.mark_synced();

        let waiter = spawn_waiter(&router, &order);

        tokio::task::yield_now().await;
        assert_eq!(order.status(), OrderStatus::InMempool);

        wallet.test_advance_tip_to(PAYMENT_TIMEOUT + 10);
        wallet.test_confirm_tx(tx);
        wallet.mark_synced();

        assert!(waiter.await.unwrap());
        assert!(!order.cancel.is_cancelled());
    }
}
