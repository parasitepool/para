use super::*;
use crate::subcommand::server::database::{FailedPayout, PendingPayout};

#[derive(Boilerplate, Debug, Deserialize, Serialize, PartialEq)]
pub struct PayoutsHtml {
    pub pending: Vec<PendingPayout>,
    pub failed: Vec<FailedPayout>,
}

impl PageContent for PayoutsHtml {
    fn title(&self) -> String {
        "Payouts".to_string()
    }
}

impl PayoutsHtml {
    pub fn display_combined_total(&self) -> String {
        format_sats(
            self.pending.iter().map(|p| p.amount_sats).sum::<i64>()
                + self.failed.iter().map(|p| p.amount_sats).sum::<i64>(),
        )
    }
    pub fn display_pending_total(&self) -> String {
        format_sats(self.pending.iter().map(|p| p.amount_sats).sum())
    }

    pub fn display_pending_count(&self) -> String {
        self.pending.len().to_string()
    }

    pub fn display_failed_total(&self) -> String {
        format_sats(self.failed.iter().map(|p| p.amount_sats).sum())
    }

    pub fn display_failed_count(&self) -> String {
        self.failed.len().to_string()
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
