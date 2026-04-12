use {super::*, crate::router::error::RouterError};

pub(crate) enum ServerError {
    Internal(Error),
    NotFound(String),
    BadRequest(String),
    UnprocessableEntity(String),
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
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message).into_response(),
            Self::UnprocessableEntity(message) => {
                (StatusCode::UNPROCESSABLE_ENTITY, message).into_response()
            }
        }
    }
}

impl From<Error> for ServerError {
    fn from(error: Error) -> Self {
        Self::Internal(error)
    }
}

impl From<RouterError> for ServerError {
    fn from(error: RouterError) -> Self {
        match &error {
            RouterError::InvalidHashdays | RouterError::HashPriceOverflow => {
                Self::BadRequest(error.to_string())
            }
            RouterError::HashPriceBelowMinimum { .. } => {
                Self::UnprocessableEntity(error.to_string())
            }
        }
    }
}

pub(crate) trait OptionExt<T> {
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
