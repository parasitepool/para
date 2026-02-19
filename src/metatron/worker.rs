use {super::*, dashmap::DashMap};

pub(crate) struct Worker {
    workername: String,
    clients: DashMap<ClientId, Arc<client::Client>>,
}

impl Worker {
    pub(crate) fn new(workername: String) -> Self {
        Self {
            workername,
            clients: DashMap::new(),
        }
    }

    pub(crate) fn register_client(&self, client_id: ClientId) -> Arc<client::Client> {
        let client = Arc::new(client::Client::new(client_id));
        self.clients.insert(client_id, client.clone());
        client
    }

    pub(crate) fn workername(&self) -> &str {
        &self.workername
    }

    pub(crate) fn connection_count(&self) -> usize {
        self.clients.len()
    }

    pub(crate) fn hashrate_1m(&self) -> HashRate {
        self.clients
            .iter()
            .map(|c| c.hashrate_1m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_5m(&self) -> HashRate {
        self.clients
            .iter()
            .map(|c| c.hashrate_5m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_15m(&self) -> HashRate {
        self.clients
            .iter()
            .map(|c| c.hashrate_15m())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_1hr(&self) -> HashRate {
        self.clients
            .iter()
            .map(|c| c.hashrate_1hr())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_6hr(&self) -> HashRate {
        self.clients
            .iter()
            .map(|c| c.hashrate_6hr())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_1d(&self) -> HashRate {
        self.clients
            .iter()
            .map(|c| c.hashrate_1d())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn hashrate_7d(&self) -> HashRate {
        self.clients
            .iter()
            .map(|c| c.hashrate_7d())
            .fold(HashRate::ZERO, |acc, r| acc + r)
    }

    pub(crate) fn sps_1m(&self) -> f64 {
        self.clients.iter().map(|c| c.sps_1m()).sum()
    }

    pub(crate) fn sps_5m(&self) -> f64 {
        self.clients.iter().map(|c| c.sps_5m()).sum()
    }

    pub(crate) fn sps_15m(&self) -> f64 {
        self.clients.iter().map(|c| c.sps_15m()).sum()
    }

    pub(crate) fn sps_1hr(&self) -> f64 {
        self.clients.iter().map(|c| c.sps_1hr()).sum()
    }

    pub(crate) fn accepted(&self) -> u64 {
        self.clients.iter().map(|c| c.accepted()).sum()
    }

    pub(crate) fn rejected(&self) -> u64 {
        self.clients.iter().map(|c| c.rejected()).sum()
    }

    pub(crate) fn best_ever(&self) -> Option<Difficulty> {
        self.clients.iter().filter_map(|c| c.best_ever()).max()
    }

    pub(crate) fn last_share(&self) -> Option<Instant> {
        self.clients.iter().filter_map(|c| c.last_share()).max()
    }

    pub(crate) fn total_work(&self) -> f64 {
        self.clients.iter().map(|c| c.total_work()).sum()
    }
}
