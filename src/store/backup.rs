use {super::*, std::ops::Bound, tempfile::NamedTempFile};

const BACKUP_CACHE_BYTES: usize = 64 * 1024 * 1024;

impl Store {
    pub(crate) fn backup(
        source: &Path,
        destination: &Path,
        cancel_token: &CancellationToken,
    ) -> Result {
        let destination_parent = validate_paths(source, destination)?;

        check_cancelled(cancel_token)?;

        let source_db = Database::builder()
            .set_concurrency_mode(ConcurrencyMode::SingleWriter)
            .set_cache_size(BACKUP_CACHE_BYTES)
            .open_read_only(source)
            .with_context(|| format!("failed to open source database {}", source.display()))?;

        let read_txn = source_db
            .begin_read()
            .context("failed to begin read transaction on source database")?;

        verify_known_tables(&read_txn, source)?;
        validate_metadata(&read_txn, source)?;

        let (temp_file, temp_path) = NamedTempFile::new_in(destination_parent)
            .with_context(|| {
                format!(
                    "failed to create temporary backup file in {}",
                    destination_parent.display(),
                )
            })?
            .into_parts();

        let mut dest_db = FileBackend::new(temp_file)
            .and_then(|backend| {
                Database::builder()
                    .set_cache_size(BACKUP_CACHE_BYTES)
                    .create_with_backend(backend)
            })
            .with_context(|| {
                format!(
                    "failed to create temporary backup database {}",
                    temp_path.display(),
                )
            })?;

        let write_txn = dest_db.begin_write()?;

        copy_table(&read_txn, &write_txn, METADATA, cancel_token)?;
        copy_table(&read_txn, &write_txn, ORDERS, cancel_token)?;
        copy_table(&read_txn, &write_txn, WALLET, cancel_token)?;
        copy_table(&read_txn, &write_txn, USERS, cancel_token)?;
        copy_table(&read_txn, &write_txn, BLOCKS, cancel_token)?;

        read_txn.close()?;
        source_db.close()?;

        check_cancelled(cancel_token)?;

        write_txn.commit()?;

        check_cancelled(cancel_token)?;

        dest_db
            .compact()
            .context("failed to compact backup database")?;

        check_cancelled(cancel_token)?;

        ensure!(
            dest_db
                .check_integrity()
                .context("failed to verify backup database integrity")?,
            "backup database failed integrity check",
        );

        dest_db.close().context("failed to close backup database")?;

        check_cancelled(cancel_token)?;

        temp_path.persist_noclobber(destination).map_err(|err| {
            anyhow!(
                "failed to finalize backup at {}: {}",
                destination.display(),
                err.error,
            )
        })?;

        Ok(())
    }
}

fn check_cancelled(cancel_token: &CancellationToken) -> Result {
    ensure!(!cancel_token.is_cancelled(), "backup cancelled");
    Ok(())
}

fn validate_paths<'a>(source: &Path, destination: &'a Path) -> Result<&'a Path> {
    let source_meta = fs::metadata(source)
        .with_context(|| format!("failed to inspect source database {}", source.display()))?;

    ensure!(
        source_meta.is_file(),
        "source {} is not a regular file",
        source.display(),
    );

    ensure!(
        !is_directory_syntax(destination),
        "destination {} must be a file path, not a directory",
        destination.display(),
    );

    let destination_parent = match destination.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };

    match fs::symlink_metadata(destination) {
        Ok(dest_meta) => {
            if dest_meta.is_dir() {
                bail!("destination {} is a directory", destination.display());
            }

            bail!("destination {} already exists", destination.display());
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| {
                format!("failed to inspect destination {}", destination.display())
            });
        }
    }

    Ok(destination_parent)
}

fn is_directory_syntax(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    let ends_with_sep = |slice: &[u8]| {
        slice
            .last()
            .is_some_and(|byte| byte.is_ascii() && std::path::is_separator(char::from(*byte)))
    };

    path.file_name().is_none()
        || ends_with_sep(bytes)
        || (bytes.ends_with(b".") && ends_with_sep(&bytes[..bytes.len() - 1]))
}

fn verify_known_tables(read_txn: &ReadTransaction, source: &Path) -> Result {
    let known = [
        METADATA.name(),
        ORDERS.name(),
        WALLET.name(),
        USERS.name(),
        BLOCKS.name(),
    ];

    for table in read_txn.list_tables()? {
        ensure!(
            known.contains(&table.name()),
            "unexpected table '{}' in source database {}",
            table.name(),
            source.display(),
        );
    }

    if let Some(table) = read_txn.list_multimap_tables()?.next() {
        bail!(
            "unexpected multimap table '{}' in source database {}",
            table.name(),
            source.display(),
        );
    }

    Ok(())
}

fn validate_metadata(read_txn: &ReadTransaction, source: &Path) -> Result {
    let table = read_txn.open_table(METADATA).with_context(|| {
        format!(
            "failed to open {} table in source database {}",
            METADATA.name(),
            source.display(),
        )
    })?;

    let bytes = table.get(METADATA_KEY)?.with_context(|| {
        format!(
            "source database {} is missing store metadata entry",
            source.display(),
        )
    })?;

    let stored: Metadata = ciborium::from_reader(bytes.value()).context("decode store meta")?;

    ensure!(
        stored.schema_version == SCHEMA_VERSION,
        "store schema version mismatch at {}: expected {SCHEMA_VERSION}, got {}",
        source.display(),
        stored.schema_version,
    );

    Ok(())
}

fn copy_table<K, V>(
    read_txn: &ReadTransaction,
    write_txn: &WriteTransaction,
    definition: TableDefinition<K, V>,
    cancel_token: &CancellationToken,
) -> Result
where
    K: Key + 'static,
    V: Value + 'static,
{
    let source = read_txn
        .open_table(definition)
        .with_context(|| format!("failed to open source table {}", definition.name()))?;
    let mut destination = write_txn
        .open_table(definition)
        .with_context(|| format!("failed to open destination table {}", definition.name()))?;
    let mut cursor = destination.upper_bound_mut(Bound::<K::SelfType<'_>>::Unbounded)?;

    for entry in source.iter()? {
        check_cancelled(cancel_token)?;
        let (key, value) = entry?;
        cursor.insert_before(key.value(), value.value())?;
    }

    cursor.close()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use {super::*, crate::store::tests::test_order_entry, redb::MultimapTableDefinition};

    fn populate_store(store: &Store) {
        let address = "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
            .parse::<Address<NetworkUnchecked>>()
            .unwrap()
            .assume_checked();
        let changeset = ChangeSet {
            network: Some(Network::Regtest),
            ..Default::default()
        };
        let blocks = [
            BlockHash::from_byte_array([1; 32]),
            BlockHash::from_byte_array([2; 32]),
        ];

        let txn = store.begin().unwrap();
        txn.insert_order(1, &test_order_entry(OrderStatus::Active))
            .unwrap();
        txn.insert_order(2, &test_order_entry(OrderStatus::Fulfilled))
            .unwrap();
        txn.merge_wallet(&changeset).unwrap();
        txn.upsert_user(
            &address,
            &entry::UserEntry {
                authorized_secs: 7,
                workers: Vec::new(),
            },
        )
        .unwrap();
        txn.write_blocks(&blocks).unwrap();
        txn.commit().unwrap();
    }

    fn write_metadata_only(path: &Path, schema_version: Option<u64>) {
        let db = Database::create(path).unwrap();
        let txn = db.begin_write().unwrap();
        {
            let mut table = txn.open_table(METADATA).unwrap();
            if let Some(schema_version) = schema_version {
                let mut bytes = Vec::new();
                ciborium::into_writer(
                    &Metadata {
                        schema_version,
                        chain: Chain::Regtest,
                    },
                    &mut bytes,
                )
                .unwrap();
                table.insert(METADATA_KEY, bytes.as_slice()).unwrap();
            }
        }
        txn.commit().unwrap();
    }

    #[test]
    fn offline_backup_preserves_all_tables() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("foo.redb");
        let destination = dir.path().join("bar.redb");

        {
            let store = Store::open(&source, Chain::Regtest).unwrap();
            populate_store(&store);
        }

        Store::backup(&source, &destination, &CancellationToken::new()).unwrap();

        let restored = Store::open(&destination, Chain::Regtest).unwrap();
        let orders = restored.read_orders().unwrap();
        assert_eq!(orders.len(), 2);
        assert_eq!(orders[0].0, 1);
        assert_eq!(orders[0].1.status, OrderStatus::Active);
        assert_eq!(orders[1].0, 2);
        assert_eq!(orders[1].1.status, OrderStatus::Fulfilled);
        assert_eq!(
            restored.read_wallet_changeset().unwrap().network,
            Some(Network::Regtest)
        );
        assert_eq!(restored.read_users().unwrap().len(), 1);
        assert_eq!(restored.read_blocks().unwrap().len(), 2);
    }

    #[test]
    fn live_concurrent_backup_does_not_block_writer() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("foo.redb");
        let store = Arc::new(Store::open(&source, Chain::Regtest).unwrap());
        populate_store(&store);

        let stop = Arc::new(AtomicBool::new(false));
        let committed = Arc::new(AtomicU32::new(2));
        let writer_store = store.clone();
        let writer_stop = stop.clone();
        let writer_committed = committed.clone();

        let writer = thread::spawn(move || {
            let mut next_id = 3u32;
            while !writer_stop.load(Ordering::Relaxed) {
                let txn = writer_store.begin().unwrap();
                txn.insert_order(next_id, &test_order_entry(OrderStatus::Active))
                    .unwrap();
                txn.commit().unwrap();
                writer_committed.store(next_id, Ordering::Release);
                next_id += 1;
            }
            next_id - 1
        });

        while committed.load(Ordering::Acquire) < 5 {
            thread::yield_now();
        }

        let mut backups = Vec::new();
        for index in 0..3 {
            let destination = dir.path().join(format!("bar-{index}.redb"));
            Store::backup(&source, &destination, &CancellationToken::new()).unwrap();
            backups.push(destination);
        }

        stop.store(true, Ordering::Relaxed);
        let final_id = writer.join().unwrap();
        assert!(final_id >= 5);

        let mut previous_count = 0;
        for destination in backups {
            let restored = Store::open(&destination, Chain::Regtest).unwrap();
            let orders = restored.read_orders().unwrap();
            assert!(orders.len() >= 5);
            assert!(orders.len() >= previous_count);
            for (offset, (id, _)) in orders.iter().enumerate() {
                assert_eq!(*id, u32::try_from(offset + 1).unwrap());
            }
            previous_count = orders.len();
        }
    }

    #[test]
    fn compacts_fragmented_live_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("foo.redb");
        let destination = dir.path().join("bar.redb");
        let store = Store::open(&source, Chain::Regtest).unwrap();
        let _pinned = store.db.begin_read().unwrap();

        for round in 0..30 {
            let txn = store.begin().unwrap();
            for id in 0..20 {
                let status = if round % 2 == 0 {
                    OrderStatus::Active
                } else {
                    OrderStatus::Fulfilled
                };
                txn.insert_order(id, &test_order_entry(status)).unwrap();
            }
            txn.commit().unwrap();
        }

        Store::backup(&source, &destination, &CancellationToken::new()).unwrap();

        assert!(fs::metadata(&destination).unwrap().len() < fs::metadata(&source).unwrap().len());
        let restored = Store::open(&destination, Chain::Regtest).unwrap();
        assert_eq!(restored.read_orders().unwrap().len(), 20);
    }

    #[test]
    fn rejects_invalid_paths() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("foo.redb");
        let existing = dir.path().join("existing.redb");
        let existing_dir = dir.path().join("subdir");
        fs::create_dir(&existing_dir).unwrap();
        fs::write(&existing, "bar").unwrap();

        {
            let store = Store::open(&source, Chain::Regtest).unwrap();
            populate_store(&store);
        }

        #[track_caller]
        fn case(src: &Path, dst: &Path, expected: &str) {
            let err = Store::backup(src, dst, &CancellationToken::new()).unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "expected error containing {expected:?}, got {err:#}"
            );
        }

        case(
            &dir.path().join("missing.redb"),
            &dir.path().join("out.redb"),
            "failed to inspect source database",
        );
        case(
            &existing_dir,
            &dir.path().join("out.redb"),
            "not a regular file",
        );
        case(&source, &source, "already exists");
        case(
            &source,
            &dir.path().join(".").join("foo.redb"),
            "already exists",
        );
        case(&source, &existing, "already exists");
        case(&source, &existing_dir, "is a directory");
        case(&source, Path::new(""), "must be a file path");
        case(&source, Path::new("."), "must be a file path");
        case(&source, &dir.path().join(".."), "must be a file path");
        case(
            &source,
            &PathBuf::from(format!("{}/bar.redb/", dir.path().display())),
            "must be a file path",
        );
        case(
            &source,
            &PathBuf::from(format!("{}/bar.redb/.", dir.path().display())),
            "must be a file path",
        );
        case(
            &source,
            &dir.path().join("missing_dir").join("bar.redb"),
            "failed to create temporary backup file",
        );
        case(
            &source,
            &existing.join("bar.redb"),
            "failed to inspect destination",
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let symlink_to_source = dir.path().join("link.redb");
            symlink(&source, &symlink_to_source).unwrap();
            case(&source, &symlink_to_source, "already exists");

            let hardlink_to_source = dir.path().join("hard.redb");
            fs::hard_link(&source, &hardlink_to_source).unwrap();
            case(&source, &hardlink_to_source, "already exists");

            let broken_symlink = dir.path().join("broken.redb");
            symlink(dir.path().join("nonexistent.redb"), &broken_symlink).unwrap();
            case(&source, &broken_symlink, "already exists");
        }

        assert_eq!(fs::read_to_string(&existing).unwrap(), "bar");
    }

    #[test]
    fn rejects_invalid_databases() {
        let dir = tempfile::tempdir().unwrap();
        let dest_dir = dir.path().join("dest");
        fs::create_dir(&dest_dir).unwrap();

        #[track_caller]
        fn case(src: &Path, dest_dir: &Path, expected: &str) {
            let dst = dest_dir.join("out.redb");
            let err = Store::backup(src, &dst, &CancellationToken::new()).unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "expected error containing {expected:?}, got {err:#}"
            );
            assert!(!dst.exists());
            assert_eq!(fs::read_dir(dest_dir).unwrap().count(), 0);
        }

        let not_redb = dir.path().join("plain.redb");
        fs::write(&not_redb, "foo").unwrap();
        case(&not_redb, &dest_dir, "failed to open source database");

        let empty_db = dir.path().join("empty.redb");
        Database::create(&empty_db).unwrap();
        case(&empty_db, &dest_dir, "failed to open METADATA table");

        let empty_meta = dir.path().join("empty_meta.redb");
        write_metadata_only(&empty_meta, None);
        case(&empty_meta, &dest_dir, "missing store metadata entry");

        let corrupt_meta = dir.path().join("corrupt_meta.redb");
        {
            let db = Database::create(&corrupt_meta).unwrap();
            let txn = db.begin_write().unwrap();
            {
                let mut table = txn.open_table(METADATA).unwrap();
                table.insert(METADATA_KEY, b"foo".as_slice()).unwrap();
            }
            txn.commit().unwrap();
        }
        case(&corrupt_meta, &dest_dir, "decode store meta");

        let bad_schema = dir.path().join("schema.redb");
        write_metadata_only(&bad_schema, Some(SCHEMA_VERSION + 1));
        case(&bad_schema, &dest_dir, "schema version mismatch");

        let missing_orders = dir.path().join("missing_orders.redb");
        write_metadata_only(&missing_orders, Some(SCHEMA_VERSION));
        case(
            &missing_orders,
            &dest_dir,
            "failed to open source table ORDERS",
        );

        let extra_table = dir.path().join("extra.redb");
        {
            let store = Store::open(&extra_table, Chain::Regtest).unwrap();
            let txn = store.db.begin_write().unwrap();
            let extra: TableDefinition<u32, u32> = TableDefinition::new("EXTRA");
            txn.open_table(extra).unwrap();
            txn.commit().unwrap();
        }
        case(&extra_table, &dest_dir, "unexpected table 'EXTRA'");

        let extra_multimap = dir.path().join("extra_multi.redb");
        {
            let store = Store::open(&extra_multimap, Chain::Regtest).unwrap();
            let txn = store.db.begin_write().unwrap();
            let extra: MultimapTableDefinition<u32, u32> =
                MultimapTableDefinition::new("EXTRA_MULTI");
            txn.open_multimap_table(extra).unwrap();
            txn.commit().unwrap();
        }
        case(
            &extra_multimap,
            &dest_dir,
            "unexpected multimap table 'EXTRA_MULTI'",
        );

        let exclusive_path = dir.path().join("exclusive.redb");
        let _exclusive = Database::create(&exclusive_path).unwrap();
        case(&exclusive_path, &dest_dir, "failed to open source database");
    }

    #[test]
    fn cancelled_backup_leaves_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("foo.redb");
        let dest_dir = dir.path().join("dest");
        fs::create_dir(&dest_dir).unwrap();
        let destination = dest_dir.join("bar.redb");

        {
            let store = Store::open(&source, Chain::Regtest).unwrap();
            populate_store(&store);
        }

        let cancel = CancellationToken::new();
        cancel.cancel();

        let err = Store::backup(&source, &destination, &cancel).unwrap_err();
        assert!(err.to_string().contains("backup cancelled"));
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(&dest_dir).unwrap().count(), 0);

        let source_db = Database::builder()
            .set_concurrency_mode(ConcurrencyMode::SingleWriter)
            .open_read_only(&source)
            .unwrap();
        let read_txn = source_db.begin_read().unwrap();
        let dest_db = Database::create(dest_dir.join("tmp.redb")).unwrap();
        let write_txn = dest_db.begin_write().unwrap();

        let err = copy_table(&read_txn, &write_txn, ORDERS, &cancel).unwrap_err();
        assert!(err.to_string().contains("backup cancelled"));
    }
}
