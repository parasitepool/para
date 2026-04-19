use super::*;

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const MAX_ATTEMPTS_AT_CAP: u32 = 3;

#[must_use]
pub(crate) enum BackoffEnd {
    Cancelled,
    Exhausted,
}

pub(crate) struct Backoff {
    delay: Duration,
    attempts_at_cap: u32,
}

impl Backoff {
    pub(crate) fn new() -> Self {
        Self {
            delay: INITIAL_BACKOFF,
            attempts_at_cap: 0,
        }
    }

    pub(crate) async fn wait(
        &mut self,
        cancel: &CancellationToken,
        label: &str,
    ) -> Result<(), BackoffEnd> {
        warn!("{label} retrying in {}s...", self.delay.as_secs());

        tokio::select! {
            _ = sleep(self.delay) => {}
            _ = cancel.cancelled() => return Err(BackoffEnd::Cancelled),
        }

        self.delay = (self.delay * 2).min(MAX_BACKOFF);

        if self.delay >= MAX_BACKOFF {
            self.attempts_at_cap += 1;
            if self.attempts_at_cap >= MAX_ATTEMPTS_AT_CAP {
                error!(
                    "{label} unreachable after {} attempts at max backoff",
                    self.attempts_at_cap,
                );
                return Err(BackoffEnd::Exhausted);
            }
        }

        Ok(())
    }
}

pub(crate) async fn retry_with_backoff<T, E, F, Fut>(
    cancel: &CancellationToken,
    label: &str,
    mut factory: F,
) -> Result<T, BackoffEnd>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: Display,
{
    let mut backoff = Backoff::new();

    loop {
        match factory().await {
            Ok(value) => return Ok(value),
            Err(err) => warn!("{label} attempt failed: {err}"),
        }

        backoff.wait(cancel, label).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn success_on_first_attempt() {
        let cancel = CancellationToken::new();
        let outcome = retry_with_backoff(&cancel, "foo", || async { Ok::<_, &str>(42) }).await;
        assert!(matches!(outcome, Ok(42)));
    }

    #[tokio::test]
    async fn cancelled_during_backoff() {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });

        let outcome = retry_with_backoff(&cancel, "foo", || async { Err::<(), _>("bar") }).await;
        assert!(matches!(outcome, Err(BackoffEnd::Cancelled)));
    }

    #[tokio::test(start_paused = true)]
    async fn exhausted_after_max_attempts_at_cap() {
        let cancel = CancellationToken::new();
        let mut backoff = Backoff::new();

        loop {
            match backoff.wait(&cancel, "foo").await {
                Ok(()) => continue,
                Err(BackoffEnd::Exhausted) => return,
                Err(BackoffEnd::Cancelled) => panic!("token was never cancelled"),
            }
        }
    }

    #[test]
    fn backoff_schedule() {
        let mut backoff = Backoff::new();
        assert_eq!(backoff.delay, INITIAL_BACKOFF);

        let mut delays = vec![backoff.delay];
        for _ in 0..10 {
            backoff.delay = (backoff.delay * 2).min(MAX_BACKOFF);
            delays.push(backoff.delay);
        }

        let seconds: Vec<u64> = delays.iter().map(|d| d.as_secs()).collect();
        assert_eq!(seconds, vec![1, 2, 4, 8, 16, 32, 60, 60, 60, 60, 60]);
    }
}
