use super::*;
use crate::subcommand::server::account::Account;
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

#[derive(Debug, Clone)]
pub struct Database {
    pub(crate) pool: Pool<Postgres>,
}

impl Database {
    pub async fn new(database_url: String) -> Result<Self> {
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

    pub async fn get_account(&self, username: &str) -> Result<Account> {
        let account = sqlx::query(
            "
            SELECT
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

        Ok(Account {
            btc_address: username,
            ln_address: lnurl,
            past_ln_addresses: past_lnurls,
            total_diff,
        })
    }

    pub async fn update_account_lnurl(&self, username: &str, new_lnurl: &str) -> Result<()> {
        let current_account = self.get_account(username).await.ok();

        let updated_past_lnurls = if let Some(account) = current_account {
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
            Some(past_lnurls)
        } else {
            None
        };

        if let Some(past_lnurls) = updated_past_lnurls {
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
        }

        Ok(())
    }

    pub async fn migrate_accounts(&self) -> Result<u64> {
        let result = sqlx::query_scalar::<_, i64>("SELECT refresh_accounts()")
            .fetch_one(&self.pool)
            .await
            .map_err(|err| anyhow!(err))?;

        Ok(result as u64)
    }
}
