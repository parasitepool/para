use super::*;

#[derive(Debug, Clone)]
pub(crate) struct Subscription {
    pub(crate) enonce1: Extranonce,
    pub(crate) user_agent: String,
    pub(crate) version_mask: Option<Version>,
}

pub(crate) struct Authorization {
    pub(crate) enonce1: Extranonce,
    pub(crate) address: Address,
    pub(crate) workername: String,
    pub(crate) username: Username,
    pub(crate) user_agent: String,
    pub(crate) version_mask: Option<Version>,
}

#[derive(Clone)]
pub(crate) enum State {
    Init,
    Configured { version_mask: Version },
    Subscribed(Subscription),
    Authorized(Arc<Authorization>),
    Working(Arc<Session>),
    Dropped,
}

impl State {
    pub(crate) fn new() -> Self {
        State::Init
    }

    pub(crate) fn drop(&mut self) {
        *self = State::Dropped;
    }

    pub(crate) fn configure(&mut self, version_mask: Version) -> bool {
        match self {
            State::Init | State::Configured { .. } => {
                *self = State::Configured { version_mask };
                true
            }
            State::Subscribed(subscription) => {
                *self = State::Subscribed(Subscription {
                    version_mask: Some(version_mask),
                    enonce1: subscription.enonce1.clone(),
                    user_agent: subscription.user_agent.clone(),
                });
                true
            }
            _ => false,
        }
    }

    pub(crate) fn can_subscribe(&self) -> bool {
        matches!(self, State::Init | State::Configured { .. })
    }

    pub(crate) fn subscribe(&mut self, enonce1: Extranonce, user_agent: String) -> bool {
        if self.can_subscribe() {
            let version_mask = self.version_mask();
            *self = State::Subscribed(Subscription {
                enonce1,
                user_agent,
                version_mask,
            });
            true
        } else {
            false
        }
    }

    pub(crate) fn authorize(&mut self, auth: Arc<Authorization>) -> bool {
        match self {
            State::Subscribed(_) => {
                *self = State::Authorized(auth);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn authorized(&self) -> Option<Arc<Authorization>> {
        match self {
            State::Authorized(auth) => Some(auth.clone()),
            _ => None,
        }
    }

    pub(crate) fn promote(&mut self, session: Arc<Session>) -> bool {
        match self {
            State::Authorized(_) => {
                *self = State::Working(session);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn identity(&self) -> Option<(Extranonce, Address)> {
        match self {
            State::Authorized(auth) => Some((auth.enonce1.clone(), auth.address.clone())),
            State::Working(session) => Some((session.enonce1().clone(), session.address().clone())),
            _ => None,
        }
    }

    pub(crate) fn version_mask(&self) -> Option<Version> {
        match self {
            State::Init | State::Dropped => None,
            State::Configured { version_mask } => Some(*version_mask),
            State::Subscribed(subscription) => subscription.version_mask,
            State::Authorized(auth) => auth.version_mask,
            State::Working(session) => session.version_mask(),
        }
    }

    pub(crate) fn subscribed(&self) -> Option<Subscription> {
        match self {
            State::Subscribed(subscription) => Some(subscription.clone()),
            _ => None,
        }
    }

    pub(crate) fn working(&self) -> Option<Arc<Session>> {
        match self {
            State::Working(session) => Some(session.clone()),
            _ => None,
        }
    }
}

impl Display for State {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            State::Init => write!(f, "Init"),
            State::Configured { .. } => write!(f, "Configured"),
            State::Subscribed { .. } => write!(f, "Subscribed"),
            State::Authorized { .. } => write!(f, "Authorized"),
            State::Working { .. } => write!(f, "Working"),
            State::Dropped => write!(f, "Dropped"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_enonce1() -> Extranonce {
        "deadbeef".parse().unwrap()
    }

    fn test_address() -> Address {
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
            .parse::<Address<NetworkUnchecked>>()
            .unwrap()
            .assume_checked()
    }

    fn test_authorization() -> Arc<Authorization> {
        Arc::new(Authorization {
            enonce1: test_enonce1(),
            address: test_address(),
            workername: "bar".into(),
            username: Username::new("tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.bar"),
            user_agent: "foo".into(),
            version_mask: None,
        })
    }

    fn test_session() -> Arc<Session> {
        Arc::new(Session::new(
            test_enonce1(),
            "127.0.0.1:1234".parse().unwrap(),
            test_address(),
            "bar".into(),
            Username::new("tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.bar"),
            "foo".into(),
            None,
        ))
    }

    #[test]
    fn new_state_is_init() {
        let state = State::new();

        assert!(matches!(state, State::Init));
        assert!(state.can_subscribe());
        assert!(state.subscribed().is_none());
        assert!(state.working().is_none());
        assert!(state.version_mask().is_none());
    }

    #[test]
    fn configure_transitions_init_to_configured() {
        let mut state = State::new();
        let mask = Version::from(0x1fffe000);

        assert!(state.configure(mask));

        assert!(matches!(state, State::Configured { .. }));
        assert!(state.can_subscribe());
        assert_eq!(state.version_mask(), Some(mask));
    }

    #[test]
    fn configure_is_idempotent_in_configured() {
        let mut state = State::new();
        let mask1 = Version::from(0x1fffe000);
        let mask2 = Version::from(0x0ffff000);

        assert!(state.configure(mask1));
        assert!(state.configure(mask2));

        assert_eq!(state.version_mask(), Some(mask2));
    }

    #[test]
    fn configure_works_in_subscribed() {
        let mut state = State::new();
        let mask = Version::from(0x1fffe000);

        assert!(state.subscribe(test_enonce1(), "foo".into()));
        assert!(state.configure(mask));

        assert!(state.subscribed().is_some());
        assert_eq!(state.version_mask(), Some(mask));
    }

    #[test]
    fn authorize_transitions_to_authorized() {
        let mut state = State::new();

        assert!(state.subscribe(test_enonce1(), "foo".into()));
        assert!(state.authorize(test_authorization()));

        assert!(state.subscribed().is_none());
        assert!(state.working().is_none());

        let auth = state.authorized().unwrap();
        assert_eq!(auth.address, test_address());
        assert_eq!(auth.workername, "bar");
    }

    #[test]
    fn authorize_in_init_fails() {
        let mut state = State::new();

        assert!(!state.authorize(test_authorization()));
        assert!(state.can_subscribe());
    }

    #[test]
    fn authorize_in_authorized_fails() {
        let mut state = State::new();

        assert!(state.subscribe(test_enonce1(), "test/1.0".into()));

        assert!(state.authorize(test_authorization()));
        assert!(!state.authorize(test_authorization()));
        assert!(state.authorized().is_some());
    }

    #[test]
    fn promote_transitions_to_working() {
        let mut state = State::new();

        assert!(state.subscribe(test_enonce1(), "foo".into()));
        assert!(state.authorize(test_authorization()));
        assert!(state.promote(test_session()));

        assert!(state.authorized().is_none());
        assert!(state.working().is_some());
    }

    #[test]
    fn resubscribe_from_authorized_is_rejected() {
        let mut state = State::new();

        assert!(state.subscribe(test_enonce1(), "test/1.0".into()));
        assert!(state.authorize(test_authorization()));

        let new_enonce1: Extranonce = "cafebabe".parse().unwrap();
        assert!(!state.subscribe(new_enonce1.clone(), "test/2.0".into()));

        assert!(state.authorized().is_some());
    }

    #[test]
    fn configure_fails_in_authorized() {
        let mut state = State::new();
        let mask = Version::from(0x1fffe000);

        assert!(state.subscribe(test_enonce1(), "foo".into()));
        assert!(state.authorize(test_authorization()));
        assert!(!state.configure(mask));
    }

    #[test]
    fn identity_returns_none_before_authorize() {
        let state = State::new();
        assert!(state.identity().is_none());
    }

    #[test]
    fn identity_works_in_authorized_and_working() {
        let mut state = State::new();

        assert!(state.subscribe(test_enonce1(), "foo".into()));
        assert!(state.authorize(test_authorization()));

        let (enonce1, address) = state.identity().unwrap();
        assert_eq!(enonce1, test_enonce1());
        assert_eq!(address, test_address());

        assert!(state.promote(test_session()));

        let (enonce1, address) = state.identity().unwrap();
        assert_eq!(enonce1, test_enonce1());
        assert_eq!(address, test_address());
    }

    #[test]
    fn display_shows_correct_state_names() {
        let mut state = State::new();
        assert_eq!(state.to_string(), "Init");

        assert!(state.configure(Version::from(0x1fffe000)));
        assert_eq!(state.to_string(), "Configured");

        assert!(state.subscribe(test_enonce1(), "test/1.0".into()));
        assert_eq!(state.to_string(), "Subscribed");

        assert!(state.authorize(test_authorization()));
        assert_eq!(state.to_string(), "Authorized");

        assert!(state.promote(test_session()));
        assert_eq!(state.to_string(), "Working");
    }
}
