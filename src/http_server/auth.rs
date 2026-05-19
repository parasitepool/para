use {
    axum::{
        body::Body,
        extract::FromRequestParts,
        http::{
            Response, StatusCode,
            header::{AUTHORIZATION, COOKIE, WWW_AUTHENTICATE},
            request::Parts,
        },
    },
    base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD},
    bitcoin::hashes::{Hash, sha256},
};

pub(crate) const AUTH_COOKIE: &str = "para_auth";

#[derive(Clone, Debug)]
pub(crate) struct BearerAuth {
    api_hash: Option<sha256::Hash>,
    admin_hash: Option<sha256::Hash>,
}

pub(crate) struct ApiAuth;

pub(crate) struct AdminAuth;

impl BearerAuth {
    pub(crate) fn new(api_token: Option<&str>, admin_token: Option<&str>) -> Self {
        Self {
            api_hash: api_token.map(Self::hash),
            admin_hash: admin_token.map(Self::hash),
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.api_hash.is_some() || self.admin_hash.is_some()
    }

    pub(crate) fn role(&self, token: &str) -> Option<&'static str> {
        let hash = Self::hash(token);

        if self
            .admin_hash
            .as_ref()
            .is_some_and(|admin| Self::hashes_equal(admin, &hash))
        {
            Some("admin")
        } else if self
            .api_hash
            .as_ref()
            .is_some_and(|api| Self::hashes_equal(api, &hash))
        {
            Some("api")
        } else {
            None
        }
    }

    pub(crate) fn session_cookie(token: &str, secure: bool) -> String {
        let value = URL_SAFE_NO_PAD.encode(token);
        format!(
            "{AUTH_COOKIE}={value}; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000{}",
            if secure { "; Secure" } else { "" },
        )
    }

    pub(crate) fn clear_session_cookie(secure: bool) -> String {
        format!(
            "{AUTH_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{}",
            if secure { "; Secure" } else { "" },
        )
    }

    fn hash(token: &str) -> sha256::Hash {
        sha256::Hash::hash(token.as_bytes())
    }

    fn hashes_equal(left: &sha256::Hash, right: &sha256::Hash) -> bool {
        left.as_byte_array()
            .iter()
            .zip(right.as_byte_array())
            .fold(0, |diff, (left, right)| diff | (left ^ right))
            == 0
    }

    fn accepts_api(&self, token: &str) -> bool {
        let hash = Self::hash(token);
        let mut accepted = false;

        if let Some(h) = &self.api_hash {
            accepted |= Self::hashes_equal(h, &hash);
        }

        if let Some(h) = &self.admin_hash {
            accepted |= Self::hashes_equal(h, &hash);
        }

        accepted
    }

    fn accepts_admin(&self, token: &str) -> bool {
        self.admin_hash
            .as_ref()
            .is_some_and(|h| Self::hashes_equal(h, &Self::hash(token)))
    }

    fn token(parts: &Parts) -> Option<String> {
        if let Some(token) = Self::bearer_token(parts) {
            return Some(token.to_string());
        }

        Self::cookie_token(parts)
    }

    fn bearer_token(parts: &Parts) -> Option<&str> {
        let header = parts.headers.get(AUTHORIZATION)?.to_str().ok()?;
        let (scheme, token) = header.split_once(' ')?;

        if scheme.eq_ignore_ascii_case("Bearer") && !token.is_empty() {
            Some(token)
        } else {
            None
        }
    }

    fn cookie_token(parts: &Parts) -> Option<String> {
        let header = parts.headers.get(COOKIE)?.to_str().ok()?;

        header.split(';').find_map(|cookie| {
            let (name, value) = cookie.trim().split_once('=')?;
            if name != AUTH_COOKIE {
                return None;
            }
            let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
            String::from_utf8(bytes).ok()
        })
    }
}

#[allow(clippy::result_large_err)]
fn check_auth(
    parts: &mut Parts,
    accept: fn(&BearerAuth, &str) -> bool,
) -> Result<(), Response<Body>> {
    let Some(auth) = parts.extensions.get::<BearerAuth>() else {
        return Ok(());
    };

    if !auth.enabled() {
        return Ok(());
    }

    if BearerAuth::token(parts).is_some_and(|token| accept(auth, &token)) {
        Ok(())
    } else {
        Err(unauthorized())
    }
}

impl<S: Send + Sync> FromRequestParts<S> for ApiAuth {
    type Rejection = Response<Body>;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        check_auth(parts, BearerAuth::accepts_api)?;
        Ok(Self)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AdminAuth {
    type Rejection = Response<Body>;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        check_auth(parts, BearerAuth::accepts_admin)?;
        Ok(Self)
    }
}

fn unauthorized() -> Response<Body> {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(WWW_AUTHENTICATE, "Bearer")
        .body(Body::empty())
        .unwrap()
}

#[cfg(test)]
mod tests {
    use {super::*, axum::http::Request};

    fn request(auth: Option<&str>) -> Request<()> {
        let mut builder = Request::builder();
        if let Some(auth) = auth {
            builder = builder.header(AUTHORIZATION, auth);
        }
        builder.body(()).unwrap()
    }

    fn cookie_request(token: &str) -> Request<()> {
        let value = URL_SAFE_NO_PAD.encode(token);
        Request::builder()
            .header(COOKIE, format!("{AUTH_COOKIE}={value}"))
            .body(())
            .unwrap()
    }

    async fn check<E>(auth: Option<BearerAuth>, request: Request<()>) -> Result<(), StatusCode>
    where
        E: FromRequestParts<(), Rejection = Response<Body>>,
    {
        let (mut parts, _) = request.into_parts();
        if let Some(auth) = auth {
            parts.extensions.insert(auth);
        }
        E::from_request_parts(&mut parts, &())
            .await
            .map(|_| ())
            .map_err(|response| response.status())
    }

    #[tokio::test]
    async fn bearer() {
        async fn case(
            api_token: Option<&str>,
            admin_token: Option<&str>,
            header: Option<&str>,
            api_ok: bool,
            admin_ok: bool,
        ) {
            let auth = BearerAuth::new(api_token, admin_token);
            assert_eq!(
                check::<ApiAuth>(Some(auth.clone()), request(header))
                    .await
                    .is_ok(),
                api_ok,
            );
            assert_eq!(
                check::<AdminAuth>(Some(auth), request(header))
                    .await
                    .is_ok(),
                admin_ok,
            );
        }

        case(Some("foo"), None, Some("Bearer foo"), true, false).await;
        case(None, Some("foo"), Some("Bearer foo"), true, true).await;
        case(Some("foo"), Some("bar"), Some("Bearer foo"), true, false).await;
        case(Some("foo"), Some("bar"), Some("Bearer bar"), true, true).await;
        case(Some("foo"), Some("bar"), Some("Bearer baz"), false, false).await;
        case(Some("foo"), None, Some("Bearer bar"), false, false).await;
        case(Some("foo"), None, None, false, false).await;
        case(Some("foo"), None, Some("Basic foo"), false, false).await;
        case(Some("foo"), None, Some("Bearer "), false, false).await;
        case(Some("foo"), None, Some("Bearer"), false, false).await;
        case(None, Some("foo"), None, false, false).await;
        case(None, None, None, true, true).await;
    }

    #[tokio::test]
    async fn cookie() {
        let auth = BearerAuth::new(Some("foo"), Some("bar"));

        assert!(
            check::<ApiAuth>(Some(auth.clone()), cookie_request("foo"))
                .await
                .is_ok()
        );
        assert!(
            check::<ApiAuth>(Some(auth.clone()), cookie_request("bar"))
                .await
                .is_ok()
        );
        assert!(
            check::<AdminAuth>(Some(auth.clone()), cookie_request("bar"))
                .await
                .is_ok()
        );
        assert!(
            check::<AdminAuth>(Some(auth), cookie_request("foo"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn passthrough_when_absent() {
        assert!(check::<ApiAuth>(None, request(None)).await.is_ok());
        assert!(check::<AdminAuth>(None, request(None)).await.is_ok());
    }

    #[test]
    fn role() {
        let auth = BearerAuth::new(Some("api"), Some("admin"));

        assert_eq!(auth.role("api"), Some("api"));
        assert_eq!(auth.role("admin"), Some("admin"));
        assert_eq!(auth.role("wrong"), None);
    }
}
