//! The state a block works on: the parent's state read-only, plus what the block has written so
//! far. It is the revm `Database` of every transaction of the block, and at the end of the block
//! it is the [`StateDiff`].

use crate::Env;
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_trie::EMPTY_ROOT_HASH;
use revm::{
    database_interface::DBErrorMarker,
    primitives::KECCAK_EMPTY,
    state::{AccountInfo, Bytecode, EvmState},
    Database,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};
use xone_primitives::{WideBalance, WideBalanceError};
use xone_state::{Account, StateDiff, StateRead};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OverlayError {
    #[error("the code {0} is not in the code store")]
    MissingCode(B256),
    #[error("the hash of block {0} is not available")]
    MissingHash(u64),
    #[error("account {0} is created after it was deleted in the same block: not supported")]
    Recreated(Address),
    #[error(transparent)]
    Balance(#[from] WideBalanceError),
}

impl DBErrorMarker for OverlayError {}

pub struct Overlay<'a, S, E> {
    base: &'a S,
    env: &'a E,
    accounts: BTreeMap<Address, Option<Account>>,
    storage: BTreeMap<(Address, B256), U256>,
    /// Accounts deleted in this block: their base storage no longer exists.
    wiped: BTreeSet<Address>,
    codes: BTreeMap<B256, Bytes>,
    /// While a transaction runs under EIP-7928: every account and slot it reads, with the value it
    /// had at the first read (geth `getStateObject` and `GetCommittedState` record each access,
    /// `core/state/statedb.go:632-636`, `state_object.go:186-190`).
    rec: RefCell<Option<Reads>>,
}

/// What one transaction read, with the value before it ran.
#[derive(Default)]
pub struct Reads {
    pub accounts: BTreeMap<Address, Option<Account>>,
    pub slots: BTreeMap<(Address, B256), U256>,
}

impl<'a, S: StateRead, E: Env> Overlay<'a, S, E> {
    pub fn new(base: &'a S, env: &'a E) -> Self {
        Self {
            base,
            env,
            accounts: BTreeMap::new(),
            storage: BTreeMap::new(),
            wiped: BTreeSet::new(),
            codes: BTreeMap::new(),
            rec: RefCell::new(None),
        }
    }

    /// Starts recording the reads of a transaction.
    pub fn record(&mut self) {
        *self.rec.get_mut() = Some(Reads::default());
    }

    /// Stops recording and returns what was read, `None` when nothing was recording.
    pub fn take_reads(&mut self) -> Option<Reads> {
        self.rec.get_mut().take()
    }

    pub fn account(&self, a: Address) -> Option<Account> {
        let x = match self.accounts.get(&a) {
            Some(x) => x.clone(),
            None => self.base.account(a),
        };
        if let Some(r) = self.rec.borrow_mut().as_mut() {
            r.accounts.entry(a).or_insert_with(|| x.clone());
        }
        x
    }

    /// A slot as this block sees it (geth `GetState`).
    pub fn slot(&self, a: Address, k: B256) -> U256 {
        let v = if let Some(v) = self.storage.get(&(a, k)) {
            *v
        } else if self.wiped.contains(&a) {
            U256::ZERO
        } else {
            self.base.storage(a, k)
        };
        if let Some(r) = self.rec.borrow_mut().as_mut() {
            r.slots.entry((a, k)).or_insert(v);
        }
        v
    }

    /// Remove an account and its storage (SELFDESTRUCT, EIP-161 deletion, the `TIPSigning` wipe).
    pub fn delete(&mut self, a: Address) {
        self.accounts.insert(a, None);
        self.wiped.insert(a);
        self.storage.retain(|(x, _), _| *x != a);
    }

    /// Write an account record. With `eip158` an empty one is deleted instead (geth
    /// `Finalise(deleteEmptyObjects)`); returns whether the account is kept.
    fn set(&mut self, a: Address, acc: Account, eip158: bool) -> Result<bool, OverlayError> {
        let empty = acc.nonce == 0
            && acc.balance == WideBalance::default()
            && acc.code_hash == KECCAK_EMPTY;
        if eip158 && empty {
            self.delete(a);
            return Ok(false);
        }
        if self.wiped.contains(&a) && self.account(a).is_none() {
            return Err(OverlayError::Recreated(a));
        }
        self.accounts.insert(a, Some(acc));
        Ok(true)
    }

    /// `AddBalance` with the EIP-161 finalise that follows it: an account left empty is deleted.
    pub fn credit(&mut self, a: Address, amount: U256, eip158: bool) -> Result<(), OverlayError> {
        let mut acc = self.account(a).unwrap_or_else(blank);
        acc.balance = acc.balance.checked_add(amount)?;
        self.set(a, acc, eip158).map(|_| ())
    }

    /// geth `SetState`: writing a slot of an account that does not exist creates it.
    pub fn set_slot(&mut self, a: Address, k: B256, v: U256) {
        if self.account(a).is_none() {
            self.accounts.insert(a, Some(blank()));
        }
        self.storage.insert((a, k), v);
    }

    /// `SubBalance` with the EIP-161 finalise that follows it.
    pub fn debit(&mut self, a: Address, amount: U256, eip158: bool) -> Result<(), OverlayError> {
        let mut acc = self.account(a).unwrap_or_else(blank);
        acc.balance = acc.balance.checked_sub(amount)?;
        self.set(a, acc, eip158).map(|_| ())
    }

    /// geth `SetCode`: the account is created if it does not exist; balance, nonce and storage stay.
    pub fn set_code(&mut self, a: Address, code: Bytes) -> Result<(), OverlayError> {
        let mut acc = self.account(a).unwrap_or_else(blank);
        acc.code_hash = alloy_primitives::keccak256(&code);
        self.codes.insert(acc.code_hash, code);
        self.set(a, acc, true).map(|_| ())
    }

    pub fn set_nonce(&mut self, a: Address, nonce: u64) -> Result<(), OverlayError> {
        let mut acc = self.account(a).unwrap_or_else(blank);
        acc.nonce = nonce;
        self.set(a, acc, true).map(|_| ())
    }

    /// Take over what one transaction changed (revm `DatabaseCommit`, with geth's `Finalise`): an
    /// untouched account is skipped, a self-destructed one is deleted, and with `eip158` a touched
    /// account that ends empty is deleted too.
    /// `true_balance` gives the full balance of an account (the journal's side map holds it when it
    /// is above 2^256, `xone_evm::journal::account_true_balance`).
    pub fn commit(
        &mut self,
        state: EvmState,
        true_balance: impl Fn(&Address, &revm::state::Account) -> WideBalance,
        eip158: bool,
    ) -> Result<(), OverlayError> {
        for (a, acct) in state {
            if !acct.is_touched() {
                continue;
            }
            if acct.is_selfdestructed() {
                self.delete(a);
                continue;
            }
            let old = self.account(a);
            let new = Account {
                nonce: acct.info.nonce,
                balance: true_balance(&a, &acct),
                storage_root: old.as_ref().map_or(EMPTY_ROOT_HASH, |o| o.storage_root),
                code_hash: acct.info.code_hash,
            };
            let slots: Vec<_> = acct
                .storage
                .iter()
                .filter(|(_, s)| s.is_changed())
                .map(|(k, s)| (B256::from(k.to_be_bytes::<32>()), s.present_value))
                .collect();
            if let (true, Some(code)) = (new.code_hash != KECCAK_EMPTY, &acct.info.code) {
                self.codes.insert(new.code_hash, code.original_bytes());
            }
            if self.set(a, new, eip158)? {
                for (k, v) in slots {
                    self.storage.insert((a, k), v);
                }
            }
        }
        Ok(())
    }

    /// What the block wrote, as the diff against the base, and the code it deployed.
    pub fn take(&mut self) -> (StateDiff, BTreeMap<B256, Bytes>) {
        let accounts = std::mem::take(&mut self.accounts)
            .into_iter()
            .filter(|(a, new)| self.base.account(*a) != *new)
            .collect();
        let storage = std::mem::take(&mut self.storage)
            .into_iter()
            .filter(|((a, k), v)| self.base.storage(*a, *k) != *v)
            .collect();
        (
            StateDiff { accounts, storage },
            std::mem::take(&mut self.codes),
        )
    }
}

fn blank() -> Account {
    Account::default()
}

impl<S: StateRead, E: Env> Database for Overlay<'_, S, E> {
    type Error = OverlayError;

    fn basic(&mut self, a: Address) -> Result<Option<AccountInfo>, OverlayError> {
        let Some(acc) = self.account(a) else {
            return Ok(None);
        };
        let code = Some(self.code_by_hash(acc.code_hash)?);
        Ok(Some(AccountInfo {
            // the EVM sees the balance modulo 2^256; the journal's side map holds the true one
            balance: acc.balance.low(),
            nonce: acc.nonce,
            code_hash: acc.code_hash,
            code,
            ..Default::default()
        }))
    }

    fn code_by_hash(&mut self, h: B256) -> Result<Bytecode, OverlayError> {
        if h == KECCAK_EMPTY {
            return Ok(Bytecode::default());
        }
        // code deployed by this block: not in the shared cache until the block commits
        if let Some(c) = self.codes.get(&h) {
            return Ok(Bytecode::new_raw(c.clone()));
        }
        let cache = self.env.code_cache();
        if let Some(b) = cache.and_then(|c| c.get(&h)) {
            return Ok(b);
        }
        let code = self.env.code(h).ok_or(OverlayError::MissingCode(h))?;
        Ok(match cache {
            Some(c) => c.analyse(h, code),
            None => Bytecode::new_raw(code),
        })
    }

    fn storage(&mut self, a: Address, k: U256) -> Result<U256, OverlayError> {
        Ok(self.slot(a, B256::from(k.to_be_bytes::<32>())))
    }

    fn block_hash(&mut self, n: u64) -> Result<B256, OverlayError> {
        self.env.hash_of(n).ok_or(OverlayError::MissingHash(n))
    }
}
