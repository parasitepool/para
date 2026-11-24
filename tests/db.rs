use {
    super::*,
    redb::{
        ConcurrencyMode, Database, Durability, ReadOnlyDatabase, ReadableDatabase, TableDefinition,
    },
};

const METADATA: TableDefinition<u32, &[u8]> = TableDefinition::new("METADATA");
const ORDERS: TableDefinition<u32, &[u8]> = TableDefinition::new("ORDERS");
const TAPS: TableDefinition<u32, &[u8]> = TableDefinition::new("TAPS");
const WALLET: TableDefinition<u32, &[u8]> = TableDefinition::new("WALLET");
const USERS: TableDefinition<&str, &[u8]> = TableDefinition::new("USERS");
const BLOCKS: TableDefinition<u64, &[u8]> = TableDefinition::new("BLOCKS");

#[derive(Serialize)]
struct Metadata {
    schema_version: u64,
    chain: &'static str,
}

fn create_live_store(path: &std::path::Path) -> Database {
    let db = Database::builder()
        .set_concurrency_mode(ConcurrencyMode::SingleWriter)
        .create(path)
        .unwrap();

    let mut txn = db.begin_write().unwrap();
    txn.set_quick_repair(true);
    txn.set_durability(Durability::Immediate).unwrap();
    {
        let mut metadata = txn.open_table(METADATA).unwrap();
        let mut bytes = Vec::new();
        ciborium::into_writer(
            &Metadata {
                schema_version: 4,
                chain: "regtest",
            },
            &mut bytes,
        )
        .unwrap();
        metadata.insert(0, bytes.as_slice()).unwrap();

        let mut orders = txn.open_table(ORDERS).unwrap();
        orders.insert(1, b"foo".as_slice()).unwrap();

        txn.open_table(TAPS).unwrap();
        txn.open_table(WALLET).unwrap();
        txn.open_table(USERS).unwrap();
        txn.open_table(BLOCKS).unwrap();
    }
    txn.commit().unwrap();

    db
}

#[test]
fn cli_live_backup() {
    let dir = Arc::new(TempDir::new().unwrap());
    let source = dir.path().join("foo.redb");
    let destination = dir.path().join("bar.redb");

    let live_db = create_live_store(&source);

    let output = CommandBuilder::new("db backup --source foo.redb --destination bar.redb")
        .with_tempdir(dir.clone())
        .spawn()
        .wait_with_output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());

    let backup = ReadOnlyDatabase::open(&destination).unwrap();
    let read_txn = backup.begin_read().unwrap();
    let orders = read_txn.open_table(ORDERS).unwrap();
    assert_eq!(orders.get(1).unwrap().unwrap().value(), b"foo");

    let mut txn = live_db.begin_write().unwrap();
    txn.set_quick_repair(true);
    {
        let mut orders = txn.open_table(ORDERS).unwrap();
        orders.insert(2, b"bar".as_slice()).unwrap();
    }
    txn.commit().unwrap();
}

#[test]
fn cli_exit_codes_and_safeguards() {
    let dir = Arc::new(TempDir::new().unwrap());
    let source = dir.path().join("foo.redb");
    let existing = dir.path().join("existing.redb");
    fs::write(&existing, "bar").unwrap();

    let _live_db = create_live_store(&source);

    #[track_caller]
    fn case(dir: &Arc<TempDir>, args: &str, expected_code: i32, expected_stderr: &str) {
        let output = CommandBuilder::new(args)
            .with_tempdir(dir.clone())
            .spawn()
            .wait_with_output()
            .unwrap();

        assert_eq!(output.status.code(), Some(expected_code));
        assert!(output.stdout.is_empty());
        let stderr = std::str::from_utf8(&output.stderr).unwrap();
        assert!(
            stderr.contains(expected_stderr),
            "expected stderr containing {expected_stderr:?}, got {stderr:?}"
        );
    }

    case(
        &dir,
        "db backup --source foo.redb --destination foo.redb",
        1,
        "already exists",
    );
    case(
        &dir,
        "db backup --source foo.redb --destination existing.redb",
        1,
        "already exists",
    );
    case(&dir, "db backup", 2, "--source");
    case(&dir, "db backup --source foo.redb", 2, "--destination");

    assert_eq!(fs::read_to_string(&existing).unwrap(), "bar");
}
