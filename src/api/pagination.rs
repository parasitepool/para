use super::*;

pub(crate) const DEFAULT_PAGE_LIMIT: usize = 50;
pub(crate) const MAX_PAGE_LIMIT: usize = 500;

pub(crate) fn effective_limit(limit: Option<usize>) -> ServerResult<usize> {
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).min(MAX_PAGE_LIMIT);

    if limit == 0 {
        return Err(ServerError::BadRequest(
            "limit must be greater than zero".into(),
        ));
    }

    Ok(limit)
}

pub(crate) fn page_response<T: Serialize>(
    items: Vec<T>,
    limit: usize,
    next_cursor: Option<String>,
) -> Response {
    let mut response = Json(items).into_response();
    let headers = response.headers_mut();

    headers.insert(
        "x-limit",
        HeaderValue::from_str(&limit.to_string()).expect("page limit is a valid header value"),
    );

    if let Some(cursor) = next_cursor {
        headers.insert(
            "x-next-cursor",
            HeaderValue::from_str(&cursor).expect("page cursor is a valid header value"),
        );
    }

    response
}

#[derive(Serialize)]
pub(crate) struct CountResponse {
    pub(crate) count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_is_clamped_and_validated() {
        assert_eq!(effective_limit(None).ok().unwrap(), DEFAULT_PAGE_LIMIT);
        assert!(effective_limit(Some(0)).is_err());
        assert_eq!(effective_limit(Some(999)).ok().unwrap(), MAX_PAGE_LIMIT);
    }
}
