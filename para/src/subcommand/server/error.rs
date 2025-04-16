use super::*;

pub(super) enum ServerError {
    Internal(Error),
    NotFound(String),
}

pub(super) type ServerResult<T> = Result<T, ServerError>;

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
