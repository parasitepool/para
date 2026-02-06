use super::*;

#[derive(Debug)]
pub(crate) struct PoolExtranonces {
    enonce1_size: usize,
    enonce2_size: usize,
}

#[derive(Debug)]
pub(crate) struct ProxyExtranonces {
    upstream_enonce1: Extranonce,
    downstream_enonce2_size: usize,
    extension_size: usize,
}

#[derive(Debug)]
pub(crate) enum Extranonces {
    Pool(PoolExtranonces),
    Proxy(ProxyExtranonces),
}

impl PoolExtranonces {
    pub(crate) fn new(enonce1_size: usize, enonce2_size: usize) -> Result<Self> {
        ensure!(
            enonce1_size >= MIN_ENONCE_SIZE,
            "enonce1_size {} below minimum {}",
            enonce1_size,
            MIN_ENONCE_SIZE
        );
        ensure!(
            enonce1_size <= MAX_ENONCE_SIZE,
            "enonce1_size {} exceeds maximum {}",
            enonce1_size,
            MAX_ENONCE_SIZE
        );
        ensure!(
            enonce2_size >= MIN_ENONCE_SIZE,
            "enonce2_size {} below minimum {}",
            enonce2_size,
            MIN_ENONCE_SIZE
        );
        ensure!(
            enonce2_size <= MAX_ENONCE_SIZE,
            "enonce2_size {} exceeds maximum {}",
            enonce2_size,
            MAX_ENONCE_SIZE
        );

        Ok(Self {
            enonce1_size,
            enonce2_size,
        })
    }

    pub(crate) fn enonce1_size(&self) -> usize {
        self.enonce1_size
    }

    pub(crate) fn enonce2_size(&self) -> usize {
        self.enonce2_size
    }
}

impl ProxyExtranonces {
    pub(crate) fn new(
        upstream_enonce1: Extranonce,
        upstream_enonce2_size: usize,
        extension_size: usize,
    ) -> Result<Self> {
        let upstream_enonce1_size = upstream_enonce1.len();

        ensure!(
            upstream_enonce1_size >= MIN_ENONCE_SIZE,
            "upstream enonce1 size {} below minimum {}",
            upstream_enonce1_size,
            MIN_ENONCE_SIZE
        );
        ensure!(
            upstream_enonce1_size <= MAX_ENONCE_SIZE,
            "upstream enonce1 size {} exceeds maximum {}",
            upstream_enonce1_size,
            MAX_ENONCE_SIZE
        );

        let downstream_enonce2_size = upstream_enonce2_size
            .checked_sub(extension_size)
            .ok_or_else(|| {
                anyhow!(
                    "upstream enonce2_size {} too small to carve out {} byte extension",
                    upstream_enonce2_size,
                    extension_size
                )
            })?;

        ensure!(
            downstream_enonce2_size >= MIN_ENONCE_SIZE,
            "miner enonce2 space {} below minimum {} (upstream enonce2_size {} - extension {})",
            downstream_enonce2_size,
            MIN_ENONCE_SIZE,
            upstream_enonce2_size,
            extension_size
        );
        ensure!(
            downstream_enonce2_size <= MAX_ENONCE_SIZE,
            "miner enonce2 space {} exceeds maximum {}",
            downstream_enonce2_size,
            MAX_ENONCE_SIZE
        );

        Ok(Self {
            upstream_enonce1,
            downstream_enonce2_size,
            extension_size,
        })
    }

    pub(crate) fn upstream_enonce1(&self) -> &Extranonce {
        &self.upstream_enonce1
    }

    pub(crate) fn extension_size(&self) -> usize {
        self.extension_size
    }

    #[cfg(test)]
    pub(crate) fn extended_enonce1_size(&self) -> usize {
        self.upstream_enonce1.len() + self.extension_size
    }

    pub(crate) fn downstream_enonce2_size(&self) -> usize {
        self.downstream_enonce2_size
    }

    pub(crate) fn reconstruct_enonce2_for_upstream(
        &self,
        miner_enonce1: &Extranonce,
        miner_enonce2: &Extranonce,
    ) -> Extranonce {
        let upstream_enonce1_size = self.upstream_enonce1.len();
        let extension = &miner_enonce1.as_bytes()[upstream_enonce1_size..];

        let mut upstream_enonce2 = Vec::with_capacity(extension.len() + miner_enonce2.len());
        upstream_enonce2.extend_from_slice(extension);
        upstream_enonce2.extend_from_slice(miner_enonce2.as_bytes());

        Extranonce::from_bytes(&upstream_enonce2)
    }
}

impl Extranonces {
    #[cfg(test)]
    pub(crate) fn enonce1_size(&self) -> usize {
        match self {
            Extranonces::Pool(p) => p.enonce1_size(),
            Extranonces::Proxy(p) => p.extended_enonce1_size(),
        }
    }

    pub(crate) fn enonce2_size(&self) -> usize {
        match self {
            Extranonces::Pool(p) => p.enonce2_size(),
            Extranonces::Proxy(p) => p.downstream_enonce2_size(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_rejects_enonce1_below_min() {
        let err = PoolExtranonces::new(1, 4).unwrap_err();
        assert!(err.to_string().contains("enonce1_size 1 below minimum"));
    }

    #[test]
    fn pool_rejects_enonce1_above_max() {
        let err = PoolExtranonces::new(9, 4).unwrap_err();
        assert!(err.to_string().contains("enonce1_size 9 exceeds maximum"));
    }

    #[test]
    fn pool_rejects_enonce2_below_min() {
        let err = PoolExtranonces::new(4, 1).unwrap_err();
        assert!(err.to_string().contains("enonce2_size 1 below minimum"));
    }

    #[test]
    fn pool_rejects_enonce2_above_max() {
        let err = PoolExtranonces::new(4, 9).unwrap_err();
        assert!(err.to_string().contains("enonce2_size 9 exceeds maximum"));
    }

    #[test]
    fn pool_accepts_valid_config() {
        let p = PoolExtranonces::new(4, 8).unwrap();
        assert_eq!(p.enonce1_size(), 4);
        assert_eq!(p.enonce2_size(), 8);
    }

    #[test]
    fn pool_accepts_boundary_values() {
        let p = PoolExtranonces::new(MIN_ENONCE_SIZE, MIN_ENONCE_SIZE).unwrap();
        assert_eq!(p.enonce1_size(), 2);
        assert_eq!(p.enonce2_size(), 2);

        let p = PoolExtranonces::new(MAX_ENONCE_SIZE, MAX_ENONCE_SIZE).unwrap();
        assert_eq!(p.enonce1_size(), 8);
        assert_eq!(p.enonce2_size(), 8);
    }

    fn test_upstream_enonce1() -> Extranonce {
        Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef])
    }

    #[test]
    fn proxy_rejects_upstream_enonce1_below_min() {
        let small_enonce1 = Extranonce::from_bytes(&[0xde]);
        let err = ProxyExtranonces::new(small_enonce1, 8, 2).unwrap_err();
        assert!(
            err.to_string()
                .contains("upstream enonce1 size 1 below minimum")
        );
    }

    #[test]
    fn proxy_rejects_upstream_enonce1_above_max() {
        let large_enonce1 = Extranonce::from_bytes(&[0; 9]);
        let err = ProxyExtranonces::new(large_enonce1, 8, 2).unwrap_err();
        assert!(
            err.to_string()
                .contains("upstream enonce1 size 9 exceeds maximum")
        );
    }

    #[test]
    fn proxy_rejects_upstream_enonce2_causing_underflow() {
        let err = ProxyExtranonces::new(test_upstream_enonce1(), 1, 2).unwrap_err();
        assert!(err.to_string().contains("too small to carve out"));
    }

    #[test]
    fn proxy_rejects_insufficient_miner_enonce2_space() {
        let err = ProxyExtranonces::new(test_upstream_enonce1(), 3, 2).unwrap_err();
        assert!(
            err.to_string()
                .contains("miner enonce2 space 1 below minimum")
        );
    }

    #[test]
    fn proxy_rejects_excessive_miner_enonce2_space() {
        let err = ProxyExtranonces::new(test_upstream_enonce1(), 11, 2).unwrap_err();
        assert!(
            err.to_string()
                .contains("miner enonce2 space 9 exceeds maximum")
        );
    }

    #[test]
    fn proxy_accepts_valid_config() {
        let p = ProxyExtranonces::new(test_upstream_enonce1(), 8, 2).unwrap();
        assert_eq!(p.downstream_enonce2_size(), 6);
        assert_eq!(p.extended_enonce1_size(), 6);
        assert_eq!(p.upstream_enonce1().as_bytes(), &[0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn proxy_accepts_boundary_minimum() {
        let p = ProxyExtranonces::new(test_upstream_enonce1(), 4, 2).unwrap();
        assert_eq!(p.downstream_enonce2_size(), 2);
    }

    #[test]
    fn proxy_accepts_boundary_maximum() {
        let p = ProxyExtranonces::new(test_upstream_enonce1(), 10, 2).unwrap();
        assert_eq!(p.downstream_enonce2_size(), 8);
    }

    #[test]
    fn proxy_computes_extended_enonce1_size() {
        let enonce1 = Extranonce::from_bytes(&[0xaa, 0xbb]);
        let p = ProxyExtranonces::new(enonce1, 6, 2).unwrap();
        assert_eq!(p.extended_enonce1_size(), 4);
    }

    #[test]
    fn proxy_extension_size_1() {
        let p = ProxyExtranonces::new(test_upstream_enonce1(), 8, 1).unwrap();
        assert_eq!(p.downstream_enonce2_size(), 7);
        assert_eq!(p.extended_enonce1_size(), 5);
        assert_eq!(p.extension_size(), 1);
    }

    #[test]
    fn extranonces_pool_delegates_correctly() {
        let pool = PoolExtranonces::new(4, 8).unwrap();
        let e = Extranonces::Pool(pool);
        assert_eq!(e.enonce1_size(), 4);
        assert_eq!(e.enonce2_size(), 8);
    }

    #[test]
    fn extranonces_proxy_delegates_correctly() {
        let proxy = ProxyExtranonces::new(test_upstream_enonce1(), 8, 2).unwrap();
        let e = Extranonces::Proxy(proxy);
        assert_eq!(e.enonce1_size(), 6);
        assert_eq!(e.enonce2_size(), 6);
    }

    #[test]
    fn proxy_reconstruct_enonce2_for_upstream() {
        let proxy = ProxyExtranonces::new(test_upstream_enonce1(), 8, 2).unwrap();
        let miner_enonce1 = Extranonce::from_bytes(&[0xde, 0xad, 0xbe, 0xef, 0x01, 0x02]);
        let miner_enonce2 = Extranonce::from_bytes(&[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);

        let upstream_enonce2 =
            proxy.reconstruct_enonce2_for_upstream(&miner_enonce1, &miner_enonce2);

        assert_eq!(
            upstream_enonce2.as_bytes(),
            &[0x01, 0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]
        );
    }
}
