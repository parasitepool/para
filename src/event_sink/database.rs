use {
    super::{Result, async_trait, event::*},
    sqlx::{Pool, Postgres, postgres::PgPoolOptions},
};

pub struct DatabaseSink {
    pool: Pool<Postgres>,
}

impl DatabaseSink {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl super::EventSink for DatabaseSink {
    async fn record(&mut self, event: Event) -> Result<u64> {
        let rows_changed = match event {
            Event::Share(share) => {
                sqlx::query(
                    "INSERT INTO shares (
                        blockheight, diff, sdiff, result, reject_reason,
                        workername, username, createdate
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, CURRENT_TIMESTAMP::TEXT)",
                )
                    .bind(share.blockheight)
                    .bind(share.pool_diff)
                    .bind(share.share_diff)
                    .bind(share.result)
                    .bind(&share.reject_reason)
                    .bind(&share.workername)
                    .bind(&share.address)
                    .execute(&self.pool)
                    .await?
            }
            Event::BlockFound(block) => {
                sqlx::query(
                    "INSERT INTO blocks (
                        blockheight, blockhash, workername, username, diff, coinbasevalue, time_found
                    ) VALUES ($1, $2, $3, $4, $5, $6,
                        COALESCE(to_timestamp($7), CURRENT_TIMESTAMP))",
                )
                    .bind(block.blockheight)
                    .bind(&block.blockhash)
                    .bind(&block.workername)
                    .bind(&block.address)
                    .bind(block.diff)
                    .bind(block.coinbase_value)
                    .bind(block.timestamp)
                    .execute(&self.pool)
                    .await?
            }
        }
            .rows_affected();
        Ok(rows_changed)
    }
}
