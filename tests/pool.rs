use super::*;

#[test]
fn pool() {
    let pool = TestPool::spawn();

    assert!(!pool.stratum_endpoint().is_empty());
}
