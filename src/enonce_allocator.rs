use super::*;

const MAX_TRACKED_CLIENTS: usize = 1 << 16;

#[derive(Default)]
struct Allocated {
    pool: HashSet<Extranonce>,
    proxy: HashMap<Extranonce, HashSet<Extranonce>>,
}

impl Allocated {
    fn count_for(&self, extranonces: &Extranonces) -> usize {
        match extranonces {
            Extranonces::Pool(_) => self.pool.len(),
            Extranonces::Proxy(proxy) => self
                .proxy
                .get(proxy.upstream_enonce1())
                .map_or(0, HashSet::len),
        }
    }

    fn insert(&mut self, extranonces: &Extranonces, enonce1: Extranonce) -> bool {
        match extranonces {
            Extranonces::Pool(_) => self.pool.insert(enonce1),
            Extranonces::Proxy(proxy) => self
                .proxy
                .entry(proxy.upstream_enonce1().clone())
                .or_default()
                .insert(enonce1),
        }
    }

    fn remove(&mut self, enonce1: &Extranonce) {
        if self.pool.remove(enonce1) {
            return;
        }
        for entries in self.proxy.values_mut() {
            if entries.remove(enonce1) {
                return;
            }
        }
    }

    fn prune_stale(&mut self, extranonces: &Extranonces) {
        match extranonces {
            Extranonces::Pool(_) => {
                self.pool.clear();
                self.proxy.clear();
            }
            Extranonces::Proxy(proxy) => {
                self.pool.clear();
                let current = proxy.upstream_enonce1();
                self.proxy.retain(|prefix, _| prefix == current);
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.pool.len() + self.proxy.values().map(HashSet::len).sum::<usize>()
    }
}

pub(crate) struct EnonceAllocator {
    extranonces: RwLock<Extranonces>,
    enonce_counter: AtomicU64,
    order_id: AtomicU32,
    allocated: Mutex<Allocated>,
}

impl EnonceAllocator {
    pub(crate) fn new(extranonces: Extranonces, order_id: u32) -> Self {
        Self {
            extranonces: RwLock::new(extranonces),
            enonce_counter: AtomicU64::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            ),
            order_id: AtomicU32::new(order_id),
            allocated: Mutex::new(Allocated::default()),
        }
    }

    pub(crate) fn next_enonce1(&self) -> Option<Extranonce> {
        let extranonces = self.extranonces.read();
        let max = extranonces.max_clients();

        if max > MAX_TRACKED_CLIENTS {
            return Some(self.build_enonce1(
                &extranonces,
                self.enonce_counter.fetch_add(1, Ordering::Relaxed),
            ));
        }

        let mut allocated = self.allocated.lock();

        if allocated.count_for(&extranonces) >= max {
            return None;
        }

        for _ in 0..max {
            let enonce1 = self.build_enonce1(
                &extranonces,
                self.enonce_counter.fetch_add(1, Ordering::Relaxed),
            );

            if allocated.insert(&extranonces, enonce1.clone()) {
                return Some(enonce1);
            }
        }

        None
    }

    fn build_enonce1(&self, extranonces: &Extranonces, counter: u64) -> Extranonce {
        match extranonces {
            Extranonces::Pool(pool) => {
                let bytes = counter.to_le_bytes();
                Extranonce::from_bytes(&bytes[..pool.enonce1_size()])
            }
            Extranonces::Proxy(proxy) => {
                let upstream = proxy.upstream_enonce1().as_bytes();
                let extension_size = proxy.extension_size();
                let mut bytes = [0u8; MAX_ENONCE_SIZE * 2];

                bytes[..upstream.len()].copy_from_slice(upstream);
                bytes[upstream.len()..upstream.len() + extension_size]
                    .copy_from_slice(&counter.to_le_bytes()[..extension_size]);

                Extranonce::from_bytes(&bytes[..upstream.len() + extension_size])
            }
        }
    }

    pub(crate) fn release_enonce1(&self, enonce1: &Extranonce) {
        self.allocated.lock().remove(enonce1);
    }

    pub(crate) fn track_enonce1(&self, enonce1: &Extranonce) {
        let extranonces = self.extranonces.read();
        if extranonces.max_clients() <= MAX_TRACKED_CLIENTS {
            self.allocated.lock().insert(&extranonces, enonce1.clone());
        }
    }

    pub(crate) fn enonce2_size(&self) -> usize {
        self.extranonces.read().enonce2_size()
    }

    pub(crate) fn extranonces(&self) -> parking_lot::RwLockReadGuard<'_, Extranonces> {
        self.extranonces.read()
    }

    pub(crate) fn update_extranonces(&self, extranonces: Extranonces) {
        let mut guard = self.extranonces.write();
        *guard = extranonces;
        self.allocated.lock().prune_stale(&guard);
    }

    pub(crate) fn order_id(&self) -> u32 {
        self.order_id.load(Ordering::Relaxed)
    }

    pub(crate) fn is_compatible_enonce1(&self, enonce1: &Extranonce) -> bool {
        let extranonces = self.extranonces.read();
        match &*extranonces {
            Extranonces::Pool(pool) => enonce1.len() == pool.enonce1_size(),
            Extranonces::Proxy(proxy) => {
                let upstream = proxy.upstream_enonce1().as_bytes();
                enonce1.len() == upstream.len() + proxy.extension_size()
                    && enonce1.as_bytes().starts_with(upstream)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn max_clients(&self) -> usize {
        self.extranonces.read().max_clients()
    }

    #[cfg(test)]
    pub(crate) fn allocated_count(&self) -> usize {
        self.allocated.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_allocator() -> EnonceAllocator {
        EnonceAllocator::new(
            Extranonces::Pool(PoolExtranonces::new(ENONCE1_SIZE, 8).unwrap()),
            0,
        )
    }

    fn proxy_allocator(extension_size: usize) -> EnonceAllocator {
        let upstream_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        EnonceAllocator::new(
            Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1, 8, extension_size).unwrap()),
            0,
        )
    }

    fn proxy_allocator_with_id(extension_size: usize, order_id: u32) -> EnonceAllocator {
        let upstream_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        EnonceAllocator::new(
            Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1, 8, extension_size).unwrap()),
            order_id,
        )
    }

    #[test]
    fn pool_enonce1() {
        let allocator = pool_allocator();
        let e1 = allocator.next_enonce1().unwrap();
        let e2 = allocator.next_enonce1().unwrap();

        assert_eq!(e1.len(), ENONCE1_SIZE);

        let v1 = u32::from_le_bytes(e1.as_bytes().try_into().unwrap());
        let v2 = u32::from_le_bytes(e2.as_bytes().try_into().unwrap());
        assert_eq!(v2, v1 + 1);
    }

    #[test]
    fn next_enonce1_is_unique() {
        let allocator = pool_allocator();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let enonce = allocator.next_enonce1().unwrap();
            assert!(seen.insert(enonce), "duplicate enonce1 generated");
        }
    }

    #[test]
    fn proxy_enonce1() {
        let allocator = proxy_allocator_with_id(2, 7);

        assert_eq!(allocator.enonce2_size(), 6);

        let e1 = allocator.next_enonce1().unwrap();
        let e2 = allocator.next_enonce1().unwrap();

        let upstream_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(e1.len(), 6);
        assert_eq!(&e1.as_bytes()[..4], upstream_enonce1.as_bytes());

        let ext1 = u16::from_le_bytes(e1.as_bytes()[4..6].try_into().unwrap());
        let ext2 = u16::from_le_bytes(e2.as_bytes()[4..6].try_into().unwrap());
        assert_eq!(ext2, ext1.wrapping_add(1));
        assert_eq!(allocator.order_id(), 7);
    }

    #[test]
    fn proxy_mode_extension_size_1() {
        let upstream_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        let allocator = proxy_allocator(1);

        let e1 = allocator.next_enonce1().unwrap();
        assert_eq!(e1.len(), 5);
        assert_eq!(&e1.as_bytes()[..4], upstream_enonce1.as_bytes());
        assert_eq!(allocator.enonce2_size(), 7);

        let e2 = allocator.next_enonce1().unwrap();
        let ext1 = e1.as_bytes()[4];
        let ext2 = e2.as_bytes()[4];
        assert_eq!(ext2, ext1.wrapping_add(1));
    }

    #[test]
    fn update_extranonces_changes_enonce_derivation() {
        let old_enonce1 = Extranonce::from_bytes(&[0xaa, 0xbb, 0xcc, 0xdd]);
        let allocator = EnonceAllocator::new(
            Extranonces::Proxy(ProxyExtranonces::new(old_enonce1.clone(), 8, 2).unwrap()),
            0,
        );

        let before = allocator.next_enonce1().unwrap();
        assert_eq!(&before.as_bytes()[..4], old_enonce1.as_bytes());
        assert_eq!(allocator.enonce2_size(), 6);

        let new_enonce1 = Extranonce::from_bytes(&[0x11, 0x22, 0x33, 0x44]);
        allocator.update_extranonces(Extranonces::Proxy(
            ProxyExtranonces::new(new_enonce1.clone(), 8, 2).unwrap(),
        ));

        let after = allocator.next_enonce1().unwrap();
        assert_eq!(&after.as_bytes()[..4], new_enonce1.as_bytes());
        assert_eq!(allocator.enonce2_size(), 6);
    }

    #[test]
    fn compatible_enonce1() {
        #[track_caller]
        fn case(allocator: &EnonceAllocator, bytes: &[u8], expected: bool) {
            assert_eq!(
                allocator.is_compatible_enonce1(&Extranonce::from_bytes(bytes)),
                expected,
            );
        }

        let allocator = pool_allocator();

        case(&allocator, &[0xaa, 0xbb, 0xcc, 0xdd], true);
        case(&allocator, &[0xaa, 0xbb, 0xcc], false);
        case(&allocator, &[0xaa, 0xbb, 0xcc, 0xdd, 0xee], false);

        let upstream_enonce1 = Extranonce::from_bytes(&[0xaa, 0xbb, 0xcc, 0xdd]);
        let allocator = EnonceAllocator::new(
            Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1, 8, 2).unwrap()),
            0,
        );

        case(&allocator, &[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff], true);
        case(&allocator, &[0xaa, 0xbb, 0xcc, 0xdd, 0xee], false);
        case(&allocator, &[0xaa, 0xbb, 0xcc, 0xee, 0xee, 0xff], false);
    }

    #[test]
    fn pool_full_returns_none() {
        let allocator = proxy_allocator(1);
        assert_eq!(allocator.max_clients(), 256);

        for _ in 0..256 {
            assert!(allocator.next_enonce1().is_some());
        }

        assert!(allocator.next_enonce1().is_none());
        assert_eq!(allocator.allocated_count(), 256);
    }

    #[test]
    fn release_frees_slot() {
        let allocator = proxy_allocator(1);

        for _ in 0..256 {
            allocator.next_enonce1().unwrap();
        }

        assert!(allocator.next_enonce1().is_none());

        let enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef, 0x00]);
        allocator.release_enonce1(&enonce1);

        assert_eq!(allocator.allocated_count(), 255);

        let new = allocator.next_enonce1().unwrap();
        assert_eq!(new, enonce1);
    }

    #[test]
    fn allocated_count_tracking() {
        let allocator = proxy_allocator(1);

        assert_eq!(allocator.allocated_count(), 0);

        let e1 = allocator.next_enonce1().unwrap();
        assert_eq!(allocator.allocated_count(), 1);

        let e2 = allocator.next_enonce1().unwrap();
        assert_eq!(allocator.allocated_count(), 2);

        allocator.release_enonce1(&e1);
        assert_eq!(allocator.allocated_count(), 1);

        allocator.release_enonce1(&e2);
        assert_eq!(allocator.allocated_count(), 0);
    }

    #[test]
    fn proxy_upstream_change_does_not_block_new_allocations() {
        let old_enonce1 = Extranonce::from_bytes(&[0xaa, 0xbb, 0xcc, 0xdd]);
        let allocator = EnonceAllocator::new(
            Extranonces::Proxy(ProxyExtranonces::new(old_enonce1, 8, 1).unwrap()),
            0,
        );

        for _ in 0..100 {
            allocator.next_enonce1().unwrap();
        }

        assert_eq!(allocator.allocated_count(), 100);

        let new_enonce1 = Extranonce::from_bytes(&[0x11, 0x22, 0x33, 0x44]);
        allocator.update_extranonces(Extranonces::Proxy(
            ProxyExtranonces::new(new_enonce1.clone(), 8, 1).unwrap(),
        ));

        assert_eq!(allocator.allocated_count(), 0);

        for _ in 0..256 {
            let e = allocator.next_enonce1().unwrap();
            assert_eq!(&e.as_bytes()[..4], new_enonce1.as_bytes());
        }

        assert!(allocator.next_enonce1().is_none());
    }

    #[test]
    fn untracked_always_returns_some() {
        let allocator = pool_allocator();

        for _ in 0..1000 {
            assert!(allocator.next_enonce1().is_some());
        }

        assert_eq!(allocator.allocated_count(), 0);
    }

    #[test]
    fn tracked_at_max_65536() {
        let allocator = proxy_allocator(2);

        allocator.next_enonce1().unwrap();

        assert_eq!(allocator.allocated_count(), 1);
    }

    #[test]
    fn update_extranonces_prunes_stale_prefix() {
        let allocator =
            EnonceAllocator::new(Extranonces::Pool(PoolExtranonces::new(2, 8).unwrap()), 0);

        for _ in 0..10 {
            allocator.next_enonce1().unwrap();
        }

        assert_eq!(allocator.allocated_count(), 10);

        let upstream = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        allocator.update_extranonces(Extranonces::Proxy(
            ProxyExtranonces::new(upstream, 8, 1).unwrap(),
        ));

        assert_eq!(allocator.allocated_count(), 0);

        for _ in 0..256 {
            assert!(allocator.next_enonce1().is_some());
        }

        assert!(allocator.next_enonce1().is_none());
    }

    #[test]
    fn update_extranonces_preserves_same_prefix_reservations() {
        let upstream_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        let allocator = EnonceAllocator::new(
            Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1.clone(), 8, 1).unwrap()),
            0,
        );

        let e1 = allocator.next_enonce1().unwrap();
        assert_eq!(allocator.allocated_count(), 1);

        allocator.update_extranonces(Extranonces::Proxy(
            ProxyExtranonces::new(upstream_enonce1, 8, 1).unwrap(),
        ));

        assert_eq!(allocator.allocated_count(), 1);

        allocator.release_enonce1(&e1);
        assert_eq!(allocator.allocated_count(), 0);

        for _ in 0..256 {
            assert!(allocator.next_enonce1().is_some());
        }

        assert!(allocator.next_enonce1().is_none());
    }

    #[test]
    fn track_enonce1_idempotent_when_already_tracked() {
        let allocator = proxy_allocator(1);

        let e1 = allocator.next_enonce1().unwrap();
        assert_eq!(allocator.allocated_count(), 1);

        allocator.track_enonce1(&e1);
        assert_eq!(allocator.allocated_count(), 1);
    }
}
