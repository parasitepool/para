use super::*;

pub(crate) const INTENT_TTL: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug)]
pub(crate) struct Intent {
    pub(crate) order_id: u32,
    pub(crate) expected: HashRate,
    pub(crate) created: Instant,
    ip: IpAddr,
}

impl Intent {
    fn fresh(&self, now: Instant) -> bool {
        now.duration_since(self.created) < INTENT_TTL
    }
}

#[derive(Default)]
pub(crate) struct Intents {
    by_enonce1: HashMap<Extranonce, Intent>,
    by_ip: HashMap<IpAddr, Extranonce>,
}

impl Intents {
    pub(crate) fn create(
        &mut self,
        enonce1: Extranonce,
        ip: IpAddr,
        order_id: u32,
        expected: HashRate,
        now: Instant,
    ) {
        let intent = Intent {
            order_id,
            expected,
            created: now,
            ip,
        };

        self.by_enonce1.insert(enonce1.clone(), intent);
        self.by_ip.insert(ip, enonce1);
    }

    pub(crate) fn claim(
        &mut self,
        enonce1: Option<&Extranonce>,
        ip: IpAddr,
        now: Instant,
    ) -> Option<Intent> {
        if let Some(enonce1) = enonce1
            && let Some(intent) = self.by_enonce1.get(enonce1).copied()
        {
            self.by_enonce1.remove(enonce1);

            if self.by_ip.get(&intent.ip) == Some(enonce1) {
                self.by_ip.remove(&intent.ip);
            }

            if intent.fresh(now) {
                return Some(intent);
            }
        }

        if let Some(key) = self.by_ip.get(&ip).cloned() {
            self.by_ip.remove(&ip);

            if let Some(intent) = self.by_enonce1.get(&key).copied() {
                self.by_enonce1.remove(&key);

                if intent.fresh(now) {
                    return Some(intent);
                }
            }
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
        self.by_ip
            .retain(|_, key| self.by_enonce1.contains_key(key));

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

    fn ip(octet: u8) -> IpAddr {
        IpAddr::from([127, 0, 0, octet])
    }

    #[test]
    fn claim_consumes_intent() {
        let mut intents = Intents::default();
        let now = Instant::now();

        intents.create(enonce1(1), ip(1), 7, HashRate::from_hps(100.0), now);

        let intent = intents.claim(Some(&enonce1(1)), ip(1), now).unwrap();
        assert_eq!(intent.order_id, 7);
        assert_eq!(intent.expected, HashRate::from_hps(100.0));

        assert!(intents.claim(Some(&enonce1(1)), ip(1), now).is_none());
    }

    #[test]
    fn claim_by_ip_consumes_both_entries() {
        let mut intents = Intents::default();
        let now = Instant::now();

        intents.create(enonce1(1), ip(1), 7, HashRate::from_hps(100.0), now);

        let intent = intents.claim(None, ip(1), now).unwrap();
        assert_eq!(intent.order_id, 7);
        assert_eq!(intent.expected, HashRate::from_hps(100.0));

        assert!(intents.claim(None, ip(1), now).is_none());
        assert!(intents.claim(Some(&enonce1(1)), ip(1), now).is_none());
        assert_eq!(intents.expected_for(7, now), HashRate::ZERO);
    }

    #[test]
    fn claim_by_enonce1_from_new_ip_removes_original_ip_entry() {
        let mut intents = Intents::default();
        let now = Instant::now();

        intents.create(enonce1(1), ip(1), 7, HashRate::from_hps(100.0), now);

        assert!(intents.claim(Some(&enonce1(1)), ip(2), now).is_some());
        assert!(intents.claim(None, ip(1), now).is_none());
    }

    #[test]
    fn ip_collision_keeps_both_enonce1_entries() {
        let mut intents = Intents::default();
        let now = Instant::now();

        intents.create(enonce1(1), ip(1), 7, HashRate::from_hps(100.0), now);
        intents.create(enonce1(2), ip(1), 8, HashRate::from_hps(50.0), now);

        assert_eq!(intents.claim(None, ip(1), now).unwrap().order_id, 8);
        assert_eq!(
            intents
                .claim(Some(&enonce1(1)), ip(2), now)
                .unwrap()
                .order_id,
            7
        );
        assert_eq!(intents.expected_for(7, now), HashRate::ZERO);
        assert_eq!(intents.expected_for(8, now), HashRate::ZERO);
    }

    #[test]
    fn claim_ignores_stale_intent() {
        let mut intents = Intents::default();
        let now = Instant::now();

        intents.create(enonce1(1), ip(1), 7, HashRate::from_hps(100.0), now);

        let later = now + INTENT_TTL + Duration::from_secs(1);

        assert!(intents.claim(Some(&enonce1(1)), ip(1), later).is_none());
    }

    #[test]
    fn expected_for_sums_fresh_intents_for_order() {
        let mut intents = Intents::default();
        let now = Instant::now();

        intents.create(enonce1(1), ip(1), 7, HashRate::from_hps(100.0), now);
        intents.create(enonce1(2), ip(2), 7, HashRate::from_hps(50.0), now);
        intents.create(enonce1(3), ip(3), 9, HashRate::from_hps(25.0), now);

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

        intents.create(enonce1(1), ip(1), 7, HashRate::from_hps(100.0), now);
        intents.create(
            enonce1(2),
            ip(2),
            7,
            HashRate::from_hps(50.0),
            now - INTENT_TTL,
        );

        assert_eq!(intents.len(), 2);
        assert_eq!(intents.expire(now), 1);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents.expire(now), 0);
    }
}
