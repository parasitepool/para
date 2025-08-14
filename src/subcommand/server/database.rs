use super::*;

#[derive(sqlx::FromRow, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub(crate) struct Split {
    pub(crate) worker_name: String,
    pub(crate) worker_total: i64,
    pub(crate) percentage: f64,
}

#[derive(sqlx::FromRow, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub(crate) struct Payout {
    pub(crate) worker_name: String,
    pub(crate) btcaddress: Option<String>,
    pub(crate) lnurl: Option<String>,
    pub(crate) payable_shares: i64,
    pub(crate) total_shares: i64,
    pub(crate) percentage: f64,
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
}
