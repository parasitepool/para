use super::*;

#[derive(Debug, Clone)]
pub(crate) enum State {
    Fresh {
        version_mask: Option<Version>,
    },

    Subscribed {
        enonce1: Extranonce,
        user_agent: String,
        version_mask: Option<Version>,
    },

    Working {
        enonce1: Extranonce,
        user_agent: String,
        version_mask: Option<Version>,
        address: Address,
        workername: String,
        authorized_username: Username,
    },
}

impl State {
    pub(crate) fn new() -> Self {
        State::Fresh { version_mask: None }
    }

    pub(crate) fn configure(&mut self, version_mask: Version) {
        match self {
            State::Fresh {
                version_mask: vm, ..
            } => *vm = Some(version_mask),
            State::Subscribed {
                version_mask: vm, ..
            } => *vm = Some(version_mask),
            State::Working {
                version_mask: vm, ..
            } => *vm = Some(version_mask),
        }
    }

    pub(crate) fn subscribe(&mut self, enonce1: Extranonce, user_agent: String) {
        let version_mask = self.version_mask();
        *self = State::Subscribed {
            enonce1,
            user_agent,
            version_mask,
        };
    }

    pub(crate) fn authorize(
        &mut self,
        address: Address,
        workername: String,
        authorized_username: Username,
    ) -> Result<(), StratumError> {
        match self {
            State::Subscribed {
                enonce1,
                user_agent,
                version_mask,
            } => {
                *self = State::Working {
                    enonce1: enonce1.clone(),
                    user_agent: user_agent.clone(),
                    version_mask: *version_mask,
                    address,
                    workername,
                    authorized_username,
                };
                Ok(())
            }
            _ => Err(StratumError::MethodNotAllowed),
        }
    }

    pub(crate) fn version_mask(&self) -> Option<Version> {
        match self {
            State::Fresh { version_mask } => *version_mask,
            State::Subscribed { version_mask, .. } => *version_mask,
            State::Working { version_mask, .. } => *version_mask,
        }
    }

    pub(crate) fn enonce1(&self) -> Option<&Extranonce> {
        match self {
            State::Fresh { .. } => None,
            State::Subscribed { enonce1, .. } => Some(enonce1),
            State::Working { enonce1, .. } => Some(enonce1),
        }
    }

    pub(crate) fn user_agent(&self) -> Option<&str> {
        match self {
            State::Fresh { .. } => None,
            State::Subscribed { user_agent, .. } => Some(user_agent),
            State::Working { user_agent, .. } => Some(user_agent),
        }
    }

    pub(crate) fn address(&self) -> Option<&Address> {
        match self {
            State::Working { address, .. } => Some(address),
            _ => None,
        }
    }

    pub(crate) fn workername(&self) -> Option<&str> {
        match self {
            State::Working { workername, .. } => Some(workername),
            _ => None,
        }
    }

    pub(crate) fn authorized_username(&self) -> Option<&Username> {
        match self {
            State::Working {
                authorized_username,
                ..
            } => Some(authorized_username),
            _ => None,
        }
    }

    pub(crate) fn is_fresh(&self) -> bool {
        matches!(self, State::Fresh { .. })
    }

    pub(crate) fn is_subscribed(&self) -> bool {
        matches!(self, State::Subscribed { .. })
    }

    pub(crate) fn is_working(&self) -> bool {
        matches!(self, State::Working { .. })
    }
}

impl Display for State {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            State::Fresh { version_mask: None } => write!(f, "Init"),
            State::Fresh {
                version_mask: Some(_),
            } => write!(f, "Configured"),
            State::Subscribed { .. } => write!(f, "Subscribed"),
            State::Working { .. } => write!(f, "Working"),
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
    fn new_state_is_fresh() {
        let state = State::new();

        assert!(state.is_fresh());
        assert!(!state.is_subscribed());
        assert!(!state.is_working());
        assert!(state.version_mask().is_none());
        assert!(state.enonce1().is_none());
    }

    #[test]
    fn configure_sets_version_mask_in_fresh() {
        let mut state = State::new();
        let mask = Version::from(0x1fffe000);

        state.configure(mask);

        assert!(state.is_fresh());
        assert_eq!(state.version_mask(), Some(mask));
    }

    #[test]
    fn subscribe_transitions_to_subscribed() {
        let mut state = State::new();
        let enonce1 = test_enonce1();

        state.subscribe(enonce1.clone(), "test/1.0".into());

        assert!(!state.is_fresh());
        assert!(state.is_subscribed());
        assert!(!state.is_working());
        assert_eq!(state.enonce1(), Some(&enonce1));
        assert_eq!(state.user_agent(), Some("test/1.0"));
    }

    #[test]
    fn subscribe_preserves_version_mask() {
        let mut state = State::new();
        let mask = Version::from(0x1fffe000);

        state.configure(mask);
        state.subscribe(test_enonce1(), "test/1.0".into());

        assert!(state.is_subscribed());
        assert_eq!(state.version_mask(), Some(mask));
    }

    #[test]
    fn authorize_in_subscribed_transitions_to_working() {
        let mut state = State::new();

        state.subscribe(test_enonce1(), "test/1.0".into());
        let result = state.authorize(test_address(), "worker1".into(), test_username());

        assert!(result.is_ok());
        assert!(state.is_working());
        assert!(!state.is_subscribed());
        assert_eq!(state.address(), Some(&test_address()));
        assert_eq!(state.workername(), Some("worker1"));
    }

    #[test]
    fn authorize_in_fresh_fails() {
        let mut state = State::new();

        let result = state.authorize(test_address(), "worker1".into(), test_username());

        assert!(result.is_err());
        assert!(state.is_fresh());
    }

    #[test]
    fn authorize_in_working_fails() {
        let mut state = State::new();

        state.subscribe(test_enonce1(), "test/1.0".into());
        state
            .authorize(test_address(), "worker1".into(), test_username())
            .unwrap();

        let result = state.authorize(test_address(), "worker2".into(), test_username());

        assert!(result.is_err());
        assert!(state.is_working());
    }

    #[test]
    fn resubscribe_from_working_resets_to_subscribed() {
        let mut state = State::new();

        state.subscribe(test_enonce1(), "test/1.0".into());
        state
            .authorize(test_address(), "worker1".into(), test_username())
            .unwrap();

        assert!(state.is_working());

        let new_enonce1: Extranonce = "cafebabe".parse().unwrap();
        state.subscribe(new_enonce1.clone(), "test/2.0".into());

        assert!(state.is_subscribed());
        assert!(!state.is_working());
        assert_eq!(state.enonce1(), Some(&new_enonce1));
        assert!(state.address().is_none());
    }

    #[test]
    fn configure_works_in_all_states() {
        let mut state = State::new();
        let mask1 = Version::from(0x1fffe000);
        let mask2 = Version::from(0x0ffff000);

        // Fresh
        state.configure(mask1);
        assert_eq!(state.version_mask(), Some(mask1));

        // Subscribed
        state.subscribe(test_enonce1(), "test/1.0".into());
        state.configure(mask2);
        assert_eq!(state.version_mask(), Some(mask2));

        // Working
        state
            .authorize(test_address(), "worker1".into(), test_username())
            .unwrap();
        state.configure(mask1);
        assert_eq!(state.version_mask(), Some(mask1));
    }

    #[test]
    fn display_shows_correct_state_names() {
        let mut state = State::new();
        assert_eq!(state.to_string(), "Init");

        state.configure(Version::from(0x1fffe000));
        assert_eq!(state.to_string(), "Configured");

        state.subscribe(test_enonce1(), "test/1.0".into());
        assert_eq!(state.to_string(), "Subscribed");

        state
            .authorize(test_address(), "worker1".into(), test_username())
            .unwrap();
        assert_eq!(state.to_string(), "Working");
    }
}
