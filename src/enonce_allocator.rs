use super::*;

pub(crate) struct EnonceAllocator {
    extranonces: RwLock<Extranonces>,
    enonce_counter: AtomicU64,
    upstream_id: AtomicU32,
}

impl EnonceAllocator {
    pub(crate) fn new(extranonces: Extranonces, upstream_id: u32) -> Self {
        Self {
            extranonces: RwLock::new(extranonces),
            enonce_counter: AtomicU64::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            ),
            upstream_id: AtomicU32::new(upstream_id),
        }
    }

    pub(crate) fn next_enonce1(&self) -> Extranonce {
        let counter = self.enonce_counter.fetch_add(1, Ordering::Relaxed);
        let extranonces = self.extranonces.read();

        match &*extranonces {
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

    pub(crate) fn enonce2_size(&self) -> usize {
        self.extranonces.read().enonce2_size()
    }

    pub(crate) fn extranonces(&self) -> parking_lot::RwLockReadGuard<'_, Extranonces> {
        self.extranonces.read()
    }

    pub(crate) fn update_extranonces(&self, extranonces: Extranonces) {
        *self.extranonces.write() = extranonces;
    }

    pub(crate) fn set_upstream_id(&self, id: u32) {
        self.upstream_id.store(id, Ordering::Relaxed);
    }

    pub(crate) fn upstream_id(&self) -> u32 {
        self.upstream_id.load(Ordering::Relaxed)
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

    #[test]
    fn pool_enonce1() {
        let allocator = pool_allocator();
        let e1 = allocator.next_enonce1();
        let e2 = allocator.next_enonce1();

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
            let enonce = allocator.next_enonce1();
            assert!(seen.insert(enonce), "duplicate enonce1 generated");
        }
    }

    #[test]
    fn proxy_enonce1() {
        let upstream_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        let allocator = EnonceAllocator::new(
            Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1.clone(), 8, 2).unwrap()),
            7,
        );

        assert_eq!(allocator.enonce2_size(), 6);

        let e1 = allocator.next_enonce1();
        let e2 = allocator.next_enonce1();

        assert_eq!(e1.len(), 6);
        assert_eq!(&e1.as_bytes()[..4], upstream_enonce1.as_bytes());

        let ext1 = u16::from_le_bytes(e1.as_bytes()[4..6].try_into().unwrap());
        let ext2 = u16::from_le_bytes(e2.as_bytes()[4..6].try_into().unwrap());
        assert_eq!(ext2, ext1.wrapping_add(1));
        assert_eq!(allocator.upstream_id(), 7);
    }

    #[test]
    fn proxy_mode_extension_size_1() {
        let upstream_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef]);
        let allocator = EnonceAllocator::new(
            Extranonces::Proxy(ProxyExtranonces::new(upstream_enonce1.clone(), 8, 1).unwrap()),
            0,
        );

        let e1 = allocator.next_enonce1();
        assert_eq!(e1.len(), 5);
        assert_eq!(&e1.as_bytes()[..4], upstream_enonce1.as_bytes());
        assert_eq!(allocator.enonce2_size(), 7);

        let e2 = allocator.next_enonce1();
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

        let before = allocator.next_enonce1();
        assert_eq!(&before.as_bytes()[..4], old_enonce1.as_bytes());
        assert_eq!(allocator.enonce2_size(), 6);

        let new_enonce1 = Extranonce::from_bytes(&[0x11, 0x22, 0x33, 0x44]);
        allocator.update_extranonces(Extranonces::Proxy(
            ProxyExtranonces::new(new_enonce1.clone(), 8, 2).unwrap(),
        ));

        let after = allocator.next_enonce1();
        assert_eq!(&after.as_bytes()[..4], new_enonce1.as_bytes());
        assert_eq!(allocator.enonce2_size(), 6);
    }

    #[test]
    fn set_upstream_id() {
        let allocator = pool_allocator();
        assert_eq!(allocator.upstream_id(), 0);
        allocator.set_upstream_id(42);
        assert_eq!(allocator.upstream_id(), 42);
    }
}
