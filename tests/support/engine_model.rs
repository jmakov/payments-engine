//! Shared independent state-machine oracle for property and fuzz tests.

use std::collections;

use payments_engine::{engine, transaction};

/// Where a model deposit is in the dispute lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Funds available; may be disputed.
    Settled,
    /// Funds held; may be resolved or charged back.
    Disputed,
    /// Reversed; final.
    ChargedBack,
}

/// A deposit as the reference model remembers it.
#[derive(Debug, Clone, Copy)]
struct Deposit {
    /// Owner.
    client: u16,
    /// Amount in ticks.
    ticks: i128,
    /// Dispute state.
    state: State,
}

/// One account in the reference model; `i128` cannot overflow for sums of `i64` amounts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Account {
    /// Available ticks.
    available: i128,
    /// Held ticks.
    held: i128,
    /// Frozen by a chargeback.
    locked: bool,
}

impl Account {
    /// Whether every balance, including the derived total, fits the engine's `i64` ticks.
    fn fits(&self) -> bool {
        let range = i128::from(i64::MIN)..=i128::from(i64::MAX);
        [self.available, self.held, self.available + self.held]
            .iter()
            .all(|value| range.contains(value))
    }
}

/// Naive reference implementation of the README's rules, written for obviousness.
#[derive(Debug, Default)]
pub(crate) struct Model {
    /// Accounts of clients with at least one accepted transaction.
    pub(crate) accounts: collections::BTreeMap<u16, Account>,
    /// Every accepted deposit.
    deposits: collections::BTreeMap<u32, Deposit>,
}

impl Model {
    /// Applies `transaction`; returns whether it was accepted. A refusal changes nothing.
    pub(crate) fn apply(&mut self, transaction: transaction::Transaction) -> bool {
        let client = transaction.client().get();
        let tx = transaction.tx().get();
        let mut account = self.accounts.get(&client).copied().unwrap_or_default();
        let mut deposit = self.deposits.get(&tx).copied();

        let accepted = match transaction.action() {
            transaction::Action::Deposit(amount) => {
                let ticks = i128::from(amount.get().ticks());
                account.available += ticks;
                deposit = Some(Deposit {
                    client,
                    ticks,
                    state: State::Settled,
                });
                !account.locked && !self.deposits.contains_key(&tx)
            }
            transaction::Action::Withdrawal(amount) => {
                let ticks = i128::from(amount.get().ticks());
                let enough = account.available >= ticks;
                account.available -= ticks;
                !account.locked && enough
            }
            transaction::Action::Dispute => match &mut deposit {
                Some(d) if d.client == client && d.state == State::Settled => {
                    account.available -= d.ticks;
                    account.held += d.ticks;
                    d.state = State::Disputed;
                    true
                }
                _ => false,
            },
            transaction::Action::Resolve => match &mut deposit {
                Some(d) if d.client == client && d.state == State::Disputed => {
                    account.available += d.ticks;
                    account.held -= d.ticks;
                    d.state = State::Settled;
                    true
                }
                _ => false,
            },
            transaction::Action::Chargeback => match &mut deposit {
                Some(d) if d.client == client && d.state == State::Disputed => {
                    account.held -= d.ticks;
                    account.locked = true;
                    d.state = State::ChargedBack;
                    true
                }
                _ => false,
            },
        };

        if !accepted || !account.fits() {
            return false;
        }
        self.accounts.insert(client, account);
        if let Some(deposit) = deposit {
            self.deposits.insert(tx, deposit);
        }
        true
    }
}

/// Every account of `engine` as `client -> (available, held, locked)` in ticks.
pub(crate) fn engine_accounts(engine: &engine::Engine) -> collections::BTreeMap<u16, Account> {
    engine
        .accounts()
        .map(|(client, account)| {
            let total = account.available().checked_add(account.held());
            assert_eq!(Some(account.total()), total, "total = available + held");
            let summary = Account {
                available: i128::from(account.available().ticks()),
                held: i128::from(account.held().ticks()),
                locked: account.is_locked(),
            };
            (client.get(), summary)
        })
        .collect()
}
