//! Byte-exact check of the eth/100, eth/101 and XDPoS V2 message encodings against `vectors.json`,
//! which `oracle/run.sh` prints from the pinned geth-XDC types. Runs offline.

use alloy_primitives::{Bytes, B256, U256};
use alloy_rlp::{Decodable, Encodable};
use net_spike::{GetHeaders, GetHeadersRid, Status62};
use serde_json::Value;
use xone_primitives::{BlockInfo, QuorumCert, SyncInfo, Timeout, TimeoutCert, Vote};

fn vectors() -> Value {
    serde_json::from_str(include_str!("../vectors.json")).unwrap()
}

/// `common.Hash{b, 0xaa, 0xbb}` in the oracle.
fn h(b: u8) -> B256 {
    let mut x = [0u8; 32];
    x[..3].copy_from_slice(&[b, 0xaa, 0xbb]);
    B256::from(x)
}

fn sig() -> Bytes {
    (0..65u8).collect::<Vec<_>>().into()
}

/// Encode `v`, compare with the geth bytes, decode them back and compare with `v`.
fn check<T: Encodable + Decodable + PartialEq + std::fmt::Debug>(name: &str, v: T) {
    let want = alloy_primitives::hex::decode(vectors()[name].as_str().unwrap()).unwrap();
    assert_eq!(
        alloy_rlp::encode(&v),
        want,
        "{name}: encode differs from geth"
    );
    assert_eq!(
        T::decode(&mut &want[..]).unwrap(),
        v,
        "{name}: decode differs"
    );
}

#[test]
fn status_has_no_fork_id() {
    let s = Status62 {
        version: 101,
        network_id: 5551,
        td: U256::from(1),
        head: h(2),
        genesis: h(3),
    };
    check("status62", s);
    // A status with a sixth element (a fork id, as eth/68+ sends) must be rejected, not ignored.
    let mut six = alloy_rlp::encode(&Status62 {
        version: 101,
        network_id: 5551,
        td: U256::from(1),
        head: h(2),
        genesis: h(3),
    });
    six.extend_from_slice(&[0xc0]);
    six[0] += 1; // list prefix 0xf8 xx: grow the payload length by one byte
    six[1] += 1;
    assert!(Status62::decode(&mut &six[..]).is_err());
}

#[test]
fn headers_requests() {
    let req = GetHeaders {
        origin: 0,
        amount: 1,
        skip: 0,
        reverse: false,
    };
    check("get_headers_bare", req.clone());
    check(
        "get_headers_rid",
        GetHeadersRid {
            request_id: 0x1122334455,
            request: req,
        },
    );
}

fn info() -> BlockInfo {
    BlockInfo {
        hash: h(1),
        round: 7,
        number: 1234,
    }
}

#[test]
fn xdpos2_consensus_messages() {
    check(
        "vote",
        Vote {
            proposed_block_info: info(),
            signature: sig(),
            gap_number: 450,
        },
    );
    check(
        "timeout",
        Timeout {
            round: 9,
            signature: sig(),
            gap_number: 450,
        },
    );
    let qc = QuorumCert {
        proposed_block_info: info(),
        signatures: vec![sig(), sig()[..64].to_vec().into()],
        gap_number: 450,
    };
    let tc = TimeoutCert {
        round: 9,
        signatures: vec![sig()],
        gap_number: 450,
    };
    check(
        "sync_info",
        SyncInfo {
            highest_quorum_cert: qc,
            highest_timeout_cert: tc,
        },
    );
}
