use super::*;

#[derive(sqlx::FromRow, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub(crate) struct Split {
    pub(crate) worker_name: String,
    pub(crate) worker_total: i64,
    pub(crate) percentage: f64,
}

#[derive(sqlx::FromRow, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct Payout {
    pub(crate) worker_name: String,
    pub btcaddress: Option<String>,
    pub(crate) lnurl: Option<String>,
    pub payable_shares: i64,
    pub total_shares: i64,
    pub percentage: f64,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct PendingPayout {
    pub ln_address: String,
    pub amount_sats: i64,
    pub payout_ids: Vec<i64>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
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
                .await?,
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
        }

        let Some(raw) = sqlx::query_as::<_, AccountRaw>(
            "
            SELECT
                username,
                lnurl,
                past_lnurls,
                total_diff,
                lnurl_updated_at::text as last_updated
            FROM accounts
            WHERE username = $1
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

    pub async fn migrate_accounts(&self) -> Result<u64> {
        let result = sqlx::query_scalar::<_, i64>("SELECT refresh_accounts()")
            .fetch_one(&self.pool)
            .await
            .map_err(|err| anyhow!(err))?;

        Ok(result as u64)
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
