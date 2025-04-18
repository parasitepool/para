use super::*;

#[derive(sqlx::FromRow, Deserialize, Serialize, Debug, Clone, PartialEq)]
pub(crate) struct Split {
    pub(crate) worker_name: String,
    pub(crate) worker_total: i64,
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
        Ok(sqlx::query_as::<_, Split>(
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
        .map_err(|err| anyhow!(err))?)
    }
}
