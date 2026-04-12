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

pub struct Order {
    pub(crate) id: u32,
    pub(crate) upstream_target: UpstreamTarget,
    pub(crate) hashdays: Option<HashDays>,
    pub(crate) upstream: OnceLock<Arc<Upstream>>,
    pub(crate) allocator: OnceLock<Arc<EnonceAllocator>>,
    pub(crate) status: Mutex<OrderStatus>,
    pub(crate) payment_address: Address,
    pub(crate) payment_derivation_index: u32,
    pub(crate) payment_amount: Amount,
    pub(crate) payment_timeout: Duration,
    pub(crate) created_at: Instant,
    pub(crate) cancel: CancellationToken,
    pub(crate) stratum_cancel: Mutex<Vec<CancellationToken>>,
}

impl Order {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: u32,
        upstream_target: UpstreamTarget,
        hashdays: Option<HashDays>,
        payment_address: Address,
        payment_derivation_index: u32,
        payment_amount: Amount,
        payment_timeout: Duration,
        cancel: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            upstream_target,
            hashdays,
            upstream: OnceLock::new(),
            allocator: OnceLock::new(),
            status: Mutex::new(OrderStatus::Pending),
            payment_address,
            payment_derivation_index,
            payment_amount,
            payment_timeout,
            created_at: Instant::now(),
            cancel,
            stratum_cancel: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn is_default(&self) -> bool {
        self.hashdays.is_none()
    }

    pub(crate) fn hashrate_1m(&self, metatron: &Metatron, now: Instant) -> HashRate {
        self.upstream()
            .map(|upstream| metatron.upstream_stats(upstream.id(), now).hashrate_1m(now))
            .unwrap_or(HashRate::ZERO)
    }

    pub(crate) fn register_session(&self) -> CancellationToken {
        let cancel = self.cancel.child_token();
        let mut cancel_tokens = self.stratum_cancel.lock();
        cancel_tokens.retain(|token| !token.is_cancelled());
        cancel_tokens.push(cancel.clone());
        cancel
    }

    pub(crate) fn trim_sessions(&self, count: usize) {
        let mut cancel_tokens = self.stratum_cancel.lock();
        let before = cancel_tokens.len();
        let n = count.min(cancel_tokens.len());

        if n == 0 {
            return;
        }

        info!(
            "Trimming {n} session(s) from order {} at {} (sessions {} -> {})",
            self.id,
            self.upstream_target,
            before,
            before - n,
        );

        for token in cancel_tokens.drain(..n) {
            token.cancel();
        }
    }

    pub(crate) async fn activate(
        &self,
        timeout: Duration,
        enonce1_extension_size: usize,
        tasks: &TaskTracker,
    ) -> Result<()> {
        let upstream = Upstream::connect(
            self.id,
            &self.upstream_target,
            timeout,
            self.cancel.clone(),
            tasks,
        )
        .await?;

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

        info!("Upstream {} connected", self.upstream_target);

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

    pub(crate) fn transition(&self, from: OrderStatus, to: OrderStatus) -> bool {
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

    pub(crate) fn ready_for_activation(&self, received: Amount, elapsed: Duration) -> bool {
        if elapsed >= self.payment_timeout {
            self.transition(OrderStatus::Pending, OrderStatus::Expired);
        }

        if received < self.payment_amount {
            return false;
        }

        match self.status() {
            OrderStatus::Pending => true,
            OrderStatus::Expired => {
                self.transition(OrderStatus::Expired, OrderStatus::PaidLate);
                false
            }
            _ => false,
        }
    }

    pub(crate) fn remaining_work(&self) -> Option<HashDays> {
        let target = self.hashdays?;
        let remaining = target.to_total_work() - self.upstream()?.accepted_work();
        (remaining != TotalWork::ZERO).then(|| remaining.to_hash_days())
    }

    pub(crate) fn is_fulfilled(&self) -> bool {
        let target = match self.hashdays {
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
