//! Retry utilities for HTTP requests.

// crates.io
use tokio::time;
// self
use crate::{_prelude::*, registry::RetryPolicy};

/// Result of budgeting a retry attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptBudget {
	/// Additional attempt is permitted with the provided per-attempt timeout.
	Granted {
		/// Timeout window allocated for the upcoming attempt.
		timeout: Duration,
	},
	/// Retry window exhausted; no further attempts allowed.
	Exhausted,
}

/// Controls retry backoff progression and attempt budgeting.
#[derive(Debug)]
pub struct RetryExecutor<'a> {
	policy: &'a RetryPolicy,
	deadline: Instant,
	retries_used: u32,
}
impl<'a> RetryExecutor<'a> {
	/// Create a new executor respecting the supplied retry policy.
	pub fn new(policy: &'a RetryPolicy) -> Self {
		let deadline = Instant::now() + policy.deadline;

		Self { policy, deadline, retries_used: 0 }
	}

	/// Budget the next attempt, returning either the permitted timeout or exhaustion.
	pub fn attempt_budget(&self) -> AttemptBudget {
		let remaining = self.remaining_budget();

		if remaining.is_zero() {
			AttemptBudget::Exhausted
		} else {
			let timeout = remaining.min(self.policy.attempt_timeout);

			if timeout.is_zero() {
				AttemptBudget::Exhausted
			} else {
				AttemptBudget::Granted { timeout }
			}
		}
	}

	/// Whether another retry is permitted under the policy.
	pub fn can_retry(&self) -> bool {
		self.retries_used < self.policy.max_retries
	}

	/// Remaining wall-clock budget for the overall retry window.
	pub fn remaining_budget(&self) -> Duration {
		self.deadline.saturating_duration_since(Instant::now())
	}

	/// Number of retries that have already been consumed.
	pub fn attempts_used(&self) -> u32 {
		self.retries_used
	}

	/// Advance retry state and compute the backoff delay for the next attempt.
	pub fn next_backoff(&mut self) -> Option<Duration> {
		if !self.can_retry() {
			tracing::debug!(attempt = self.retries_used, "retry budget exhausted");

			return None;
		}

		let attempt = self.retries_used;

		self.retries_used = self.retries_used.saturating_add(1);

		let mut delay = self.policy.compute_backoff(attempt);
		let remaining = self.remaining_budget();

		if !remaining.is_zero() {
			delay = delay.min(remaining);
		} else {
			delay = Duration::ZERO;
		}

		tracing::debug!(attempt = attempt + 1, ?delay, remaining = ?remaining, "retry backoff computed");

		Some(delay)
	}

	/// Sleep for the computed backoff window if retrying is permitted.
	pub async fn sleep_backoff(&mut self) {
		if let Some(delay) = self.next_backoff()
			&& !delay.is_zero()
		{
			time::sleep(delay).await;
		}
	}
}
