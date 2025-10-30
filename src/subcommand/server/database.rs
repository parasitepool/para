use super::*;
use sqlx::Row;

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

#[derive(sqlx::FromRow, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub(crate) struct AccountData {
    pub(crate) id: i64,
    pub(crate) username: String,
    pub(crate) lnurl: Option<String>,
    pub(crate) past_lnurls: Vec<String>,
    pub(crate) total_diff: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct Database {
    pub(crate) pool: Pool<Postgres>,
}

impl Database {
    pub(crate) async fn new(database_url: String) -> Result<Self> {
        Ok(Self {
            pool: PgPoolOptions::new()
                .max_connections(5)
                .acquire_timeout(
                    if std::env::var("PARA_INTEGRATION_TEST")
                        .ok()
                        .filter(|v| v == "1")
                        .is_some()
                    {
                        Duration::from_millis(50)
                    } else {
                        Duration::from_secs(5)
                    },
                )
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

    pub(crate) async fn get_account(&self, username: &str) -> Result<AccountData> {
        let account = sqlx::query(
            "
            SELECT
                id,
                username,
                lnurl,
                past_lnurls,
                total_diff
            FROM accounts
            WHERE username = $1
            ",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| anyhow!(err))?;

        let row = match account {
            Some(r) => r,
            None => {
                return Err(anyhow!("Account not found"));
            }
        };

        let id: i64 = row.try_get("id").map_err(|err| anyhow!(err))?;
        let username: String = row.try_get("username").map_err(|err| anyhow!(err))?;
        let lnurl: Option<String> = row.try_get("lnurl").ok();
        let total_diff: i64 = row.try_get("total_diff").map_err(|err| anyhow!(err))?;

        let past_lnurls_json: sqlx::types::JsonValue =
            row.try_get("past_lnurls").map_err(|err| anyhow!(err))?;

        let past_lnurls: Vec<String> = match past_lnurls_json.as_array() {
            Some(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            None => vec![],
        };

        Ok(AccountData {
            id,
            username,
            lnurl,
            past_lnurls,
            total_diff,
        })
    }

    pub(crate) async fn get_account_payouts(
        &self,
        account_id: i64,
    ) -> Result<Vec<account::HistoricalPayout>> {
        let payouts = sqlx::query_as::<_, (i64, i64, i32, i32, String, Option<String>)>(
            "
            SELECT
                amount,
                diff_paid,
                blockheight_start,
                blockheight_end,
                status,
                failure_reason
            FROM payouts
            WHERE account_id = $1
            ORDER BY blockheight_end DESC
            ",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| anyhow!(err))?;

        Ok(payouts
            .into_iter()
            .map(
                |(amount, diff_paid, block_start, block_end, status, failure_reason)| {
                    account::HistoricalPayout {
                        amount: (amount as u32),
                        allocated_diff: diff_paid,
                        block_start: block_start as u32,
                        block_end: block_end as u32,
                        status,
                        failure_reason,
                    }
                },
            )
            .collect())
    }

    pub(crate) async fn update_account_lnurl(&self, username: &str, new_lnurl: &str) -> Result<()> {
        let rows_affected = sqlx::query(
            "
            UPDATE accounts
            SET lnurl = $1
            WHERE username = $2
            ",
        )
        .bind(new_lnurl)
        .bind(username)
        .execute(&self.pool)
        .await
        .map_err(|err| anyhow!(err))?
        .rows_affected();

        if rows_affected == 0 {
            sqlx::query(
                "
                INSERT INTO accounts (username, lnurl, total_diff, created_at, updated_at)
                VALUES ($1, $2, 0, NOW(), NOW())
                ",
            )
            .bind(username)
            .bind(new_lnurl)
            .execute(&self.pool)
            .await
            .map_err(|err| anyhow!(err))?;
        }

        Ok(())
    }
}
