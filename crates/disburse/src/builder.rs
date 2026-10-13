use super::*;

#[derive(Clone, Debug)]
pub struct TransactionBuilder {
    entries: Vec<Entry>,
    funding: Funding,
    change_address: Address,
    fee_rate: FeeRate,
    payouts: Vec<Payout>,
    allow_dust: bool,
    max_weight: Weight,
    witness: Weight,
    leftover: Amount,
    outputs: Vec<TxOut>,
    fee: Amount,
    weight: Weight,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub class: Class,
    pub payout: Option<Payout>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Class {
    Standard,
    Dust { limit: Amount },
    Invalid { reason: String },
}

#[derive(Clone, Debug)]
pub struct Built {
    pub plan: Plan,
    pub trimmed: Vec<usize>,
}

impl TransactionBuilder {
    pub fn new(
        payouts: &[(String, u64)],
        network: Network,
        funding: Funding,
        change_address: Address,
        fee_rate: FeeRate,
    ) -> Self {
        Self {
            entries: payouts
                .iter()
                .map(
                    |(address, amount_sats)| match Payout::parse(address, *amount_sats, network) {
                        Ok(parsed) => Entry {
                            class: if parsed.is_dust() {
                                Class::Dust {
                                    limit: parsed.dust_limit(),
                                }
                            } else {
                                Class::Standard
                            },
                            payout: Some(parsed),
                        },
                        Err(err) => Entry {
                            class: Class::Invalid {
                                reason: err.to_string(),
                            },
                            payout: None,
                        },
                    },
                )
                .collect(),
            funding,
            change_address,
            fee_rate,
            payouts: Vec::new(),
            allow_dust: false,
            max_weight: Weight::ZERO,
            witness: Weight::ZERO,
            leftover: Amount::ZERO,
            outputs: Vec::new(),
            fee: Amount::ZERO,
            weight: Weight::ZERO,
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn build_standard(&self) -> Result<Built> {
        self.build_selected(
            self.select(|class| matches!(class, Class::Standard)),
            false,
            false,
            true,
        )
    }

    pub fn build_payable(&self) -> Result<Built> {
        self.build_selected(
            self.select(|class| !matches!(class, Class::Invalid { .. })),
            true,
            true,
            false,
        )
    }

    fn select(&self, keep: fn(&Class) -> bool) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| keep(&entry.class))
            .map(|(index, _)| index)
            .collect()
    }

    fn build_selected(
        &self,
        indices: Vec<usize>,
        allow_dust: bool,
        allow_overweight: bool,
        trim: bool,
    ) -> Result<Built> {
        if indices.is_empty() {
            return Err(Error::EmptyOutputs);
        }

        match self.build_with(&indices, allow_dust, allow_overweight) {
            Ok(plan) => Ok(Built {
                plan,
                trimmed: Vec::new(),
            }),
            Err(err @ Error::ExceedsWeight { .. }) => {
                if !trim || indices.len() == 1 {
                    Err(err)
                } else {
                    self.build_trimmed(&indices)
                }
            }
            Err(err) => Err(err),
        }
    }

    fn build_trimmed(&self, indices: &[usize]) -> Result<Built> {
        let mut order = indices.to_vec();

        order.sort_by(|&a, &b| {
            let (payout_a, payout_b) = (self.payout(a), self.payout(b));

            payout_a.amount.cmp(&payout_b.amount).then_with(|| {
                payout_a
                    .address
                    .to_string()
                    .cmp(&payout_b.address.to_string())
            })
        });

        fn kept(order: &[usize], dropped: usize) -> Vec<usize> {
            let mut kept = order[dropped..].to_vec();
            kept.sort_unstable();
            kept
        }

        self.build_with(&kept(&order, order.len() - 1), false, false)?;

        let mut lo = 1;
        let mut hi = order.len() - 1;

        while lo < hi {
            let mid = (lo + hi) / 2;

            match self.build_with(&kept(&order, mid), false, false) {
                Ok(_) => hi = mid,
                Err(Error::ExceedsWeight { .. }) => lo = mid + 1,
                Err(err) => return Err(err),
            }
        }

        let plan = self.build_with(&kept(&order, lo), false, false)?;

        let mut trimmed = order[..lo].to_vec();
        trimmed.sort_unstable();

        Ok(Built { plan, trimmed })
    }

    fn payout(&self, index: usize) -> &Payout {
        self.entries[index]
            .payout
            .as_ref()
            .expect("selected entries always have a payout")
    }

    fn build_with(
        &self,
        indices: &[usize],
        allow_dust: bool,
        allow_overweight: bool,
    ) -> Result<Plan> {
        let mut builder = self.clone();

        builder.payouts = indices
            .iter()
            .map(|&index| self.payout(index).clone())
            .collect();
        builder.allow_dust = allow_dust;
        builder.max_weight = if allow_overweight {
            Weight::MAX_BLOCK
        } else {
            Weight::from_wu(MAX_STANDARD_TX_WEIGHT as u64)
        };

        builder.build()
    }

    fn build(self) -> Result<Plan> {
        self.validate()?
            .add_payouts()
            .add_change()
            .deduct_fee()?
            .require_outputs()?
            .check_weight()?
            .finish()
    }

    fn validate(mut self) -> Result<Self> {
        self.witness = witness_weight(&self.funding.txout.script_pubkey)?;

        let sum = self
            .payouts
            .iter()
            .try_fold(Amount::ZERO, |sum, payout| sum.checked_add(payout.amount))
            .ok_or(Error::Overflow)?;

        self.leftover = self
            .funding
            .txout
            .value
            .checked_sub(sum)
            .ok_or(Error::ExceedsFunding {
                payouts: sum,
                funding: self.funding.txout.value,
            })?;

        if !self.allow_dust {
            for payout in &self.payouts {
                if payout.is_dust() {
                    return Err(Error::BelowDust {
                        address: payout.address.clone(),
                        amount: payout.amount,
                        limit: payout.dust_limit(),
                    });
                }
            }
        }

        Ok(self)
    }

    fn add_payouts(mut self) -> Self {
        self.outputs.extend(self.payouts.iter().map(|payout| TxOut {
            value: payout.amount,
            script_pubkey: payout.address.script_pubkey(),
        }));

        self
    }

    fn add_change(mut self) -> Self {
        if self.is_non_dust_change(self.leftover) {
            self.outputs.push(TxOut {
                value: self.leftover,
                script_pubkey: self.change_address.script_pubkey(),
            });
        }

        self
    }

    fn deduct_fee(mut self) -> Result<Self> {
        if self.has_change() {
            let (weight, fee) = self.estimate()?;

            if let Some(change) = self
                .leftover
                .checked_sub(fee)
                .filter(|change| self.is_non_dust_change(*change))
            {
                self.outputs.last_mut().unwrap().value = change;
                self.weight = weight;
                self.fee = fee;
                return Ok(self);
            }

            self.outputs.pop();
        }

        let (weight, fee) = self.estimate()?;

        if self.leftover < fee {
            return Err(Error::InsufficientForFee {
                available: self.leftover,
                fee,
            });
        }

        self.weight = weight;
        self.fee = self.leftover;

        Ok(self)
    }

    fn require_outputs(self) -> Result<Self> {
        if self.outputs.is_empty() {
            return Err(Error::EmptyOutputs);
        }

        Ok(self)
    }

    fn check_weight(self) -> Result<Self> {
        if self.weight > self.max_weight {
            return Err(Error::ExceedsWeight {
                weight: self.weight,
                max: self.max_weight,
            });
        }

        Ok(self)
    }

    fn finish(self) -> Result<Plan> {
        let tx = self.transaction();
        let weight = signed_weight(&tx, self.witness);

        assert!(!tx.output.is_empty(), "invariant: transaction has outputs");

        assert!(
            tx.output.len() >= self.payouts.len()
                && self
                    .payouts
                    .iter()
                    .zip(&tx.output)
                    .all(|(payout, output)| output.value == payout.amount
                        && output.script_pubkey == payout.address.script_pubkey()),
            "invariant: payouts appear in order with exact amounts",
        );

        let change = &tx.output[self.payouts.len()..];

        assert!(
            change.len() <= 1
                && change
                    .iter()
                    .all(|output| output.script_pubkey == self.change_address.script_pubkey()),
            "invariant: only non-payout output is change",
        );

        if !self.allow_dust {
            for output in &tx.output {
                assert!(
                    output.value >= output.script_pubkey.minimal_non_dust(),
                    "invariant: all outputs are above dust limit",
                );
            }
        }

        assert_eq!(
            tx.output.iter().map(|output| output.value).sum::<Amount>() + self.fee,
            self.funding.txout.value,
            "invariant: value is conserved",
        );

        assert_eq!(
            self.weight, weight,
            "invariant: weight estimate matches transaction",
        );

        assert!(weight <= self.max_weight, "invariant: weight is within max",);

        let required = self.fee_rate.fee_wu(weight).ok_or(Error::Overflow)?;

        assert!(self.fee >= required, "invariant: fee meets fee rate");

        assert!(
            change.is_empty() || self.fee == required,
            "invariant: change absorbs excess fee",
        );

        let mut psbt =
            Psbt::from_unsigned_tx(tx.clone()).map_err(|source| Error::Psbt { source })?;
        psbt.inputs[0].witness_utxo = Some(self.funding.txout.clone());

        Ok(Plan {
            transaction: tx,
            psbt,
            fee: self.fee,
            weight,
        })
    }

    fn has_change(&self) -> bool {
        self.outputs.len() > self.payouts.len()
    }

    fn is_non_dust_change(&self, value: Amount) -> bool {
        value >= self.change_address.script_pubkey().minimal_non_dust()
    }

    fn estimate(&self) -> Result<(Weight, Amount)> {
        let weight = signed_weight(&self.transaction(), self.witness);
        let fee = self.fee_rate.fee_wu(weight).ok_or(Error::Overflow)?;
        Ok((weight, fee))
    }

    fn transaction(&self) -> Transaction {
        Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: self.funding.outpoint,
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::new(),
            }],
            output: self.outputs.clone(),
        }
    }
}

fn signed_weight(tx: &Transaction, witness: Weight) -> Weight {
    tx.weight() + Weight::from_wu(2) + witness * tx.input.len() as u64
}

fn witness_weight(script: &ScriptBuf) -> Result<Weight> {
    if script.is_p2tr() {
        Ok(Weight::from_wu(67))
    } else if script.is_p2wpkh() {
        Ok(Weight::from_wu(108))
    } else {
        Err(Error::UnsupportedScript)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        bitcoin::{
            PubkeyHash, Txid, WPubkeyHash, WitnessProgram, WitnessVersion,
            hashes::{Hash, sha256},
        },
    };

    fn address(i: usize) -> Address {
        let program = sha256::Hash::hash(&i.to_le_bytes());

        Address::from_witness_program(
            WitnessProgram::new(WitnessVersion::V1, program.as_byte_array()).unwrap(),
            Network::Regtest,
        )
    }

    fn wpkh_script(byte: u8) -> ScriptBuf {
        ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([byte; 20]))
    }

    fn fee_rate(sat_per_vb: u64) -> FeeRate {
        FeeRate::from_sat_per_vb(sat_per_vb).unwrap()
    }

    fn funding(value: u64) -> Funding {
        Funding {
            outpoint: OutPoint {
                txid: Txid::from_byte_array([1; 32]),
                vout: 0,
            },
            txout: TxOut {
                value: Amount::from_sat(value),
                script_pubkey: address(0).script_pubkey(),
            },
        }
    }

    fn pay(i: usize, sats: u64) -> Payout {
        Payout {
            address: address(i),
            amount: Amount::from_sat(sats),
        }
    }

    fn payout(i: u32, sats: u64) -> Payout {
        let mut program = [0; 32];
        program[..4].copy_from_slice(&i.to_le_bytes());

        Payout {
            address: Address::from_witness_program(
                WitnessProgram::new(WitnessVersion::V1, &program).unwrap(),
                Network::Bitcoin,
            ),
            amount: Amount::from_sat(sats),
        }
    }

    fn unverified(i: u32, sats: u64) -> (String, u64) {
        (payout(i, sats).address.to_string(), sats)
    }

    fn funding_for(payouts: &[(String, u64)]) -> Funding {
        let value = payouts
            .iter()
            .map(|(_, amount_sats)| Amount::from_sat(*amount_sats))
            .sum::<Amount>()
            + Amount::from_sat(10_000_000);

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

    fn builder(payouts: &[(String, u64)]) -> TransactionBuilder {
        TransactionBuilder::new(
            payouts,
            Network::Bitcoin,
            funding_for(payouts),
            change_address(),
            fee_rate(1),
        )
    }

    fn pipeline(payouts: Vec<Payout>, funding_value: u64, sat_per_vb: u64) -> TransactionBuilder {
        TransactionBuilder {
            entries: Vec::new(),
            funding: funding(funding_value),
            change_address: address(9_999),
            fee_rate: fee_rate(sat_per_vb),
            payouts,
            allow_dust: false,
            max_weight: Weight::from_wu(MAX_STANDARD_TX_WEIGHT as u64),
            witness: Weight::ZERO,
            leftover: Amount::ZERO,
            outputs: Vec::new(),
            fee: Amount::ZERO,
            weight: Weight::ZERO,
        }
    }

    fn plan(payouts: Vec<Payout>, funding_value: u64, sat_per_vb: u64) -> Result<Plan> {
        pipeline(payouts, funding_value, sat_per_vb).build()
    }

    fn ready(payouts: Vec<Payout>, funding_value: u64, sat_per_vb: u64) -> TransactionBuilder {
        pipeline(payouts, funding_value, sat_per_vb)
            .validate()
            .unwrap()
            .add_payouts()
            .add_change()
            .deduct_fee()
            .unwrap()
            .require_outputs()
            .unwrap()
            .check_weight()
            .unwrap()
    }

    #[track_caller]
    fn check(plan: &Plan, funding: &Funding) {
        let output = plan
            .transaction
            .output
            .iter()
            .map(|output| output.value)
            .sum::<Amount>();

        assert_eq!(output + plan.fee, funding.txout.value);
        assert_eq!(plan.psbt.unsigned_tx, plan.transaction);
        assert_eq!(
            plan.psbt.inputs[0].witness_utxo.as_ref(),
            Some(&funding.txout)
        );
        assert_eq!(
            plan.psbt.unsigned_tx.input[0].previous_output,
            funding.outpoint
        );
        assert_eq!(plan.transaction.input.len(), 1);
        assert_eq!(plan.transaction.lock_time, LockTime::ZERO);
        assert!(plan.transaction.is_explicitly_rbf());
    }

    #[track_caller]
    fn case<T: std::fmt::Debug>(result: Result<T>, expected: &str) {
        assert_eq!(result.unwrap_err().to_string(), expected);
    }

    #[test]
    fn new_classifies_payouts_in_order() {
        let builder = builder(&[unverified(1, 1_000), unverified(2, 100), unverified(3, 0)]);

        assert_eq!(builder.entries().len(), 3);
        assert_eq!(builder.entries()[0].class, Class::Standard);
        assert_eq!(
            builder.entries()[1].class,
            Class::Dust {
                limit: Amount::from_sat(330),
            },
        );
        assert_eq!(
            builder.entries()[2].class,
            Class::Dust {
                limit: Amount::from_sat(330),
            },
            "zero amounts are dust",
        );
        assert_eq!(
            builder.entries()[1].payout.as_ref().unwrap().amount,
            Amount::from_sat(100),
        );
    }

    #[test]
    fn new_classifies_invalid_addresses() {
        let builder = builder(&[
            unverified(1, 1_000),
            unverified(2, 100),
            ("not-an-address".into(), 1_000),
        ]);

        assert_eq!(builder.entries()[0].class, Class::Standard);
        assert!(matches!(builder.entries()[1].class, Class::Dust { .. }));
        assert!(builder.entries()[0].payout.is_some());
        assert!(builder.entries()[2].payout.is_none());

        let Class::Invalid { reason } = &builder.entries()[2].class else {
            panic!("expected invalid class");
        };
        assert!(reason.contains("invalid payout address"), "{reason}");
    }

    #[test]
    fn new_classifies_wrong_network() {
        let builder = TransactionBuilder::new(
            &[unverified(1, 1_000)],
            Network::Regtest,
            funding_for(&[]),
            change_address(),
            fee_rate(1),
        );

        let Class::Invalid { reason } = &builder.entries()[0].class else {
            panic!("expected invalid class");
        };
        assert!(reason.contains("not valid on"), "{reason}");
    }

    #[test]
    fn payable_pays_everything_valid() {
        let payouts = [unverified(1, 1_000), unverified(2, 100), unverified(3, 0)];
        let built = builder(&payouts).build_payable().unwrap();

        assert!(built.trimmed.is_empty());
        assert!(built.plan.includes_dust());
        assert!(!built.plan.is_standard());
        assert_eq!(built.plan.transaction.output.len(), 4);

        let paid = built.plan.transaction.output[..3]
            .iter()
            .map(|output| output.value.to_sat())
            .collect::<Vec<_>>();

        assert_eq!(paid, [1_000, 100, 0]);
    }

    #[test]
    fn standard_skips_dust_and_zero() {
        let payouts = [unverified(1, 1_000), unverified(2, 100), unverified(3, 0)];
        let built = builder(&payouts).build_standard().unwrap();

        assert!(built.trimmed.is_empty());
        assert!(built.plan.is_standard());
        assert_eq!(built.plan.transaction.output.len(), 2);
        assert_eq!(
            built.plan.transaction.output[0].value,
            Amount::from_sat(1_000),
        );
    }

    #[test]
    fn empty_selection_is_rejected() {
        let builder = builder(&[("not-an-address".to_string(), 1_000)]);

        assert!(matches!(builder.build_standard(), Err(Error::EmptyOutputs)));
        assert!(matches!(builder.build_payable(), Err(Error::EmptyOutputs)));
    }

    #[test]
    fn standard_trims_smallest_until_weight_fits() {
        let payouts = (0..2_500)
            .map(|i| unverified(i, 1_000 + u64::from(i as u16)))
            .collect::<Vec<_>>();

        let builder = builder(&payouts);

        let payable = builder.build_payable().unwrap();
        assert!(payable.plan.weight > Weight::from_wu(MAX_STANDARD_TX_WEIGHT as u64));

        let built = builder.build_standard().unwrap();

        assert!(!built.trimmed.is_empty());
        assert!(built.plan.is_standard());
        assert!(built.plan.weight <= Weight::from_wu(MAX_STANDARD_TX_WEIGHT as u64));
        assert_eq!(built.trimmed, (0..built.trimmed.len()).collect::<Vec<_>>());
    }

    #[test]
    fn trimming_is_deterministic() {
        let payouts = (0..2_500)
            .map(|i| unverified(i, 1_000 + u64::from(i as u16)))
            .collect::<Vec<_>>();

        let a = builder(&payouts).build_standard().unwrap();
        let b = builder(&payouts).build_standard().unwrap();

        assert_eq!(a.trimmed, b.trimmed);
        assert_eq!(a.plan.transaction, b.plan.transaction);
    }

    #[test]
    fn kept_payouts_stay_in_entry_order() {
        let payouts = (0..2_500)
            .map(|i| unverified(i, 1_000 + u64::from(i as u16)))
            .collect::<Vec<_>>();

        let built = builder(&payouts).build_standard().unwrap();

        let kept = (0..payouts.len())
            .filter(|index| !built.trimmed.contains(index))
            .collect::<Vec<_>>();

        assert_eq!(
            built.plan.transaction.output.len(),
            kept.len() + 1,
            "one output per kept payout plus change",
        );

        for (output, &index) in built.plan.transaction.output.iter().zip(&kept) {
            let payout = payout(index as u32, 0);
            assert_eq!(output.value, Amount::from_sat(payouts[index].1));
            assert_eq!(output.script_pubkey, payout.address.script_pubkey(),);
        }
    }

    #[test]
    fn payable_exceeding_consensus_weight_is_rejected() {
        let payouts = (0..25_000)
            .map(|i| unverified(i, 1_000))
            .collect::<Vec<_>>();

        assert!(matches!(
            builder(&payouts).build_payable(),
            Err(Error::ExceedsWeight { .. }),
        ));
    }

    #[test]
    fn validate_sets_witness_and_leftover() {
        let builder = pipeline(vec![pay(1, 1_000)], 10_000, 0).validate().unwrap();

        assert_eq!(builder.witness, Weight::from_wu(67));
        assert_eq!(builder.leftover, Amount::from_sat(9_000));
        assert!(builder.outputs.is_empty());
    }

    #[test]
    fn add_payouts_preserves_order() {
        let builder = pipeline(vec![pay(2, 2_000), pay(1, 1_000)], 10_000, 0)
            .validate()
            .unwrap()
            .add_payouts();

        assert_eq!(
            builder.outputs,
            [
                TxOut {
                    value: Amount::from_sat(2_000),
                    script_pubkey: address(2).script_pubkey(),
                },
                TxOut {
                    value: Amount::from_sat(1_000),
                    script_pubkey: address(1).script_pubkey(),
                },
            ]
        );
    }

    #[test]
    fn add_change_requires_leftover_above_dust() {
        #[track_caller]
        fn case(funding: u64, outputs: usize) {
            let builder = pipeline(vec![pay(1, 1_000)], funding, 0)
                .validate()
                .unwrap()
                .add_payouts()
                .add_change();

            assert_eq!(builder.outputs.len(), outputs);
        }

        case(1_000, 1);
        case(1_329, 1);
        case(1_330, 2);
    }

    #[test]
    fn exact_spend_has_no_change() {
        let plan = plan(vec![pay(1, 10_000)], 10_000, 0).unwrap();

        assert_eq!(
            plan.transaction.output,
            [TxOut {
                value: Amount::from_sat(10_000),
                script_pubkey: address(1).script_pubkey(),
            }]
        );
        assert_eq!(plan.fee, Amount::ZERO);
        check(&plan, &funding(10_000));
    }

    #[test]
    fn payouts_with_change() {
        let plan = plan(vec![pay(1, 1_000), pay(2, 2_000)], 10_000, 0).unwrap();

        assert_eq!(plan.fee, Amount::ZERO);
        assert_eq!(
            plan.transaction.output,
            [
                TxOut {
                    value: Amount::from_sat(1_000),
                    script_pubkey: address(1).script_pubkey(),
                },
                TxOut {
                    value: Amount::from_sat(2_000),
                    script_pubkey: address(2).script_pubkey(),
                },
                TxOut {
                    value: Amount::from_sat(7_000),
                    script_pubkey: address(9_999).script_pubkey(),
                },
            ]
        );
        check(&plan, &funding(10_000));
    }

    #[test]
    fn fee_is_deducted_from_change() {
        let plan = plan(vec![pay(1, 1_000), pay(2, 2_000)], 10_000, 1).unwrap();

        assert_eq!(plan.fee, fee_rate(1).fee_wu(plan.weight).unwrap());
        assert_eq!(
            plan.transaction
                .output
                .iter()
                .map(|output| output.value.to_sat())
                .collect::<Vec<_>>(),
            [1_000, 2_000, 7_000 - plan.fee.to_sat()],
        );
        check(&plan, &funding(10_000));
    }

    #[test]
    fn sub_dust_change_folds_into_fee() {
        #[track_caller]
        fn case(sat_per_vb: u64) {
            let plan = plan(vec![pay(1, 1_500), pay(2, 1_500)], 3_300, sat_per_vb).unwrap();

            assert_eq!(plan.transaction.output.len(), 2);
            assert_eq!(plan.fee, Amount::from_sat(300));
            check(&plan, &funding(3_300));
        }

        case(0);
        case(1);
    }

    #[test]
    fn change_dropped_when_fee_exceeds_leftover() {
        let with_change = pipeline(vec![pay(1, 1_000)], 1_400, 3)
            .validate()
            .unwrap()
            .add_payouts()
            .add_change();

        assert!(with_change.estimate().unwrap().1 > Amount::from_sat(400));

        let plan = plan(vec![pay(1, 1_000)], 1_400, 3).unwrap();

        assert_eq!(plan.transaction.output.len(), 1);
        assert_eq!(plan.fee, Amount::from_sat(400));
        assert!(plan.fee >= fee_rate(3).fee_wu(plan.weight).unwrap());
        check(&plan, &funding(1_400));
    }

    #[test]
    fn empty_payouts_sweep_funding_to_change() {
        let plan = plan(vec![], 10_000, 0).unwrap();

        assert_eq!(plan.transaction.output.len(), 1);
        assert_eq!(plan.transaction.output[0].value, Amount::from_sat(10_000));
        assert_eq!(plan.fee, Amount::ZERO);
        check(&plan, &funding(10_000));
    }

    #[test]
    fn below_dust_payouts_are_rejected() {
        #[track_caller]
        fn case(sats: u64) {
            let result = plan(vec![pay(1, sats)], 10_000, 0);

            assert!(result.unwrap_err().to_string().contains("dust"));
        }

        case(1);
        case(329);
    }

    #[test]
    fn dust_limit_payout_is_accepted() {
        let plan = plan(vec![pay(1, 330)], 1_000, 0).unwrap();

        assert_eq!(plan.transaction.output.len(), 2);
        check(&plan, &funding(1_000));
    }

    #[test]
    fn zero_amounts_are_rejected_as_dust() {
        let result = plan(vec![pay(1, 0)], 10_000, 0);

        assert_eq!(
            result.unwrap_err().to_string(),
            format!(
                "payout of 0 sat to {} is below dust limit of 330 sat",
                address(1),
            ),
        );
    }

    #[test]
    fn payouts_exceeding_funding_are_rejected() {
        let result = plan(vec![pay(1, 6_000), pay(2, 5_000)], 10_000, 0);

        case(
            result,
            "payouts sum to 11000 sat but funding is only 10000 sat",
        );
    }

    #[test]
    fn fees_exceeding_leftover_are_rejected() {
        let result = plan(vec![pay(1, 5_000), pay(2, 5_000)], 10_000, 1);

        assert!(result.unwrap_err().to_string().contains("cannot cover fee"));
    }

    #[test]
    fn non_witness_funding_is_rejected() {
        #[track_caller]
        fn case(script: ScriptBuf) {
            let mut builder = pipeline(vec![pay(1, 1_000)], 10_000, 0);
            builder.funding.txout.script_pubkey = script;

            assert_eq!(
                builder.build().unwrap_err().to_string(),
                "funding script must be p2wpkh or p2tr",
            );
        }

        case(ScriptBuf::new_p2pkh(&PubkeyHash::from_byte_array([0; 20])));
        case(ScriptBuf::new_op_return([0; 8]));
    }

    #[test]
    fn ten_thousand_payouts_exceed_standard_weight() {
        let payouts = (1..=10_000).map(|i| pay(i, 1_000)).collect::<Vec<_>>();

        assert!(
            plan(payouts, 10_000_000, 0)
                .unwrap_err()
                .to_string()
                .contains("exceeds maximum of 400000")
        );
    }

    #[test]
    fn overweight_is_allowed_with_flag() {
        let payouts = (1..=10_000).map(|i| pay(i, 1_000)).collect::<Vec<_>>();

        let mut builder = pipeline(payouts, 10_000_000, 0);
        builder.max_weight = Weight::MAX_BLOCK;

        let plan = builder.build().unwrap();

        assert!(plan.weight > Weight::from_wu(MAX_STANDARD_TX_WEIGHT as u64));
        assert_eq!(plan.transaction.output.len(), 10_000);
        check(&plan, &funding(10_000_000));
    }

    #[test]
    fn consensus_weight_limit_is_always_enforced() {
        let payouts = (1..=25_000).map(|i| pay(i, 1_000)).collect::<Vec<_>>();

        let mut builder = pipeline(payouts, 25_000_000, 0);
        builder.max_weight = Weight::MAX_BLOCK;

        assert!(
            builder
                .build()
                .unwrap_err()
                .to_string()
                .contains("exceeds maximum of 4000000")
        );
    }

    #[test]
    fn below_dust_payout_is_allowed_with_flag() {
        let mut builder = pipeline(vec![pay(1, 100)], 10_000, 0);
        builder.allow_dust = true;

        let plan = builder.build().unwrap();

        assert_eq!(plan.transaction.output[0].value, Amount::from_sat(100));
        check(&plan, &funding(10_000));
    }

    #[test]
    fn exceeds_funding_is_rejected_regardless_of_flags() {
        let mut builder = pipeline(vec![pay(1, 6_000), pay(2, 5_000)], 10_000, 0);
        builder.allow_dust = true;
        builder.max_weight = Weight::MAX_BLOCK;

        case(
            builder.build(),
            "payouts sum to 11000 sat but funding is only 10000 sat",
        );
    }

    #[test]
    fn empty_outputs_are_rejected() {
        let result = plan(vec![], 0, 0);

        case(result, "transaction would have no outputs");
    }

    #[test]
    fn duplicate_addresses_are_paid_independently() {
        let plan = plan(vec![pay(1, 500), pay(1, 500)], 2_000, 0).unwrap();

        assert_eq!(plan.transaction.output.len(), 3);
        assert_eq!(
            plan.transaction.output[..2]
                .iter()
                .map(|output| output.value.to_sat())
                .sum::<u64>(),
            1_000,
        );
        check(&plan, &funding(2_000));
    }

    #[test]
    fn planning_is_deterministic() {
        let a = plan(vec![pay(1, 1_000), pay(2, 2_000)], 10_000, 1).unwrap();
        let b = plan(vec![pay(1, 1_000), pay(2, 2_000)], 10_000, 1).unwrap();

        assert_eq!(
            encode::serialize_hex(&a.transaction),
            encode::serialize_hex(&b.transaction),
        );
        assert_eq!(
            encode::serialize_hex(&a.psbt.unsigned_tx),
            encode::serialize_hex(&b.psbt.unsigned_tx),
        );
    }

    #[test]
    fn dummy_outpoint_is_allowed() {
        let mut builder = pipeline(vec![pay(1, 1_000)], 10_000, 0);
        builder.funding.outpoint = OutPoint::null();

        let funding = builder.funding.clone();
        let plan = builder.build().unwrap();

        assert_eq!(plan.transaction.input[0].previous_output, OutPoint::null());
        check(&plan, &funding);
    }

    #[test]
    fn witness_weight_matches_script_type() {
        assert_eq!(
            witness_weight(&address(0).script_pubkey()).unwrap(),
            Weight::from_wu(67)
        );
        assert_eq!(
            witness_weight(&wpkh_script(0)).unwrap(),
            Weight::from_wu(108)
        );

        let taproot = plan(vec![pay(1, 1_000)], 10_000, 0).unwrap();

        let mut builder = pipeline(vec![pay(1, 1_000)], 10_000, 0);
        builder.funding.txout.script_pubkey = wpkh_script(0);

        let wpkh = builder.build().unwrap();

        assert_eq!(wpkh.weight - taproot.weight, Weight::from_wu(41));
    }

    #[test]
    fn overflow_is_rejected() {
        let max = Amount::from_sat(u64::MAX);
        let payouts = vec![
            Payout {
                address: address(1),
                amount: max,
            },
            Payout {
                address: address(2),
                amount: max,
            },
        ];

        let result = plan(payouts, 10_000, 0);

        assert!(result.unwrap_err().to_string().contains("overflow"));
    }

    #[test]
    #[should_panic(expected = "invariant: transaction has outputs")]
    fn invariant_transaction_has_outputs() {
        let mut builder = ready(vec![], 10_000, 0);
        builder.outputs.clear();
        builder.finish().unwrap();
    }

    #[test]
    #[should_panic(expected = "invariant: payouts appear in order with exact amounts")]
    fn invariant_payouts_appear_in_order() {
        let mut builder = ready(vec![pay(1, 1_000), pay(2, 2_000)], 10_000, 0);
        builder.outputs.swap(0, 1);
        builder.finish().unwrap();
    }

    #[test]
    #[should_panic(expected = "invariant: only non-payout output is change")]
    fn invariant_only_non_payout_output_is_change() {
        let mut builder = ready(vec![pay(1, 1_000)], 10_000, 0);
        builder.outputs.last_mut().unwrap().script_pubkey = address(2).script_pubkey();
        builder.finish().unwrap();
    }

    #[test]
    #[should_panic(expected = "invariant: only non-payout output is change")]
    fn invariant_change_appears_at_most_once() {
        let mut builder = ready(vec![pay(1, 1_000)], 10_000, 0);
        let change = builder.outputs.last().unwrap().clone();
        builder.outputs.push(change);
        builder.finish().unwrap();
    }

    #[test]
    #[should_panic(expected = "invariant: all outputs are above dust limit")]
    fn invariant_outputs_are_above_dust() {
        let mut builder = ready(vec![pay(1, 1_000)], 10_000, 0);
        builder.outputs.last_mut().unwrap().value = Amount::from_sat(1);
        builder.finish().unwrap();
    }

    #[test]
    #[should_panic(expected = "invariant: value is conserved")]
    fn invariant_value_is_conserved() {
        let mut builder = ready(vec![pay(1, 1_000)], 10_000, 1);
        builder.fee += Amount::from_sat(1);
        builder.finish().unwrap();
    }

    #[test]
    #[should_panic(expected = "invariant: weight estimate matches transaction")]
    fn invariant_weight_estimate_matches_transaction() {
        let mut builder = ready(vec![pay(1, 1_000)], 10_000, 1);
        builder.weight += Weight::from_wu(1);
        builder.finish().unwrap();
    }

    #[test]
    #[should_panic(expected = "invariant: weight is within max")]
    fn invariant_weight_is_within_max() {
        let payouts = (1..=3_000).map(|i| pay(i, 1_000)).collect::<Vec<_>>();

        pipeline(payouts, 3_000_000, 0)
            .validate()
            .unwrap()
            .add_payouts()
            .add_change()
            .deduct_fee()
            .unwrap()
            .require_outputs()
            .unwrap()
            .finish()
            .unwrap();
    }

    #[test]
    #[should_panic(expected = "invariant: fee meets fee rate")]
    fn invariant_fee_meets_fee_rate() {
        let mut builder = ready(vec![pay(1, 1_000)], 10_000, 1);
        builder.outputs.last_mut().unwrap().value += builder.fee;
        builder.fee = Amount::ZERO;
        builder.finish().unwrap();
    }

    #[test]
    #[should_panic(expected = "invariant: change absorbs excess fee")]
    fn invariant_change_absorbs_excess_fee() {
        let mut builder = ready(vec![pay(1, 1_000)], 10_000, 1);
        builder.outputs.last_mut().unwrap().value -= Amount::from_sat(1);
        builder.fee += Amount::from_sat(1);
        builder.finish().unwrap();
    }
}
