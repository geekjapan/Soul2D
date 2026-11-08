#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InputEpoch(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputRefusal {
    Unavailable,
    StaleInput,
    IdentityUnavailable,
}

impl InputRefusal {
    pub(crate) fn state(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable", Self::StaleInput => "stale_input",
            Self::IdentityUnavailable => "identity_unavailable",
        }
    }
}

pub(crate) struct InputEpochs {
    instance: Option<String>,
    generation: u64,
}

impl Default for InputEpochs {
    fn default() -> Self {
        Self { instance:soul2d_core::mcp_http::random_token().ok(), generation:0 }
    }
}

impl InputEpochs {
    pub(crate) fn try_next(&mut self) -> Option<InputEpoch> {
        self.generation = self.generation.checked_add(1)?;
        (self.generation != u64::MAX).then_some(InputEpoch(self.generation))
    }

    pub(crate) fn next(&mut self) -> InputEpoch {
        self.generation = self.generation.saturating_add(1);
        InputEpoch(self.generation)
    }

    pub(crate) fn identity(&self, action: &'static str, epochs: &[Option<InputEpoch>]) -> Option<String> {
        let instance = self.instance.as_ref().filter(|_| self.generation != u64::MAX)?;
        let mut identity = format!("{instance}:{action}");
        for epoch in epochs {
            identity.push(':');
            match epoch {
                Some(epoch) => identity.push_str(&epoch.0.to_string()),
                None => identity.push('-'),
            }
        }
        Some(identity)
    }

    #[cfg(test)]
    pub(crate) fn exhaust_for_test(&mut self) { self.generation = u64::MAX; }

    #[cfg(test)]
    pub(crate) fn fail_instance_for_test(&mut self) { self.instance = None; }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_reservations_stop_before_exhaustion_and_never_reuse_a_ticket() {
        let mut epochs = InputEpochs::default();
        epochs.generation = u64::MAX - 2;
        assert_eq!(epochs.try_next(), Some(InputEpoch(u64::MAX - 1)));
        assert_eq!(epochs.try_next(), None);
        assert_eq!(epochs.try_next(), None);
        assert!(epochs.identity("request", &[]).is_none());
        let mut epochs = InputEpochs::default();
        epochs.fail_instance_for_test();
        assert!(epochs.try_next().is_some());
    }

    #[test]
    fn scoped_identity_changes_only_with_its_owners_and_action() {
        let mut epochs = InputEpochs::default();
        let main = epochs.next();
        let identity = epochs.identity("main", &[Some(main)]).unwrap();
        let _settings = epochs.next();
        assert_eq!(epochs.identity("main", &[Some(main)]).as_ref(), Some(&identity));
        assert_ne!(epochs.identity("all", &[Some(main)]).as_ref(), Some(&identity));
        let replacement = epochs.next();
        assert_ne!(epochs.identity("main", &[Some(replacement)]).as_ref(), Some(&identity));
        assert_ne!(epochs.identity("main", &[None]).as_ref(), Some(&identity));
    }

    #[test]
    fn exhaustion_and_instance_failure_disable_old_remote_identities_without_panicking() {
        let mut epochs = InputEpochs::default();
        epochs.generation = u64::MAX - 2;
        let previous = epochs.next();
        assert!(epochs.identity("main", &[Some(previous)]).is_some());
        epochs.next();
        assert!(epochs.identity("main", &[Some(previous)]).is_none());
        epochs.next();
        assert!(epochs.identity("main", &[Some(previous)]).is_none());
        let mut epochs = InputEpochs::default();
        epochs.fail_instance_for_test();
        let current = epochs.next();
        assert!(epochs.identity("main", &[Some(current)]).is_none());
    }
}
