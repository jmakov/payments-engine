//! Balance bookkeeping for a single client account.
//!
//! [`Account`] knows how money moves between "available" and "held", and nothing about
//! transaction ids or dispute history; that lives in [`crate::engine::Engine`], which decides
//! *whether* a movement is allowed. Client operations require an `OpenAccount` borrow;
//! only an unlocked account can grant one.

use crate::money;
use crate::rejection;

// An explicit path also lets compiler-test crates import this module without changing its
// child-module resolution. They exercise the real private API, not a copied implementation.
#[path = "account/deposit.rs"]
pub(crate) mod deposit;

/// Funds and status of one client's single asset account.
///
/// Only `available` and `held` are stored; [`Account::total`] is always derived from them, so
/// the spec's rule `total = available + held` holds by construction instead of by discipline.
///
/// Every mutating method is all-or-nothing: it computes the new balances with checked
/// arithmetic first and only writes them when the whole operation succeeds. A returned
/// [`rejection::Rejection`] therefore guarantees the account is unchanged.
///
/// `available` may be negative. That happens when a client withdraws a deposit and the
/// deposit is then disputed: the hold must still be placed (see the README).
///
/// # Examples
///
/// ```
/// # fn main() -> eyre::Result<()> {
/// use payments_engine::{engine, money, transaction};
///
/// let mut engine = engine::Engine::new();
/// let client = transaction::ClientId::new(1);
/// let amount = money::Amount::parse("10")?;
/// let tx = transaction::TxId::new(1);
/// engine.apply(transaction::Transaction::new(client, tx, transaction::Action::Deposit(amount)))?;
///
/// let account = engine.account(client).expect("deposit created the account");
/// assert_eq!(account.available(), amount.get());
/// assert_eq!(account.total(), money::Money::from_ticks(100_000));
/// # Ok(()) }
/// ```
///
/// Not recommended: balances cannot be edited directly, which would bypass the invariants.
///
/// ```compile_fail,E0616
/// let mut account = payments_engine::account::Account::default();
/// account.available = payments_engine::money::Money::MAX;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Account {
    /// Funds the client may use; negative after a dispute of already withdrawn funds.
    available: money::Money,
    /// Funds frozen by open disputes; never negative.
    held: money::Money,
    /// A chargeback moves an open account permanently into the locked state.
    state: State,
}

/// Whether the client may initiate new deposits and withdrawals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
enum State {
    /// Client operations are permitted.
    #[default]
    Open,
    /// Only partner dispute operations remain permitted; there is no unlock transition.
    Locked,
}

/// Exclusive permission to perform client operations on an open account.
///
/// The private field prevents callers from constructing this permission. Its lifetime keeps
/// the account exclusively borrowed, so it cannot be locked while this permission exists.
#[derive(Debug)]
pub(crate) struct OpenAccount<'account> {
    /// Account whose open state was checked before granting the borrow.
    account: &'account mut Account,
}

impl Account {
    /// Funds the client can withdraw or trade right now.
    #[must_use]
    pub fn available(&self) -> money::Money {
        self.available
    }

    /// Funds frozen because the deposits that brought them in are under dispute.
    #[must_use]
    pub fn held(&self) -> money::Money {
        self.held
    }

    /// All funds on the account, available plus held.
    ///
    /// # Panics
    ///
    /// Never in practice: every mutation refuses changes after which `available + held` would
    /// overflow, so the sum always fits.
    #[must_use]
    pub fn total(&self) -> money::Money {
        self.available
            .checked_add(self.held)
            .expect("every mutation checks that available + held stays representable")
    }

    /// Whether a chargeback has frozen the account.
    #[must_use]
    pub fn is_locked(&self) -> bool {
        matches!(self.state, State::Locked)
    }

    /// Borrows the account for client operations, rejecting a permanently locked account.
    pub(crate) fn open(
        &mut self,
        client: crate::transaction::ClientId,
    ) -> Result<OpenAccount<'_>, rejection::Rejection> {
        match self.state {
            State::Open => Ok(OpenAccount { account: self }),
            State::Locked => Err(rejection::Rejection::AccountLocked(client)),
        }
    }

    /// Moves a disputed deposit's amount from available to held; `available` may go negative.
    fn hold(&mut self, amount: money::Amount) -> Result<(), rejection::Rejection> {
        let amount = amount.get();
        self.set_balances(
            self.available.checked_sub(amount),
            self.held.checked_add(amount),
        )
    }

    /// Returns a resolved dispute's amount from held to available.
    fn release(&mut self, amount: money::Amount) -> Result<(), rejection::Rejection> {
        let amount = amount.get();
        self.set_balances(
            self.available.checked_add(amount),
            self.held.checked_sub(amount),
        )
    }

    /// Removes a charged-back amount from held funds and freezes the account.
    fn charge_back(&mut self, amount: money::Amount) -> Result<(), rejection::Rejection> {
        self.set_balances(Some(self.available), self.held.checked_sub(amount.get()))?;
        self.state = State::Locked;
        Ok(())
    }

    /// Stores both new balances, or neither if computing either of them overflowed.
    fn set_balances(
        &mut self,
        available: Option<money::Money>,
        held: Option<money::Money>,
    ) -> Result<(), rejection::Rejection> {
        // Checking the sum here is what lets `total()` add without a fallible return type.
        match (available, held) {
            (Some(available), Some(held)) if available.checked_add(held).is_some() => {
                self.available = available;
                self.held = held;
                Ok(())
            }
            _ => Err(rejection::Rejection::Overflow),
        }
    }
}

impl OpenAccount<'_> {
    /// Credits available funds; the receiver proves that client operations are permitted.
    pub(crate) fn deposit(self, amount: money::Amount) -> Result<(), rejection::Rejection> {
        self.account.set_balances(
            self.account.available.checked_add(amount.get()),
            Some(self.account.held),
        )
    }

    /// Debits available funds without spending money held by a dispute.
    pub(crate) fn withdraw(self, amount: money::Amount) -> Result<(), rejection::Rejection> {
        let amount = amount.get();
        if self.account.available < amount {
            return Err(rejection::Rejection::InsufficientFunds);
        }
        self.account.set_balances(
            self.account.available.checked_sub(amount),
            Some(self.account.held),
        )
    }
}

#[cfg(test)]
mod tests {
    /// Whole currency units as an [`super::money::Amount`].
    fn amount(units: i64) -> super::money::Amount {
        super::money::Amount::parse(&units.to_string()).expect("test amounts are positive")
    }

    /// Whole currency units as a (possibly negative) balance.
    fn money(units: i64) -> super::money::Money {
        let ticks = units.checked_mul(super::money::TICKS_PER_UNIT);
        super::money::Money::from_ticks(ticks.expect("test balances fit"))
    }

    fn balances(available: i64, held: i64) -> super::Account {
        super::Account {
            available: money(available),
            held: money(held),
            state: super::State::Open,
        }
    }

    fn max_amount() -> super::money::Amount {
        super::money::Amount::parse(&super::money::Money::MAX.to_string())
            .expect("the largest balance is a valid amount")
    }

    #[test]
    fn deposit_increases_available_and_total() {
        let mut account = balances(1, 2);

        account
            .open(crate::transaction::ClientId::new(1))
            .unwrap()
            .deposit(amount(5))
            .expect("deposit fits");

        assert_eq!(account, balances(6, 2));
        assert_eq!(account.total(), money(8));
    }

    #[test]
    fn withdraw_decreases_available_when_funds_suffice() {
        let mut account = balances(5, 2);

        account
            .open(crate::transaction::ClientId::new(1))
            .unwrap()
            .withdraw(amount(5))
            .expect("exactly enough funds");

        assert_eq!(account, balances(0, 2));
    }

    #[test]
    fn withdraw_refuses_to_use_held_funds() {
        let mut account = balances(1, 10);

        let result = account
            .open(crate::transaction::ClientId::new(1))
            .unwrap()
            .withdraw(amount(2));

        assert_eq!(result, Err(super::rejection::Rejection::InsufficientFunds));
        assert_eq!(account, balances(1, 10));
    }

    #[test]
    fn hold_and_release_move_funds_without_changing_total() {
        let mut account = balances(3, 0);

        account
            .hold(amount(5))
            .expect("hold may overdraw available");
        assert_eq!(account, balances(-2, 5));
        assert_eq!(account.total(), money(3));

        account.release(amount(5)).expect("release fits");
        assert_eq!(account, balances(3, 0));
    }

    #[test]
    fn charge_back_removes_held_funds_and_locks() {
        let mut account = balances(-2, 5);

        account.charge_back(amount(5)).expect("charge back fits");

        assert_eq!(account.held(), super::money::Money::ZERO);
        assert_eq!(account.total(), money(-2));
        assert!(account.is_locked());
    }

    #[test]
    fn overflow_is_rejected_and_leaves_balances_unchanged() {
        let max = max_amount();
        let mut account = balances(1, 0);

        let result = account
            .open(crate::transaction::ClientId::new(1))
            .unwrap()
            .deposit(max);

        assert_eq!(result, Err(super::rejection::Rejection::Overflow));
        assert_eq!(account, balances(1, 0));
    }

    #[test]
    fn overflow_of_the_derived_total_is_rejected() {
        let max = max_amount();
        let mut account = balances(0, 1);

        // available alone would fit, but available + held would not.
        let result = account
            .open(crate::transaction::ClientId::new(1))
            .unwrap()
            .deposit(max);

        assert_eq!(result, Err(super::rejection::Rejection::Overflow));
        assert_eq!(account, balances(0, 1));
    }
}
