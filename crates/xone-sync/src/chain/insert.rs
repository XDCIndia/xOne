//! Block import: verify header, locate the parent, execute on the parent's state, verify body and
//! roots, then commit, adopt or store as a side block. Spec: `spec/chain/insert.md`.

use alloy_primitives::B256;
use xone_consensus::{
    v2::commit::{commit_blocks, Headers, Link},
    verify::{BodyError, BodyVerifier, HeaderError},
};
use xone_primitives::Header;

use super::{
    fork_choice::{decide, Choice, Facts},
    reorg::{self, ReorgError},
    side_index::{self, SideError},
    Chain, ChainRead, HeaderCheck,
};
use crate::{
    executor::{BlockExecutor, ExecError},
    store::{Block, Record, Store, StoreError, Unwound},
};

/// What an accepted block did to the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Already canonical at or below the head, or a side block already stored and still not chosen.
    Known,
    /// The block extends the head.
    Extended,
    /// The block became the head on another branch.
    Adopted { dropped: usize, added: usize },
    /// Executed and stored; the head stays.
    Side,
    /// Parent state is not available: stored without state.
    Stateless,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum InsertError {
    /// geth `consensus.ErrUnknownAncestor`.
    #[error("unknown parent {parent} of block {number}")]
    UnknownParent { number: u64, parent: B256 },
    /// The block is at a required height with another hash: it is on another branch.
    #[error("{0}")]
    RequiredBlock(#[from] xone_chainspec::RequiredBlockMismatch),
    #[error("header: {0}")]
    Header(#[from] HeaderError),
    #[error("body: {0}")]
    Body(#[from] BodyError),
    #[error("execution: {0}")]
    Exec(#[from] ExecError),
    /// geth `ValidateState` gas check, `core/block_validator.go:206-214`.
    #[error("invalid gas used: header {header}, executed {executed}")]
    GasUsed { header: u64, executed: u64 },
    /// geth `ValidateState` state root check, `core/block_validator.go:256-259`.
    #[error("invalid state root: header {header}, executed {executed}")]
    StateRoot { header: B256, executed: B256 },
    /// geth `ValidateState`, `core/block_validator.go:238-252`.
    #[error("access list hash: header {header:?}, executed {executed:?}")]
    AccessListHash {
        header: Option<B256>,
        executed: Option<B256>,
    },
    #[error("reorg: {0}")]
    Reorg(#[from] ReorgError),
    #[error("side branch: {0}")]
    Side(#[from] SideError),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    /// A block of the branch to adopt has no stored execution.
    #[error("no stored execution for block {0}")]
    MissingRecord(B256),
}

struct Hdrs<'a, S>(&'a S);

impl<S: Store> Headers for Hdrs<'_, S> {
    fn link(&self, hash: B256) -> Option<Link> {
        let h = self.0.header(hash)?;
        let round = h.extra_fields_v2().ok()?.round;
        Some(Link {
            number: h.number,
            round,
            hash,
            parent: h.parent_hash,
        })
    }
}

impl<S: Store, E: BlockExecutor, H: HeaderCheck> Chain<S, E, H> {
    /// Imports one block.
    ///
    /// Header verification, and every consensus read it makes, runs first and takes no import lock.
    /// The import lock then covers execution, body verification and the store writes. `now` is the
    /// caller's clock in seconds.
    pub fn insert(&self, block: Block, now: u64) -> Result<Outcome, InsertError> {
        self.check_required(&block.header)?;
        let parent = self
            .reader()
            .header(block.parent())
            .ok_or(InsertError::UnknownParent {
                number: block.number(),
                parent: block.parent(),
            })?;
        if parent.number + 1 != block.number() {
            return Err(InsertError::UnknownParent {
                number: block.number(),
                parent: block.parent(),
            });
        }
        self.check
            .check(&self.reader(), &block.header, &parent, now)?;
        let _import = self.lock_import();
        self.insert_locked(block)
    }

    fn insert_locked(&self, block: Block) -> Result<Outcome, InsertError> {
        let (hash, number) = (block.hash(), block.number());
        let head_number = self.rd().head().0;
        let known = self.rd().record(hash);
        if let Some(rec) = known {
            // geth `skipBlock` + `writeKnownBlock` (`core/blockchain.go:2551-2569`, `:2236-2248`).
            if number <= head_number && self.rd().canonical_hash(number) == Some(hash) {
                return Ok(Outcome::Known);
            }
            return self.settle(rec, true);
        }
        // geth `ValidateBody` runs before execution: a body that does not match its header is never
        // executed (`core/block_validator.go:52-190`).
        let verifier = BodyVerifier::new(&self.spec);
        verifier.verify_body(&block.header, &block.body)?;
        // Executed on the parent's own post-state, never on the head's (xOneGo 5c7be429, e8c845bd).
        let executed = {
            let g = self.rd();
            if g.header(block.parent()).is_none() {
                return Err(InsertError::UnknownParent {
                    number,
                    parent: block.parent(),
                });
            }
            let r = match g.state_at(block.parent()) {
                Ok(parent_state) => Some(self.exec.execute(
                    &*parent_state,
                    &block.header,
                    &block.body,
                )),
                Err(_) => None,
            };
            r
        };
        let Some(executed) = executed else {
            return self.store_stateless(&block, head_number);
        };
        let ex = executed?;
        // geth `ValidateState` (`core/block_validator.go:198-259`): receipts, bloom, then gas used,
        // access list hash and state root. No branch skips them.
        verifier.verify_receipts(&block.header, block.body.transactions.len(), &ex.receipts)?;
        if ex.gas_used != block.header.gas_used {
            return Err(InsertError::GasUsed {
                header: block.header.gas_used,
                executed: ex.gas_used,
            });
        }
        if ex.access_list_hash != block.header.block_access_list_hash {
            return Err(InsertError::AccessListHash {
                header: block.header.block_access_list_hash,
                executed: ex.access_list_hash,
            });
        }
        if ex.state_root != block.header.state_root {
            return Err(InsertError::StateRoot {
                header: block.header.state_root,
                executed: ex.state_root,
            });
        }
        self.settle(
            Record {
                block,
                receipts: ex.receipts,
                diff: ex.diff,
                aux: ex.aux,
            },
            false,
        )
    }

    fn store_stateless(&self, block: &Block, head_number: u64) -> Result<Outcome, InsertError> {
        let canonical = side_index::admit(
            &*self.rd(),
            block.hash(),
            block.number(),
            block.header.state_root,
            block.parent(),
            head_number,
        )?;
        if canonical {
            return Ok(Outcome::Known);
        }
        if self.rd().header(block.hash()).is_none() {
            self.wr().put_stateless(block)?;
        }
        Ok(Outcome::Stateless)
    }

    /// Fork choice for an executed block, then the writes. `known`: the record is already stored.
    fn settle(&self, rec: Record, known: bool) -> Result<Outcome, InsertError> {
        let (head_number, head_hash) = self.rd().head();
        let facts = Facts {
            v2: self.is_v2(rec.block.number()),
            parent_is_head: rec.block.parent() == head_hash,
            number: rec.block.number(),
            head_number,
        };
        match decide(facts) {
            Choice::Extend => {
                let floor = self.floor_after(&*self.rd(), &rec.block.header);
                self.wr().commit_block(&rec, floor)?;
                Ok(Outcome::Extended)
            }
            Choice::Side => {
                if !known {
                    self.wr().put_side(&rec)?;
                }
                Ok(Outcome::Side)
            }
            Choice::Reorg => {
                if !known {
                    self.wr().put_side(&rec)?;
                }
                self.adopt(&rec.block)
            }
        }
    }

    /// geth `reorg` plus the head write of `writeBlockAndSetHead`, on the persisted store: refuse
    /// below the committed block, unwind to the common ancestor in one atomic step, then commit the
    /// blocks of the new branch in order. A crash between two of these steps leaves a consistent
    /// head (the ancestor or a block of the new branch), which `resume` finds.
    fn adopt(&self, new_head: &Block) -> Result<Outcome, InsertError> {
        let plan = {
            let g = self.rd();
            let old = g.header(g.head().1).ok_or(ReorgError::InvalidOldChain)?;
            let plan = reorg::plan(&|h| g.header(h), &old, &new_head.header)?;
            reorg::check_floor(&plan, g.committed().as_ref())?;
            plan
        };
        if !plan.old.is_empty() {
            self.wr().unwind_to(plan.common.number, Unwound::Keep)?;
        }
        for h in plan.new.iter().rev() {
            let rec = self
                .rd()
                .record(h.hash())
                .ok_or(InsertError::MissingRecord(h.hash()))?;
            let floor = self.floor_after(&*self.rd(), &rec.block.header);
            self.wr().commit_block(&rec, floor)?;
        }
        Ok(Outcome::Adopted {
            dropped: plan.old.len(),
            added: plan.new.len(),
        })
    }

    /// The committed block after `header` becomes canonical: the 3-chain rule on the quorum
    /// certificate it carries (`engine_v2/utils.go:476-598`). It only ever moves up. A certificate
    /// whose blocks are not held (a store that starts at a trusted anchor) commits nothing, as geth's
    /// `commitBlocks` error does not stop the block.
    fn floor_after(&self, store: &S, header: &Header) -> Option<Link> {
        let current = store.committed();
        let Some(v2) = self
            .spec
            .xdpos
            .v2
            .as_ref()
            .filter(|v| header.number > v.switch_block)
        else {
            return current;
        };
        let Ok(extra) = header.extra_fields_v2() else {
            return current;
        };
        let q = &extra.quorum_cert.proposed_block_info;
        let Some(parent) = store.header(q.hash).map(|h| h.parent_hash) else {
            return current;
        };
        let proposed = Link {
            number: q.number,
            round: q.round,
            hash: q.hash,
            parent,
        };
        match commit_blocks(&Hdrs(store), v2.switch_block, current.as_ref(), &proposed) {
            Ok(Some(l)) => Some(l),
            _ => current,
        }
    }
}
