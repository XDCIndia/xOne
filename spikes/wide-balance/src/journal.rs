//! Journal wrapper that gives revm geth-XDC wide-balance semantics.
//!
//! revm stores balances as U256 and its transfer, selfdestruct and affordability code
//! cannot see a wider value. `WideJournal` wraps `Journal<DB>` and keeps the true value in
//! one of two places ([`Carry`]):
//!
//! * option A: `AccountInfo::extension` (feature `account-ext`), journaled and reverted by revm;
//! * option B: a side map with its own undo log, kept in step with revm's checkpoints.
//!
//! The wrapped low part always stays in `AccountInfo::balance`, so BALANCE, SELFBALANCE and
//! the rest of revm see exactly what geth's `GetBalance` returns.

use alloy_primitives::{Address, B256, U256};
use revm::{
    context::Journal,
    context_interface::{
        context::{SStoreResult, SelfDestructResult, StateLoad},
        journaled_state::{
            account::{JournaledAccount, JournaledAccountTr},
            AccountInfoLoad, AccountLoad, JournalCheckpoint, JournalLoadErasedError,
            JournalLoadError, JournalTr, TransferError,
        },
        Database,
    },
    primitives::{
        hardfork::SpecId, AddressMap, AddressSet, HashSet, Log, StorageKey, StorageValue,
    },
    state::{Account, Bytecode, EvmState, EvmStorageSlot},
};
use std::collections::HashMap;
use xone_primitives::WideBalance;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carry {
    /// Option B.
    Side,
    /// Option A.
    #[cfg(feature = "account-ext")]
    Ext,
}

/// Option B storage: true balances of wide accounts plus an undo log.
#[derive(Debug, Default)]
pub struct SideMap {
    map: HashMap<Address, WideBalance>,
    log: Vec<(Address, Option<WideBalance>)>,
    marks: Vec<usize>,
}

impl SideMap {
    fn set(&mut self, a: Address, v: Option<WideBalance>) {
        let old = match &v {
            Some(w) => self.map.insert(a, w.clone()),
            None => self.map.remove(&a),
        };
        self.log.push((a, old));
    }
    fn mark(&mut self) {
        self.marks.push(self.log.len());
    }
    fn commit(&mut self) {
        self.marks.pop();
    }
    fn revert(&mut self) {
        let to = self.marks.pop().unwrap_or(0);
        while self.log.len() > to {
            let (a, old) = self.log.pop().expect("len checked");
            match old {
                Some(w) => self.map.insert(a, w),
                None => self.map.remove(&a),
            };
        }
    }
    fn clear_log(&mut self) {
        self.log.clear();
        self.marks.clear();
    }
    /// Drop the entry of a deleted account (after the state is applied).
    pub fn forget(&mut self, a: &Address) {
        self.map.remove(a);
    }
    pub fn insert(&mut self, a: Address, w: WideBalance) {
        self.map.insert(a, w);
    }
    pub fn get(&self, a: &Address) -> Option<&WideBalance> {
        self.map.get(a)
    }
}

/// Where a wide balance lives, as seen from one account.
enum Slot<'a> {
    Side(&'a mut SideMap),
    #[cfg(feature = "account-ext")]
    Ext,
}

/// The carried (wide) value of an account, if any.
fn carried(acc: &Account, side: Option<&WideBalance>) -> Option<WideBalance> {
    #[cfg(feature = "account-ext")]
    if !acc.info.extension.is_empty() {
        return Some(WideBalance::from_be_bytes(&acc.info.extension));
    }
    let _ = acc;
    side.cloned()
}

/// The true balance stored on an account (option A reads the extension, option B the side map).
pub fn account_true_balance(acc: &Account, side: Option<&WideBalance>) -> WideBalance {
    carried(acc, side).unwrap_or_else(|| WideBalance::from(acc.info.balance))
}

pub struct WideJournal<DB: Database> {
    pub inner: Journal<DB>,
    pub carry: Carry,
    pub side: SideMap,
}

static MAX: U256 = U256::MAX;

/// `JournaledAccount` that applies the geth balance rules.
pub struct WideAcct<'a, DB: Database> {
    inner: JournaledAccount<'a, DB>,
    slot: Slot<'a>,
    addr: Address,
}

impl<DB: Database> WideAcct<'_, DB> {
    /// True when the account carries a wide value (no allocation).
    fn carries(&self) -> bool {
        match &self.slot {
            Slot::Side(s) => s.map.contains_key(&self.addr),
            #[cfg(feature = "account-ext")]
            Slot::Ext => !self.inner.account().info.extension.is_empty(),
        }
    }

    fn true_balance(&self) -> WideBalance {
        let side = match &self.slot {
            Slot::Side(s) => s.map.get(&self.addr),
            #[cfg(feature = "account-ext")]
            Slot::Ext => None,
        };
        account_true_balance(self.inner.account(), side)
    }

    fn set_true(&mut self, new: WideBalance) {
        self.inner.set_balance(new.low());
        let wide = new.is_wide();
        match &mut self.slot {
            Slot::Side(s) => s.set(self.addr, wide.then_some(new)),
            #[cfg(feature = "account-ext")]
            Slot::Ext => self.inner.set_extension(if wide {
                revm::state::AccountExtension::copy_from_slice(&new.to_be_bytes())
            } else {
                revm::state::AccountExtension::new()
            }),
        }
    }
}

impl<DB: Database> JournaledAccountTr for WideAcct<'_, DB> {
    fn account(&self) -> &Account {
        self.inner.account()
    }
    fn sload(
        &mut self,
        key: StorageKey,
        skip_cold_load: bool,
    ) -> Result<StateLoad<&mut EvmStorageSlot>, JournalLoadErasedError> {
        self.inner.sload(key, skip_cold_load)
    }
    fn sstore(
        &mut self,
        key: StorageKey,
        new: StorageValue,
        skip_cold_load: bool,
    ) -> Result<StateLoad<SStoreResult>, JournalLoadErasedError> {
        self.inner.sstore(key, new, skip_cold_load)
    }
    fn load_code(&mut self) -> Result<&Bytecode, JournalLoadErasedError> {
        self.inner.load_code()
    }
    /// geth `CanTransfer` is true for any wide account, so every affordability check
    /// in revm (`balance >= amount`, amounts are U256) must pass for it.
    fn balance(&self) -> &U256 {
        if self.carries() {
            &MAX
        } else {
            self.inner.balance()
        }
    }
    fn nonce(&self) -> u64 {
        self.inner.nonce()
    }
    fn code_hash(&self) -> &B256 {
        self.inner.code_hash()
    }
    fn code(&self) -> Option<&Bytecode> {
        self.inner.code()
    }
    fn touch(&mut self) {
        self.inner.touch()
    }
    fn unsafe_mark_cold(&mut self) {
        self.inner.unsafe_mark_cold()
    }
    /// geth `SetBalance`: sets the value and clears `BigBalance`.
    fn set_balance(&mut self, balance: U256) {
        if self.carries() {
            self.set_true(WideBalance::from(balance));
        } else {
            self.inner.set_balance(balance);
        }
    }
    #[cfg(feature = "account-ext")]
    fn set_extension(&mut self, extension: revm::state::AccountExtension) {
        self.inner.set_extension(extension)
    }
    fn incr_balance(&mut self, balance: U256) -> bool {
        // Narrow fast path: no wide value and no overflow, plain revm.
        if !self.carries()
            && self
                .inner
                .account()
                .info
                .balance
                .checked_add(balance)
                .is_some()
        {
            return self.inner.incr_balance(balance);
        }
        match self.true_balance().checked_add(balance) {
            Ok(v) => {
                self.set_true(v);
                true
            }
            Err(_) => false,
        }
    }
    fn decr_balance(&mut self, balance: U256) -> bool {
        if !self.carries() {
            return self.inner.decr_balance(balance);
        }
        match self.true_balance().checked_sub(balance) {
            Ok(v) => {
                self.set_true(v);
                true
            }
            Err(_) => false,
        }
    }
    fn bump_nonce(&mut self) -> bool {
        self.inner.bump_nonce()
    }
    fn set_nonce(&mut self, nonce: u64) {
        self.inner.set_nonce(nonce)
    }
    fn unsafe_set_nonce(&mut self, nonce: u64) {
        self.inner.unsafe_set_nonce(nonce)
    }
    fn set_code(&mut self, code_hash: B256, code: Bytecode) {
        self.inner.set_code(code_hash, code)
    }
    fn set_code_and_hash_slow(&mut self, code: Bytecode) {
        self.inner.set_code_and_hash_slow(code)
    }
    fn delegate(&mut self, address: Address) {
        self.inner.delegate(address)
    }
}

impl<DB: Database> WideJournal<DB> {
    fn carries(&self, a: &Address, _acc: &Account) -> bool {
        match self.carry {
            Carry::Side => self.side.map.contains_key(a),
            #[cfg(feature = "account-ext")]
            Carry::Ext => !_acc.info.extension.is_empty(),
        }
    }

    pub fn with_carry(mut self, carry: Carry) -> Self {
        self.carry = carry;
        self
    }

    /// True balance of a loaded account.
    pub fn true_balance(&mut self, a: Address) -> Result<WideBalance, DB::Error> {
        let acc = self.inner.load_account(a)?.data;
        Ok(account_true_balance(acc, self.side.get(&a)))
    }

    /// Set the true balance with journaling (the wrapped part goes to `AccountInfo::balance`).
    fn set_true(&mut self, a: Address, new: WideBalance) -> Result<(), DB::Error> {
        let acc = self.inner.load_account_mut(a)?.data;
        wrap(self.carry, &mut self.side, acc, a).set_true(new);
        Ok(())
    }

    /// Carry metadata only (the low part was already moved by revm).
    fn set_carry_only(&mut self, a: Address, new: &WideBalance) -> Result<(), DB::Error> {
        match self.carry {
            Carry::Side => self.side.set(a, new.is_wide().then(|| new.clone())),
            #[cfg(feature = "account-ext")]
            Carry::Ext => self
                .inner
                .load_account_mut(a)?
                .data
                .set_extension(if new.is_wide() {
                    revm::state::AccountExtension::copy_from_slice(&new.to_be_bytes())
                } else {
                    revm::state::AccountExtension::new()
                }),
        }
        Ok(())
    }
}

type R<T, DB> = Result<T, <DB as Database>::Error>;

impl<DB: Database> JournalTr for WideJournal<DB> {
    type Database = DB;
    type State = EvmState;
    type JournaledAccount<'a>
        = WideAcct<'a, DB>
    where
        DB: 'a;

    fn new(database: DB) -> Self {
        Self {
            inner: Journal::new(database),
            carry: Carry::Side,
            side: SideMap::default(),
        }
    }
    fn db_and_state(&self) -> (&DB, &EvmState) {
        self.inner.db_and_state()
    }
    fn db_and_state_mut(&mut self) -> (&mut DB, &mut EvmState) {
        self.inner.db_and_state_mut()
    }
    fn sload_skip_cold_load(
        &mut self,
        a: Address,
        k: StorageKey,
        s: bool,
    ) -> Result<StateLoad<StorageValue>, JournalLoadError<DB::Error>> {
        self.inner.sload_skip_cold_load(a, k, s)
    }
    fn sstore_skip_cold_load(
        &mut self,
        a: Address,
        k: StorageKey,
        v: StorageValue,
        s: bool,
    ) -> Result<StateLoad<SStoreResult>, JournalLoadError<DB::Error>> {
        self.inner.sstore_skip_cold_load(a, k, v, s)
    }
    fn tload(&mut self, a: Address, k: StorageKey) -> StorageValue {
        self.inner.tload(a, k)
    }
    fn tstore(&mut self, a: Address, k: StorageKey, v: StorageValue) {
        self.inner.tstore(a, k, v)
    }
    fn log(&mut self, l: Log) {
        self.inner.log(l)
    }
    fn take_logs(&mut self) -> Vec<Log> {
        self.inner.take_logs()
    }
    fn logs(&self) -> &[Log] {
        self.inner.logs()
    }

    /// geth moves `GetBalance(this)`, the wrapped part, then `SubBalance(this, low)`: the
    /// high part stays behind (and is deleted with the account unless EIP-6780 keeps it).
    fn selfdestruct(
        &mut self,
        address: Address,
        target: Address,
        skip_cold_load: bool,
    ) -> Result<StateLoad<SelfDestructResult>, JournalLoadError<DB::Error>> {
        let old = self
            .true_balance(address)
            .map_err(JournalLoadError::DBError)?;
        let old_low = old.low();
        let res = self.inner.selfdestruct(address, target, skip_cold_load)?;
        let after = self
            .inner
            .load_account(address)
            .map_err(JournalLoadError::DBError)?
            .data
            .info
            .balance;
        let moved = if !old_low.is_zero() && after.is_zero() {
            old_low
        } else {
            U256::ZERO
        };
        if !moved.is_zero() {
            // revm moved (and wrapped) the low part; bring the carried part in step.
            let tgt = if address == target {
                None
            } else {
                // The target's low part already changed. Its true value is the carried one,
                // or (if narrow) the new low minus what was moved.
                let acc = self
                    .inner
                    .load_account(target)
                    .map_err(JournalLoadError::DBError)?
                    .data;
                Some(
                    carried(acc, self.side.get(&target))
                        .unwrap_or_else(|| WideBalance::from(acc.info.balance.wrapping_sub(moved))),
                )
            };
            if let Some(t) = tgt {
                let new = t.checked_add(moved).expect("legal bound; spike");
                self.set_carry_only(target, &new)
                    .map_err(JournalLoadError::DBError)?;
            }
            let rest = old.checked_sub(moved).expect("moved <= true");
            self.set_carry_only(address, &rest)
                .map_err(JournalLoadError::DBError)?;
        }
        Ok(res)
    }

    fn warm_access_list(&mut self, l: AddressMap<HashSet<StorageKey>>) {
        self.inner.warm_access_list(l)
    }
    fn warm_coinbase_account(&mut self, a: Address) {
        self.inner.warm_coinbase_account(a)
    }
    fn warm_precompiles(&mut self, a: &AddressSet) {
        self.inner.warm_precompiles(a)
    }
    fn precompile_addresses(&self) -> &AddressSet {
        self.inner.precompile_addresses()
    }
    fn set_spec_id(&mut self, s: SpecId) {
        self.inner.set_spec_id(s)
    }
    fn set_eip7708_config(&mut self, d: bool, e: bool) {
        self.inner.set_eip7708_config(d, e)
    }
    fn touch_account(&mut self, a: Address) {
        self.inner.touch_account(a)
    }

    fn transfer(&mut self, from: Address, to: Address, v: U256) -> R<Option<TransferError>, DB> {
        self.inner.load_account(from)?;
        self.inner.load_account(to)?;
        Ok(self.transfer_loaded(from, to, v))
    }

    /// geth `CanTransfer` (true for wide senders, else `low >= v`) then `SubBalance`/`AddBalance`.
    fn transfer_loaded(&mut self, from: Address, to: Address, v: U256) -> Option<TransferError> {
        // Narrow fast path: neither side carries a wide value and nothing overflows.
        let st = &self.inner.state;
        if let (Some(f), Some(t)) = (st.get(&from), st.get(&to)) {
            if !self.carries(&from, f)
                && !self.carries(&to, t)
                && f.info.balance >= v
                && t.info.balance.checked_add(v).is_some()
            {
                return self.inner.transfer_loaded(from, to, v);
            }
        }
        let f = self.true_balance(from).ok()?;
        if from == to {
            return f.checked_sub(v).err().map(|_| TransferError::OutOfFunds);
        }
        if v.is_zero() {
            self.inner.touch_account(to);
            return None;
        }
        let Ok(new_from) = f.checked_sub(v) else {
            return Some(TransferError::OutOfFunds);
        };
        let new_to = match self.true_balance(to).ok()?.checked_add(v) {
            Ok(x) => x,
            Err(_) => return Some(TransferError::OverflowPayment),
        };
        self.set_true(from, new_from).ok()?;
        self.set_true(to, new_to).ok()?;
        None
    }

    fn caller_accounting_journal_entry(&mut self, a: Address, old: U256, bump: bool) {
        #[expect(deprecated)]
        self.inner.caller_accounting_journal_entry(a, old, bump)
    }
    fn balance_incr(&mut self, a: Address, v: U256) -> R<(), DB> {
        let new = self
            .true_balance(a)?
            .checked_add(v)
            .expect("legal bound; spike");
        self.set_true(a, new)
    }
    fn nonce_bump_journal_entry(&mut self, a: Address) {
        #[expect(deprecated)]
        self.inner.nonce_bump_journal_entry(a)
    }
    fn load_account(&mut self, a: Address) -> R<StateLoad<&Account>, DB> {
        self.inner.load_account(a)
    }
    fn load_account_with_code(&mut self, a: Address) -> R<StateLoad<&Account>, DB> {
        self.inner.load_account_with_code(a)
    }
    fn load_account_delegated(&mut self, a: Address) -> R<StateLoad<AccountLoad>, DB> {
        self.inner.load_account_delegated(a)
    }
    fn load_account_mut_skip_cold_load(
        &mut self,
        a: Address,
        skip: bool,
    ) -> Result<StateLoad<WideAcct<'_, DB>>, JournalLoadError<DB::Error>> {
        let l = self.inner.load_account_mut_skip_cold_load(a, skip)?;
        Ok(StateLoad {
            data: wrap(self.carry, &mut self.side, l.data, a),
            is_cold: l.is_cold,
        })
    }
    fn load_account_mut_optional_code(
        &mut self,
        a: Address,
        code: bool,
    ) -> R<StateLoad<WideAcct<'_, DB>>, DB> {
        let l = self.inner.load_account_mut_optional_code(a, code)?;
        Ok(StateLoad {
            data: wrap(self.carry, &mut self.side, l.data, a),
            is_cold: l.is_cold,
        })
    }
    fn set_code_with_hash(&mut self, a: Address, c: Bytecode, h: B256) {
        self.inner.set_code_with_hash(a, c, h)
    }

    fn checkpoint(&mut self) -> JournalCheckpoint {
        self.side.mark();
        self.inner.checkpoint()
    }
    fn checkpoint_commit(&mut self) {
        self.side.commit();
        self.inner.checkpoint_commit()
    }
    fn checkpoint_revert(&mut self, cp: JournalCheckpoint) {
        self.side.revert();
        self.inner.checkpoint_revert(cp)
    }

    /// revm debits the creator's low part (wrapping) and credits the new account; only the
    /// carried part of the creator needs adjusting.
    fn create_account_checkpoint(
        &mut self,
        caller: Address,
        address: Address,
        balance: U256,
        spec: SpecId,
    ) -> Result<JournalCheckpoint, TransferError> {
        let pre = self.true_balance(caller).ok();
        let cp = self
            .inner
            .create_account_checkpoint(caller, address, balance, spec)?;
        self.side.mark();
        if let (Some(pre), false) = (pre, balance.is_zero()) {
            let new = pre
                .checked_sub(balance)
                .map_err(|_| TransferError::OutOfFunds)?;
            self.set_carry_only(caller, &new)
                .map_err(|_| TransferError::OutOfFunds)?;
        }
        Ok(cp)
    }
    fn depth(&self) -> usize {
        self.inner.depth()
    }
    fn commit_tx(&mut self) {
        self.side.clear_log();
        self.inner.commit_tx()
    }
    fn discard_tx(&mut self) {
        self.side.clear_log();
        self.inner.discard_tx()
    }
    fn finalize(&mut self) -> EvmState {
        self.side.clear_log();
        self.inner.finalize()
    }
    fn load_account_info_skip_cold_load(
        &mut self,
        a: Address,
        code: bool,
        skip: bool,
    ) -> Result<AccountInfoLoad<'_>, JournalLoadError<DB::Error>> {
        self.inner.load_account_info_skip_cold_load(a, code, skip)
    }
}

fn wrap<'a, DB: Database>(
    carry: Carry,
    side: &'a mut SideMap,
    inner: JournaledAccount<'a, DB>,
    addr: Address,
) -> WideAcct<'a, DB> {
    let slot = match carry {
        Carry::Side => Slot::Side(side),
        #[cfg(feature = "account-ext")]
        Carry::Ext => Slot::Ext,
    };
    WideAcct { inner, slot, addr }
}
