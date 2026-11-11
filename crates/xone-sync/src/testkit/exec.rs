use alloy_primitives::{Address, B256, U256};
use std::{
    collections::{BTreeMap, HashMap},
    sync::atomic::{AtomicUsize, Ordering},
};
use xone_consensus::verify::Body;
use xone_primitives::Header;
use xone_state::{Account, StateRead};

use crate::executor::{BlockExecutor, ExecError, Execution};

/// Replays a recorded execution per block hash. It also asserts the parent state the recording was
/// made on: the accounts and slots the block reads must have the recorded values, or the execution
/// fails. That is what a state root mismatch would be for a real executor, so a block executed on
/// the wrong parent state is refused here too.
#[derive(Default)]
pub struct VectorExecutor {
    table: HashMap<B256, Recorded>,
    calls: AtomicUsize,
}

#[derive(Clone)]
struct Recorded {
    pre_accounts: BTreeMap<Address, Option<Account>>,
    pre_slots: BTreeMap<(Address, B256), U256>,
    execution: Execution,
}

impl Clone for VectorExecutor {
    /// The recordings, with a fresh call counter.
    fn clone(&self) -> Self {
        Self {
            table: self.table.clone(),
            calls: AtomicUsize::new(0),
        }
    }
}

impl VectorExecutor {
    pub fn record(
        &mut self,
        hash: B256,
        pre_accounts: BTreeMap<Address, Option<Account>>,
        pre_slots: BTreeMap<(Address, B256), U256>,
        execution: Execution,
    ) {
        self.table.insert(
            hash,
            Recorded {
                pre_accounts,
                pre_slots,
                execution,
            },
        );
    }

    /// Number of `execute` calls so far.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl BlockExecutor for VectorExecutor {
    fn execute(
        &self,
        parent: &dyn StateRead,
        header: &Header,
        _body: &Body,
    ) -> Result<Execution, ExecError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let r = self
            .table
            .get(&header.hash())
            .ok_or_else(|| ExecError::Failed(format!("no recording for {}", header.hash())))?;
        for (a, want) in &r.pre_accounts {
            if parent.account(*a) != *want {
                return Err(ExecError::Failed(format!("parent state differs at {a}")));
            }
        }
        for ((a, s), want) in &r.pre_slots {
            if parent.storage(*a, *s) != *want {
                return Err(ExecError::Failed(format!(
                    "parent state differs at {a} {s}"
                )));
            }
        }
        Ok(r.execution.clone())
    }
}
