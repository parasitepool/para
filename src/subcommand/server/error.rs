use super::*;

pub(crate) enum ServerError {
    Internal(Error),
    NotFound(String),
}

pub(crate) type ServerResult<T> = Result<T, ServerError>;

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        match self {
            Self::Internal(error) => {
                error!("error serving request: {error}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    StatusCode::INTERNAL_SERVER_ERROR
                        .canonical_reason()
                        .unwrap_or_default(),
                )
                    .into_response()
            }
            Self::NotFound(message) => (StatusCode::NOT_FOUND, message).into_response(),
        }
    }
}

impl From<Error> for ServerError {
    fn from(error: Error) -> Self {
        Self::Internal(error)
    }
}

pub(super) trait OptionExt<T> {
    fn ok_or_not_found<F: FnOnce() -> S, S: Into<String>>(self, f: F) -> ServerResult<T>;
}

impl<T> OptionExt<T> for Option<T> {
    fn ok_or_not_found<F: FnOnce() -> S, S: Into<String>>(self, f: F) -> ServerResult<T> {
        match self {
            Some(value) => Ok(value),
            None => Err(ServerError::NotFound(f().into() + " not found")),
        }
    }
}
