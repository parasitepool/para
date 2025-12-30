use axum::{
    extract::FromRequestParts,
    http::{self, StatusCode},
};

pub(crate) struct AcceptJson(pub(crate) bool);

impl<S> FromRequestParts<S> for AcceptJson
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let json_header = parts
            .headers
            .get("accept")
            .map(|value| value == "application/json")
            .unwrap_or_default();

        if json_header {
            Ok(Self(true))
        } else {
            Ok(Self(false))
        }
    }
}
