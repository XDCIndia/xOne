//! Positional argument counts of the served methods, as the pinned geth-XDC oracle declares them
//! (`internal/ethapi/api.go`, `eth/filters/api.go`, `eth/api_debug.go` at `conformance/PIN`).
//! geth `rpc/json.go` `parsePositionalArguments`: more arguments than parameters is
//! "too many arguments, want at most N"; a missing non-pointer parameter is "missing value for
//! required argument N". Methods not listed here are not checked (XDC namespaces whose
//! signatures are pinned elsewhere).

use crate::types::RpcError;

/// `(required, at most)` for `method`.
pub fn arity(method: &str) -> Option<(usize, usize)> {
    Some(match method {
        "web3_clientVersion" | "net_listening" | "net_peerCount" | "net_version"
        | "eth_chainId" | "eth_blockNumber" | "eth_gasPrice" | "eth_syncing" | "eth_mining"
        | "eth_coinbase" | "txpool_content" | "txpool_status" | "txpool_inspect" => (0, 0),
        "eth_getBalance" | "eth_getCode" | "eth_getTransactionCount" => (1, 2),
        "eth_getStorageAt" => (2, 3),
        // `fullTx bool` is not a pointer: required
        "eth_getBlockByNumber" | "eth_getBlockByHash" => (2, 2),
        "eth_getUncleCountByBlockNumber"
        | "eth_getUncleCountByBlockHash"
        | "eth_getBlockTransactionCountByNumber"
        | "eth_getBlockTransactionCountByHash"
        | "eth_getBlockReceipts"
        | "eth_getTransactionByHash"
        | "eth_getTransactionReceipt"
        | "eth_sendRawTransaction"
        | "eth_getLogs"
        | "debug_getRawHeader"
        | "debug_getRawBlock" => (1, 1),
        "eth_getTransactionByBlockNumberAndIndex" | "eth_getTransactionByBlockHashAndIndex" => {
            (2, 2)
        }
        "eth_call" | "eth_estimateGas" => (1, 4),
        // `rewardPercentiles []float64` is not a pointer: required
        "eth_feeHistory" => (3, 3),
        _ => return None,
    })
}

/// The arity check of `method` against `n` positional arguments.
pub fn check(method: &str, n: usize) -> Result<(), RpcError> {
    let Some((min, max)) = arity(method) else {
        return Ok(());
    };
    if n > max {
        return Err(RpcError::invalid_params(format!(
            "too many arguments, want at most {max}"
        )));
    }
    if n < min {
        return Err(RpcError::invalid_params(format!(
            "missing value for required argument {n}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_follow_the_oracle() {
        assert!(check("eth_chainId", 0).is_ok());
        assert!(check("eth_chainId", 1).is_err());
        assert!(check("eth_getBalance", 1).is_ok());
        assert!(check("eth_getBalance", 3).is_err());
        assert_eq!(
            check("eth_getBlockByNumber", 1).map_err(|e| e.to_value()["message"].clone()),
            Err("missing value for required argument 1".into())
        );
        assert!(check("eth_feeHistory", 2).is_err());
        assert!(check("XDPoS_getSigners", 9).is_ok());
    }

    #[test]
    fn a_transaction_index_is_a_hex_quantity_not_a_tag() {
        use crate::eth::parse_index;
        assert_eq!(parse_index("0x0").ok(), Some(0));
        assert_eq!(parse_index("0x1f").ok(), Some(31));
        for bad in [
            "latest", "pending", "earliest", "0", "12", "0x", "0x01", "0xg",
        ] {
            assert!(parse_index(bad).is_err(), "{bad}");
        }
    }
}
