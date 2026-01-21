use super::*;

fn setup_pg_db() -> PgTempDB {
    let psql_binpath = match Command::new("pg_config").arg("--bindir").output() {
        Ok(output) if output.status.success() => String::from_utf8(output.stdout)
            .ok()
            .map(|s| PathBuf::from(s.trim())),
        _ => None,
    };
    PgTempDB::from_builder(PgTempDBBuilder {
        initdb_args: Default::default(),
        temp_dir_prefix: None,
        db_user: None,
        password: None,
        port: None,
        dbname: None,
        persist_data_dir: false,
        dump_path: None,
        load_path: None,
        server_configs: Default::default(),
        bin_path: psql_binpath,
    })
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_file_sink_json() {
    let tempdir = TempDir::new().unwrap();
    let events_file = tempdir.path().join("events.json");

    {
        let pool = TestPool::spawn_with_args(format!(
            "--events-file {} --start-diff 0.000001",
            events_file.display()
        ));

        let mut miner = CommandBuilder::new(format!(
            "miner --mode continuous --username {} {}",
            signet_username(),
            pool.stratum_endpoint()
        ))
        .spawn();
        tokio::time::sleep(Duration::from_secs(1)).await;

        let _ = miner.kill();
        let _ = miner.wait();

        let status = pool.get_status().await;
        if let Ok(status) = status {
            println!("Pool status: {} accepted shares", status.accepted);
            assert!(status.accepted > 0, "No shares were submitted to pool");
        }
    }

    assert!(events_file.exists(), "JSON events file should be created");
    let contents = fs::read_to_string(&events_file).unwrap();

    if contents.is_empty() {
        panic!("Events file is empty - events may not have been flushed. Check pool logs.");
    }

    let share_count = contents
        .lines()
        .filter(|line| line.contains("\"type\":\"share\""))
        .count();
    assert!(
        share_count >= 3,
        "JSON file should have at least 3 shares, got {}. File contents:\n{}",
        share_count,
        if contents.len() > 1000 {
            &contents[..1000]
        } else {
            &contents
        }
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_file_sink_csv() {
    let tempdir = TempDir::new().unwrap();
    let events_csv = tempdir.path().join("events.csv");

    {
        let pool = TestPool::spawn_with_args(format!(
            "--events-file {} --start-diff 0.000001",
            events_csv.display()
        ));

        let mut miner = CommandBuilder::new(format!(
            "miner --mode continuous --username {} {}",
            signet_username(),
            pool.stratum_endpoint()
        ))
        .spawn();
        tokio::time::sleep(Duration::from_secs(1)).await;

        let _ = miner.kill();
        let _ = miner.wait();
    }

    assert!(events_csv.exists(), "CSV events file should be created");
    let contents = fs::read_to_string(&events_csv).unwrap();
    let share_lines = contents
        .lines()
        .filter(|line| line.contains(",share,"))
        .count();
    assert!(
        share_lines >= 3,
        "CSV file should have at least 3 shares, got {}",
        share_lines
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_database_sink() {
    use sqlx::PgPool;

    let pg_db = setup_pg_db();
    let database_url = pg_db.connection_uri();
    setup_test_schema(database_url.clone()).await.unwrap();

    {
        let pool = TestPool::spawn_with_args(format!(
            "--database-url {} --start-diff 0.000001",
            database_url
        ));

        let mut miner = CommandBuilder::new(format!(
            "miner --mode continuous --username {} {}",
            signet_username(),
            pool.stratum_endpoint()
        ))
        .spawn();
        tokio::time::sleep(Duration::from_secs(1)).await;

        let _ = miner.kill();
        let _ = miner.wait();
    }

    let db_pool = PgPool::connect(&database_url).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM shares")
        .fetch_one(&db_pool)
        .await
        .unwrap();
    assert!(
        count >= 5,
        "Database should have at least 5 shares, got {}",
        count
    );

    let (username, workername, result): (Option<String>, Option<String>, Option<bool>) =
        sqlx::query_as("SELECT username, workername, result FROM shares LIMIT 1")
            .fetch_one(&db_pool)
            .await
            .unwrap();

    assert!(username.is_some(), "Username should be present");
    assert!(workername.is_some(), "Workername should be present");
    assert!(result.is_some(), "Result should be present");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_multi_sink() {
    use sqlx::PgPool;

    let pg_db = setup_pg_db();
    let database_url = pg_db.connection_uri();
    setup_test_schema(database_url.clone()).await.unwrap();

    let tempdir = TempDir::new().unwrap();
    let multi_events = tempdir.path().join("multi-events.json");

    {
        let pool = TestPool::spawn_with_args(format!(
            "--events-file {} --database-url {} --start-diff 0.000001",
            multi_events.display(),
            database_url
        ));

        let mut miner = CommandBuilder::new(format!(
            "miner --mode continuous --username {} {}",
            signet_username(),
            pool.stratum_endpoint()
        ))
        .spawn();
        tokio::time::sleep(Duration::from_secs(1)).await;

        let _ = miner.kill();
        let _ = miner.wait();
    }

    assert!(multi_events.exists(), "Multi-sink file should exist");
    let file_contents = fs::read_to_string(&multi_events).unwrap();
    let file_shares = file_contents
        .lines()
        .filter(|line| line.contains("\"type\":\"share\""))
        .count();
    assert!(
        file_shares >= 5,
        "File should have at least 5 shares, got {}",
        file_shares
    );

    let db_pool = PgPool::connect(&database_url).await.unwrap();
    let db_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM shares")
        .fetch_one(&db_pool)
        .await
        .unwrap();
    assert!(
        db_count >= 5,
        "Database should have at least 5 shares, got {}",
        db_count
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_block_found_event() {
    use sqlx::PgPool;

    let pg_db = setup_pg_db();
    let database_url = pg_db.connection_uri();
    setup_test_schema(database_url.clone()).await.unwrap();

    let tempdir = TempDir::new().unwrap();
    let block_events = tempdir.path().join("block-events.json");

    {
        let pool = TestPool::spawn_with_args(format!(
            "--events-file {} --database-url {} --start-diff 0.0000001",
            block_events.display(),
            database_url
        ));

        pool.mine_block();
    }

    assert!(block_events.exists(), "Block events file should exist");
    let contents = fs::read_to_string(&block_events).unwrap();
    let has_block_event = contents.contains("\"type\":\"block_found\"");
    assert!(has_block_event, "File should have block found event");

    let db_pool = PgPool::connect(&database_url).await.unwrap();
    let block_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blocks")
        .fetch_one(&db_pool)
        .await
        .unwrap();
    assert_eq!(block_count, 1, "Database should have exactly 1 block");

    let (blockheight, blockhash, diff): (Option<i32>, Option<String>, Option<f64>) =
        sqlx::query_as("SELECT blockheight, blockhash, diff FROM blocks LIMIT 1")
            .fetch_one(&db_pool)
            .await
            .unwrap();

    assert!(
        blockheight.is_some() && blockheight.unwrap() > 0,
        "Block height should be positive"
    );
    assert!(
        blockhash.is_some() && !blockhash.unwrap().is_empty(),
        "Block hash should be present"
    );
    assert!(
        diff.is_some() && diff.unwrap() > 0.0,
        "Difficulty should be positive"
    );
}
