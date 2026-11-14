//! Error object, parameters and the input forms geth accepts.

use alloy_primitives::{Address, B256};
use serde_json::{json, Value};
use xone_sync::bootstrap::retention::Missing;

/// A JSON-RPC error object (geth `rpc/errors.go`, `rpc/json.go`).
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

pub type R = Result<Value, RpcError>;

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
    /// geth `errcodeDefault`: any error a handler returns.
    pub fn server(message: impl Into<String>) -> Self {
        Self::new(-32000, message)
    }
    /// `rpc/errors.go:78-82`.
    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            -32601,
            format!("the method {method} does not exist/is not available"),
        )
    }
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(-32602, message)
    }
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(-32600, message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(-32603, message)
    }
    /// ADR-007 section 4: the reply for every quota reject; `data` is the seconds to wait or
    /// `"node busy"` under load shedding.
    pub fn limit(data: Value) -> Self {
        Self {
            code: -32005,
            message: "limit exceeded".into(),
            data: Some(data),
        }
    }
    pub fn to_value(&self) -> Value {
        match &self.data {
            Some(d) => json!({"code": self.code, "message": self.message, "data": d}),
            None => json!({"code": self.code, "message": self.message}),
        }
    }
}

/// The typed unsupported-history error (XB-14) as a geth-like error object: code -32000 like every
/// handler error, the message of geth's pruned-state refusal, the typed facts in `data`.
impl From<Missing> for RpcError {
    fn from(m: Missing) -> Self {
        let Missing::UnsupportedHistory {
            class,
            block,
            floor,
            pivot,
        } = &m;
        Self {
            code: -32000,
            message: format!("unsupported history: {m}"),
            data: Some(
                json!({"class": format!("{class:?}"), "block": block, "firstRetained": floor, "pivot": pivot}),
            ),
        }
    }
}

/// A block selector: a number or a hash (EIP-1898 object or a 32 byte string).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockRef {
    Number(u64),
    Hash(B256),
}

/// A block selector string resolved against the chain (geth-XDC `EthAPIBackend.HeaderByNumber`,
/// `eth/api_backend.go:125-160`): `latest` and `pending` are the head; `finalized` is the latest
/// QC-committed V2 block (`HookCommitBlock` sets it, `eth/backend.go:433-436`), an error before
/// the first commit and never the head; `safe` is never set on an XDPoS chain, so it is an error;
/// `earliest` is 0.
pub fn resolve_tag(s: &str, head: u64, finalized: Option<u64>) -> Result<u64, RpcError> {
    match s {
        "latest" | "pending" => Ok(head),
        "finalized" => finalized.ok_or_else(|| RpcError::server("finalized block not found")),
        "safe" => Err(RpcError::server("safe block not found")),
        "earliest" => Ok(0),
        _ => parse_quantity(s),
    }
}

/// A quantity that also accepts the geth tags, all mapped to `head` (an index, a gas amount or a
/// count, where no block is selected). Block selectors use [`resolve_tag`].
pub fn parse_tag(s: &str, head: u64) -> Result<u64, RpcError> {
    match s {
        "latest" | "pending" | "safe" | "finalized" => Ok(head),
        "earliest" => Ok(0),
        _ => parse_quantity(s),
    }
}

/// `hexutil.Uint64`: `0x` and no leading zero.
pub fn parse_quantity(s: &str) -> Result<u64, RpcError> {
    let h = s
        .strip_prefix("0x")
        .ok_or_else(|| RpcError::invalid_params("hex string without 0x prefix"))?;
    if h.is_empty() {
        return Err(RpcError::invalid_params("hex string \"0x\""));
    }
    if h.len() > 1 && h.starts_with('0') {
        return Err(RpcError::invalid_params(
            "hex number with leading zero digits",
        ));
    }
    u64::from_str_radix(h, 16).map_err(|_| RpcError::invalid_params("hex number > 64 bits"))
}

/// `common.Address.UnmarshalText` (`common/types.go:695-707`): `0x`, `xdc` and `XDC`, case of the
/// hex digits free, exactly 20 bytes.
pub fn parse_address(s: &str) -> Result<Address, RpcError> {
    let h = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .or_else(|| {
            s.get(..3)
                .filter(|p| p.eq_ignore_ascii_case("xdc"))
                .map(|_| &s[3..])
        })
        .ok_or_else(|| RpcError::invalid_params("hex string without 0x prefix"))?;
    if h.len() != 40 {
        return Err(RpcError::invalid_params(format!(
            "invalid argument: hex string has length {}, want 40 for common.Address",
            h.len()
        )));
    }
    h.parse::<Address>()
        .map_err(|_| RpcError::invalid_params("invalid hex string"))
}

pub fn parse_hash(s: &str) -> Result<B256, RpcError> {
    s.parse::<B256>().map_err(|_| {
        RpcError::invalid_params("invalid argument: hex string has wrong length for common.Hash")
    })
}

/// Positional parameters.
pub struct Params<'a> {
    pub v: &'a [Value],
    pub head: u64,
    /// The latest QC-committed block (the `finalized` tag), read with `head`.
    pub finalized: Option<u64>,
}

impl Params<'_> {
    fn at(&self, i: usize) -> Option<&Value> {
        self.v.get(i).filter(|x| !x.is_null())
    }
    fn missing(i: usize) -> RpcError {
        RpcError::invalid_params(format!("missing value for required argument {i}"))
    }
    pub fn str(&self, i: usize) -> Result<&str, RpcError> {
        self.at(i)
            .ok_or_else(|| Self::missing(i))?
            .as_str()
            .ok_or_else(|| {
                RpcError::invalid_params(format!("invalid argument {i}: expected string"))
            })
    }
    pub fn address(&self, i: usize) -> Result<Address, RpcError> {
        parse_address(self.str(i)?)
    }
    pub fn hash(&self, i: usize) -> Result<B256, RpcError> {
        parse_hash(self.str(i)?)
    }
    pub fn bool_or(&self, i: usize, d: bool) -> Result<bool, RpcError> {
        match self.at(i) {
            None => Ok(d),
            Some(v) => v.as_bool().ok_or_else(|| {
                RpcError::invalid_params(format!("invalid argument {i}: expected bool"))
            }),
        }
    }
    pub fn obj(&self, i: usize) -> Result<&serde_json::Map<String, Value>, RpcError> {
        self.at(i)
            .ok_or_else(|| Self::missing(i))?
            .as_object()
            .ok_or_else(|| {
                RpcError::invalid_params(format!("invalid argument {i}: expected object"))
            })
    }
    pub fn opt(&self, i: usize) -> Option<&Value> {
        self.at(i)
    }
    /// [`resolve_tag`] against this request's head and committed block.
    pub fn tag(&self, s: &str) -> Result<u64, RpcError> {
        resolve_tag(s, self.head, self.finalized)
    }
    /// A block number argument; absent or null is `latest` (geth `*rpc.BlockNumber` nil).
    pub fn number_or_latest(&self, i: usize) -> Result<u64, RpcError> {
        match self.at(i) {
            None => Ok(self.head),
            Some(Value::String(s)) => self.tag(s),
            Some(Value::Number(n)) => n
                .as_u64()
                .ok_or_else(|| RpcError::invalid_params("invalid block number")),
            Some(_) => Err(RpcError::invalid_params("invalid block number")),
        }
    }
    /// A required block selector: tag, quantity, hash string or `{blockNumber|blockHash}`.
    pub fn block_ref(&self, i: usize) -> Result<BlockRef, RpcError> {
        match self.at(i).ok_or_else(|| Self::missing(i))? {
            Value::String(s) if s.len() == 66 && s.starts_with("0x") => {
                parse_hash(s).map(BlockRef::Hash)
            }
            Value::String(s) => self.tag(s).map(BlockRef::Number),
            Value::Object(o) => match (o.get("blockHash"), o.get("blockNumber")) {
                (Some(Value::String(h)), _) => parse_hash(h).map(BlockRef::Hash),
                (_, Some(Value::String(n))) => self.tag(n).map(BlockRef::Number),
                _ => Err(RpcError::invalid_params("invalid block selector")),
            },
            _ => Err(RpcError::invalid_params("invalid block selector")),
        }
    }
    /// Like [`Self::block_ref`], `latest` when absent.
    pub fn block_ref_or_latest(&self, i: usize) -> Result<BlockRef, RpcError> {
        if self.at(i).is_none() {
            return Ok(BlockRef::Number(self.head));
        }
        self.block_ref(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_prefixes() {
        let a = "0x0000000000000000000000000000000000000088";
        let want = parse_address(a).unwrap();
        for p in ["xdc", "XDC", "xDc"] {
            assert_eq!(parse_address(&format!("{p}{}", &a[2..])).unwrap(), want);
        }
        assert!(parse_address("0x88").is_err());
        assert!(parse_address("0000000000000000000000000000000000000088").is_err());
    }

    #[test]
    fn quantity_rules() {
        assert_eq!(parse_quantity("0x0").unwrap(), 0);
        assert!(parse_quantity("0x00").is_err());
        assert!(parse_quantity("0x").is_err());
        assert!(parse_quantity("10").is_err());
        assert_eq!(parse_tag("earliest", 9).unwrap(), 0);
        assert_eq!(resolve_tag("latest", 105, Some(102)).unwrap(), 105);
        assert_eq!(resolve_tag("pending", 105, Some(102)).unwrap(), 105);
        assert_eq!(resolve_tag("finalized", 105, Some(102)).unwrap(), 102);
        assert!(resolve_tag("finalized", 105, None).is_err());
        assert!(resolve_tag("safe", 105, Some(102)).is_err());
    }
}
