//! The sign transaction of a block (`spec/producer/construction.md`): `sign(uint256 number, bytes32 hash)`
//! to the BlockSigners contract at 0x89, sent by the node for every eligible block, also while the node is
//! penalised (it is how a penalised masternode returns).
//!
//! geth `CreateTransactionSign` and `CreateTxSign` (`contracts/utils.go:123-231`, `:233-242`). The
//! transaction costs nothing: gas price 0 (a special transaction is exempt from the price floor and, from
//! `TIPSigning`, runs without the EVM), gas 200,000 declared, value 0. The key signs it only through the
//! safety unit ([`Sealer::sign_tx`]).

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rlp::{Encodable, Header as RlpHeader};
use xone_chainspec::ChainSpec;
use xone_consensus::unit::{Sealer, UnitError};
use xone_evm::route::BLOCK_SIGNERS;

use super::timing::SIGN_FRESHNESS;
use crate::txpool::special::SIGN_SELECTOR;

/// Declared gas of a sign transaction (`contracts/utils.go:239`).
pub const GAS: u64 = 200_000;
/// geth `common.MergeSignRange`, `common/types.go:76`.
pub const MERGE_SIGN_RANGE: u64 = 15;

/// geth `isSignBlock` (`miner/xdpos_worker.go:1184-1186`): every 15th block, and every block before TIP2019.
pub fn is_sign_block(spec: &ChainSpec, number: u64) -> bool {
    number % MERGE_SIGN_RANGE == 0 || !spec.effective.tip2019.is_active(number)
}

/// geth `isSigningTxStale` (`miner/xdpos_worker.go:1207-1209`): a block more than 30 behind the head is not
/// signed (the contract refuses a block older than two epochs, and a refused transaction at the lowest
/// pending nonce jams the sender).
pub fn is_stale(block: u64, head: u64) -> bool {
    head > block && head - block > SIGN_FRESHNESS
}

/// geth `signTxNonce` (`contracts/utils.go:94-100`): the pool nonce, floored at the nonce of the head state.
pub fn nonce(pool_nonce: u64, state_nonce: Option<u64>) -> u64 {
    state_nonce.map_or(pool_nonce, |s| s.max(pool_nonce))
}

/// `sign(uint256,bytes32)`: selector, block number as 32 bytes, block hash (`contracts/utils.go:233-237`).
pub fn calldata(number: u64, hash: B256) -> Bytes {
    let mut d = Vec::with_capacity(68);
    d.extend_from_slice(&SIGN_SELECTOR);
    d.extend_from_slice(&U256::from(number).to_be_bytes::<32>());
    d.extend_from_slice(hash.as_slice());
    d.into()
}

/// A sign transaction before the signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignTx {
    pub nonce: u64,
    pub gas_price: u128,
    pub gas: u64,
    pub to: Address,
    pub value: U256,
    pub data: Bytes,
}

/// `CreateTxSign`: gas price 0, gas 200,000, value 0, to 0x89.
pub fn build(number: u64, hash: B256, nonce: u64) -> SignTx {
    SignTx {
        nonce,
        gas_price: 0,
        gas: GAS,
        to: BLOCK_SIGNERS,
        value: U256::ZERO,
        data: calldata(number, hash),
    }
}

fn payload(t: &SignTx, tail: &[&dyn Fn(&mut Vec<u8>)]) -> Vec<u8> {
    let mut p = Vec::new();
    t.nonce.encode(&mut p);
    t.gas_price.encode(&mut p);
    t.gas.encode(&mut p);
    t.to.encode(&mut p);
    t.value.encode(&mut p);
    t.data.encode(&mut p);
    for f in tail {
        f(&mut p);
    }
    let mut out = Vec::new();
    RlpHeader { list: true, payload_length: p.len() }.encode(&mut out);
    out.extend_from_slice(&p);
    out
}

/// The EIP-155 digest the key signs: the six fields, the chain id and two empty items.
pub fn digest(t: &SignTx, chain_id: u64) -> B256 {
    keccak256(payload(
        t,
        &[&|p| chain_id.encode(p), &|p| 0u8.encode(p), &|p| 0u8.encode(p)],
    ))
}

/// The signed transaction as the pool and a block carry it: a legacy RLP list, `v = 35 + 2 * chain_id + parity`.
/// The signature comes from the safety unit.
pub fn encode_signed<S: Sealer>(
    t: &SignTx,
    chain_id: u64,
    unit: &mut S,
) -> Result<Bytes, UnitError> {
    let sig = unit.sign_tx(&digest(t, chain_id))?;
    let b: &[u8] = &sig.bytes;
    let (rs, parity) = match (b.get(..64), b.get(64)) {
        (Some(rs), Some(v)) => (rs, u64::from(*v & 1)),
        _ => return Err(UnitError::Sign("a signature of 65 bytes was expected".into())),
    };
    let r = U256::from_be_slice(&rs[..32]);
    let s = U256::from_be_slice(&rs[32..]);
    let v = 35 + 2 * chain_id + parity;
    Ok(payload(t, &[&|p| v.encode(p), &|p| r.encode(p), &|p| s.encode(p)]).into())
}

/// The sign transaction of a block, or `None` when none is due: not a sign block, a stale block, or a node
/// that is no candidate. `candidate` is the gate of `maybeBroadcastImportedSignTx`
/// (`miner/xdpos_worker.go:1119-1172`): the whole candidate pool of the epoch, penalised nodes included.
#[allow(clippy::too_many_arguments)]
pub fn emit<S: Sealer>(
    spec: &ChainSpec,
    number: u64,
    hash: B256,
    head: u64,
    candidate: bool,
    pool_nonce: u64,
    state_nonce: Option<u64>,
    unit: &mut S,
) -> Result<Option<Bytes>, UnitError> {
    if !candidate || !is_sign_block(spec, number) || is_stale(number, head) {
        return Ok(None);
    }
    let t = build(number, hash, nonce(pool_nonce, state_nonce));
    encode_signed(&t, spec.chain_id, unit).map(Some)
}
