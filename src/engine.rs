//! The transaction state machine: applies each [`transaction::Transaction`] to client accounts.
//!
//! [`Engine`] owns all state that must outlive a single CSV row:
//!
//! - one [`account::Account`] per client seen so far, and
//! - one deposit record per accepted deposit, because a later dispute, resolve or chargeback
//!   only names the deposit's id and the engine must remember its amount, owner and dispute
//!   status.
//!
//! Withdrawals are not remembered: they cannot be disputed (see the README for why), so
//! keeping them would cost memory for nothing.
//!
//! The engine performs no I/O. The transaction worker feeds it parsed transactions, and a
//! server could just as well feed it from a socket.
//!
//! Runtime input is checked once to obtain an exclusive typed state borrow. Only an open
//! account exposes deposit/withdrawal operations; only a settled deposit exposes dispute;
//! only a disputed deposit exposes resolve/chargeback. Each operation consumes that borrow.

use std::collections;
use std::collections::hash_map;

use crate::account;
use crate::account::deposit;
use crate::rejection;
use crate::transaction;

/// Applies transactions in input order and tracks every client's balances.
///
/// Feed it transactions with [`Engine::apply`] and read the result with [`Engine::accounts`].
/// Each call either applies the whole transaction or returns a [`rejection::Rejection`] and
/// changes nothing, so a bad row can never leave an account half-updated or create one.
///
/// Memory grows with the number of clients (at most 65,536) and the number of accepted
/// deposits; withdrawals and dispute rows add nothing.
///
/// # Examples
///
/// ```
/// # fn main() -> eyre::Result<()> {
/// use payments_engine::{engine, money, rejection, transaction};
///
/// let client = transaction::ClientId::new(1);
/// let deposit_tx = transaction::TxId::new(1);
/// let amount = money::Amount::parse("10")?;
/// let mut engine = engine::Engine::new();
///
/// engine.apply(transaction::Transaction::new(client, deposit_tx, transaction::Action::Deposit(amount)))?;
/// engine.apply(transaction::Transaction::new(client, deposit_tx, transaction::Action::Dispute))?;
/// engine.apply(transaction::Transaction::new(client, deposit_tx, transaction::Action::Chargeback))?;
///
/// let account = engine.account(client).expect("client has an account");
/// assert!(account.is_locked());
/// assert_eq!(account.total(), money::Money::ZERO);
///
/// // Rejected transactions are reported, not applied.
/// let refused = engine.apply(transaction::Transaction::new(
///     client, transaction::TxId::new(2), transaction::Action::Deposit(amount),
/// ));
/// assert_eq!(refused, Err(rejection::Rejection::AccountLocked(client)));
/// # Ok(()) }
/// ```
///
/// Not recommended: feeding transactions out of chronological order. The engine trusts input
/// order, so a dispute that arrives before its deposit is rejected as unknown and is not
/// replayed later.
///
/// ```
/// # fn main() -> eyre::Result<()> {
/// use payments_engine::{engine, money, rejection, transaction};
///
/// let (client, tx) = (transaction::ClientId::new(1), transaction::TxId::new(1));
/// let amount = money::Amount::parse("1")?;
/// let mut engine = engine::Engine::new();
///
/// let early = engine.apply(transaction::Transaction::new(client, tx, transaction::Action::Dispute));
/// assert_eq!(early, Err(rejection::Rejection::UnknownTransaction(tx)));
/// engine.apply(transaction::Transaction::new(client, tx, transaction::Action::Deposit(amount)))?;
/// assert_eq!(engine.account(client).map(|account| account.held()), Some(money::Money::ZERO));
/// # Ok(()) }
/// ```
#[derive(Debug, Default)]
pub struct Engine {
    /// Balances of every client named by an applied transaction.
    accounts: Map<transaction::ClientId, account::Account>,
    /// Every accepted deposit, kept so later dispute rows can find its amount and owner.
    deposits: Map<transaction::TxId, deposit::Record>,
}

/// Hash map keyed by small integer ids, hashed with foldhash instead of the default hasher for
/// performance reasons.
type Map<K, V> = collections::HashMap<K, V, foldhash::fast::RandomState>;

impl Engine {
    /// Creates an engine with no accounts.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one transaction, or rejects it and leaves the engine unchanged.
    ///
    /// A client's account is created by the client's first accepted transaction. A rejected
    /// transaction creates nothing, so rows that only name a client (a withdrawal from an empty
    /// account, a dispute of someone else's deposit) do not add empty accounts to the report.
    ///
    /// # Errors
    ///
    /// Returns a [`rejection::Rejection`] when the transaction is not allowed, for example a
    /// withdrawal without sufficient funds, a deposit or withdrawal on a locked account, or a
    /// dispute that references an unknown deposit. Each variant documents its case.
    pub fn apply(
        &mut self,
        transaction: transaction::Transaction,
    ) -> Result<(), rejection::Rejection> {
        let client = transaction.client();
        let tx = transaction.tx();
        tracing::trace!(%client, %tx, kind = transaction.kind(), "applying transaction");
        // Work on a copy (`Account` is `Copy`) and store it only once everything succeeded, so
        // every early `?` return leaves both the balances and the set of accounts untouched.
        let mut account = self.accounts.get(&client).copied().unwrap_or_default();

        match transaction.action() {
            transaction::Action::Deposit(amount) => {
                let open = account.open(client)?;
                // `let ... else` binds the vacant slot or leaves the function early.
                let hash_map::Entry::Vacant(slot) = self.deposits.entry(tx) else {
                    return Err(rejection::Rejection::DuplicateTransaction(tx));
                };
                slot.insert(deposit::Record::deposit(client, amount, open)?);
            }
            transaction::Action::Withdrawal(amount) => {
                account.open(client)?.withdraw(amount)?;
            }
            transaction::Action::Dispute => {
                self.deposit(tx)?
                    .settled(client, tx, &mut account)?
                    .dispute()?;
            }
            transaction::Action::Resolve => {
                self.deposit(tx)?
                    .disputed(client, tx, &mut account)?
                    .resolve()?;
            }
            transaction::Action::Chargeback => {
                self.deposit(tx)?
                    .disputed(client, tx, &mut account)?
                    .charge_back()?;
            }
        }
        self.accounts.insert(client, account);
        tracing::trace!(%client, %tx, kind = transaction.kind(), locked = account.is_locked(), "account state committed");
        Ok(())
    }

    /// Returns the account of `client`, or `None` if no transaction of theirs was accepted.
    #[must_use]
    pub fn account(&self, client: transaction::ClientId) -> Option<&account::Account> {
        self.accounts.get(&client)
    }

    /// Iterates over every known account in unspecified order.
    pub fn accounts(&self) -> impl Iterator<Item = (transaction::ClientId, &account::Account)> {
        self.accounts
            .iter()
            .map(|(client, account)| (*client, account))
    }

    /// Looks up deposit `tx`; its typed state borrow will check ownership before acting.
    fn deposit(
        &mut self,
        tx: transaction::TxId,
    ) -> Result<&mut deposit::Record, rejection::Rejection> {
        self.deposits
            .get_mut(&tx)
            .ok_or(rejection::Rejection::UnknownTransaction(tx))
    }
}

#[cfg(test)]
mod tests {
    use crate::money;
    const CLIENT: u16 = 1;
    const OTHER_CLIENT: u16 = 2;

    fn client(id: u16) -> super::transaction::ClientId {
        super::transaction::ClientId::new(id)
    }

    fn tx(id: u32) -> super::transaction::TxId {
        super::transaction::TxId::new(id)
    }

    fn money(units: i64) -> money::Amount {
        money::Amount::parse(&units.to_string()).expect("test amounts are positive")
    }

    fn deposit(client_id: u16, tx_id: u32, units: i64) -> super::transaction::Transaction {
        super::transaction::Transaction::new(
            client(client_id),
            tx(tx_id),
            super::transaction::Action::Deposit(money(units)),
        )
    }

    fn withdrawal(client_id: u16, tx_id: u32, units: i64) -> super::transaction::Transaction {
        super::transaction::Transaction::new(
            client(client_id),
            tx(tx_id),
            super::transaction::Action::Withdrawal(money(units)),
        )
    }

    fn dispute(client_id: u16, tx_id: u32) -> super::transaction::Transaction {
        super::transaction::Transaction::new(
            client(client_id),
            tx(tx_id),
            super::transaction::Action::Dispute,
        )
    }

    fn resolve(client_id: u16, tx_id: u32) -> super::transaction::Transaction {
        super::transaction::Transaction::new(
            client(client_id),
            tx(tx_id),
            super::transaction::Action::Resolve,
        )
    }

    fn chargeback(client_id: u16, tx_id: u32) -> super::transaction::Transaction {
        super::transaction::Transaction::new(
            client(client_id),
            tx(tx_id),
            super::transaction::Action::Chargeback,
        )
    }

    /// Builds an engine by applying `transactions`, all of which must be accepted.
    fn engine_after(transactions: &[super::transaction::Transaction]) -> super::Engine {
        let mut engine = super::Engine::new();
        for transaction in transactions {
            engine
                .apply(*transaction)
                .expect("setup transaction is accepted");
        }
        engine
    }

    /// Returns `(available, held, total, locked)` with balances as whole units.
    fn balances(engine: &super::Engine, client_id: u16) -> (i64, i64, i64, bool) {
        let account = engine.account(client(client_id)).expect("account exists");
        let units = |value: crate::money::Money| {
            let ticks = value.ticks();
            assert_eq!(
                ticks.checked_rem(crate::money::TICKS_PER_UNIT),
                Some(0),
                "test balances are whole units"
            );
            ticks
                .checked_div(crate::money::TICKS_PER_UNIT)
                .expect("divisor is non-zero")
        };
        (
            units(account.available()),
            units(account.held()),
            units(account.total()),
            account.is_locked(),
        )
    }

    #[test]
    fn deposit_creates_account_and_credits_available() {
        let engine = engine_after(&[deposit(CLIENT, 1, 10)]);

        assert_eq!(balances(&engine, CLIENT), (10, 0, 10, false));
    }

    #[test]
    fn withdrawal_debits_available() {
        let engine = engine_after(&[deposit(CLIENT, 1, 10), withdrawal(CLIENT, 2, 4)]);

        assert_eq!(balances(&engine, CLIENT), (6, 0, 6, false));
    }

    #[test]
    fn withdrawal_beyond_available_is_rejected() {
        let mut engine = engine_after(&[deposit(CLIENT, 1, 10)]);

        let result = engine.apply(withdrawal(CLIENT, 2, 11));

        assert_eq!(result, Err(super::rejection::Rejection::InsufficientFunds));
        assert_eq!(balances(&engine, CLIENT), (10, 0, 10, false));
    }

    #[test]
    fn rejected_transactions_create_no_account() {
        let mut engine = engine_after(&[deposit(CLIENT, 1, 10)]);

        let results = [
            engine.apply(withdrawal(OTHER_CLIENT, 2, 1)),
            engine.apply(dispute(OTHER_CLIENT, 1)),
        ];

        assert_eq!(
            results,
            [
                Err(super::rejection::Rejection::InsufficientFunds),
                Err(super::rejection::Rejection::ClientMismatch(tx(1))),
            ]
        );
        assert_eq!(engine.account(client(OTHER_CLIENT)), None);
        assert_eq!(engine.accounts().count(), 1);
    }

    #[test]
    fn duplicate_deposit_id_is_rejected() {
        let mut engine = engine_after(&[deposit(CLIENT, 1, 10)]);

        let result = engine.apply(deposit(OTHER_CLIENT, 1, 5));

        assert_eq!(
            result,
            Err(super::rejection::Rejection::DuplicateTransaction(tx(1)))
        );
        assert_eq!(engine.account(client(OTHER_CLIENT)), None);
    }

    #[test]
    fn dispute_moves_deposit_from_available_to_held() {
        let engine = engine_after(&[deposit(CLIENT, 1, 10), dispute(CLIENT, 1)]);

        assert_eq!(balances(&engine, CLIENT), (0, 10, 10, false));
    }

    #[test]
    fn dispute_of_unknown_or_withdrawal_tx_is_rejected() {
        let mut engine = engine_after(&[deposit(CLIENT, 1, 10), withdrawal(CLIENT, 2, 3)]);

        assert_eq!(
            engine.apply(dispute(CLIENT, 99)),
            Err(super::rejection::Rejection::UnknownTransaction(tx(99)))
        );
        assert_eq!(
            engine.apply(dispute(CLIENT, 2)),
            Err(super::rejection::Rejection::UnknownTransaction(tx(2)))
        );
        assert_eq!(balances(&engine, CLIENT), (7, 0, 7, false));
    }

    #[test]
    fn dispute_flow_by_another_client_is_rejected() {
        let mut engine = engine_after(&[deposit(CLIENT, 1, 10)]);

        assert_eq!(
            engine.apply(dispute(OTHER_CLIENT, 1)),
            Err(super::rejection::Rejection::ClientMismatch(tx(1)))
        );
        engine.apply(dispute(CLIENT, 1)).expect("owner may dispute");
        assert_eq!(
            engine.apply(resolve(OTHER_CLIENT, 1)),
            Err(super::rejection::Rejection::ClientMismatch(tx(1)))
        );
        assert_eq!(
            engine.apply(chargeback(OTHER_CLIENT, 1)),
            Err(super::rejection::Rejection::ClientMismatch(tx(1)))
        );
        assert_eq!(balances(&engine, CLIENT), (0, 10, 10, false));
    }

    #[test]
    fn second_dispute_of_same_deposit_is_rejected() {
        let mut engine = engine_after(&[deposit(CLIENT, 1, 10), dispute(CLIENT, 1)]);

        let result = engine.apply(dispute(CLIENT, 1));

        assert_eq!(
            result,
            Err(super::rejection::Rejection::AlreadyDisputed(tx(1)))
        );
        assert_eq!(balances(&engine, CLIENT), (0, 10, 10, false));
    }

    #[test]
    fn resolve_releases_held_funds_and_allows_a_new_dispute() {
        let mut engine = engine_after(&[
            deposit(CLIENT, 1, 10),
            dispute(CLIENT, 1),
            resolve(CLIENT, 1),
        ]);
        assert_eq!(balances(&engine, CLIENT), (10, 0, 10, false));

        engine
            .apply(dispute(CLIENT, 1))
            .expect("resolved deposit is disputable");
        assert_eq!(balances(&engine, CLIENT), (0, 10, 10, false));
    }

    #[test]
    fn resolve_or_chargeback_without_dispute_is_rejected() {
        let mut engine = engine_after(&[deposit(CLIENT, 1, 10)]);

        assert_eq!(
            engine.apply(resolve(CLIENT, 1)),
            Err(super::rejection::Rejection::NotDisputed(tx(1)))
        );
        assert_eq!(
            engine.apply(chargeback(CLIENT, 1)),
            Err(super::rejection::Rejection::NotDisputed(tx(1)))
        );
        assert_eq!(
            engine.apply(resolve(CLIENT, 99)),
            Err(super::rejection::Rejection::UnknownTransaction(tx(99)))
        );
        assert_eq!(balances(&engine, CLIENT), (10, 0, 10, false));
    }

    #[test]
    fn chargeback_removes_held_funds_and_locks_account() {
        let engine = engine_after(&[
            deposit(CLIENT, 1, 10),
            deposit(CLIENT, 2, 5),
            dispute(CLIENT, 1),
            chargeback(CLIENT, 1),
        ]);

        assert_eq!(balances(&engine, CLIENT), (5, 0, 5, true));
    }

    #[test]
    fn locked_account_rejects_deposits_and_withdrawals() {
        let mut engine = engine_after(&[
            deposit(CLIENT, 1, 10),
            deposit(CLIENT, 2, 5),
            dispute(CLIENT, 1),
            chargeback(CLIENT, 1),
        ]);
        let locked = Err(super::rejection::Rejection::AccountLocked(client(CLIENT)));

        for transaction in [deposit(CLIENT, 3, 1), withdrawal(CLIENT, 4, 1)] {
            assert_eq!(engine.apply(transaction), locked, "{transaction:?}");
        }
        assert_eq!(balances(&engine, CLIENT), (5, 0, 5, true));
    }

    #[test]
    fn locked_account_still_processes_dispute_actions() {
        let mut engine = engine_after(&[
            deposit(CLIENT, 1, 10),
            deposit(CLIENT, 2, 5),
            deposit(CLIENT, 3, 2),
            dispute(CLIENT, 2),
            dispute(CLIENT, 1),
            chargeback(CLIENT, 1),
        ]);
        assert_eq!(balances(&engine, CLIENT), (2, 5, 7, true));

        engine
            .apply(resolve(CLIENT, 2))
            .expect("resolve applies after the lock");
        engine
            .apply(dispute(CLIENT, 3))
            .expect("dispute applies after the lock");
        engine
            .apply(chargeback(CLIENT, 3))
            .expect("chargeback applies after the lock");

        assert_eq!(balances(&engine, CLIENT), (5, 0, 5, true));
    }

    #[test]
    fn deposit_withdraw_chargeback_fraud_leaves_negative_balance() {
        let engine = engine_after(&[
            deposit(CLIENT, 1, 10),
            withdrawal(CLIENT, 2, 10),
            dispute(CLIENT, 1),
            chargeback(CLIENT, 1),
        ]);

        assert_eq!(balances(&engine, CLIENT), (-10, 0, -10, true));
    }

    #[test]
    fn overflowing_deposit_is_rejected_and_not_recorded() {
        let max = money::Amount::parse(&crate::money::Money::MAX.to_string())
            .expect("the largest balance is a valid amount");
        let mut engine = engine_after(&[deposit(CLIENT, 1, 1)]);
        let overflowing = super::transaction::Transaction::new(
            client(CLIENT),
            tx(2),
            super::transaction::Action::Deposit(max),
        );

        assert_eq!(
            engine.apply(overflowing),
            Err(super::rejection::Rejection::Overflow)
        );
        assert_eq!(
            engine.apply(dispute(CLIENT, 2)),
            Err(super::rejection::Rejection::UnknownTransaction(tx(2)))
        );
        assert_eq!(balances(&engine, CLIENT), (1, 0, 1, false));
    }

    #[test]
    fn charged_back_deposit_cannot_be_disputed_again() {
        let mut engine = engine_after(&[
            deposit(CLIENT, 1, 10),
            dispute(CLIENT, 1),
            chargeback(CLIENT, 1),
        ]);

        let result = engine.apply(dispute(CLIENT, 1));

        assert_eq!(
            result,
            Err(super::rejection::Rejection::AlreadyChargedBack(tx(1)))
        );
        assert_eq!(balances(&engine, CLIENT), (0, 0, 0, true));
    }

    #[test]
    fn overflowing_dispute_preserves_both_balances_and_deposit_state() {
        let max = money::Amount::parse(&crate::money::Money::MAX.to_string()).unwrap();
        let mut engine = super::Engine::new();
        engine
            .apply(super::transaction::Transaction::new(
                client(CLIENT),
                tx(1),
                super::transaction::Action::Deposit(max),
            ))
            .unwrap();
        engine
            .apply(super::transaction::Transaction::new(
                client(CLIENT),
                tx(2),
                super::transaction::Action::Withdrawal(max),
            ))
            .unwrap();
        engine.apply(dispute(CLIENT, 1)).unwrap();
        engine.apply(deposit(CLIENT, 3, 1)).unwrap();
        let accounts_before = engine.accounts.clone();
        let deposits_before = engine.deposits.clone();

        let rejected = engine.apply(dispute(CLIENT, 3));

        assert_eq!(rejected, Err(super::rejection::Rejection::Overflow));
        assert_eq!(engine.accounts, accounts_before);
        assert_eq!(engine.deposits, deposits_before);
        engine.apply(resolve(CLIENT, 1)).unwrap();
        engine
            .apply(dispute(CLIENT, 3))
            .expect("failed dispute left the deposit settled");
        assert_eq!(balances(&engine, CLIENT), (0, 1, 1, false));
    }

    #[test]
    fn terminal_deposit_rejects_every_follow_up_without_changing_state() {
        let mut engine = engine_after(&[
            deposit(CLIENT, 1, 10),
            dispute(CLIENT, 1),
            chargeback(CLIENT, 1),
        ]);
        let accounts_before = engine.accounts.clone();
        let deposits_before = engine.deposits.clone();

        for event in [
            dispute(CLIENT, 1),
            resolve(CLIENT, 1),
            chargeback(CLIENT, 1),
        ] {
            assert!(engine.apply(event).is_err());
            assert_eq!(engine.accounts, accounts_before);
            assert_eq!(engine.deposits, deposits_before);
        }
    }

    #[test]
    fn ledger_entry_stays_compact() {
        // The deposit ledger is the only state that grows with input size; the README's memory
        // estimate assumes 24 bytes per entry plus hashbrown's 1-byte control tag.
        let entry = std::mem::size_of::<(super::transaction::TxId, super::deposit::Record)>();

        assert_eq!(entry, 24);
    }

    #[test]
    fn clients_are_independent() {
        let engine = engine_after(&[
            deposit(CLIENT, 1, 10),
            deposit(OTHER_CLIENT, 2, 3),
            dispute(CLIENT, 1),
        ]);

        assert_eq!(balances(&engine, CLIENT), (0, 10, 10, false));
        assert_eq!(balances(&engine, OTHER_CLIENT), (3, 0, 3, false));
        assert_eq!(engine.accounts().count(), 2);
    }
}
