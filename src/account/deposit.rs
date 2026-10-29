//! Typed, exclusive transitions of accepted deposits and their associated account balances.
//!
//! The ledger stores runtime states because CSV events choose deposits dynamically. Checking
//! that state grants a short-lived typed borrow: [`Settled`] can only dispute, [`Disputed`]
//! can only resolve or charge back. Each transition consumes its borrow and updates the
//! balances before changing the ledger state, so a rejected movement changes neither.

use crate::account;
use crate::money;
use crate::rejection;
use crate::transaction;

/// Deposit lifecycle; a chargeback is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum State {
    /// The deposit may be disputed.
    Settled,
    /// The deposit's funds are held pending resolution or chargeback.
    Disputed,
    /// Funds were reversed; no transition is permitted.
    ChargedBack,
}

/// Accepted deposit retained so partner events can locate its owner, amount and state.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(Clone))]
pub(crate) struct Record {
    /// Only this client may initiate dispute actions for this deposit.
    client: transaction::ClientId,
    /// Original validated amount; never taken from a follow-up event.
    amount: money::Amount,
    /// Current lifecycle position, writable only by this module's transitions.
    state: State,
}

/// Exclusive permission to dispute a settled deposit.
#[derive(Debug)]
pub(crate) struct Settled<'ledger> {
    /// Exclusively borrowed record, checked to be settled.
    record: &'ledger mut Record,
    /// Account staged by the engine for the same client.
    account: &'ledger mut account::Account,
}

/// Exclusive permission to resolve or charge back a disputed deposit.
#[derive(Debug)]
pub(crate) struct Disputed<'ledger> {
    /// Exclusively borrowed record, checked to be disputed.
    record: &'ledger mut Record,
    /// Account staged by the engine for the same client.
    account: &'ledger mut account::Account,
}

impl Record {
    /// Credits an open account and creates its settled record as one fallible operation.
    pub(crate) fn deposit(
        client: transaction::ClientId,
        amount: money::Amount,
        open: account::OpenAccount<'_>,
    ) -> Result<Self, rejection::Rejection> {
        open.deposit(amount)?;
        Ok(Self {
            client,
            amount,
            state: State::Settled,
        })
    }

    /// Grants a settled-state borrow after checking ownership and the runtime state.
    pub(crate) fn settled<'ledger>(
        &'ledger mut self,
        client: transaction::ClientId,
        tx: transaction::TxId,
        account: &'ledger mut account::Account,
    ) -> Result<Settled<'ledger>, rejection::Rejection> {
        self.ensure_owner(client, tx)?;
        match self.state {
            State::Settled => Ok(Settled {
                record: self,
                account,
            }),
            State::Disputed => Err(rejection::Rejection::AlreadyDisputed(tx)),
            State::ChargedBack => Err(rejection::Rejection::AlreadyChargedBack(tx)),
        }
    }

    /// Grants a disputed-state borrow after checking ownership and the runtime state.
    pub(crate) fn disputed<'ledger>(
        &'ledger mut self,
        client: transaction::ClientId,
        tx: transaction::TxId,
        account: &'ledger mut account::Account,
    ) -> Result<Disputed<'ledger>, rejection::Rejection> {
        self.ensure_owner(client, tx)?;
        match self.state {
            State::Disputed => Ok(Disputed {
                record: self,
                account,
            }),
            State::Settled | State::ChargedBack => Err(rejection::Rejection::NotDisputed(tx)),
        }
    }

    /// Rejects events that name another client's deposit before granting any state borrow.
    fn ensure_owner(
        &self,
        client: transaction::ClientId,
        tx: transaction::TxId,
    ) -> Result<(), rejection::Rejection> {
        if self.client != client {
            return Err(rejection::Rejection::ClientMismatch(tx));
        }
        Ok(())
    }
}

impl Settled<'_> {
    /// Moves available funds to held funds, then marks the deposit disputed.
    pub(crate) fn dispute(self) -> Result<(), rejection::Rejection> {
        self.account.hold(self.record.amount)?;
        self.record.state = State::Disputed;
        tracing::trace!(client = %self.record.client, "deposit funds held");
        Ok(())
    }
}

impl Disputed<'_> {
    /// Releases held funds and makes the deposit disputable again.
    pub(crate) fn resolve(self) -> Result<(), rejection::Rejection> {
        self.account.release(self.record.amount)?;
        self.record.state = State::Settled;
        tracing::trace!(client = %self.record.client, "deposit funds released");
        Ok(())
    }

    /// Reverses held funds, locks the account and makes the deposit terminal.
    pub(crate) fn charge_back(self) -> Result<(), rejection::Rejection> {
        self.account.charge_back(self.record.amount)?;
        self.record.state = State::ChargedBack;
        tracing::trace!(client = %self.record.client, "deposit reversed and account locked");
        Ok(())
    }
}
