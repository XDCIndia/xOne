use crate::{
    case::Case,
    db::{addr, bytes, code_hash, u256, CaseDb, DbError},
};
use alloy_primitives::{Address, B256, U256};
use revm::{
    context::{result::EVMError, ContextSetters, TxEnv},
    context_interface::{
        result::{ExecutionResult, Output},
        JournalTr,
    },
    handler::Handler,
    primitives::{TxKind, KECCAK_EMPTY},
};
use std::{collections::BTreeMap, str::FromStr};
use xone_chainspec::Network;
use xone_evm::{
    handler::{build_evm_with_rules, XdcHandler},
    spec::{XdcBlock, XdcRules},
};

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("the transaction reads the hash of block {0}")]
    MissingBlockHash(u64),
    #[error("not covered by xone-evm: {0}")]
    Unsupported(String),
    #[error("xone-evm rejected the transaction: {0}")]
    Rejected(String),
}

/// Final state of one account the transaction touched, as xone-evm computed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fin {
    pub deleted: bool,
    pub balance: U256,
    pub nonce: u64,
    pub code_hash: B256,
    /// Storage slots whose value differs from the prestate.
    pub slots: BTreeMap<U256, U256>,
}

#[derive(Debug)]
pub struct Outcome {
    pub gas_used: u64,
    pub success: bool,
    pub created: Option<Address>,
    pub logs: Vec<(Address, Vec<B256>, Vec<u8>)>,
    pub accounts: BTreeMap<Address, Fin>,
}

pub fn replay(c: &Case) -> Result<Outcome, ReplayError> {
    replay_with(c, &|_| {})
}

/// Replay with `tweak` applied to the rules first: how a mutant is run.
pub fn replay_with(c: &Case, tweak: &dyn Fn(&mut XdcRules)) -> Result<Outcome, ReplayError> {
    let net = Network::from_str(&c.network).map_err(|e| ReplayError::Unsupported(e.to_string()))?;
    let spec = net.spec();
    let b = &c.block;
    let block = XdcBlock {
        number: b.number,
        timestamp: b.timestamp,
        beneficiary: addr(&b.miner),
        gas_limit: b.gas_limit,
        difficulty: u256(&b.difficulty),
        base_fee: b
            .base_fee
            .as_deref()
            .map(|f| u256(f).saturating_to::<u64>()),
    };
    let owner = c.owner.as_deref().map(addr).filter(|a| !a.is_zero());
    let mut rules = xone_evm::spec::rules(&spec, block.number)
        .map_err(|e| ReplayError::Unsupported(e.to_string()))?;
    tweak(&mut rules);
    let mut evm = build_evm_with_rules(CaseDb::new(c), &spec, rules, &block, owner);

    let t = &c.tx;
    let mut access_list = Vec::new();
    for (a, keys) in &t.access_list {
        access_list.push(revm::context::transaction::AccessListItem {
            address: addr(a),
            storage_keys: keys
                .iter()
                .map(|k| B256::from_str(k).unwrap_or_default())
                .collect(),
        });
    }
    let dynamic = t.ty == 2;
    evm.ctx.set_tx(TxEnv {
        tx_type: t.ty,
        caller: addr(&t.from),
        gas_limit: t.gas,
        gas_price: u256(&t.gas_price).saturating_to::<u128>(),
        gas_priority_fee: dynamic
            .then(|| u256(t.max_priority_fee.as_deref().unwrap_or("0x0")).saturating_to::<u128>()),
        kind: t
            .to
            .as_deref()
            .map_or(TxKind::Create, |a| TxKind::Call(addr(a))),
        value: u256(&t.value),
        data: bytes(&t.input),
        nonce: t.nonce,
        // revm wants the chain id on typed transactions; the archive accepted them, so it matches.
        chain_id: (t.ty >= 1).then_some(spec.chain_id),
        access_list: access_list.into(),
        ..Default::default()
    });

    let result = XdcHandler::<CaseDb>::default()
        .run(&mut evm)
        .map_err(|e| match e {
            EVMError::Database(DbError::MissingBlockHash(n)) => ReplayError::MissingBlockHash(n),
            EVMError::Custom(m) if m.contains("not implemented") => ReplayError::Unsupported(m),
            other => ReplayError::Rejected(format!("{other}")),
        })?;
    let (gas_used, success, logs, created) = match &result {
        ExecutionResult::Success {
            gas, logs, output, ..
        } => (
            gas.tx_gas_used(),
            true,
            logs.clone(),
            match output {
                Output::Create(_, a) => *a,
                Output::Call(_) => None,
            },
        ),
        ExecutionResult::Revert { gas, .. } => (gas.tx_gas_used(), false, vec![], None),
        ExecutionResult::Halt { gas, .. } => (gas.tx_gas_used(), false, vec![], None),
    };
    let logs = logs
        .into_iter()
        .map(|l| (l.address, l.data.topics().to_vec(), l.data.data.to_vec()))
        .collect();

    let state = evm.ctx.journaled_state.finalize();
    let mut accounts = BTreeMap::new();
    for (a, acct) in state {
        let deleted = acct.is_selfdestructed()
            || ((acct.is_touched() || acct.is_created()) && acct.is_empty());
        let slots = acct
            .storage
            .iter()
            .filter(|(_, s)| s.is_changed())
            .map(|(k, s)| (*k, s.present_value))
            .collect();
        accounts.insert(
            a,
            Fin {
                deleted,
                balance: acct.info.balance,
                nonce: acct.info.nonce,
                code_hash: acct.info.code_hash,
                slots,
            },
        );
    }
    Ok(Outcome {
        gas_used,
        success,
        created,
        logs,
        accounts,
    })
}

type Norm = (U256, u64, B256);

fn empty() -> Norm {
    (U256::ZERO, 0, KECCAK_EMPTY)
}

/// The archive's view of every account the transaction changed.
fn archive(c: &Case) -> BTreeMap<Address, Fin> {
    let mut out = BTreeMap::new();
    let keys: std::collections::BTreeSet<&String> =
        c.diff_pre.keys().chain(c.diff_post.keys()).collect();
    for k in keys {
        let base = c.pre.get(k);
        let (mut balance, mut nonce, mut ch) = base.map_or(empty(), |b| {
            (
                u256(b.balance.as_deref().unwrap_or("0x0")),
                b.nonce.unwrap_or(0),
                code_hash(&bytes(b.code.as_deref().unwrap_or("0x"))),
            )
        });
        // In a diff, an account that is in `pre` and not in `post` was deleted.
        let Some(post) = c.diff_post.get(k) else {
            out.insert(
                addr(k),
                Fin {
                    deleted: true,
                    balance,
                    nonce,
                    code_hash: ch,
                    slots: BTreeMap::new(),
                },
            );
            continue;
        };
        if let Some(v) = &post.balance {
            balance = u256(v);
        }
        if let Some(v) = post.nonce {
            nonce = v;
        }
        if let Some(v) = &post.code {
            ch = code_hash(&bytes(v));
        }
        // A slot that is in `pre` and not in `post` was set to zero.
        let mut slots = BTreeMap::new();
        if let Some(pre) = c.diff_pre.get(k) {
            for s in pre.storage.keys() {
                slots.insert(u256(s), U256::ZERO);
            }
        }
        for (s, v) in &post.storage {
            slots.insert(u256(s), u256(v));
        }
        out.insert(
            addr(k),
            Fin {
                deleted: false,
                balance,
                nonce,
                code_hash: ch,
                slots,
            },
        );
    }
    out
}

fn pre_norm(c: &Case, a: &Address) -> Norm {
    c.pre
        .iter()
        .find(|(k, _)| addr(k) == *a)
        .map_or(empty(), |(_, b)| {
            (
                u256(b.balance.as_deref().unwrap_or("0x0")),
                b.nonce.unwrap_or(0),
                code_hash(&bytes(b.code.as_deref().unwrap_or("0x"))),
            )
        })
}

fn norm(f: &Fin) -> Norm {
    if f.deleted {
        empty()
    } else {
        (f.balance, f.nonce, f.code_hash)
    }
}

/// Accounts whose content differs from the prestate. Existence of an empty account is not
/// content: the archive cannot show it.
fn changed(c: &Case, accounts: &BTreeMap<Address, Fin>) -> BTreeMap<Address, Fin> {
    accounts
        .iter()
        .filter(|(a, f)| norm(f) != pre_norm(c, a) || (!f.deleted && !f.slots.is_empty()))
        .map(|(a, f)| (*a, f.clone()))
        .collect()
}

/// What geth charges per gas: the price of a legacy or access-list tx, `min(feeCap, tip + baseFee)`
/// of a dynamic-fee tx.
fn effective_gas_price(c: &Case) -> U256 {
    let price = u256(&c.tx.gas_price);
    if c.tx.ty != 2 {
        return price;
    }
    let base = c.block.base_fee.as_deref().map_or(U256::ZERO, u256);
    price.min(base + u256(c.tx.max_priority_fee.as_deref().unwrap_or("0x0")))
}

/// The first field in which xone-evm and the archive differ, or `None`.
pub fn compare(c: &Case, o: &Outcome) -> Option<String> {
    let r = &c.receipt;
    if o.gas_used != r.gas_used {
        return Some(format!(
            "gasUsed: xone-evm {}, archive {}",
            o.gas_used, r.gas_used
        ));
    }
    if o.success != (r.status == 1) {
        return Some(format!(
            "status: xone-evm {}, archive {}",
            o.success as u8, r.status
        ));
    }
    let created = match (&c.tx.to, o.created) {
        (None, Some(a)) => Some(a),
        (None, None) => Some(addr(&c.tx.from).create(c.tx.nonce)),
        _ => None,
    };
    if created != r.contract_address.as_deref().map(addr) {
        return Some(format!(
            "contractAddress: xone-evm {created:?}, archive {:?}",
            r.contract_address
        ));
    }
    if o.logs.len() != r.logs.len() {
        return Some(format!(
            "logs: xone-evm {}, archive {}",
            o.logs.len(),
            r.logs.len()
        ));
    }
    for (i, (m, a)) in o.logs.iter().zip(&r.logs).enumerate() {
        if m.0 != addr(&a.address) {
            return Some(format!(
                "log {i} address: xone-evm {}, archive {}",
                m.0, a.address
            ));
        }
        let topics: Vec<B256> = a
            .topics
            .iter()
            .map(|t| B256::from_str(t).unwrap_or_default())
            .collect();
        if m.1 != topics {
            return Some(format!(
                "log {i} topics: xone-evm {:?}, archive {:?}",
                m.1, a.topics
            ));
        }
        if m.2 != bytes(&a.data).to_vec() {
            return Some(format!("log {i} data differs"));
        }
    }
    let (mut mine, theirs) = (changed(c, &o.accounts), changed(c, &archive(c)));
    // The coinbase owner is not in `prestateTracer`'s lookups (it traces the coinbase), so its fee
    // credit is not in the diff. Check it against the receipt instead of skipping it.
    let spec = Network::from_str(&c.network).map(|n| n.spec());
    let to_owner =
        spec.is_ok_and(|s| xone_evm::spec::rules(&s, c.block.number).is_ok_and(|r| r.fee_to_owner));
    if let (true, Some(owner)) = (
        to_owner,
        c.owner.as_deref().map(addr).filter(|a| !a.is_zero()),
    ) {
        if !theirs.contains_key(&owner) {
            if let Some(m) = mine.remove(&owner) {
                let (b, n, h) = pre_norm(c, &owner);
                let fee = U256::from(c.receipt.gas_used) * effective_gas_price(c);
                if (m.balance, m.nonce, m.code_hash) != (b + fee, n, h)
                    || !m.slots.is_empty()
                    || m.deleted
                {
                    return Some(format!(
                        "fee owner {owner}: xone-evm balance {} (deleted {}), receipt says {}",
                        m.balance,
                        m.deleted,
                        b + fee
                    ));
                }
            }
        }
    }
    for a in mine
        .keys()
        .chain(theirs.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        let (Some(m), Some(t)) = (mine.get(a), theirs.get(a)) else {
            return Some(match mine.get(a) {
                Some(_) => format!("account {a}: changed by xone-evm, not in the archive diff"),
                None => format!("account {a}: in the archive diff, not changed by xone-evm"),
            });
        };
        if m.deleted != t.deleted {
            return Some(format!(
                "account {a} deleted: xone-evm {}, archive {}",
                m.deleted, t.deleted
            ));
        }
        if m.deleted {
            continue;
        }
        if m.balance != t.balance {
            return Some(format!(
                "balance of {a}: xone-evm {}, archive {}",
                m.balance, t.balance
            ));
        }
        if m.nonce != t.nonce {
            return Some(format!(
                "nonce of {a}: xone-evm {}, archive {}",
                m.nonce, t.nonce
            ));
        }
        if m.code_hash != t.code_hash {
            return Some(format!(
                "code of {a}: xone-evm {}, archive {}",
                m.code_hash, t.code_hash
            ));
        }
        for k in m
            .slots
            .keys()
            .chain(t.slots.keys())
            .collect::<std::collections::BTreeSet<_>>()
        {
            let (x, y) = (m.slots.get(k), t.slots.get(k));
            if x != y {
                return Some(format!("slot {k:#x} of {a}: xone-evm {x:?}, archive {y:?}"));
            }
        }
    }
    None
}
