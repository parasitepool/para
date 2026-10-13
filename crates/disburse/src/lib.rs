use {
    bitcoin::{
        Address, Amount, FeeRate, Network, OutPoint, Psbt, ScriptBuf, Sequence, Transaction, TxIn,
        TxOut, Weight, Witness, absolute::LockTime, address::ParseError, consensus::encode,
        policy::MAX_STANDARD_TX_WEIGHT, transaction::Version,
    },
    snafu::Snafu,
};

pub use {
    bitcoin,
    builder::{Built, Class, Entry, TransactionBuilder},
    error::{Error, Result},
};

mod builder;
mod error;

#[derive(Clone, Debug)]
pub struct Payout {
    pub address: Address,
    pub amount: Amount,
}

impl Payout {
    pub fn parse(address: &str, amount_sats: u64, network: Network) -> Result<Self> {
        let address = address.trim();

        let unchecked = address
            .parse::<Address<_>>()
            .map_err(|source| Error::InvalidAddress {
                address: address.to_string(),
                source,
            })?;

        if !unchecked.is_valid_for_network(network) {
            Err(Error::WrongNetwork {
                address: address.to_string(),
                network,
            })?;
        }

        Ok(Self {
            address: unchecked.assume_checked(),
            amount: Amount::from_sat(amount_sats),
        })
    }

    pub fn dust_limit(&self) -> Amount {
        self.address.script_pubkey().minimal_non_dust()
    }

    pub fn is_dust(&self) -> bool {
        self.amount < self.dust_limit()
    }
}

#[derive(Clone, Debug)]
pub struct Funding {
    pub outpoint: OutPoint,
    pub txout: TxOut,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub transaction: Transaction,
    pub psbt: Psbt,
    pub fee: Amount,
    pub weight: Weight,
}

impl Plan {
    pub fn tx_hex(&self) -> String {
        encode::serialize_hex(&self.transaction)
    }

    pub fn psbt_base64(&self) -> String {
        self.psbt.to_string()
    }

    pub fn includes_dust(&self) -> bool {
        self.transaction
            .output
            .iter()
            .any(|output| output.value < output.script_pubkey.minimal_non_dust())
    }

    pub fn is_standard(&self) -> bool {
        self.weight <= Weight::from_wu(MAX_STANDARD_TX_WEIGHT as u64) && !self.includes_dust()
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        bitcoin::{FeeRate, ScriptBuf, WitnessProgram, WitnessVersion},
    };

    fn address(i: u32, network: Network) -> String {
        let mut program = [0; 32];
        program[..4].copy_from_slice(&i.to_le_bytes());

        Address::from_witness_program(
            WitnessProgram::new(WitnessVersion::V1, &program).unwrap(),
            network,
        )
        .to_string()
    }

    fn payout(i: u32, sats: u64) -> Payout {
        Payout::parse(&address(i, Network::Bitcoin), sats, Network::Bitcoin).unwrap()
    }

    fn unverified(i: u32, sats: u64) -> (String, u64) {
        (address(i, Network::Bitcoin), sats)
    }

    fn funding(payouts: &[(String, u64)]) -> Funding {
        let value = payouts
            .iter()
            .map(|(_, amount_sats)| Amount::from_sat(*amount_sats))
            .sum::<Amount>()
            + Amount::from_sat(1_000_000);

        Funding {
            outpoint: OutPoint::null(),
            txout: TxOut {
                value,
                script_pubkey: ScriptBuf::new_witness_program(
                    &WitnessProgram::new(WitnessVersion::V1, &[0; 32]).unwrap(),
                ),
            },
        }
    }

    fn change_address() -> Address {
        Address::from_witness_program(
            WitnessProgram::new(WitnessVersion::V1, &[0; 32]).unwrap(),
            Network::Bitcoin,
        )
    }

    fn fee_rate() -> FeeRate {
        FeeRate::from_sat_per_vb(1).unwrap()
    }

    fn builder(payouts: &[(String, u64)]) -> TransactionBuilder {
        TransactionBuilder::new(
            payouts,
            Network::Bitcoin,
            funding(payouts),
            change_address(),
            fee_rate(),
        )
    }

    #[test]
    fn payout_parse_parses_valid_address() {
        let address = address(1, Network::Bitcoin);
        let payout = Payout::parse(&address, 1_000, Network::Bitcoin).unwrap();

        assert_eq!(payout.amount, Amount::from_sat(1_000));
        assert_eq!(payout.address.to_string(), address);
    }

    #[test]
    fn payout_parse_rejects_invalid_address() {
        let err = Payout::parse("not-an-address", 1_000, Network::Bitcoin).unwrap_err();

        assert!(
            matches!(err, Error::InvalidAddress { .. }),
            "unexpected error: {err}"
        );
        assert!(err.to_string().contains("not-an-address"), "{err}");
    }

    #[test]
    fn payout_parse_rejects_wrong_network() {
        let err =
            Payout::parse(&address(1, Network::Regtest), 1_000, Network::Bitcoin).unwrap_err();

        assert!(
            matches!(err, Error::WrongNetwork { .. }),
            "unexpected error: {err}"
        );
        assert!(err.to_string().contains("not valid on bitcoin"), "{err}");
    }

    #[test]
    fn payout_parse_trims_whitespace() {
        let padded = format!("  {}\n", address(1, Network::Bitcoin));

        assert!(Payout::parse(&padded, 1_000, Network::Bitcoin).is_ok());
    }

    #[test]
    fn payout_dust_limit_matches_script() {
        let payout = payout(1, 1_000);

        assert_eq!(
            payout.dust_limit(),
            payout.address.script_pubkey().minimal_non_dust(),
        );
        assert_eq!(payout.dust_limit(), Amount::from_sat(330));
    }

    #[test]
    fn payout_is_dust() {
        assert!(payout(1, 329).is_dust());
        assert!(!payout(1, 330).is_dust());
        assert!(!payout(1, 331).is_dust());
    }

    #[test]
    fn tx_hex_roundtrips() {
        let payouts = [unverified(1, 1_000)];
        let built = builder(&payouts).build_standard().unwrap();

        assert_eq!(
            encode::deserialize_hex::<Transaction>(&built.plan.tx_hex()).unwrap(),
            built.plan.transaction,
        );
    }

    #[test]
    fn psbt_base64_roundtrips() {
        let payouts = [unverified(1, 1_000)];
        let built = builder(&payouts).build_standard().unwrap();

        assert_eq!(
            built.plan.psbt_base64().parse::<Psbt>().unwrap(),
            built.plan.psbt
        );
    }

    #[test]
    fn standard_plan_reports_standard() {
        let payouts = [unverified(1, 1_000), unverified(2, 2_000)];
        let built = builder(&payouts).build_standard().unwrap();

        assert!(built.plan.is_standard());
        assert!(!built.plan.includes_dust());
    }

    #[test]
    fn payable_plan_reports_dust_and_non_standard() {
        let payouts = [unverified(1, 1_000), unverified(2, 100)];
        let built = builder(&payouts).build_payable().unwrap();

        assert!(built.plan.includes_dust());
        assert!(!built.plan.is_standard());
    }

    #[test]
    fn overweight_plan_reports_non_standard() {
        let payouts = (0..2_500).map(|i| unverified(i, 1_000)).collect::<Vec<_>>();
        let built = builder(&payouts).build_payable().unwrap();

        assert!(!built.plan.includes_dust());
        assert!(!built.plan.is_standard());
        assert!(built.plan.weight > Weight::from_wu(MAX_STANDARD_TX_WEIGHT as u64));
    }
}
