use super::*;
use crate::subcommand::server::database::SimulatedPayout;

#[derive(Boilerplate, Debug, Deserialize, Serialize, PartialEq)]
pub struct SimulatePayoutsHtml {
    pub payouts: Vec<SimulatedPayout>,
    pub coinbase_value: i64,
    pub finder_username: String,
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
        if self.finder_username.is_empty() {
            format!(
                "/payouts/simulate?format=json&coinbase_value={}",
                self.coinbase_value
            )
        } else {
            format!(
                "/payouts/simulate?format=json&coinbase_value={}&finder_username={}",
                self.coinbase_value, self.finder_username
            )
        }
    }

    pub fn csv_url(&self) -> String {
        if self.finder_username.is_empty() {
            format!(
                "/payouts/simulate?format=csv&coinbase_value={}",
                self.coinbase_value
            )
        } else {
            format!(
                "/payouts/simulate?format=csv&coinbase_value={}&finder_username={}",
                self.coinbase_value, self.finder_username
            )
        }
    }
}

pub fn format_sats(sats: i64) -> String {
    if sats >= 100_000_000 {
        format!("{:.3} BTC", sats as f64 / 100_000_000.0)
    } else if sats >= 1_000_000 {
        format!("{:.2}M sats", sats as f64 / 1_000_000.0)
    } else if sats >= 1_000 {
        format!("{:.2}K sats", sats as f64 / 1_000.0)
    } else {
        format!("{} sats", sats)
    }
}
