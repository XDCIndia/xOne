//! Header vectors exported from the archives by `conformance/exporter headers`.
//! Every expected value here comes from the vector file: the archive's block
//! hash and the values pinned geth-XDC computed for the header.

use alloy_primitives::{Address, Bytes, B256, U64};
use alloy_rlp::{encode, Decodable, Encodable, Header as RlpHeader};
use serde_json::Value;
use xone_primitives::{
    BlockInfo, BlockNonce, ExtraFieldsV2, ExtraV2Error, Header, QuorumCert, SealError, SyncInfo,
    Timeout, TimeoutCert, Vote,
};

struct Vector {
    network: &'static str,
    number: u64,
    hash: B256,
    header: Header,
    xone: Value,
}

macro_rules! get {
    ($v:expr, $k:expr) => {
        serde_json::from_value($v[$k].clone()).unwrap_or_else(|e| panic!("{}: {e}", $k))
    };
}

fn qty(v: &Value, k: &str) -> u64 {
    let x: U64 = get!(v, k);
    x.to()
}

fn opt<T>(v: &Value, k: &str, f: impl Fn(&Value, &str) -> T) -> Option<T> {
    v.get(k).map(|_| f(v, k))
}

/// The RPC block object to a header; the field map of geth `gen_header_json.go`.
fn header_from_json(v: &Value) -> Header {
    Header {
        parent_hash: get!(v, "parentHash"),
        uncle_hash: get!(v, "sha3Uncles"),
        coinbase: get!(v, "miner"),
        state_root: get!(v, "stateRoot"),
        tx_root: get!(v, "transactionsRoot"),
        receipt_root: get!(v, "receiptsRoot"),
        bloom: get!(v, "logsBloom"),
        difficulty: get!(v, "difficulty"),
        number: qty(v, "number"),
        gas_limit: qty(v, "gasLimit"),
        gas_used: qty(v, "gasUsed"),
        timestamp: qty(v, "timestamp"),
        extra_data: get!(v, "extraData"),
        mix_digest: get!(v, "mixHash"),
        nonce: BlockNonce(get!(v, "nonce")),
        validators: get!(v, "validators"),
        validator: get!(v, "validator"),
        penalties: get!(v, "penalties"),
        base_fee: opt(v, "baseFeePerGas", |v, k| get!(v, k)),
        withdrawals_hash: opt(v, "withdrawalsRoot", |v, k| get!(v, k)),
        blob_gas_used: opt(v, "blobGasUsed", qty),
        excess_blob_gas: opt(v, "excessBlobGas", qty),
        parent_beacon_root: opt(v, "parentBeaconBlockRoot", |v, k| get!(v, k)),
        requests_hash: opt(v, "requestsHash", |v, k| get!(v, k)),
        block_access_list_hash: opt(v, "blockAccessListHash", |v, k| get!(v, k)),
        slot_number: opt(v, "slotNumber", qty),
    }
}

fn load(network: &'static str) -> Vec<Vector> {
    let path = format!(
        "{}/../../conformance/vectors/headers/{network}.jsonl",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    text.lines()
        .map(|line| {
            let v: Value = serde_json::from_str(line).unwrap();
            let header = header_from_json(&v);
            Vector {
                network,
                number: header.number,
                hash: get!(v, "hash"),
                header,
                xone: v["xone"].clone(),
            }
        })
        .collect()
}

fn all() -> Vec<Vector> {
    let mut v = load("apothem");
    v.extend(load("net2040"));
    v
}

impl Vector {
    fn name(&self) -> String {
        format!("{} #{} {}", self.network, self.number, self.hash)
    }
    fn is_v2(&self) -> bool {
        self.xone["consensus"] == "v2"
    }
    fn oracle(&self, k: &str) -> B256 {
        get!(self.xone, k)
    }
}

fn expect_none(failures: Vec<String>, what: &str) {
    assert!(
        failures.is_empty(),
        "{what}: {} failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn vector_files_are_complete() {
    assert!(load("apothem").len() >= 2000);
    assert!(load("net2040").len() >= 500);
}

#[test]
fn json_to_rlp_to_hash_and_back() {
    let mut failures = vec![];
    for v in all() {
        let rlp = encode(&v.header);
        if alloy_primitives::keccak256(&rlp) != v.hash || v.header.hash() != v.hash {
            failures.push(format!(
                "{}: keccak(encode) is not the canonical hash",
                v.name()
            ));
        }
        match alloy_rlp::decode_exact::<Header>(&rlp) {
            Ok(back) if back == v.header && encode(&back) == rlp => {}
            other => failures.push(format!(
                "{}: decode(rlp) round trip: {:?}",
                v.name(),
                other.map(|_| ())
            )),
        }
    }
    expect_none(failures, "header round trip");
}

#[test]
fn seal_hashes_and_signers_match_geth() {
    let (mut seals, mut v2_sigs, mut signers, mut failures) = (0, 0, 0, vec![]);
    for v in all() {
        let h = &v.header;
        if h.seal_hash() != v.oracle("sealHash") {
            failures.push(format!("{}: seal_hash", v.name()));
        }
        if h.v2_sig_hash() != v.oracle("v2SigHash") {
            failures.push(format!("{}: v2_sig_hash", v.name()));
        }
        if h.hash_no_validator() != v.oracle("hashNoValidator") {
            failures.push(format!("{}: hash_no_validator", v.name()));
        }
        seals += 1;
        v2_sigs += 1;

        let want: Option<Address> = get!(v.xone, "signer");
        let got = if v.is_v2() {
            h.v2_signer()
        } else {
            h.v1_signer()
        };
        match (want, got) {
            (Some(w), Ok(g)) if w == g => {
                signers += 1;
                // geth verifyHeader.go: the V2 validator must be the coinbase.
                if v.is_v2() && h.coinbase != w {
                    failures.push(format!(
                        "{}: signer {w} is not the coinbase {}",
                        v.name(),
                        h.coinbase
                    ));
                }
            }
            (None, Err(SealError::Missing | SealError::Invalid)) => {}
            (w, g) => failures.push(format!("{}: signer geth {w:?} here {g:?}", v.name())),
        }
    }
    expect_none(failures, "seal hash or signer");
    assert!(
        seals >= 1000 && v2_sigs >= 1000 && signers >= 1000,
        "{seals} {v2_sigs} {signers}"
    );
}

#[test]
fn v2_extra_fields_round_trip() {
    let (mut epoch_switches, mut others, mut failures) = (0, 0, vec![]);
    for v in all().iter().filter(|v| v.is_v2()) {
        if v.header.validators.is_empty() {
            others += 1;
        } else {
            epoch_switches += 1;
        }
        match v.header.extra_fields_v2() {
            Ok(x) if x.encode_extra() == v.header.extra_data.as_ref() => {}
            other => failures.push(format!("{}: {:?}", v.name(), other.map(|_| ()))),
        }
    }
    expect_none(failures, "V2 extra fields");
    assert!(
        epoch_switches >= 20 && others >= 500,
        "{epoch_switches} {others}"
    );
}

// ---- negative vectors, built from a real V2 header ----

fn real_extra() -> (Vec<u8>, ExtraFieldsV2) {
    let v = all()
        .into_iter()
        .find(|v| v.is_v2() && !v.header.validators.is_empty())
        .expect("a V2 epoch switch in the vectors");
    let x = v.header.extra_fields_v2().unwrap();
    (v.header.extra_data.to_vec(), x)
}

fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
    let payload: Vec<u8> = items.concat();
    let mut out = vec![];
    RlpHeader {
        list: true,
        payload_length: payload.len(),
    }
    .encode(&mut out);
    out.extend(payload);
    out
}

/// Top-level items of an RLP list, as raw encodings.
fn items(mut rlp: &[u8]) -> Vec<Vec<u8>> {
    let h = RlpHeader::decode(&mut rlp).unwrap();
    assert!(h.list && h.payload_length == rlp.len());
    let mut out = vec![];
    while !rlp.is_empty() {
        let before = rlp;
        let item = RlpHeader::decode(&mut rlp).unwrap();
        let end = before.len() - rlp.len() + item.payload_length;
        out.push(before[..end].to_vec());
        rlp = &before[end..];
    }
    out
}

fn extra_with(parts: &[Vec<u8>]) -> Vec<u8> {
    [&[2u8][..], &rlp_list(parts)].concat()
}

#[test]
fn v2_extra_rejects_wrong_version_and_trailing_bytes() {
    let (extra, x) = real_extra();
    assert_eq!(ExtraFieldsV2::decode_extra(&extra).unwrap(), x);
    assert_eq!(ExtraFieldsV2::decode_extra(&[]), Err(ExtraV2Error::Empty));
    for version in [0u8, 1, 3, 0xff] {
        let mut e = extra.clone();
        e[0] = version;
        assert_eq!(
            ExtraFieldsV2::decode_extra(&e),
            Err(ExtraV2Error::Version(version))
        );
    }
    for n in [1usize, 2, 33] {
        let mut e = extra.clone();
        e.extend(vec![0u8; n]);
        assert_eq!(
            ExtraFieldsV2::decode_extra(&e),
            Err(ExtraV2Error::TrailingBytes(n))
        );
    }
}

#[test]
fn v2_extra_rejects_non_canonical_rlp() {
    let (extra, _) = real_extra();
    let top = items(&extra[1..]); // [round, quorum cert]
    let qc = items(&top[1]); // [block info, signatures, gap number]
    let info = items(&qc[0]); // [hash, round, number]
    let rejected = |e: Vec<u8>| {
        assert!(
            matches!(ExtraFieldsV2::decode_extra(&e), Err(ExtraV2Error::Rlp(_))),
            "{e:02x?}"
        )
    };

    // Leading zero in the outer round, in the block number and in the gap number.
    rejected(extra_with(&[vec![0x82, 0, 5], top[1].clone()]));
    let info_nz = rlp_list(&[info[0].clone(), info[1].clone(), vec![0x82, 0, 5]]);
    rejected(extra_with(&[
        top[0].clone(),
        rlp_list(&[info_nz, qc[1].clone(), qc[2].clone()]),
    ]));
    rejected(extra_with(&[
        top[0].clone(),
        rlp_list(&[qc[0].clone(), qc[1].clone(), vec![0x82, 0, 5]]),
    ]));
    // A single byte below 0x80 written as a one byte string.
    rejected(extra_with(&[vec![0x81, 5], top[1].clone()]));
    // Wrong list length: missing, extra, in the outer list and in the QC.
    rejected(extra_with(&top[..1]));
    rejected(extra_with(&[top[0].clone(), top[1].clone(), vec![0x80]]));
    rejected(extra_with(&[top[0].clone(), rlp_list(&qc[..2])]));
    rejected(extra_with(&[
        top[0].clone(),
        rlp_list(&[qc[0].clone(), qc[1].clone(), qc[2].clone(), vec![0x80]]),
    ]));
    // The quorum cert as a string, and a signature list entry as a list.
    rejected(extra_with(&[top[0].clone(), vec![0x80]]));
    rejected(extra_with(&[
        top[0].clone(),
        rlp_list(&[qc[0].clone(), rlp_list(&[vec![0xc0]]), qc[2].clone()]),
    ]));
}

#[test]
fn v2_messages_round_trip_and_reject_extra_fields() {
    let (_, x) = real_extra();
    let qc: QuorumCert = x.quorum_cert;
    let tc = TimeoutCert {
        round: x.round,
        signatures: qc.signatures.clone(),
        gap_number: qc.gap_number,
    };
    let vote = Vote {
        proposed_block_info: qc.proposed_block_info.clone(),
        signature: Bytes::from_static(&[7; 65]),
        gap_number: 450,
    };
    let timeout = Timeout {
        round: x.round,
        signature: vote.signature.clone(),
        gap_number: 450,
    };
    let sync = SyncInfo {
        highest_quorum_cert: qc.clone(),
        highest_timeout_cert: tc.clone(),
    };

    fn check<T: Encodable + Decodable + PartialEq + std::fmt::Debug>(t: &T) {
        let rlp = encode(t);
        assert_eq!(&alloy_rlp::decode_exact::<T>(&rlp).unwrap(), t);
        let mut more = items(&rlp);
        more.push(vec![0x80]);
        assert!(
            alloy_rlp::decode_exact::<T>(&rlp_list(&more)).is_err(),
            "extra element"
        );
        more.truncate(more.len() - 2);
        assert!(
            alloy_rlp::decode_exact::<T>(&rlp_list(&more)).is_err(),
            "missing element"
        );
    }
    check(&vote);
    check(&timeout);
    check(&qc);
    check(&tc);
    check(&sync);
    check(&BlockInfo {
        hash: B256::ZERO,
        round: 0,
        number: u64::MAX,
    });
}

// ---- header negatives ----

fn net2040_amsterdam() -> Vector {
    all()
        .into_iter()
        .find(|v| v.network == "net2040" && v.header.block_access_list_hash.is_some())
        .expect("an Amsterdam header in the net2040 vectors")
}

#[test]
fn header_rejects_non_canonical_rlp() {
    let v = net2040_amsterdam();
    let good = items(&encode(&v.header));
    assert_eq!(good.len(), 18 + 7, "base fee .. block access list hash");
    let rejected = |parts: &[Vec<u8>]| {
        assert!(
            alloy_rlp::decode_exact::<Header>(&rlp_list(parts)).is_err(),
            "{} items",
            parts.len()
        );
    };
    assert!(alloy_rlp::decode_exact::<Header>(&rlp_list(&good)).is_ok());

    // Wrong list length: the validator item removed, and one item too many.
    let mut short = good.clone();
    short.remove(16);
    rejected(&short);
    let mut long = good.clone();
    long.extend([vec![0x80], vec![0x80]]);
    rejected(&long);
    // An unexpected optional field: a placeholder written after the last real
    // one (here the withdrawals hash slot after the base fee).
    let mut placeholder = good[..19].to_vec();
    placeholder.push(vec![0x80]);
    rejected(&placeholder);
    // Leading zero in the number (item 8) and the gas limit (item 9).
    for at in [8, 9] {
        let mut p = good.clone();
        p[at] = vec![0x82, 0, 1];
        rejected(&p);
    }
    // A trailing byte after the header is not one value.
    let mut tail = encode(&v.header);
    tail.push(0);
    assert!(alloy_rlp::decode_exact::<Header>(&tail).is_err());
}

#[test]
fn header_decodes_without_chain_context() {
    // xOneGo #711: the header format must not depend on a chain id. The decoder
    // has no chain parameter; chain 2040 (not on any allow-list) and Apothem
    // headers go through the same function, and an XDPoS chain with a custom id
    // is checked in xone-chainspec (tests/custom_chain_id.rs).
    let v = net2040_amsterdam();
    let got = Header::decode(&mut encode(&v.header).as_slice()).unwrap();
    assert_eq!(got.hash(), v.hash);
}
