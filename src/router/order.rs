use super::*;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderStatus {
    Pending,
    Active,
    Fulfilled,
    Cancelled,
    Disconnected,
    Expired,
    #[serde(rename = "paid_late")]
    PaidLate,
}

impl OrderStatus {
    pub(super) fn to_u8(self) -> u8 {
        match self {
            Self::Pending => 0,
            Self::Active => 1,
            Self::Fulfilled => 2,
            Self::Cancelled => 3,
            Self::Disconnected => 4,
            Self::Expired => 5,
            Self::PaidLate => 6,
        }
    }

    pub(super) fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Pending,
            1 => Self::Active,
            2 => Self::Fulfilled,
            3 => Self::Cancelled,
            4 => Self::Disconnected,
            5 => Self::Expired,
            6 => Self::PaidLate,
            _ => Self::Pending,
        }
    }
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
    pub(super) upstream: OnceLock<Arc<Upstream>>,
    pub(super) allocator: OnceLock<Arc<EnonceAllocator>>,
    pub(crate) cancel: CancellationToken,
    pub(super) status: AtomicU8,
    pub(crate) address: Address,
    pub(crate) derivation_index: u32,
    pub(crate) amount: Amount,
    pub(crate) created_at: Instant,
    pub(crate) timeout: Duration,
}

impl Order {
    pub(crate) fn new(
        id: u32,
        target: UpstreamTarget,
        target_work: Option<HashDays>,
        cancel: CancellationToken,
        payment: Payment,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            target,
            target_work,
            upstream: OnceLock::new(),
            allocator: OnceLock::new(),
            cancel,
            status: AtomicU8::new(OrderStatus::Pending.to_u8()),
            address: payment.address,
            derivation_index: payment.derivation_index,
            amount: payment.amount,
            created_at: Instant::now(),
            timeout: payment.timeout,
        })
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
        OrderStatus::from_u8(self.status.load(Ordering::Relaxed))
    }

    pub(crate) fn transition_status(&self, from: OrderStatus, to: OrderStatus) -> bool {
        self.status
            .compare_exchange(
                from.to_u8(),
                to.to_u8(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    pub(crate) fn set_status(&self, status: OrderStatus) {
        self.status.store(status.to_u8(), Ordering::Relaxed);
    }

    pub(crate) fn mark_active(&self) -> bool {
        self.transition_status(OrderStatus::Pending, OrderStatus::Active)
    }

    pub(crate) fn mark_disconnected_while_pending(&self) -> bool {
        self.transition_status(OrderStatus::Pending, OrderStatus::Disconnected)
    }

    pub(crate) fn ready_for_activation(&self, received: Amount, elapsed: Duration) -> bool {
        if elapsed >= self.timeout {
            let _ = self.transition_status(OrderStatus::Pending, OrderStatus::Expired);
        }

        if received < self.amount {
            return false;
        }

        match self.status() {
            OrderStatus::Pending => true,
            OrderStatus::Expired => {
                let _ = self.transition_status(OrderStatus::Expired, OrderStatus::PaidLate);
                false
            }
            _ => false,
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.status() == OrderStatus::Active
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
            "foo@bar:3333".parse().unwrap(),
            None,
            CancellationToken::new(),
            Payment {
                address: test_address(),
                derivation_index: 0,
                amount: Amount::from_sat(1000),
                timeout: Duration::from_secs(60),
            },
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
    fn activation_requires_pending_status() {
        let order = test_order();
        order.set_status(OrderStatus::Cancelled);

        assert!(!order.mark_active());
        assert_eq!(order.status(), OrderStatus::Cancelled);
    }

    #[test]
    fn pending_disconnect_requires_pending_status() {
        let order = test_order();
        order.set_status(OrderStatus::Cancelled);

        assert!(!order.mark_disconnected_while_pending());
        assert_eq!(order.status(), OrderStatus::Cancelled);
    }
}
