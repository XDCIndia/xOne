//! `eth_call` / `eth_estimateGas` arguments: geth `internal/ethapi/transaction_args.go`
//! `TransactionArgs`, decoded with the `hexutil` rules and checked as `CallDefaults` checks them.
//!
//! Every field is typed: a wrong JSON type or a malformed quantity is refused with the text
//! `encoding/json` gives for it (`invalid argument 0: json: cannot unmarshal ... into Go struct field
//! TransactionArgs.<field> of type <type>`), never read as "absent". The nonce, the chain id and the
//! access list are kept for the execution backend. Blob and set-code fields are refused: this
//! node cannot execute those transaction types.

use crate::{
    api::{AccessTuple, CallRequest},
    types::RpcError,
};
use alloy_primitives::{Address, Bytes, B256, U256};
use serde_json::{Map, Value};

const BIG: &str = "*hexutil.Big";
const U64: &str = "hexutil.Uint64";
const BYTES: &str = "hexutil.Bytes";

fn type_error(field: &str, what: &str, ty: &str) -> RpcError {
    RpcError::invalid_params(format!(
        "invalid argument 0: json: cannot unmarshal {what} into Go struct field TransactionArgs.{field} of type {ty}"
    ))
}

/// The string of `field`; `None` when absent or `null` (a Go pointer left nil).
fn text<'a>(o: &'a Map<String, Value>, field: &str, ty: &str) -> Result<Option<&'a str>, RpcError> {
    match o.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(type_error(field, "non-string", ty)),
    }
}

/// hexutil `checkNumberText` and the digit decoding of `Big` / `Uint64`, at most `bits` bits.
fn number(
    o: &Map<String, Value>,
    field: &str,
    ty: &str,
    bits: usize,
) -> Result<Option<U256>, RpcError> {
    let Some(s) = text(o, field, ty)? else {
        return Ok(None);
    };
    let e = |what: &str| type_error(field, what, ty);
    if s.is_empty() {
        return Ok(Some(U256::ZERO)); // hexutil: "empty strings are allowed"
    }
    let raw = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .ok_or_else(|| e("hex string without 0x prefix"))?;
    if raw.is_empty() {
        return Err(e("hex string \"0x\""));
    }
    if raw.len() > 1 && raw.starts_with('0') {
        return Err(e("hex number with leading zero digits"));
    }
    if raw.len() > bits / 4 {
        return Err(e(&format!("hex number > {bits} bits")));
    }
    U256::from_str_radix(raw, 16)
        .map(Some)
        .map_err(|_| e("invalid hex string"))
}

fn quantity(o: &Map<String, Value>, field: &str) -> Result<Option<u64>, RpcError> {
    Ok(number(o, field, U64, 64)?.map(|v| v.to::<u64>()))
}

fn big(o: &Map<String, Value>, field: &str) -> Result<Option<U256>, RpcError> {
    number(o, field, BIG, 256)
}

/// hexutil `Bytes`: `0x`-prefixed, even length; the empty string is empty bytes.
fn bytes(o: &Map<String, Value>, field: &str) -> Result<Option<Bytes>, RpcError> {
    let Some(s) = text(o, field, BYTES)? else {
        return Ok(None);
    };
    if s.is_empty() {
        return Ok(Some(Bytes::new()));
    }
    let e = |what: &str| type_error(field, what, BYTES);
    let raw = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .ok_or_else(|| e("hex string without 0x prefix"))?;
    if raw.len() % 2 != 0 {
        return Err(e("hex string of odd length"));
    }
    alloy_primitives::hex::decode(raw)
        .map(|b| Some(Bytes::from(b)))
        .map_err(|_| e("invalid hex string"))
}

/// hexutil `UnmarshalFixedJSON`: `0x` and exactly `2 * N` digits.
fn fixed<const N: usize>(v: &Value, field: &str, ty: &str) -> Result<[u8; N], RpcError> {
    let Value::String(s) = v else {
        return Err(type_error(field, "non-string", ty));
    };
    let raw = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .ok_or_else(|| type_error(field, "hex string without 0x prefix", ty))?;
    if raw.len() != 2 * N {
        return Err(RpcError::invalid_params(format!(
            "invalid argument 0: hex string has length {}, want {} for {ty}",
            raw.len(),
            2 * N
        )));
    }
    let mut out = [0u8; N];
    alloy_primitives::hex::decode_to_slice(raw, &mut out)
        .map_err(|_| type_error(field, "invalid hex string", ty))?;
    Ok(out)
}

fn address(o: &Map<String, Value>, field: &str) -> Result<Option<Address>, RpcError> {
    match o.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => Ok(Some(Address::from(fixed::<20>(
            v,
            field,
            "common.Address",
        )?))),
    }
}

/// `types.AccessList`: `[{address, storageKeys}]`, both required (`gen_access_tuple.go`).
fn access_list(o: &Map<String, Value>) -> Result<Option<Vec<AccessTuple>>, RpcError> {
    let list = match o.get("accessList") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Array(a)) => a,
        Some(_) => {
            return Err(RpcError::invalid_params(
                "invalid argument 0: json: cannot unmarshal non-array into Go struct field TransactionArgs.accessList of type types.AccessList",
            ))
        }
    };
    let missing = |f: &str| {
        RpcError::invalid_params(format!(
            "invalid argument 0: missing required field '{f}' for AccessTuple"
        ))
    };
    list.iter()
        .map(|t| {
            let t = t.as_object().ok_or_else(|| {
                RpcError::invalid_params(
                    "invalid argument 0: json: cannot unmarshal non-object into Go struct field TransactionArgs.accessList of type types.AccessTuple",
                )
            })?;
            let a = t.get("address").filter(|v| !v.is_null()).ok_or_else(|| missing("address"))?;
            let keys = match t.get("storageKeys") {
                None | Some(Value::Null) => return Err(missing("storageKeys")),
                Some(Value::Array(k)) => k,
                Some(_) => {
                    return Err(RpcError::invalid_params(
                        "invalid argument 0: json: cannot unmarshal non-array into Go struct field AccessTuple.accessList.storageKeys of type []common.Hash",
                    ))
                }
            };
            Ok(AccessTuple {
                address: Address::from(fixed::<20>(a, "accessList.address", "common.Address")?),
                storage_keys: keys
                    .iter()
                    .map(|k| fixed::<32>(k, "accessList.storageKeys", "common.Hash").map(B256::from))
                    .collect::<Result<_, _>>()?,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// Fields of transaction types this node does not execute (EIP-4844 blobs, EIP-7702 set-code).
const UNSUPPORTED: [&str; 6] = [
    "maxFeePerBlobGas",
    "blobVersionedHashes",
    "blobs",
    "commitments",
    "proofs",
    "authorizationList",
];

/// Decode and check the call object. `cap` is `--rpc.gascap` (0: no cap), `chain_id` the node's.
pub fn parse(o: &Map<String, Value>, cap: u64, chain_id: u64) -> Result<CallRequest, RpcError> {
    let from = address(o, "from")?;
    let to = address(o, "to")?;
    let gas = quantity(o, "gas")?;
    let gas_price = big(o, "gasPrice")?;
    let max_fee = big(o, "maxFeePerGas")?;
    let max_priority_fee = big(o, "maxPriorityFeePerGas")?;
    let value = big(o, "value")?;
    let nonce = quantity(o, "nonce")?;
    let data = bytes(o, "data")?;
    let input = bytes(o, "input")?;
    let access_list = access_list(o)?;
    let chain = big(o, "chainId")?;
    if let Some(f) = UNSUPPORTED
        .iter()
        .find(|f| o.get(**f).is_some_and(|v| !v.is_null()))
    {
        return Err(RpcError::invalid_params(format!(
            "{f} is not supported: blob and set-code transactions are not executed by this node"
        )));
    }
    // `CallDefaults`, in its order
    if gas_price.is_some() && (max_fee.is_some() || max_priority_fee.is_some()) {
        return Err(RpcError::server(
            "both gasPrice and (maxFeePerGas or maxPriorityFeePerGas) specified",
        ));
    }
    if let Some(have) = chain {
        if have != U256::from(chain_id) {
            return Err(RpcError::server(format!(
                "chainId does not match node's (have={have}, want={chain_id})"
            )));
        }
    }
    let gas = match gas {
        None if cap == 0 => u64::MAX / 2,
        None => cap,
        Some(g) if cap > 0 && cap < g => cap,
        Some(g) => g,
    };
    Ok(CallRequest {
        from,
        to,
        gas: Some(gas),
        gas_price,
        max_fee,
        max_priority_fee,
        value,
        // `TransactionArgs.data()`: "input" wins; a call does not compare the two (only
        // `setDefaults` of a sent transaction does)
        data: input.or(data).unwrap_or_else(Bytes::new),
        nonce,
        access_list,
        chain_id: chain.map(|_| chain_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn p(v: Value) -> Result<CallRequest, RpcError> {
        parse(v.as_object().unwrap(), 50_000_000, 51)
    }

    fn msg(r: Result<CallRequest, RpcError>) -> String {
        match r {
            Err(e) => e.message,
            Ok(_) => String::new(),
        }
    }

    #[test]
    fn full_envelope_is_kept() {
        let r = p(json!({
            "from": "0x0000000000000000000000000000000000000001",
            "to": "0x0000000000000000000000000000000000000002",
            "gas": "0x5208", "maxFeePerGas": "0x10", "maxPriorityFeePerGas": "0x1",
            "value": "0x0", "nonce": "0x7", "input": "0xab", "data": "0xcd", "chainId": "0x33",
            "accessList": [{"address": "0x0000000000000000000000000000000000000003",
                            "storageKeys": ["0x0000000000000000000000000000000000000000000000000000000000000004"]}]
        }))
        .unwrap();
        assert_eq!(r.gas, Some(21_000));
        assert_eq!(r.nonce, Some(7));
        assert_eq!(r.chain_id, Some(51));
        assert_eq!(r.data.as_ref(), &[0xab]); // input wins
        let al = r.access_list.unwrap();
        assert_eq!(al[0].address, Address::with_last_byte(3));
        assert_eq!(al[0].storage_keys, vec![B256::with_last_byte(4)]);
    }

    #[test]
    fn mistyped_and_malformed_fields_are_refused() {
        assert_eq!(
            msg(p(json!({"gas": 21000}))),
            "invalid argument 0: json: cannot unmarshal non-string into Go struct field TransactionArgs.gas of type hexutil.Uint64"
        );
        assert_eq!(
            msg(p(json!({"value": "0x01"}))),
            "invalid argument 0: json: cannot unmarshal hex number with leading zero digits into Go struct field TransactionArgs.value of type *hexutil.Big"
        );
        assert_eq!(
            msg(p(json!({"nonce": "0x10000000000000000"}))),
            "invalid argument 0: json: cannot unmarshal hex number > 64 bits into Go struct field TransactionArgs.nonce of type hexutil.Uint64"
        );
        assert_eq!(
            msg(p(json!({"data": "0xabc"}))),
            "invalid argument 0: json: cannot unmarshal hex string of odd length into Go struct field TransactionArgs.data of type hexutil.Bytes"
        );
        assert_eq!(
            msg(p(json!({"to": "0x12"}))),
            "invalid argument 0: hex string has length 2, want 40 for common.Address"
        );
        assert!(p(json!({"gasPrice": "12"})).is_err());
        assert!(p(
            json!({"accessList": [{"address": "0x0000000000000000000000000000000000000003"}]})
        )
        .is_err());
        assert!(p(json!({"accessList": {}})).is_err());
        // null is "not set"
        assert!(p(json!({"to": null, "gas": null, "accessList": null})).is_ok());
    }

    #[test]
    fn call_defaults_rules() {
        assert_eq!(
            msg(p(json!({"gasPrice": "0x1", "maxFeePerGas": "0x2"}))),
            "both gasPrice and (maxFeePerGas or maxPriorityFeePerGas) specified"
        );
        assert_eq!(
            msg(p(json!({"chainId": "0x32"}))),
            "chainId does not match node's (have=50, want=51)"
        );
        assert!(msg(p(json!({"authorizationList": []})))
            .starts_with("authorizationList is not supported"));
        assert!(p(json!({"blobVersionedHashes": null})).is_ok());
        // gas: capped, defaulted to the cap; cap 0 is no cap
        assert_eq!(
            p(json!({"gas": "0xffffffffff"})).unwrap().gas,
            Some(50_000_000)
        );
        assert_eq!(p(json!({})).unwrap().gas, Some(50_000_000));
        let o = json!({});
        assert_eq!(
            parse(o.as_object().unwrap(), 0, 51).unwrap().gas,
            Some(u64::MAX / 2)
        );
    }
}
