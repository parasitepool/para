use {super::*, crate::subcommand::server::database::PendingPayout};

#[derive(Boilerplate, Debug, Deserialize, Serialize, PartialEq)]
pub struct SimulatePayoutsHtml {
    pub payouts: Vec<PendingPayout>,
    pub coinbase_value: i64,
    pub winner_address: String,
}

impl PageContent for SimulatePayoutsHtml {
    fn title(&self) -> String {
        "Simulate Payouts".to_string()
    }
}

impl SimulatePayoutsHtml {
    pub fn display_total(&self) -> String {
        format_sats(self.payouts.iter().map(|p| p.amount_sats).sum())
    }

    pub fn display_count(&self) -> String {
        self.payouts.len().to_string()
    }

    pub fn display_coinbase_value(&self) -> String {
        format_sats(self.coinbase_value)
    }

    pub fn json_url(&self) -> String {
        if self.winner_address.is_empty() {
            format!(
                "/payouts/simulate?format=json&coinbase_value={}",
                self.coinbase_value
            )
        } else {
            format!(
                "/payouts/simulate?format=json&coinbase_value={}&winner_address={}",
                self.coinbase_value, self.winner_address
            )
        }
    }

    pub fn csv_url(&self) -> String {
        if self.winner_address.is_empty() {
            format!(
                "/payouts/simulate?format=csv&coinbase_value={}",
                self.coinbase_value
            )
        } else {
            format!(
                "/payouts/simulate?format=csv&coinbase_value={}&winner_address={}",
                self.coinbase_value, self.winner_address
            )
        }
    }
}
