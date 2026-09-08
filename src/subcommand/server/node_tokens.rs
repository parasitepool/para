use {
    super::*, crate::http_server::auth::unauthorized, axum::http::request::Parts,
    bitcoin::hashes::sha256,
};

/// Passes with a live per-node token, or the admin token
pub(crate) struct NodeAuth {
    /// Node the token was minted for; `None` for admin or when auth is off
    pub(crate) name: Option<String>,
}

impl<S: Send + Sync> FromRequestParts<S> for NodeAuth {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let token = BearerAuth::token(&parts.headers);

        if let Some(token) = &token
            && let Some(database) = parts.extensions.get::<Database>()
        {
            match database.node_token_name(token).await {
                Ok(Some(name)) => return Ok(Self { name: Some(name) }),
                Ok(None) => {}
                Err(err) => warn!("Node token lookup failed: {err}"),
            }
        }

        let admin = parts.extensions.get::<BearerAuth>().is_none_or(|auth| {
            !auth.enabled()
                || token
                    .as_deref()
                    .is_some_and(|token| auth.accepts_admin(token))
        });

        if admin {
            Ok(Self { name: None })
        } else {
            Err(unauthorized())
        }
    }
}

#[derive(sqlx::FromRow, Debug, Clone, Serialize)]
pub struct NodeToken {
    pub name: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
    pub last_seen_at: Option<String>,
}

const NODE_TOKENS_DDL: &str = "CREATE TABLE IF NOT EXISTS node_tokens (
    name TEXT PRIMARY KEY,
    token_hash BYTEA NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    last_seen_at TIMESTAMPTZ
)";

fn hash(token: &str) -> Vec<u8> {
    sha256::Hash::hash(token.as_bytes())
        .to_byte_array()
        .to_vec()
}

impl Database {
    pub async fn ensure_node_tokens_table(&self) -> Result {
        sqlx::query(NODE_TOKENS_DDL)
            .execute(&self.pool)
            .await
            .context("failed to create node_tokens table")?;

        Ok(())
    }

    /// Returns the plaintext once; re-minting a name rotates its token
    pub async fn mint_node_token(&self, name: &str) -> Result<String> {
        let token = general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());

        sqlx::query(
            "INSERT INTO node_tokens (name, token_hash) VALUES ($1, $2)
             ON CONFLICT (name) DO UPDATE SET
                token_hash = EXCLUDED.token_hash,
                created_at = now(),
                revoked_at = NULL,
                last_seen_at = NULL",
        )
        .bind(name)
        .bind(hash(&token))
        .execute(&self.pool)
        .await
        .context("failed to mint node token")?;

        Ok(token)
    }

    pub async fn revoke_node_token(&self, name: &str) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE node_tokens SET revoked_at = now() WHERE name = $1 AND revoked_at IS NULL",
        )
        .bind(name)
        .execute(&self.pool)
        .await
        .context("failed to revoke node token")?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn list_node_tokens(&self) -> Result<Vec<NodeToken>> {
        sqlx::query_as(
            "SELECT name,
                    created_at::TEXT AS created_at,
                    revoked_at::TEXT AS revoked_at,
                    last_seen_at::TEXT AS last_seen_at
             FROM node_tokens ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list node tokens")
    }

    pub(crate) async fn node_token_name(&self, token: &str) -> Result<Option<String>> {
        sqlx::query_scalar(
            "SELECT name FROM node_tokens WHERE token_hash = $1 AND revoked_at IS NULL",
        )
        .bind(hash(token))
        .fetch_optional(&self.pool)
        .await
        .context("failed to look up node token")
    }

    pub(crate) async fn touch_node_token(&self, name: &str) -> Result {
        sqlx::query("UPDATE node_tokens SET last_seen_at = now() WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await
            .context("failed to update node token last_seen_at")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddl_matches_postgres_init() {
        let script = include_str!("../../../bin/postgres-init");
        let start = script
            .find("CREATE TABLE IF NOT EXISTS node_tokens")
            .expect("postgres-init should define node_tokens");
        let end = start + script[start..].find(");").unwrap() + 1;

        let normalize = |ddl: &str| ddl.split_whitespace().collect::<Vec<_>>().join(" ");

        assert_eq!(normalize(&script[start..end]), normalize(NODE_TOKENS_DDL));
    }
}
