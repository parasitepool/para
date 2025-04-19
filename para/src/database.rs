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
    pub(crate) lightning_address: Option<String>,
    pub(crate) payable_shares: i64,
    pub(crate) total_shares: i64,
    pub(crate) percentage: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct Database {
    pub(crate) pool: Pool<Postgres>,
}

impl Database {
    pub(crate) async fn new(options: &Options) -> Result<Self> {
        Ok(Self {
            pool: PgPoolOptions::new()
                .max_connections(5)
                .connect(&options.database_url())
                .await?,
        })
    }

    pub(crate) async fn get_splits(&self) -> Result<Vec<Split>> {
        sqlx::query_as::<_, Split>(
            "
            WITH worker_sums AS (
                SELECT
                    workername,
                    SUM(diff) AS worker_total
                FROM
                    shares
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
                (ws.worker_total / ts.grand_total) AS percentage
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

    pub(crate) async fn get_payouts(&self, blockheight: i32) -> Result<Vec<Payout>> {
        if blockheight <= 0 {
            return Err(anyhow!("Block height must be greater than 0"));
        }

        sqlx::query_as::<_, Payout>(
            "
            WITH target_block AS (
                SELECT id, blockheight
                FROM blocks
                WHERE blockheight = $1
                ORDER BY id ASC
                LIMIT 1
            ),
            previous_block AS (
                SELECT MAX(b.blockheight) AS prev_height
                FROM blocks b
                JOIN target_block tb ON b.blockheight < tb.blockheight
            ),
            qualified_shares AS (
                SELECT s.workername, SUM(s.diff) as total_diff
                FROM shares s, target_block tb, previous_block pb
                WHERE s.blockheight <= tb.blockheight
                    AND s.blockheight > pb.prev_height
                    AND s.reject_reason IS NULL
                GROUP BY s.workername
            ),
            sum_shares AS (
                SELECT SUM(total_diff) as grand_total
                FROM qualified_shares
            )
            SELECT
                qs.workername AS worker_name,
                u.username AS btcaddress,
                u.lightning_address,
                CAST(qs.total_diff as INT8) AS payable_shares,
                CAST(ss.grand_total as INT8) AS total_shares,
                ROUND((qs.total_diff / ss.grand_total * 100)::numeric, 4)::FLOAT8 as percentage
            FROM qualified_shares qs
            CROSS JOIN sum_shares ss
            LEFT JOIN users u ON qs.workername = u.workername
            ORDER BY qs.total_diff DESC;
            ",
        )
        .bind(blockheight)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| anyhow!(err))
    }
}
