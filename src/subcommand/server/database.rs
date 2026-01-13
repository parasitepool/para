use super::*;

#[derive(sqlx::FromRow, Deserialize, Serialize, Debug, Clone, PartialEq, ToSchema)]
pub struct HighestDiff {
    pub blockheight: i32,
    pub username: String,
    pub diff: f64,
}

#[derive(sqlx::FromRow, Deserialize, Serialize, Debug, Clone, PartialEq, ToSchema)]
pub(crate) struct Split {
    pub(crate) worker_name: String,
    pub(crate) worker_total: i64,
    pub(crate) percentage: f64,
}

#[derive(sqlx::FromRow, Deserialize, Serialize, Debug, Clone, PartialEq, ToSchema)]
pub struct Payout {
    pub(crate) worker_name: String,
    pub btcaddress: Option<String>,
    pub(crate) lnurl: Option<String>,
    pub payable_shares: i64,
    pub total_shares: i64,
    pub percentage: f64,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, ToSchema)]
pub struct PendingPayout {
    pub ln_address: String,
    pub amount_sats: i64,
    pub payout_ids: Vec<i64>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, ToSchema)]
pub struct SimulatedPayout {
    pub ln_address: String,
    pub btc_address: String,
    pub amount_sats: i64,
    pub percentage: f64,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, ToSchema)]
pub struct FailedPayout {
    pub btc_address: String,
    pub amount_sats: i64,
    pub payout_ids: Vec<i64>,
}

#[derive(Deserialize, Serialize, Debug, Clone, ToSchema)]
pub struct UpdatePayoutStatusRequest {
    pub payout_ids: Vec<i64>,
    pub status: String,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Database {
    pub(crate) pool: Pool<Postgres>,
}

impl Database {
    pub async fn new(database_url: String) -> Result<Self> {
        Ok(Self {
            pool: PgPoolOptions::new()
                .max_connections(5)
                .acquire_timeout(if integration_test() {
                    Duration::from_millis(50)
                } else {
                    Duration::from_secs(5)
                })
                .connect(&database_url)
                .await
                .with_context(|| format!("failed to connect to database at `{database_url}`"))?,
        })
    }

    pub(crate) async fn get_split(&self) -> Result<Vec<Split>> {
        sqlx::query_as::<_, Split>(
            "
            WITH worker_sums AS (
                SELECT
                    workername,
                    SUM(diff) AS worker_total
                FROM
                    remote_shares
                GROUP BY
                    workername
            ),
                 total_sum AS (
                     SELECT
                         SUM(worker_total) AS grand_total
                     FROM
                         worker_sums
                 )
            SELECT
                ws.workername AS worker_name,
                CAST(ws.worker_total AS INT8),
                ROUND((ws.worker_total / ts.grand_total)::numeric, 8)::FLOAT8 AS percentage
            FROM
                worker_sums ws
                    CROSS JOIN
                total_sum ts
            ORDER BY
                percentage DESC;
            ",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| anyhow!(err))
    }

    pub(crate) async fn get_total_coinbase(
        &self,
        blockheight: i32,
    ) -> Result<Option<(i32, String, i64, String, String)>> {
        sqlx::query_as::<_, (i32, String, i64, String, String)>(
            "
            SELECT blockheight, blockhash, coinbasevalue, workername, username
            FROM blocks
            WHERE blockheight = $1
            ",
        )
        .bind(blockheight)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| anyhow!(err))
    }

    pub(crate) async fn get_payouts(
        &self,
        blockheight: i32,
        btcaddress: String,
    ) -> Result<Vec<Payout>> {
        sqlx::query_as::<_, Payout>(
            "
            WITH target_block AS (
                SELECT id, blockheight
                FROM blocks
                WHERE blockheight = $1
                ORDER BY id
                LIMIT 1
            ),
            previous_block AS (
                SELECT MAX(b.blockheight) AS prev_height
                FROM blocks b
                JOIN target_block tb ON b.blockheight < tb.blockheight
            ),
            qualified_shares AS (
                SELECT s.workername, s.lnurl, s.username AS btcaddress, SUM(s.diff) as total_diff
                FROM remote_shares s, target_block tb, previous_block pb
                WHERE s.blockheight <= tb.blockheight
                    AND s.blockheight > pb.prev_height
                    AND s.username != $2
                    AND s.reject_reason IS NULL
                GROUP BY s.lnurl, s.username, s.workername
            ),
            sum_shares AS (
                SELECT SUM(total_diff) as grand_total
                FROM qualified_shares
            )
            SELECT
                qs.workername AS worker_name,
                qs.btcaddress,
                qs.lnurl,
                CAST(qs.total_diff as INT8) AS payable_shares,
                CAST(ss.grand_total as INT8) AS total_shares,
                ROUND((qs.total_diff / ss.grand_total)::numeric, 8)::FLOAT8 as percentage
            FROM qualified_shares qs
            CROSS JOIN sum_shares ss
            ORDER BY qs.total_diff DESC;
            ",
        )
        .bind(blockheight)
        .bind(btcaddress)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| anyhow!(err))
    }

    pub(crate) async fn get_payouts_range(
        &self,
        start_blockheight: i32,
        end_blockheight: i32,
        excluded_usernames: Vec<String>,
    ) -> Result<Vec<Payout>> {
        let exclusion_list = if excluded_usernames.is_empty() {
            vec!["".to_string()]
        } else {
            excluded_usernames
        };

        sqlx::query_as::<_, Payout>(
            "
            WITH qualified_shares AS (
                SELECT
                    s.workername,
                    s.lnurl,
                    s.username AS btcaddress,
                    SUM(s.diff) as total_diff
                FROM remote_shares s
                WHERE s.blockheight >= $1
                    AND s.blockheight < $2
                    AND s.username != ALL($3)
                    AND s.reject_reason IS NULL
                GROUP BY s.lnurl, s.username, s.workername
            ),
            sum_shares AS (
                SELECT SUM(total_diff) as grand_total
                FROM qualified_shares
            )
            SELECT
                qs.workername AS worker_name,
                qs.btcaddress,
                qs.lnurl,
                CAST(qs.total_diff as INT8) AS payable_shares,
                CAST(ss.grand_total as INT8) AS total_shares,
                ROUND((qs.total_diff / NULLIF(ss.grand_total, 0))::numeric, 8)::FLOAT8 as percentage
            FROM qualified_shares qs
            CROSS JOIN sum_shares ss
            WHERE ss.grand_total > 0
            ORDER BY qs.total_diff DESC;
        ",
        )
        .bind(start_blockheight)
        .bind(end_blockheight)
        .bind(&exclusion_list)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| anyhow!(err))
    }

    pub(crate) async fn get_user_payout_range(
        &self,
        start_blockheight: i32,
        end_blockheight: i32,
        target_username: String,
        excluded_usernames: Vec<String>,
    ) -> Result<Vec<Payout>> {
        if excluded_usernames.contains(&target_username) {
            return Ok(vec![]);
        }

        let exclusion_list = if excluded_usernames.is_empty() {
            vec!["".to_string()]
        } else {
            excluded_usernames
        };

        sqlx::query_as::<_, Payout>(
            "
        WITH qualified_shares AS (
            SELECT
                s.workername,
                s.lnurl,
                s.username AS btcaddress,
                SUM(s.diff) as total_diff
            FROM remote_shares s
            WHERE s.blockheight >= $1
                AND s.blockheight < $2
                AND s.username != ALL($4)
                AND s.reject_reason IS NULL
            GROUP BY s.lnurl, s.username, s.workername
        ),
        sum_shares AS (
            SELECT SUM(total_diff) as grand_total
            FROM qualified_shares
        ),
        user_shares AS (
            SELECT
                s.workername,
                s.lnurl,
                s.username AS btcaddress,
                SUM(s.diff) as total_diff
            FROM remote_shares s
            WHERE s.blockheight >= $1
                AND s.blockheight < $2
                AND s.username = $3
                AND s.reject_reason IS NULL
            GROUP BY s.lnurl, s.username, s.workername
        )
        SELECT
            us.workername AS worker_name,
            us.btcaddress,
            us.lnurl,
            CAST(us.total_diff as INT8) AS payable_shares,
            CAST(ss.grand_total as INT8) AS total_shares,
            ROUND((us.total_diff / NULLIF(ss.grand_total, 0))::numeric, 8)::FLOAT8 as percentage
        FROM user_shares us
        CROSS JOIN sum_shares ss
        WHERE ss.grand_total > 0
        ORDER BY us.total_diff DESC;
        ",
        )
        .bind(start_blockheight)
        .bind(end_blockheight)
        .bind(&target_username)
        .bind(&exclusion_list)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| anyhow!(err))
    }

    pub async fn get_account(&self, username: &str) -> Result<Option<Account>> {
        #[derive(sqlx::FromRow)]
        pub struct AccountRaw {
            pub username: String,
            pub lnurl: Option<String>,
            pub past_lnurls: sqlx::types::Json<Vec<String>>,
            pub total_diff: i64,
            pub last_updated: Option<String>,
            pub metadata: Option<serde_json::Value>,
        }

        let Some(raw) = sqlx::query_as::<_, AccountRaw>(
            "
            SELECT
                a.username,
                a.lnurl,
                a.past_lnurls,
                a.total_diff,
                a.lnurl_updated_at::text as last_updated,
                m.data as metadata
            FROM accounts a
            LEFT JOIN account_metadata m ON a.id = m.account_id
            WHERE a.username = $1
            ",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };

        Ok(Some(Account {
            btc_address: raw.username,
            ln_address: raw.lnurl,
            past_ln_addresses: raw.past_lnurls.0,
            total_diff: raw.total_diff,
            last_updated: raw.last_updated,
            metadata: raw.metadata,
        }))
    }

    pub async fn update_account_lnurl(
        &self,
        username: &str,
        new_lnurl: &str,
    ) -> Result<Option<Account>> {
        if let Some(account) = self.get_account(username).await? {
            if let Some(old_lnurl) = &account.ln_address
                && old_lnurl == new_lnurl
            {
                return Err(anyhow!(
                    "New lightning address matches existing lightning address"
                ));
            }
            let mut past_lnurls = account.past_ln_addresses.clone();

            if let Some(current_lnurl) = &account.ln_address
                && !current_lnurl.is_empty()
                && !past_lnurls.contains(current_lnurl)
            {
                past_lnurls.insert(0, current_lnurl.clone());
            }

            past_lnurls.truncate(10);

            let past_lnurls_json = serde_json::to_value(past_lnurls)
                .map_err(|err| anyhow!("Failed to serialize past_lnurls: {}", err))?;

            let rows_affected = sqlx::query(
                "
                UPDATE accounts
                SET lnurl = $1, past_lnurls = $2, lnurl_updated_at = NOW(), updated_at = NOW()
                WHERE username = $3
                ",
            )
            .bind(new_lnurl)
            .bind(past_lnurls_json)
            .bind(username)
            .execute(&self.pool)
            .await
            .map_err(|err| anyhow!(err))?
            .rows_affected();

            if rows_affected == 0 {
                return Err(anyhow!(
                    "Expected to update existing account but no rows affected"
                ));
            }
        } else {
            sqlx::query(
                "
                INSERT INTO accounts (username, lnurl, past_lnurls, total_diff, lnurl_updated_at, created_at, updated_at)
                VALUES ($1, $2, '[]'::jsonb, 0, NOW(), NOW(), NOW())
                ",
            )
                .bind(username)
                .bind(new_lnurl)
                .execute(&self.pool)
                .await
                .map_err(|err| anyhow!(err))?;
        };

        self.get_account(username).await
    }

    pub async fn update_account_metadata(
        &self,
        username: &str,
        metadata: &serde_json::Value,
    ) -> Result<Option<Account>> {
        let account_id: Option<i64> =
            sqlx::query_scalar("SELECT id FROM accounts WHERE username = $1")
                .bind(username)
                .fetch_optional(&self.pool)
                .await?;

        let Some(account_id) = account_id else {
            return Ok(None);
        };

        sqlx::query(
            "
            INSERT INTO account_metadata (account_id, data, created_at, updated_at)
            VALUES ($1, $2, NOW(), NOW())
            ON CONFLICT (account_id) DO UPDATE
            SET data = account_metadata.data || $2, updated_at = NOW()
            ",
        )
        .bind(account_id)
        .bind(metadata)
        .execute(&self.pool)
        .await
        .map_err(|err| anyhow!(err))?;

        self.get_account(username).await
    }

    pub async fn migrate_accounts(&self) -> Result<u64> {
        let result = sqlx::query_scalar::<_, i64>("SELECT refresh_accounts()")
            .fetch_one(&self.pool)
            .await
            .map_err(|err| anyhow!(err))?;

        Ok(result as u64)
    }

    pub async fn get_highestdiff(&self, blockheight: i32) -> Result<Option<HighestDiff>> {
        sqlx::query_as::<_, HighestDiff>(
            "
            SELECT
                blockheight,
                COALESCE(username, '') AS username,
                sdiff as diff
            FROM remote_shares
            WHERE blockheight = $1
            ORDER BY sdiff DESC
            LIMIT 1
            ",
        )
        .bind(blockheight)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| anyhow!(err))
    }

    pub async fn get_highestdiff_by_user(
        &self,
        blockheight: i32,
        username: &str,
    ) -> Result<Option<HighestDiff>> {
        sqlx::query_as::<_, HighestDiff>(
            "
            SELECT
                blockheight,
                COALESCE(username, '') AS username,
                sdiff as diff
            FROM remote_shares
            WHERE blockheight = $1 AND username = $2
            ORDER BY sdiff DESC
            LIMIT 1
            ",
        )
        .bind(blockheight)
        .bind(username)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| anyhow!(err))
    }

    pub async fn get_highestdiff_all_users(&self, blockheight: i32) -> Result<Vec<HighestDiff>> {
        sqlx::query_as::<_, HighestDiff>(
            "
            SELECT DISTINCT ON (username)
                blockheight,
                COALESCE(username, '') AS username,
                sdiff as diff
            FROM remote_shares
            WHERE blockheight = $1
            ORDER BY username, sdiff DESC
            ",
        )
        .bind(blockheight)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| anyhow!(err))
    }

    pub async fn get_pending_payouts(&self) -> Result<Vec<PendingPayout>> {
        #[derive(sqlx::FromRow)]
        struct PayoutRow {
            payout_id: i64,
            ln_address: String,
            amount: i64,
        }

        let rows = sqlx::query_as::<_, PayoutRow>(
            "
            SELECT
                p.id as payout_id,
                COALESCE(a.lnurl, '') as ln_address,
                p.amount
            FROM payouts p
            JOIN accounts a ON p.account_id = a.id
            WHERE p.status IN ('pending', 'failure')
                AND a.lnurl IS NOT NULL
                AND a.lnurl != ''
            ORDER BY a.lnurl, p.id
            ",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| anyhow!(err))?;

        let mut grouped: HashMap<String, (i64, Vec<i64>)> = HashMap::new();

        for row in rows {
            let entry = grouped
                .entry(row.ln_address.clone())
                .or_insert((0, Vec::new()));
            entry.0 += row.amount;
            entry.1.push(row.payout_id);
        }

        let mut result = Vec::new();
        for (ln_address, (amount_sats, payout_ids)) in grouped {
            result.push(PendingPayout {
                ln_address,
                amount_sats,
                payout_ids,
            });
        }

        result.sort_by(|a, b| b.amount_sats.cmp(&a.amount_sats));

        Ok(result)
    }

    pub async fn get_failed_payouts(&self) -> Result<Vec<FailedPayout>> {
        #[derive(sqlx::FromRow)]
        struct FailedPayoutRow {
            payout_id: i64,
            btc_address: String,
            amount: i64,
        }

        let rows = sqlx::query_as::<_, FailedPayoutRow>(
            "
            SELECT
                p.id as payout_id,
                a.username as btc_address,
                p.amount
            FROM payouts p
            JOIN accounts a ON p.account_id = a.id
            WHERE p.status IN ('failure')
            ORDER BY a.lnurl, p.id
            ",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| anyhow!(err))?;

        let mut grouped: HashMap<String, (i64, Vec<i64>)> = HashMap::new();

        for row in rows {
            let entry = grouped
                .entry(row.btc_address.clone())
                .or_insert((0, Vec::new()));
            entry.0 += row.amount;
            entry.1.push(row.payout_id);
        }

        let mut result = Vec::new();
        for (btc_address, (amount_sats, payout_ids)) in grouped {
            result.push(FailedPayout {
                btc_address,
                amount_sats,
                payout_ids,
            });
        }

        result.sort_by(|a, b| b.amount_sats.cmp(&a.amount_sats));

        Ok(result)
    }

    pub async fn get_simulated_payouts(
        &self,
        total_reward: i64,
        finder_username: &str,
    ) -> Result<Vec<SimulatedPayout>> {
        if total_reward <= 100_000_000 {
            // 1 BTC of coinbase value is reserved for miner who found the block
            return Ok(Vec::new());
        }
        #[derive(sqlx::FromRow)]
        struct PayoutRow {
            username: String,
            ln_address: String,
            amount: i64,
            percentage: f64,
        }

        let rows = sqlx::query_as::<_, PayoutRow>(
            "
            WITH eligible_accounts AS (
                SELECT
                    a.id as account_id,
                    a.username,
                    COALESCE(a.lnurl, '') as lnurl,
                    a.total_diff,
                    COALESCE(SUM(p.diff_paid), 0) as already_paid_diff
                FROM accounts a
                LEFT JOIN payouts p ON p.account_id = a.id
                    AND p.status != 'cancelled'
                GROUP BY a.id, a.username, a.lnurl, a.total_diff
            ),
            payable_accounts AS (
                SELECT
                    account_id,
                    username,
                    lnurl,
                    total_diff - already_paid_diff as unpaid_diff
                FROM eligible_accounts
                WHERE total_diff - already_paid_diff > 0
                    AND username != COALESCE($2, '')
            ),
            total_unpaid AS (
                SELECT SUM(unpaid_diff) as total_diff
                FROM payable_accounts
            )
            SELECT
                pa.username,
                pa.lnurl as ln_address,
                CASE
                    WHEN tu.total_diff > 0
                    THEN FLOOR((pa.unpaid_diff::NUMERIC / tu.total_diff::NUMERIC) * $1)::BIGINT
                    ELSE 0
                END as amount,
                CASE
                    WHEN tu.total_diff > 0
                    THEN ROUND((pa.unpaid_diff::NUMERIC / tu.total_diff::NUMERIC), 8)::FLOAT8
                    ELSE 0
                END as percentage
            FROM payable_accounts pa
            CROSS JOIN total_unpaid tu
            WHERE tu.total_diff > 0
            ORDER BY pa.unpaid_diff DESC
            ",
        )
        .bind(total_reward)
        .bind(finder_username)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| anyhow!(err))?;

        let mut grouped: HashMap<String, SimulatedPayout> = HashMap::new();
        for row in rows {
            let entry = grouped
                .entry(row.ln_address.clone())
                .or_insert_with(|| SimulatedPayout {
                    ln_address: row.ln_address.clone(),
                    btc_address: row.username.clone(),
                    amount_sats: 0,
                    percentage: 0.0,
                });
            entry.amount_sats += row.amount;
            entry.percentage += row.percentage;
        }

        let mut result: Vec<SimulatedPayout> = grouped.into_values().collect();
        result.sort_by(|a, b| b.amount_sats.cmp(&a.amount_sats));

        Ok(result)
    }

    pub async fn update_payout_status(
        &self,
        payout_ids: &[i64],
        status: &str,
        failure_reason: Option<&str>,
    ) -> Result<u64> {
        if payout_ids.is_empty() {
            return Ok(0);
        }

        let valid_statuses = ["pending", "processing", "success", "failure", "cancelled"];
        if !valid_statuses.contains(&status) {
            return Err(anyhow!("Invalid status: {}", status));
        }

        let rows_affected = sqlx::query(
            "
            UPDATE payouts
            SET status = $1,
                failure_reason = $2,
                updated_at = NOW()
            WHERE id = ANY($3)
            ",
        )
        .bind(status)
        .bind(failure_reason)
        .bind(payout_ids)
        .execute(&self.pool)
        .await
        .map_err(|err| anyhow!(err))?
        .rows_affected();

        Ok(rows_affected)
    }
}
