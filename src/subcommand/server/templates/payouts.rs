use {
    super::*,
    crate::subcommand::server::database::{FailedPayout, PendingPayout},
};

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
