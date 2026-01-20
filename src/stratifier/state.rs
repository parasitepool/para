use super::*;

#[derive(Debug, Clone)]
pub(crate) struct Session {
    pub(crate) enonce1: Extranonce,
    #[allow(dead_code)]
    pub(crate) user_agent: String,
    pub(crate) version_mask: Option<Version>,
    pub(crate) username: Username,
    pub(crate) address: Address,
    pub(crate) workername: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Subscription {
    pub(crate) enonce1: Extranonce,
    pub(crate) user_agent: String,
    pub(crate) version_mask: Option<Version>,
}

#[derive(Debug, Clone)]
pub(crate) enum State {
    Init,
    Configured { version_mask: Version },
    Subscribed(Subscription),
    Working(Arc<Session>),
    Dropped,
}

impl State {
    pub(crate) fn new() -> Self {
        State::Init
    }

    pub(crate) fn drop_connection(&mut self) {
        *self = State::Dropped;
    }

    pub(crate) fn configure(&mut self, version_mask: Version) -> bool {
        match self {
            State::Init | State::Configured { .. } => {
                *self = State::Configured { version_mask };
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

    pub(crate) fn authorize(
        &mut self,
        address: Address,
        workername: String,
        username: Username,
    ) -> bool {
        match self {
            State::Subscribed(subscription) => {
                *self = State::Working(Arc::new(Session {
                    enonce1: subscription.enonce1.clone(),
                    user_agent: subscription.user_agent.clone(),
                    version_mask: subscription.version_mask,
                    username,
                    address,
                    workername,
                }));
                true
            }
            _ => false,
        }
    }

    pub(crate) fn version_mask(&self) -> Option<Version> {
        match self {
            State::Init | State::Dropped => None,
            State::Configured { version_mask } => Some(*version_mask),
            State::Subscribed(subscription) => subscription.version_mask,
            State::Working(session) => session.version_mask,
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

    fn test_username() -> Username {
        Username::new("tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker1")
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
    fn configure_fails_in_subscribed() {
        let mut state = State::new();

        assert!(state.subscribe(test_enonce1(), "test/1.0".into()));
        assert!(!state.configure(Version::from(0x1fffe000)));

        assert!(state.subscribed().is_some());
    }

    #[test]
    fn configure_fails_in_working() {
        let mut state = State::new();

        assert!(state.subscribe(test_enonce1(), "test/1.0".into()));
        assert!(state.authorize(test_address(), "worker1".into(), test_username()));

        assert!(state.subscribed().is_none());

        let session = state.working().unwrap();
        assert_eq!(session.address, test_address());
        assert_eq!(session.workername, "worker1");
    }

    #[test]
    fn authorize_in_init_fails() {
        let mut state = State::new();

        assert!(!state.authorize(test_address(), "worker1".into(), test_username()));
        assert!(state.can_subscribe());
    }

    #[test]
    fn authorize_in_working_fails() {
        let mut state = State::new();

        assert!(state.subscribe(test_enonce1(), "test/1.0".into()));

        assert!(state.authorize(test_address(), "worker1".into(), test_username()));
        assert!(!state.authorize(test_address(), "worker2".into(), test_username()));
        assert!(state.working().is_some());
    }

    #[test]
    fn resubscribe_from_working_is_rejected() {
        let mut state = State::new();

        assert!(state.subscribe(test_enonce1(), "test/1.0".into()));
        assert!(state.authorize(test_address(), "worker1".into(), test_username()));

        assert!(state.working().is_some());

        let new_enonce1: Extranonce = "cafebabe".parse().unwrap();
        assert!(!state.subscribe(new_enonce1.clone(), "test/2.0".into()));

        assert!(state.working().is_some());
    }

    #[test]
    fn configure_only_works_in_init_and_configured() {
        let mut state = State::new();
        let mask1 = Version::from(0x1fffe000);
        let mask2 = Version::from(0x0ffff000);

        assert!(state.configure(mask1));
        assert_eq!(state.version_mask(), Some(mask1));

        assert!(state.configure(mask2));
        assert_eq!(state.version_mask(), Some(mask2));

        assert!(state.subscribe(test_enonce1(), "test/1.0".into()));
        assert!(!state.configure(mask1));

        assert!(state.authorize(test_address(), "worker1".into(), test_username()));
        assert!(!state.configure(mask1));
    }

    #[test]
    fn display_shows_correct_state_names() {
        let mut state = State::new();
        assert_eq!(state.to_string(), "Init");

        assert!(state.configure(Version::from(0x1fffe000)));
        assert_eq!(state.to_string(), "Configured");

        assert!(state.subscribe(test_enonce1(), "test/1.0".into()));
        assert_eq!(state.to_string(), "Subscribed");

        assert!(state.authorize(test_address(), "worker1".into(), test_username()));
        assert_eq!(state.to_string(), "Working");
    }
}
