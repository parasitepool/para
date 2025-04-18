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
                eprintln!("error serving request: {error}");
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
