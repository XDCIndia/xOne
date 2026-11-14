//! `newHeads`, `logs` and `newPendingTransactions` (geth `eth/filters/api.go`). The hub fans an event
//! out to the out-queue of each subscribing connection. The queue is bounded (ADR-007 Q13, 1,024
//! messages); a subscriber whose queue is full is dropped, never waited for. There is no replay:
//! events published while a client is disconnected are gone (the documented event gap).

use crate::{
    json::{decode_receipts, h256, header_json, log_json, Loc},
    types::{parse_address, parse_hash, RpcError},
};
use alloy_primitives::{keccak256, Address, Bytes, B256};
use serde_json::{json, Value};
use std::sync::{Mutex, PoisonError};
use tokio::sync::mpsc::{error::TrySendError, Sender};
use xone_primitives::Header;

/// geth `filters.FilterCriteria` for logs: any of `addresses` and, per position, any of the topics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogFilter {
    pub addresses: Vec<Address>,
    pub topics: Vec<Vec<B256>>,
}

impl LogFilter {
    /// Parses `{"address": a | [a], "topics": [null | h | [h]]}`; `xdc` addresses are accepted.
    pub fn parse(o: &serde_json::Map<String, Value>) -> Result<Self, RpcError> {
        let mut f = Self::default();
        match o.get("address") {
            Some(Value::String(s)) => f.addresses.push(parse_address(s)?),
            Some(Value::Array(a)) => {
                for x in a {
                    let s = x
                        .as_str()
                        .ok_or_else(|| RpcError::invalid_params("invalid address"))?;
                    f.addresses.push(parse_address(s)?);
                }
            }
            Some(Value::Null) | None => {}
            Some(_) => return Err(RpcError::invalid_params("invalid addresses in query")),
        }
        if let Some(Value::Array(ts)) = o.get("topics") {
            for t in ts {
                f.topics.push(match t {
                    Value::Null => vec![],
                    Value::String(s) => vec![parse_hash(s)?],
                    Value::Array(a) => a
                        .iter()
                        .map(|x| {
                            parse_hash(
                                x.as_str()
                                    .ok_or_else(|| RpcError::invalid_params("invalid topic"))?,
                            )
                        })
                        .collect::<Result<_, _>>()?,
                    _ => return Err(RpcError::invalid_params("invalid topic(s)")),
                });
            }
        }
        Ok(f)
    }

    /// geth `filterLogs`.
    pub fn matches(&self, address: Address, topics: &[B256]) -> bool {
        if !self.addresses.is_empty() && !self.addresses.contains(&address) {
            return false;
        }
        if self.topics.len() > topics.len() {
            return false;
        }
        self.topics
            .iter()
            .zip(topics)
            .all(|(want, have)| want.is_empty() || want.contains(have))
    }
}

#[derive(Debug, Clone)]
pub enum Kind {
    NewHeads,
    Logs(LogFilter),
    /// geth `NewPendingTransactions(fullTx *bool)`: hashes, or `newRPCPendingTransaction` objects.
    PendingTransactions {
        full: bool,
    },
}

/// A log to publish: the filter fields and the full JSON.
pub struct LogEvent {
    pub address: Address,
    pub topics: Vec<B256>,
    pub json: Value,
}

struct Sub {
    id: String,
    conn: u64,
    kind: Kind,
    tx: Sender<String>,
}

#[derive(Default)]
pub struct Hub {
    subs: Mutex<(u64, Vec<Sub>)>,
}

impl Hub {
    /// Registers a subscription that writes notifications to `tx`; returns its id (a hex quantity).
    pub fn subscribe(&self, conn: u64, kind: Kind, tx: Sender<String>) -> String {
        let mut g = self.subs.lock().unwrap_or_else(PoisonError::into_inner);
        g.0 += 1;
        let id = format!("{:#034x}", u128::from(g.0) << 64 | u128::from(conn));
        g.1.push(Sub {
            id: id.clone(),
            conn,
            kind,
            tx,
        });
        id
    }

    pub fn unsubscribe(&self, conn: u64, id: &str) -> bool {
        let mut g = self.subs.lock().unwrap_or_else(PoisonError::into_inner);
        let n = g.1.len();
        g.1.retain(|s| !(s.conn == conn && s.id == id));
        g.1.len() != n
    }

    /// A closed connection cancels all of its subscriptions: no worker or filter outlives it.
    pub fn drop_conn(&self, conn: u64) {
        self.subs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .retain(|s| s.conn != conn);
    }

    pub fn len(&self) -> usize {
        self.subs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Sends `result` to every subscription `pick` selects. A full queue drops the subscription and
    /// its id is returned (the slow consumers); a closed queue is removed silently.
    fn fan(&self, pick: impl Fn(&Kind) -> Option<Value>) -> Vec<String> {
        let mut dropped = vec![];
        let mut g = self.subs.lock().unwrap_or_else(PoisonError::into_inner);
        g.1.retain(|s| {
            let Some(result) = pick(&s.kind) else { return true };
            let msg = json!({"jsonrpc": "2.0", "method": "eth_subscription", "params": {"subscription": s.id, "result": result}});
            match s.tx.try_send(msg.to_string()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    dropped.push(s.id.clone());
                    false
                }
                Err(TrySendError::Closed(_)) => false,
            }
        });
        dropped
    }

    /// `header` is the `RPCMarshalHeader` object, validators and penalties included.
    pub fn publish_head(&self, header: &Value) -> Vec<String> {
        self.fan(|k| matches!(k, Kind::NewHeads).then(|| header.clone()))
    }

    /// `removed` is true for the logs of blocks a reorg took off the canonical chain; they are sent
    /// before the logs of the new branch (geth `filters.go` `rmLogsCh` before `logsCh`).
    pub fn publish_logs(&self, logs: &[LogEvent], removed: bool) -> Vec<String> {
        let mut dropped = vec![];
        for l in logs {
            let mut j = l.json.clone();
            j["removed"] = json!(removed);
            dropped.extend(self.fan(|k| match k {
                Kind::Logs(f) if f.matches(l.address, &l.topics) => Some(j.clone()),
                _ => None,
            }));
        }
        dropped
    }

    /// One block that became canonical (geth `handleChainEvent`, `eth/filters/filter_system.go:453`,
    /// and the `logsFeed` of `core/blockchain.go:2447-2454`): its header to `newHeads`, its logs to
    /// `logs`. With `removed` the block left the canonical chain: only its logs are sent, marked
    /// removed (geth `RemovedLogsEvent`, `core/blockchain.go:3572-3653`). `txs` are the consensus
    /// encodings of the body, `receipts` the stored receipts. Returns the dropped slow subscriptions.
    pub fn publish_block(
        &self,
        header: &Header,
        hash: B256,
        txs: &[Bytes],
        receipts: &[Bytes],
        removed: bool,
    ) -> Result<Vec<String>, RpcError> {
        if self.is_empty() {
            return Ok(vec![]);
        }
        let mut dropped = if removed {
            vec![]
        } else {
            self.publish_head(&Value::Object(header_json(header, hash)))
        };
        let mut logs = vec![];
        let mut idx = 0u64;
        for (i, r) in decode_receipts(receipts)?.iter().enumerate() {
            let at = Loc {
                block: hash,
                number: header.number,
                time: header.timestamp,
                index: u64::try_from(i).unwrap_or(u64::MAX),
                base_fee: header.base_fee,
            };
            let tx = txs
                .get(i)
                .map(keccak256)
                .ok_or_else(|| RpcError::internal("receipt without transaction"))?;
            for l in &r.logs {
                logs.push(LogEvent {
                    address: l.address,
                    topics: l.data.topics().to_vec(),
                    json: log_json(l, &at, tx, at.index, idx, removed),
                });
                idx += 1;
            }
        }
        dropped.extend(self.publish_logs(&logs, removed));
        Ok(dropped)
    }

    /// One transaction the pool made executable (geth `NewTxsEvent`): its hash to every
    /// `newPendingTransactions` subscription, the full object to those that asked for it. `raw` is
    /// the consensus encoding; the object is built once, and only when a subscriber wants it.
    pub fn publish_pending(&self, tx_hash: B256, raw: &[u8], chain_id: u64) -> Vec<String> {
        let wants_full = self
            .subs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .iter()
            .any(|s| matches!(s.kind, Kind::PendingTransactions { full: true }));
        // a transaction the decoder refuses goes out as its hash
        let full = wants_full
            .then(|| crate::json::tx_json(raw, chain_id, None))
            .and_then(Result::ok);
        let hash = h256(tx_hash);
        self.fan(|k| match k {
            Kind::PendingTransactions { full: true } => {
                Some(full.clone().unwrap_or_else(|| hash.clone()))
            }
            Kind::PendingTransactions { full: false } => Some(hash.clone()),
            _ => None,
        })
    }
}

/// The JSON type name `encoding/json` reports in an unmarshal error.
fn go_json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `eth_subscribe` params to a [`Kind`].
pub fn parse_kind(params: &[Value]) -> Result<Kind, RpcError> {
    match params.first().and_then(Value::as_str) {
        Some("newHeads") => Ok(Kind::NewHeads),
        Some("newPendingTransactions") => match params.get(1) {
            None | Some(Value::Null) => Ok(Kind::PendingTransactions { full: false }),
            Some(Value::Bool(full)) => Ok(Kind::PendingTransactions { full: *full }),
            // geth's decoding error for a `*bool` argument
            Some(v) => Err(RpcError::invalid_params(format!(
                "invalid argument 1: json: cannot unmarshal {} into Go value of type bool",
                go_json_kind(v)
            ))),
        },
        Some("logs") => match params.get(1) {
            None | Some(Value::Null) => Ok(Kind::Logs(LogFilter::default())),
            Some(Value::Object(o)) => Ok(Kind::Logs(LogFilter::parse(o)?)),
            Some(_) => Err(RpcError::invalid_params("invalid filter")),
        },
        Some(other) => Err(RpcError::new(
            -32601,
            format!("no \"{other}\" subscription in eth namespace"),
        )),
        None => Err(RpcError::invalid_params(
            "missing value for required argument 0",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[test]
    fn pending_subscriptions_get_hashes_or_full_objects_as_asked() {
        assert!(matches!(
            parse_kind(&[json!("newPendingTransactions")]),
            Ok(Kind::PendingTransactions { full: false })
        ));
        assert!(matches!(
            parse_kind(&[json!("newPendingTransactions"), json!(false)]),
            Ok(Kind::PendingTransactions { full: false })
        ));
        assert!(matches!(
            parse_kind(&[json!("newPendingTransactions"), json!(true)]),
            Ok(Kind::PendingTransactions { full: true })
        ));
        let e = parse_kind(&[json!("newPendingTransactions"), json!("yes")]).unwrap_err();
        assert_eq!(
            e.message,
            "invalid argument 1: json: cannot unmarshal string into Go value of type bool"
        );
        let hub = Hub::default();
        let (htx, mut hrx) = mpsc::channel(4);
        let (ftx, mut frx) = mpsc::channel(4);
        let (xtx, mut xrx) = mpsc::channel(4);
        hub.subscribe(1, Kind::PendingTransactions { full: false }, htx);
        hub.subscribe(2, Kind::PendingTransactions { full: true }, ftx);
        hub.subscribe(3, Kind::NewHeads, xtx);
        // a legacy transfer signed for chain 1 (EIP-155)
        let raw = alloy_primitives::hex::decode(
            "f86c098504a817c800825208943535353535353535353535353535353535353535880de0b6b3a76400008025a028ef61340bd939bc2195fe537567866003e1a15d3c71ff63e1590620aa636276a067cbe9d8997f761aecb703304b3800ccf555c9f3dc64214b297fb1966a3b6d83",
        )
        .unwrap();
        let h = alloy_primitives::keccak256(&raw);
        assert!(hub.publish_pending(h, &raw, 1).is_empty());
        let got = |rx: &mut mpsc::Receiver<String>| -> Value {
            serde_json::from_str::<Value>(&rx.try_recv().unwrap()).unwrap()["params"]["result"]
                .clone()
        };
        assert_eq!(got(&mut hrx), json!(format!("{h:#x}")));
        let full = got(&mut frx);
        assert_eq!(full["hash"], json!(format!("{h:#x}")));
        assert_eq!(full["blockHash"], Value::Null);
        assert_eq!(full["nonce"], json!("0x9"));
        assert!(xrx.try_recv().is_err());
    }

    #[test]
    fn slow_consumer_is_dropped_and_others_keep_receiving() {
        let hub = Hub::default();
        let (slow_tx, mut slow_rx) = mpsc::channel(2);
        let (fast_tx, mut fast_rx) = mpsc::channel(8);
        let slow = hub.subscribe(1, Kind::NewHeads, slow_tx);
        hub.subscribe(2, Kind::NewHeads, fast_tx);
        assert!(hub.publish_head(&json!({"number": "0x1"})).is_empty());
        assert!(hub.publish_head(&json!({"number": "0x2"})).is_empty());
        // The third event finds the slow queue full: its subscription is dropped, not waited for.
        assert_eq!(hub.publish_head(&json!({"number": "0x3"})), vec![slow]);
        assert_eq!(hub.len(), 1);
        assert!(
            slow_rx.try_recv().is_ok() && slow_rx.try_recv().is_ok() && slow_rx.try_recv().is_err()
        );
        for _ in 0..3 {
            assert!(fast_rx.try_recv().is_ok());
        }
    }

    #[test]
    fn log_filter_semantics() {
        let a = Address::repeat_byte(1);
        let (t1, t2) = (B256::repeat_byte(1), B256::repeat_byte(2));
        let f = LogFilter {
            addresses: vec![a],
            topics: vec![vec![], vec![t2]],
        };
        assert!(f.matches(a, &[t1, t2]));
        assert!(!f.matches(a, &[t1]));
        assert!(!f.matches(Address::ZERO, &[t1, t2]));
        assert!(!f.matches(a, &[t1, t1]));
    }

    #[test]
    fn closed_connection_removes_subscriptions_and_removed_flag_is_set() {
        let hub = Hub::default();
        let (tx, mut rx) = mpsc::channel(8);
        hub.subscribe(7, Kind::Logs(LogFilter::default()), tx);
        let ev = LogEvent {
            address: Address::ZERO,
            topics: vec![],
            json: json!({"logIndex": "0x0"}),
        };
        hub.publish_logs(&[ev], true);
        let m: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(m["params"]["result"]["removed"], json!(true));
        hub.drop_conn(7);
        assert!(hub.is_empty());
    }
}
