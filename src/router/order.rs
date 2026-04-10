use super::*;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Pending,
    Active,
    Fulfilled,
    Cancelled,
    Disconnected,
    Expired,
    PaidLate,
}

pub(crate) struct Payment {
    pub(crate) address: Address,
    pub(crate) derivation_index: u32,
    pub(crate) amount: Amount,
    pub(crate) timeout: Duration,
}

pub struct Order {
    pub(crate) id: u32,
    pub(crate) target: UpstreamTarget,
    pub(crate) target_work: Option<HashDays>,
    pub(crate) upstream: OnceLock<Arc<Upstream>>,
    pub(crate) allocator: OnceLock<Arc<EnonceAllocator>>,
    pub(crate) cancel: CancellationToken,
    pub(crate) status: Mutex<OrderStatus>,
    pub(crate) payment: Payment,
    pub(crate) created_at: Instant,
    pub(crate) default: bool,
    pub(crate) stratum_sessions: Mutex<Vec<CancellationToken>>,
}

impl Order {
    pub(crate) fn new(
        id: u32,
        target: UpstreamTarget,
        target_work: Option<HashDays>,
        cancel: CancellationToken,
        payment: Payment,
        default: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            target,
            target_work,
            upstream: OnceLock::new(),
            allocator: OnceLock::new(),
            cancel,
            status: Mutex::new(OrderStatus::Pending),
            payment,
            created_at: Instant::now(),
            default,
            stratum_sessions: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn register_session(&self) -> CancellationToken {
        let token = self.cancel.child_token();
        let mut tokens = self.stratum_sessions.lock();
        tokens.retain(|t| !t.is_cancelled());
        tokens.push(token.clone());
        token
    }

    pub(crate) fn trim_sessions(&self, count: usize) {
        let mut tokens = self.stratum_sessions.lock();
        let n = count.min(tokens.len());
        for token in tokens.drain(..n) {
            token.cancel();
        }
    }

    pub(crate) async fn activate(
        &self,
        timeout: Duration,
        enonce1_extension_size: usize,
        tasks: &TaskTracker,
    ) -> Result<()> {
        let upstream =
            Upstream::connect(self.id, &self.target, timeout, self.cancel.clone(), tasks).await?;

        let proxy_extranonces = ProxyExtranonces::new(
            upstream.enonce1().clone(),
            upstream.enonce2_size(),
            enonce1_extension_size,
        )?;

        let allocator = Arc::new(EnonceAllocator::new(
            Extranonces::Proxy(proxy_extranonces),
            self.id,
        ));

        self.upstream
            .set(upstream)
            .map_err(|_| anyhow!("activate called twice"))?;
        self.allocator
            .set(allocator)
            .map_err(|_| anyhow!("activate called twice"))?;

        info!("Upstream {} connected", self.target);

        Ok(())
    }

    pub(crate) fn upstream(&self) -> Option<&Arc<Upstream>> {
        self.upstream.get()
    }

    pub(crate) fn allocator(&self) -> Option<&Arc<EnonceAllocator>> {
        self.allocator.get()
    }

    pub(crate) fn status(&self) -> OrderStatus {
        *self.status.lock()
    }

    pub(crate) fn transition_status(&self, from: OrderStatus, to: OrderStatus) -> bool {
        let mut status = self.status.lock();
        if *status == from {
            *status = to;
            true
        } else {
            false
        }
    }

    pub(crate) fn set_status(&self, status: OrderStatus) {
        *self.status.lock() = status;
    }

    pub(crate) fn mark_active(&self) -> bool {
        self.transition_status(OrderStatus::Pending, OrderStatus::Active)
    }

    pub(crate) fn mark_disconnected_while_pending(&self) -> bool {
        self.transition_status(OrderStatus::Pending, OrderStatus::Disconnected)
    }

    pub(crate) fn ready_for_activation(&self, received: Amount, elapsed: Duration) -> bool {
        let mut status = self.status.lock();

        if elapsed >= self.payment.timeout && *status == OrderStatus::Pending {
            *status = OrderStatus::Expired;
        }

        if received < self.payment.amount {
            return false;
        }

        match *status {
            OrderStatus::Pending => true,
            OrderStatus::Expired => {
                *status = OrderStatus::PaidLate;
                false
            }
            _ => false,
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.status() == OrderStatus::Active
    }

    #[cfg(test)]
    pub(crate) fn session_token_count(&self) -> usize {
        self.stratum_sessions.lock().len()
    }

    pub(crate) fn is_fulfilled(&self) -> bool {
        let target = match self.target_work {
            Some(target) => target,
            None => return false,
        };

        let upstream = match self.upstream.get() {
            Some(upstream) => upstream,
            None => return false,
        };

        upstream.accepted_work().to_hash_days() >= target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_address() -> Address {
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
            .parse::<Address<NetworkUnchecked>>()
            .unwrap()
            .assume_checked()
    }

    fn test_order() -> Arc<Order> {
        Order::new(
            0,
            "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            None,
            CancellationToken::new(),
            Payment {
                address: test_address(),
                derivation_index: 0,
                amount: Amount::from_sat(1000),
                timeout: Duration::from_secs(60),
            },
            false,
        )
    }

    #[test]
    fn ready_for_activation_before_timeout() {
        let order = test_order();

        assert!(order.ready_for_activation(Amount::from_sat(1000), Duration::from_secs(59)));
        assert_eq!(order.status(), OrderStatus::Pending);
    }

    #[test]
    fn payment_after_timeout_is_paid_late() {
        let order = test_order();

        assert!(!order.ready_for_activation(Amount::from_sat(1000), Duration::from_secs(60)));
        assert_eq!(order.status(), OrderStatus::PaidLate);
    }

    #[test]
    fn transitions_require_pending_status() {
        let order = test_order();
        order.set_status(OrderStatus::Cancelled);

        assert!(!order.mark_active());
        assert_eq!(order.status(), OrderStatus::Cancelled);

        assert!(!order.mark_disconnected_while_pending());
        assert_eq!(order.status(), OrderStatus::Cancelled);
    }
}
