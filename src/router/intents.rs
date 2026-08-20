use super::*;

pub(crate) const INTENT_TTL: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug)]
pub(crate) struct Intent {
    pub(crate) order_id: u32,
    pub(crate) expected: HashRate,
    pub(crate) created: Instant,
}

impl Intent {
    fn fresh(&self, now: Instant) -> bool {
        now.duration_since(self.created) < INTENT_TTL
    }
}

#[derive(Default)]
pub(crate) struct Intents {
    by_enonce1: HashMap<Extranonce, Intent>,
}

impl Intents {
    pub(crate) fn create(
        &mut self,
        enonce1: Extranonce,
        order_id: u32,
        expected: HashRate,
        now: Instant,
    ) {
        let intent = Intent {
            order_id,
            expected,
            created: now,
        };

        self.by_enonce1.insert(enonce1.clone(), intent);
    }

    pub(crate) fn claim(&mut self, enonce1: Option<&Extranonce>, now: Instant) -> Option<Intent> {
        if let Some(enonce1) = enonce1
            && let Some(intent) = self.by_enonce1.remove(enonce1)
            && intent.fresh(now)
        {
            return Some(intent);
        }

        None
    }

    pub(crate) fn expected_for(&self, order_id: u32, now: Instant) -> HashRate {
        self.by_enonce1
            .values()
            .filter(|intent| intent.order_id == order_id && intent.fresh(now))
            .map(|intent| intent.expected)
            .sum()
    }

    pub(crate) fn expire(&mut self, now: Instant) -> usize {
        let before = self.by_enonce1.len();

        self.by_enonce1.retain(|_, intent| intent.fresh(now));

        before - self.by_enonce1.len()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.by_enonce1.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enonce1(byte: u8) -> Extranonce {
        Extranonce::from_bytes(&[byte; 4])
    }

    #[test]
    fn claim_consumes_intent() {
        let mut intents = Intents::default();
        let now = Instant::now();

        intents.create(enonce1(1), 7, HashRate::from_hps(100.0), now);

        let intent = intents.claim(Some(&enonce1(1)), now).unwrap();
        assert_eq!(intent.order_id, 7);
        assert_eq!(intent.expected, HashRate::from_hps(100.0));

        assert!(intents.claim(Some(&enonce1(1)), now).is_none());
    }

    #[test]
    fn claim_without_enonce1_returns_none() {
        let mut intents = Intents::default();
        let now = Instant::now();

        intents.create(enonce1(1), 7, HashRate::from_hps(100.0), now);

        assert!(intents.claim(None, now).is_none());
        assert_eq!(intents.expected_for(7, now), HashRate::from_hps(100.0));
    }

    #[test]
    fn claim_ignores_stale_intent() {
        let mut intents = Intents::default();
        let now = Instant::now();

        intents.create(enonce1(1), 7, HashRate::from_hps(100.0), now);

        let later = now + INTENT_TTL + Duration::from_secs(1);

        assert!(intents.claim(Some(&enonce1(1)), later).is_none());
    }

    #[test]
    fn expected_for_sums_fresh_intents_for_order() {
        let mut intents = Intents::default();
        let now = Instant::now();

        intents.create(enonce1(1), 7, HashRate::from_hps(100.0), now);
        intents.create(enonce1(2), 7, HashRate::from_hps(50.0), now);
        intents.create(enonce1(3), 9, HashRate::from_hps(25.0), now);

        assert_eq!(intents.expected_for(7, now), HashRate::from_hps(150.0));
        assert_eq!(intents.expected_for(9, now), HashRate::from_hps(25.0));
        assert_eq!(intents.expected_for(1, now), HashRate::ZERO);

        let later = now + INTENT_TTL + Duration::from_secs(1);
        assert_eq!(intents.expected_for(7, later), HashRate::ZERO);
    }

    #[test]
    fn expire_removes_stale_and_reports_count() {
        let mut intents = Intents::default();
        let now = Instant::now();

        intents.create(enonce1(1), 7, HashRate::from_hps(100.0), now);
        intents.create(enonce1(2), 7, HashRate::from_hps(50.0), now - INTENT_TTL);

        assert_eq!(intents.len(), 2);
        assert_eq!(intents.expire(now), 1);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents.expire(now), 0);
    }
}
