use {
    crate::address,
    para::subcommand::sync::{FoundBlockRecord, Share},
};

pub(crate) fn create_test_shares(count: u32, blockheight: i64) -> Vec<Share> {
    (0..count)
        .map(|i| Share {
            id: i as i64 + 1,
            blockheight: Some(blockheight as i32),
            workinfoid: Some(i as i64 + 1000),
            clientid: Some(i as i64 + 100),
            enonce1: Some(format!("enonce1_{}", i)),
            nonce2: Some(format!("nonce2_{}", i)),
            nonce: Some(format!("nonce_{}", i)),
            ntime: Some("507f1f77".to_string()),
            diff: Some(1000.0 + i as f64),
            sdiff: Some(500.0 + i as f64),
            hash: Some(format!("hash_{:064x}", i)),
            result: Some(true),
            reject_reason: None,
            error: None,
            errn: None,
            createdate: Some("2024-01-01 12:00:00".to_string()),
            createby: Some("ckpool".to_string()),
            createcode: Some("".to_string()),
            createinet: Some("127.0.0.1".to_string()),
            workername: Some(format!("worker_{}", i % 5)),
            username: Some(format!("user_{}", i % 10)),
            lnurl: None,
            address: Some(address(i % 10u32).to_string()),
            agent: Some("test-agent".to_string()),
        })
        .collect()
}

pub(crate) fn create_test_block(blockheight: i64) -> FoundBlockRecord {
    FoundBlockRecord {
        id: blockheight as i32,
        blockheight: blockheight as i32,
        blockhash: format!(
            "00000000000000000008a89e854d57e5667df88f1bc3ba94de4c2d1f8c{:08x}",
            blockheight
        ),
        confirmed: Some(true),
        workername: Some("test_worker".to_string()),
        username: Some("test_user".to_string()),
        diff: Some(1000000.0),
        coinbasevalue: Some(625000000),
        rewards_processed: Some(false),
    }
}

pub(crate) async fn setup_test_schema(db_url: String) -> Result<(), Box<dyn std::error::Error>> {
    let pool = sqlx::PgPool::connect(&db_url).await?;

    sqlx::query(
        r#"
                CREATE TABLE IF NOT EXISTS shares (
                    id BIGSERIAL PRIMARY KEY,
                    blockheight INTEGER,
                    workinfoid BIGINT,
                    clientid BIGINT,
                    enonce1 TEXT,
                    nonce2 TEXT,
                    nonce TEXT,
                    ntime TEXT,
                    diff DOUBLE PRECISION,
                    sdiff DOUBLE PRECISION,
                    hash TEXT,
                    result BOOLEAN,
                    reject_reason TEXT,
                    error TEXT,
                    errn INTEGER,
                    createdate TEXT,
                    createby TEXT,
                    createcode TEXT,
                    createinet TEXT,
                    workername TEXT,
                    username TEXT,
                    lnurl TEXT,
                    address TEXT,
                    agent TEXT
                )
                "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
    CREATE TABLE IF NOT EXISTS remote_shares (
        id BIGINT,
        origin TEXT,
        blockheight INTEGER,
        workinfoid BIGINT,
        clientid BIGINT,
        enonce1 TEXT,
        nonce2 TEXT,
        nonce TEXT,
        ntime TEXT,
        diff DOUBLE PRECISION,
        sdiff DOUBLE PRECISION,
        hash TEXT,
        result BOOLEAN,
        reject_reason TEXT,
        error TEXT,
        errn INTEGER,
        createdate TEXT,
        createby TEXT,
        createcode TEXT,
        createinet TEXT,
        workername TEXT,
        username TEXT,
        lnurl TEXT,
        address TEXT,
        agent TEXT,

        PRIMARY KEY (id, origin)
    )"#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
                CREATE TABLE IF NOT EXISTS blocks (
                    id SERIAL PRIMARY KEY,
                    blockheight INTEGER UNIQUE NOT NULL,
                    blockhash TEXT NOT NULL,
                    confirmed BOOLEAN,
                    workername TEXT,
                    username TEXT,
                    diff DOUBLE PRECISION,
                    time_found TEXT,
                    coinbasevalue BIGINT,
                    rewards_processed BOOLEAN
                )
                "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
                CREATE TABLE IF NOT EXISTS accounts
                (
                    id               BIGSERIAL PRIMARY KEY,
                    username         VARCHAR(128) NOT NULL UNIQUE,
                    lnurl            VARCHAR(255),
                    past_lnurls      JSONB                    DEFAULT '[]'::JSONB,
                    total_diff       BIGINT                   DEFAULT 0,
                    lnurl_updated_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
                    created_at       TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
                    updated_at       TIMESTAMP WITH TIME ZONE DEFAULT NOW()
                )"#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
                CREATE TABLE IF NOT EXISTS payouts (
                    id BIGSERIAL PRIMARY KEY,
                    account_id BIGINT NOT NULL REFERENCES accounts(id),
                    amount BIGINT NOT NULL,
                    diff_paid BIGINT NOT NULL,
                    blockheight_start INTEGER NOT NULL,
                    blockheight_end INTEGER NOT NULL,
                    status TEXT NOT NULL,
                    failure_reason TEXT,
                    created_at TIMESTAMP DEFAULT NOW()
                )
                "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
                CREATE OR REPLACE FUNCTION compress_shares(start_id BIGINT, end_id BIGINT)
                RETURNS BIGINT AS $$
                BEGIN
                    -- This is a dummy function for testing
                    -- In production, this would implement actual compression logic
                    RETURN (SELECT COUNT(*) FROM shares WHERE id >= start_id AND id <= end_id);
                END;
                $$ LANGUAGE plpgsql;
                "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
CREATE OR REPLACE FUNCTION update_accounts_from_remote_shares()
    RETURNS TRIGGER AS $$
BEGIN
    INSERT INTO public.accounts (username, lnurl, total_diff, created_at, updated_at)
    VALUES (
        NEW.username,
        CASE
            WHEN NEW.lnurl IS NOT NULL AND TRIM(BOTH ' ' FROM NEW.lnurl) != ''
            THEN TRIM(BOTH ' ' FROM NEW.lnurl)
            ELSE NULL
        END,
        CASE WHEN NEW.result = true THEN NEW.diff ELSE 0 END,
        NOW(),
        NOW()
    )
    ON CONFLICT (username) DO UPDATE
    SET
        lnurl = CASE
            WHEN accounts.lnurl IS NULL
                AND EXCLUDED.lnurl IS NOT NULL
            THEN EXCLUDED.lnurl
            ELSE accounts.lnurl
        END,
        total_diff = accounts.total_diff + EXCLUDED.total_diff;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;"#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
CREATE TRIGGER trigger_update_accounts_on_remote_share
    AFTER INSERT ON remote_shares
    FOR EACH ROW
    WHEN (NEW.username IS NOT NULL AND NEW.username != '')
    EXECUTE FUNCTION update_accounts_from_remote_shares();"#,
    )
    .execute(&pool)
    .await?;

    pool.close().await;
    Ok(())
}

pub(crate) async fn insert_test_shares(
    db: String,
    count: u32,
    blockheight: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = sqlx::PgPool::connect(&db).await?;

    for i in 0..count {
        sqlx::query(
            r#"
                    INSERT INTO shares (
                        blockheight, workinfoid, clientid, enonce1, nonce2, nonce,
                        ntime, diff, sdiff, hash, result, workername, username, address
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
                    "#,
        )
        .bind(blockheight)
        .bind(i as i64 + 1000)
        .bind(i as i64 + 100)
        .bind(format!("enonce1_{}", i))
        .bind(format!("nonce2_{}", i))
        .bind(format!("nonce_{}", i))
        .bind("507f1f77")
        .bind(1000.0 + i as f64)
        .bind(500.0 + i as f64)
        .bind(format!("hash_{:064x}", i))
        .bind(true)
        .bind(format!("worker_{}", i % 5))
        .bind(format!("user_{}", i % 10))
        .bind(address(i % 10).to_string())
        .execute(&pool)
        .await?;
    }

    pool.close().await;
    Ok(())
}

pub(crate) async fn insert_test_remote_shares(
    db: String,
    count: u32,
    blockheight: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = sqlx::PgPool::connect(&db).await?;

    for i in 0..count {
        sqlx::query(
            r#"
                    INSERT INTO remote_shares (
                                               id, origin,
                        blockheight, workinfoid, clientid, enonce1, nonce2, nonce,
                        ntime, diff, sdiff, hash, result, workername, username, lnurl, address
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
                    "#,
        )
            .bind(i as i64)
            .bind("test_origin")
            .bind(blockheight)
            .bind(i as i64 + 1000)
            .bind(i as i64 + 100)
            .bind(format!("enonce1_{}", i))
            .bind(format!("nonce2_{}", i))
            .bind(format!("nonce_{}", i))
            .bind("507f1f77")
            .bind(1000.0 + i as f64)
            .bind(500.0 + i as f64)
            .bind(format!("hash_{:064x}", i))
            .bind(true)
            .bind(format!("worker_{}", i % 5))
            .bind(format!("user_{}", i % 10))
            .bind(format!("lnurl{}@test.gov", i % 10))
            .bind(address(i % 10).to_string())
            .execute(&pool)
            .await?;
    }

    pool.close().await;
    Ok(())
}

pub(crate) async fn insert_test_account(
    db_url: String,
    username: &str,
    lnurl: Option<&str>,
    past_lnurls: Vec<String>,
    total_diff: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = sqlx::PgPool::connect(&db_url).await?;

    let past_lnurls_json = serde_json::to_value(past_lnurls)?;

    sqlx::query(
        "
        INSERT INTO accounts (username, lnurl, past_lnurls, total_diff, created_at, updated_at)
        VALUES ($1, $2, $3, $4, NOW(), NOW())
        ",
    )
    .bind(username)
    .bind(lnurl)
    .bind(past_lnurls_json)
    .bind(total_diff)
    .execute(&pool)
    .await?;

    pool.close().await;
    Ok(())
}

pub(crate) async fn insert_test_block(
    db: String,
    blockheight: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = sqlx::PgPool::connect(&db).await?;

    sqlx::query(
        r#"
                INSERT INTO blocks (
                    blockheight, blockhash, confirmed, workername, username,
                    diff, time_found, coinbasevalue, rewards_processed
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                ON CONFLICT (blockheight) DO NOTHING
                "#,
    )
    .bind(blockheight)
    .bind(format!(
        "00000000000000000008a89e854d57e5667df88f1bc3ba94de4c2d1f8c{:08x}",
        blockheight
    ))
    .bind(true)
    .bind("test_worker")
    .bind("test_user")
    .bind(1000000.0)
    .bind("2024-01-01 12:00:00")
    .bind(625000000i64)
    .bind(false)
    .execute(&pool)
    .await?;

    pool.close().await;
    Ok(())
}
