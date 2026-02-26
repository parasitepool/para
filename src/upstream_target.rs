use super::*;

#[derive(Clone, Debug)]
pub(crate) struct UpstreamTarget {
    endpoint: String,
    username: Username,
    password: Option<String>,
}

impl UpstreamTarget {
    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) fn username(&self) -> &Username {
        &self.username
    }

    pub(crate) fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }
}

impl FromStr for UpstreamTarget {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let (creds, endpoint) = s
            .rsplit_once('@')
            .ok_or_else(|| anyhow!("expected `USER[:PASS]@HOST:PORT`, missing `@` in `{s}`"))?;

        ensure!(!endpoint.is_empty(), "empty endpoint in `{s}`");

        let (username, password) = if let Some((user, pass)) = creds.split_once(':') {
            (user, Some(pass.to_string()))
        } else {
            (creds, None)
        };

        ensure!(!username.is_empty(), "empty username in `{s}`");

        Ok(Self {
            endpoint: endpoint.to_string(),
            username: Username::new(username),
            password,
        })
    }
}

impl Display for UpstreamTarget {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        if let Some(password) = &self.password {
            write!(f, "{}:{}@{}", self.username, password, self.endpoint)
        } else {
            write!(f, "{}@{}", self.username, self.endpoint)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        #[track_caller]
        fn case(s: &str, username: &str, password: Option<&str>, endpoint: &str) {
            let target: UpstreamTarget = s.parse().unwrap();
            assert_eq!(target.username.as_str(), username);
            assert_eq!(target.password.as_deref(), password);
            assert_eq!(target.endpoint, endpoint);
        }

        case("foo@bar:3333", "foo", None, "bar:3333");
        case("foo:baz@bar:3333", "foo", Some("baz"), "bar:3333");
        case(
            "foo.worker:x@bar.com:3333",
            "foo.worker",
            Some("x"),
            "bar.com:3333",
        );
    }

    #[test]
    fn missing_at() {
        let err = "foo:bar:3333".parse::<UpstreamTarget>().unwrap_err();
        assert!(err.to_string().contains("missing `@`"));
    }

    #[test]
    fn empty_endpoint() {
        let err = "foo@".parse::<UpstreamTarget>().unwrap_err();
        assert!(err.to_string().contains("empty endpoint"));
    }

    #[test]
    fn empty_username() {
        let err = "@bar:3333".parse::<UpstreamTarget>().unwrap_err();
        assert!(err.to_string().contains("empty username"));
    }

    #[test]
    fn password_with_colons() {
        let target: UpstreamTarget = "foo:pass:word@bar:3333".parse().unwrap();
        assert_eq!(target.username.as_str(), "foo");
        assert_eq!(target.password.as_deref(), Some("pass:word"));
        assert_eq!(target.endpoint, "bar:3333");
    }

    #[test]
    fn display() {
        let target: UpstreamTarget = "foo:x@bar:3333".parse().unwrap();
        assert_eq!(target.to_string(), "foo:x@bar:3333");

        let target: UpstreamTarget = "foo@bar:3333".parse().unwrap();
        assert_eq!(target.to_string(), "foo@bar:3333");
    }
}
