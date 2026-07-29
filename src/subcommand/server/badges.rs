use super::*;

/// Number of block-find participations that are awarded as individual,
/// non-stacking badges. Every block beyond this folds into the stacking bucket.
pub(crate) const BLOCK_UNIQUE_CAP: i64 = 3;

/// Version of the cached badge payload written into `account_metadata.data`.
/// Bump this when the payload shape or computation changes so stale caches are
/// recomputed on the next read.
pub(crate) const BADGES_VERSION: u32 = 1;

/// A loyalty badge instance is earned per this many blocks the user has
/// submitted a share in (`account_metadata.data.block_count`).
pub(crate) const LOYALTY_BLOCKS_PER_INSTANCE: i64 = 10_000;

pub(crate) const BLOCK_BADGE_ID: &str = "block";
pub(crate) const BLOCK_WINNER_BADGE_ID: &str = "block_winner";
pub(crate) const LOYALTY_BADGE_ID: &str = "loyalty";
pub(crate) const REFINERY_BADGE_ID: &str = "refinery";
pub(crate) const DISPENSER_BADGE_ID: &str = "dispenser";

/// Stacking policies (the `kind` field on `BadgeType`).
pub(crate) const KIND_UNIQUE_THEN_BUCKET: &str = "unique_then_bucket";
pub(crate) const KIND_UNIQUE: &str = "unique";
pub(crate) const KIND_BUCKET: &str = "bucket";

/// How long externally-fetched badges (refinery/dispenser) are served before a
/// re-poll. Only applies when external sources are configured; the
/// Postgres-derived badges are instead invalidated by a change fingerprint
/// (found-block tip/count + loyalty count), so they are not re-checked on this
/// cadence.
pub(crate) const BADGE_CACHE_TTL_SECS: i64 = 600;

/// Backstop age after which a payload is recomputed regardless of the change
/// fingerprint. The fingerprint catches inserts and deletions in `blocks`, but
/// not an in-place update (e.g. a re-org rewriting a block's winner at an
/// unchanged height), so this bounds how long such a change can go unnoticed.
pub(crate) const BADGE_MAX_AGE_SECS: i64 = 3600;

/// A single non-stacking badge instance, carrying the metadata that makes it
/// unique (for block badges, the block height it was earned on).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, ToSchema)]
pub(crate) struct BadgeInstance {
    pub(crate) blockheight: i32,
}

/// A stacking bucket: `count` identical badges collapsed into one, rendered with
/// a `+N` bubble on the client.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, ToSchema)]
pub(crate) struct BadgeBucket {
    pub(crate) count: i64,
}

/// The earned state of one badge type for an account.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, ToSchema)]
pub(crate) struct BadgeType {
    /// Stacking policy, e.g. `unique_then_bucket`.
    pub(crate) kind: String,
    /// Individually-earned, non-stacking instances (the earliest `unique_cap`).
    pub(crate) unique: Vec<BadgeInstance>,
    /// Everything beyond `unique_cap`, collapsed into a stacking count.
    pub(crate) bucket: BadgeBucket,
    /// Total earned across `unique` + `bucket` (`unique.len() + bucket.count`).
    pub(crate) total: i64,
}

/// Canonical per-account badge state. Cached inside `account_metadata.data`
/// under the `badges` key and served verbatim by `GET /badges/{address}`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, ToSchema)]
pub(crate) struct BadgesPayload {
    pub(crate) version: u32,
    pub(crate) computed_at: String,
    /// Max found-block height at compute time. Used to skip recomputing the
    /// Postgres-derived badges when no new block has been found since.
    #[serde(default)]
    pub(crate) source_tip: i64,
    /// Number of rows in `blocks` at compute time. Paired with `source_tip` so
    /// a deletion or a backfilled historical block also invalidates the cache.
    #[serde(default)]
    pub(crate) source_blocks: i64,
    /// Keyed by badge id (e.g. `block`).
    pub(crate) types: std::collections::HashMap<String, BadgeType>,
}

/// Static description of a badge type, served by `GET /badges` so clients keep
/// copy/icon/stacking policy server-authoritative.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, ToSchema)]
pub(crate) struct BadgeDefinition {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) icon: String,
    pub(crate) stacking: String,
    pub(crate) unique_cap: Option<i64>,
}

/// Result of looking up cached badges for an account.
pub(crate) enum BadgeLookup {
    /// No `accounts` row for this address.
    NoAccount,
    /// Account exists but has no current-version cached badges — recompute.
    Stale,
    /// Fresh cached badges of the current version.
    Cached(BadgesPayload),
}

/// Build the `block` badge type from a total count and the earliest heights.
/// The earliest `BLOCK_UNIQUE_CAP` heights become individual medals; the rest
/// stack into the bucket. Pure so it can be unit-tested without a database.
pub(crate) fn build_block_badge_type(total: i64, earliest: Vec<i32>) -> BadgeType {
    let unique: Vec<BadgeInstance> = earliest
        .into_iter()
        .take(BLOCK_UNIQUE_CAP.max(0) as usize)
        .map(|blockheight| BadgeInstance { blockheight })
        .collect();

    BadgeType {
        kind: KIND_UNIQUE_THEN_BUCKET.to_string(),
        unique,
        bucket: BadgeBucket {
            count: (total - BLOCK_UNIQUE_CAP).max(0),
        },
        total: total.max(0),
    }
}

/// Build a pure `unique` badge: every instance is shown individually, nothing
/// stacks. Used for rare achievements like winning a block.
pub(crate) fn build_unique_badge(blockheights: Vec<i32>) -> BadgeType {
    let total = blockheights.len() as i64;
    BadgeType {
        kind: KIND_UNIQUE.to_string(),
        unique: blockheights
            .into_iter()
            .map(|blockheight| BadgeInstance { blockheight })
            .collect(),
        bucket: BadgeBucket { count: 0 },
        total,
    }
}

/// Build a pure `bucket` (stacking) badge: a single medal with a count. Used for
/// loyalty, dispenser and refinery badges.
pub(crate) fn build_bucket_badge(count: i64) -> BadgeType {
    let count = count.max(0);
    BadgeType {
        kind: KIND_BUCKET.to_string(),
        unique: Vec::new(),
        bucket: BadgeBucket { count },
        total: count,
    }
}

/// The static badge catalog. Add new badge types here as they are implemented.
pub(crate) fn badge_catalog_definitions() -> Vec<BadgeDefinition> {
    vec![
        BadgeDefinition {
            id: BLOCK_BADGE_ID.to_string(),
            name: "Block Finder".to_string(),
            description:
                "Earned for submitting a share at the exact height where the pool found a block. \
                 The first three are awarded individually; every block after that stacks into one \
                 badge."
                    .to_string(),
            icon: "pickaxe".to_string(),
            stacking: KIND_UNIQUE_THEN_BUCKET.to_string(),
            unique_cap: Some(BLOCK_UNIQUE_CAP),
        },
        BadgeDefinition {
            id: BLOCK_WINNER_BADGE_ID.to_string(),
            name: "Block Winner".to_string(),
            description: "Awarded to the miner whose share actually solved a block. Each win is \
                          shown individually."
                .to_string(),
            icon: "trophy".to_string(),
            stacking: KIND_UNIQUE.to_string(),
            unique_cap: None,
        },
        BadgeDefinition {
            id: LOYALTY_BADGE_ID.to_string(),
            name: "Loyalty".to_string(),
            description: format!(
                "A stacking badge earned once per {LOYALTY_BLOCKS_PER_INSTANCE} blocks the miner \
                 has submitted a share in."
            ),
            icon: "medal".to_string(),
            stacking: KIND_BUCKET.to_string(),
            unique_cap: None,
        },
        BadgeDefinition {
            id: REFINERY_BADGE_ID.to_string(),
            name: "Refinery Operator".to_string(),
            description: "A stacking badge earned for each order fulfilled through the Refinery."
                .to_string(),
            icon: "refinery".to_string(),
            stacking: KIND_BUCKET.to_string(),
            unique_cap: None,
        },
        BadgeDefinition {
            id: DISPENSER_BADGE_ID.to_string(),
            name: "Dispenser".to_string(),
            description: "A stacking badge earned for each distinct asset type collected from the \
                          dispenser."
                .to_string(),
            icon: "dispenser".to_string(),
            stacking: KIND_BUCKET.to_string(),
            unique_cap: None,
        },
    ]
}

pub(crate) fn badges_router(database: Database) -> axum::Router {
    axum::Router::new()
        .route("/badges", get(badge_catalog))
        .route("/badges/{address}", get(badges_for_account))
        .layer(from_extractor::<ApiAuth>())
        .layer(Extension(database))
}

/// Static catalog of badge definitions.
#[utoipa::path(
    get,
    path = "/badges",
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Badge catalog", body = Vec<BadgeDefinition>),
    ),
    tag = "badges"
)]
pub(crate) async fn badge_catalog() -> ServerResult<Response> {
    Ok(Json(badge_catalog_definitions()).into_response())
}

/// Canonical badge state for an account.
#[utoipa::path(
    get,
    path = "/badges/{address}",
    security(("api_token" = [])),
    params(
        ("address" = String, Path, description = "BTC address")
    ),
    responses(
        (status = 200, description = "Badge state for the account", body = BadgesPayload),
        (status = 404, description = "Account not found"),
    ),
    tag = "badges"
)]
pub(crate) async fn badges_for_account(
    Extension(database): Extension<Database>,
    Path(address): Path<String>,
) -> ServerResult<Response> {
    match database.get_account_badges(&address).await? {
        BadgeLookup::Cached(payload) => Ok(Json(payload).into_response()),
        BadgeLookup::Stale => {
            let payload = database
                .compute_and_store_badges(&address)
                .await?
                .ok_or_not_found(|| "Account")?;
            Ok(Json(payload).into_response())
        }
        BadgeLookup::NoAccount => Err(ServerError::NotFound("Account not found".into())),
    }
}

/// A configured external HTTP badge source (the Router service for refinery
/// badges, or the guac dispenser service).
#[derive(Clone, Debug)]
pub(crate) struct ServiceEndpoint {
    base_url: String,
    token: Option<String>,
}

impl ServiceEndpoint {
    fn base(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }
}

/// Consolidated fetch layer for badges whose truth lives in other services
/// (Refinery orders on the Router, dispensed assets in guac). Both are optional;
/// when unconfigured the corresponding badge is simply omitted.
#[derive(Clone, Debug, Default)]
pub(crate) struct ExternalBadgeSources {
    client: Client,
    router: Option<ServiceEndpoint>,
    guac: Option<ServiceEndpoint>,
}

impl ExternalBadgeSources {
    pub(crate) fn new(
        router_url: Option<String>,
        router_token: Option<String>,
        guac_url: Option<String>,
        guac_token: Option<String>,
    ) -> Self {
        let client = ClientBuilder::new()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();

        Self {
            client,
            router: router_url.map(|base_url| ServiceEndpoint {
                base_url,
                token: router_token,
            }),
            guac: guac_url.map(|base_url| ServiceEndpoint {
                base_url,
                token: guac_token,
            }),
        }
    }

    /// Whether any external badge source is configured (so a read-time TTL
    /// re-poll is warranted). When false, badges only change on block finds.
    pub(crate) fn is_configured(&self) -> bool {
        self.router.is_some() || self.guac.is_some()
    }

    async fn get_json(&self, url: String, endpoint: &ServiceEndpoint) -> Result<serde_json::Value> {
        let mut request = self.client.get(&url);
        if let Some(token) = &endpoint.token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("request to {url} failed"))?
            .error_for_status()
            .with_context(|| format!("{url} returned an error status"))?;
        response
            .json()
            .await
            .with_context(|| format!("failed to decode JSON from {url}"))
    }

    /// Number of Refinery orders this address has fulfilled. `Ok(None)` when the
    /// router source is not configured.
    pub(crate) async fn refinery_count(&self, address: &str) -> Result<Option<i64>> {
        let Some(router) = &self.router else {
            return Ok(None);
        };

        // Percent-encode: `address` is caller-supplied and must not be able to
        // inject extra query parameters into the outbound request.
        let url = format!(
            "{}/api/router/orders?address={}",
            router.base(),
            urlencoding::encode(address)
        );
        let orders = self.get_json(url, router).await?;

        let count = orders
            .as_array()
            .map(|orders| {
                orders
                    .iter()
                    .filter(|order| {
                        order.get("status").and_then(|s| s.as_str()) == Some("fulfilled")
                    })
                    .count() as i64
            })
            .unwrap_or(0);

        Ok(Some(count))
    }

    /// Number of DISTINCT dispenser asset types this address has been assigned.
    /// `Ok(None)` when the guac source is not configured.
    pub(crate) async fn dispenser_count(&self, address: &str) -> Result<Option<i64>> {
        let Some(guac) = &self.guac else {
            return Ok(None);
        };

        // Percent-encode: `address` is caller-supplied and must not be able to
        // escape this path segment (e.g. `..%2F`) into other guac endpoints.
        let eligibility = self
            .get_json(
                format!(
                    "{}/eligibility/{}",
                    guac.base(),
                    urlencoding::encode(address)
                ),
                guac,
            )
            .await?;
        let tiers = self
            .get_json(format!("{}/tiers", guac.base()), guac)
            .await?;

        // Map tier name -> asset name so multiple tiers drawing from the same
        // asset only count once.
        let mut tier_to_asset: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        if let Some(tiers) = tiers.as_array() {
            for tier in tiers {
                if let (Some(name), Some(asset)) = (
                    tier.get("name").and_then(|v| v.as_str()),
                    tier.get("asset").and_then(|v| v.as_str()),
                ) {
                    tier_to_asset.insert(name.to_string(), asset.to_string());
                }
            }
        }

        let mut assets: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(assigned) = eligibility
            .get("assigned_utxos")
            .and_then(|v| v.as_object())
        {
            for (tier, outpoints) in assigned {
                let has_any = outpoints.as_array().is_some_and(|list| !list.is_empty());
                if !has_any {
                    continue;
                }
                // Overrides and any tier without an asset mapping count under
                // their own key.
                let asset = tier_to_asset
                    .get(tier)
                    .cloned()
                    .unwrap_or_else(|| tier.clone());
                assets.insert(asset);
            }
        }

        Ok(Some(assets.len() as i64))
    }
}

/// Age in seconds of a cached payload, or `None` if the timestamp is unparseable.
fn payload_age_secs(computed_at: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(computed_at)
        .ok()
        .map(|timestamp| (chrono::Utc::now() - timestamp.with_timezone(&chrono::Utc)).num_seconds())
}

/// True when a cached payload is within the external re-poll TTL.
pub(crate) fn payload_is_fresh(computed_at: &str) -> bool {
    payload_age_secs(computed_at).is_some_and(|age| age < BADGE_CACHE_TTL_SECS)
}

/// True when a cached payload is within the backstop max age.
pub(crate) fn payload_within_max_age(computed_at: &str) -> bool {
    payload_age_secs(computed_at).is_some_and(|age| age < BADGE_MAX_AGE_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_blocks_is_empty() {
        let badge = build_block_badge_type(0, vec![]);
        assert_eq!(badge.total, 0);
        assert!(badge.unique.is_empty());
        assert_eq!(badge.bucket.count, 0);
    }

    #[test]
    fn one_block_is_a_single_unique() {
        let badge = build_block_badge_type(1, vec![811001]);
        assert_eq!(badge.total, 1);
        assert_eq!(
            badge.unique,
            vec![BadgeInstance {
                blockheight: 811001
            }]
        );
        assert_eq!(badge.bucket.count, 0);
    }

    #[test]
    fn three_blocks_are_all_unique_no_bucket() {
        let badge = build_block_badge_type(3, vec![10, 20, 30]);
        assert_eq!(badge.total, 3);
        assert_eq!(badge.unique.len(), 3);
        assert_eq!(badge.bucket.count, 0);
    }

    #[test]
    fn four_blocks_are_three_unique_plus_one_stacked() {
        let badge = build_block_badge_type(4, vec![10, 20, 30]);
        assert_eq!(badge.total, 4);
        assert_eq!(badge.unique.len(), 3);
        assert_eq!(badge.bucket.count, 1);
    }

    #[test]
    fn thirteen_blocks_stack_the_overflow() {
        let badge = build_block_badge_type(13, vec![10, 20, 30]);
        assert_eq!(badge.total, 13);
        assert_eq!(badge.unique.len(), 3);
        // 3 unique medals + a "+10" bucket.
        assert_eq!(badge.bucket.count, 10);
    }
}
