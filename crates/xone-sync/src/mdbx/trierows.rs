//! The trie node rows of `--state.disk`: `TrieNodes` as a [`NodeRows`], the writes of a [`Flush`],
//! and the rebuild of the rows from the flat tables in one streamed pass.

use std::sync::Arc;

use alloy_primitives::{b256, B256};
use xone_db::mdbx::{prefix_end, Env, MdbxError, Wr, T};
use xone_state::{
    mpt::{ACCOUNT_PREFIX, PRUNE_DEPTH, STORAGE_PREFIX},
    Flush, NodeRows, Trie,
};

use super::diskflat::DiskFlat;
use xone_state::FlatRead;

/// The root of an empty trie.
const EMPTY_ROOT_HASH: B256 =
    b256!("56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421");

type R<V> = Result<V, MdbxError>;

/// `Meta` row: the state root that the rows of `TrieNodes` are. Written in the transaction that
/// writes the rows; absent while a rebuild runs.
pub const TRIEROOT_KEY: &[u8] = b"trieroot";

/// Rows of a rebuild that are written in one transaction.
const CHUNK_ROWS: usize = 200_000;
const CHUNK_ACCOUNTS: usize = 100_000;
/// Slots of one account that a rebuild holds before it writes the rows and prunes.
const CHUNK_SLOTS: usize = 200_000;

pub struct MdbxRows(pub Arc<Env>);

impl NodeRows for MdbxRows {
    fn node(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.0.read().ok()?.get(T::TrieNodes, key).ok()?
    }
}

/// Writes `f` into `tx`: the storage tries of deleted accounts, the rows of nodes that went away,
/// then the new rows.
pub fn write_flush(tx: &Wr, f: &Flush) -> R<()> {
    for p in &f.del_prefix {
        let end = prefix_end(p);
        loop {
            let rows = tx.range_limit(T::TrieNodes, p, end.as_deref(), 10_000)?;
            if rows.is_empty() {
                break;
            }
            for (k, _) in rows {
                tx.del(T::TrieNodes, &k)?;
            }
        }
    }
    for k in &f.del {
        tx.del(T::TrieNodes, k)?;
    }
    for (k, v) in &f.put {
        tx.put(T::TrieNodes, k, v)?;
    }
    Ok(())
}

fn commit(env: &Env, f: &mut Flush) -> R<()> {
    let tx = env.write()?;
    write_flush(&tx, f)?;
    tx.commit()?;
    *f = Flush::default();
    Ok(())
}

/// Rebuilds `TrieNodes` from the flat tables behind `flat` and returns the state root. The marker is
/// removed first and not written here: the caller checks the root against the header, then writes it.
/// Memory is bounded: the trie is written and pruned every [`CHUNK_ACCOUNTS`] accounts, and a storage
/// trie every [`CHUNK_SLOTS`] slots (rows go in keys' order, so a finished subtree is not read again).
pub fn rebuild(env: &Arc<Env>, flat: &DiskFlat) -> R<B256> {
    {
        let tx = env.write()?;
        tx.del(T::Meta, TRIEROOT_KEY)?;
        tx.clear(T::TrieNodes)?;
        tx.commit()?;
    }
    let rows: Arc<dyn NodeRows> = Arc::new(MdbxRows(env.clone()));
    let mut accounts = Trie::persisted(rows.clone(), vec![ACCOUNT_PREFIX], EMPTY_ROOT_HASH);
    let mut batch = Flush::default();
    let mut n = 0usize;
    for (ha, mut acc) in flat.hashed_accounts() {
        let mut slots = flat.hashed_slots(ha).peekable();
        if slots.peek().is_some() {
            let prefix = [&[STORAGE_PREFIX][..], ha.as_slice()].concat();
            let mut st = Trie::persisted(rows.clone(), prefix, EMPTY_ROOT_HASH);
            let mut k = 0usize;
            for (hs, v) in slots {
                st.set(hs, Some(alloy_rlp::encode(v)));
                k += 1;
                if k.is_multiple_of(CHUNK_SLOTS) {
                    st.collect(&mut batch);
                    commit(env, &mut batch)?;
                    st.finish(PRUNE_DEPTH);
                }
            }
            acc.storage_root = st.root();
            st.collect(&mut batch);
        }
        accounts.set(ha, Some(acc.rlp()));
        n += 1;
        if n.is_multiple_of(CHUNK_ACCOUNTS) || batch.put.len() >= CHUNK_ROWS {
            accounts.collect(&mut batch);
            commit(env, &mut batch)?;
            accounts.finish(PRUNE_DEPTH);
        }
    }
    let root = accounts.root();
    accounts.collect(&mut batch);
    commit(env, &mut batch)?;
    Ok(root)
}
