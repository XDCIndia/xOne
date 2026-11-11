//! `StatusPacket62`: the status of eth/100 and eth/101 (geth-XDC `protocol.go:196-202`,
//! `handshake.go:149-236`). Five fields, no ForkID. Validation checks the network id, the
//! protocol version and the genesis hash, in that order. TD only ranks peers. A ForkID is never
//! required and never rejected (regression for xOneGo #684): a sixth element is skipped unread.

use alloy_primitives::{B256, U256};
use alloy_rlp::{Decodable, RlpEncodable};

#[derive(Debug, Clone, PartialEq, Eq, RlpEncodable)]
pub struct Status62 {
    pub version: u32,
    pub network_id: u64,
    /// "Block number" in the geth comment, real total difficulty on the fleet. Ranking only.
    pub td: U256,
    pub head: B256,
    pub genesis: B256,
}

impl Decodable for Status62 {
    /// Strict on the five fields (canonical integers, exact hash lengths). One extra trailing
    /// element is skipped without inspection: it can only be a ForkID, and no ForkID value may
    /// reject a peer. Geth's own decoder rejects it, so we never send one. Two or more extra
    /// elements are an error.
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let h = alloy_rlp::Header::decode(buf)?;
        if !h.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        if buf.len() < h.payload_length {
            return Err(alloy_rlp::Error::InputTooShort);
        }
        let (mut p, rest) = buf.split_at(h.payload_length);
        *buf = rest;
        let p = &mut p;
        let s = Status62 {
            version: Decodable::decode(p)?,
            network_id: Decodable::decode(p)?,
            td: Decodable::decode(p)?,
            head: Decodable::decode(p)?,
            genesis: Decodable::decode(p)?,
        };
        if !p.is_empty() {
            let fork_id = alloy_rlp::Header::decode(p)?;
            if p.len() < fork_id.payload_length {
                return Err(alloy_rlp::Error::InputTooShort);
            }
            *p = &p[fork_id.payload_length..];
        }
        if !p.is_empty() {
            return Err(alloy_rlp::Error::ListLengthMismatch {
                expected: 0,
                got: p.len(),
            });
        }
        Ok(s)
    }
}

/// What we compare a remote status with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub network_id: u64,
    pub genesis: B256,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StatusError {
    #[error("network id mismatch: {got} (!= {want})")]
    NetworkId { got: u64, want: u64 },
    #[error("protocol version mismatch: {got} (!= {want})")]
    Version { got: u32, want: u64 },
    #[error("genesis mismatch: {got} (!= {want})")]
    Genesis { got: B256, want: B256 },
}

impl Status62 {
    /// `version` is the negotiated eth version. On success returns the ranking value: the TD,
    /// saturated to `u64` as geth does (`handshake.go:206-209`). It is not a block number.
    pub fn validate(&self, local: &Local, version: u64) -> Result<u64, StatusError> {
        if self.network_id != local.network_id {
            return Err(StatusError::NetworkId {
                got: self.network_id,
                want: local.network_id,
            });
        }
        if u64::from(self.version) != version {
            return Err(StatusError::Version {
                got: self.version,
                want: version,
            });
        }
        if self.genesis != local.genesis {
            return Err(StatusError::Genesis {
                got: self.genesis,
                want: local.genesis,
            });
        }
        Ok(u64::try_from(self.td).unwrap_or(u64::MAX))
    }

    pub fn rank(&self) -> u64 {
        u64::try_from(self.td).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> Status62 {
        Status62 {
            version: 101,
            network_id: 5551,
            td: U256::from(7),
            head: B256::repeat_byte(2),
            genesis: B256::repeat_byte(3),
        }
    }

    fn local() -> Local {
        Local {
            network_id: 5551,
            genesis: B256::repeat_byte(3),
        }
    }

    /// The five-field list with `extra` appended as further list elements.
    fn with_extra(extra: &[u8]) -> Vec<u8> {
        let mut b = alloy_rlp::encode(status());
        let payload = b.split_off(2); // `0xf8 len` (payload is 71 bytes)
        let mut body = payload;
        body.extend_from_slice(extra);
        let mut out = vec![0xf8, u8::try_from(body.len()).unwrap()];
        out.extend(body);
        out
    }

    #[test]
    fn validation_order_and_rank() {
        assert_eq!(status().validate(&local(), 101), Ok(7));
        let mut s = status();
        s.network_id = 1;
        s.genesis = B256::ZERO;
        assert_eq!(
            s.validate(&local(), 99),
            Err(StatusError::NetworkId { got: 1, want: 5551 })
        );
        s.network_id = 5551;
        assert_eq!(
            s.validate(&local(), 99),
            Err(StatusError::Version { got: 101, want: 99 })
        );
        assert!(matches!(
            s.validate(&local(), 101),
            Err(StatusError::Genesis { .. })
        ));
        // TD above u64 saturates; it never rejects
        let mut s = status();
        s.td = U256::MAX;
        assert_eq!(s.validate(&local(), 101), Ok(u64::MAX));
    }

    /// xOneGo #684: a peer whose status carries a ForkID, of any value, is not rejected.
    #[test]
    fn fork_id_is_never_required_nor_rejected() {
        assert_eq!(
            Status62::decode(&mut &alloy_rlp::encode(status())[..]).unwrap(),
            status()
        );
        // `[hash4, next]` with the value of a fork we do not know, a zero ForkID, and a mismatching one
        for fork_id in [
            vec![
                0xca, 0x84, 0xde, 0xad, 0xbe, 0xef, 0x84, 0x00, 0x00, 0x00, 0x2a,
            ],
            vec![0xc6, 0x84, 0, 0, 0, 0, 0x80],
            vec![0xc0],
        ] {
            let b = with_extra(&fork_id);
            let s = Status62::decode(&mut &b[..]).unwrap();
            assert_eq!(s, status());
            assert!(s.validate(&local(), 101).is_ok());
        }
        // two extra elements are not a ForkID
        assert!(Status62::decode(&mut &with_extra(&[0xc0, 0xc0])[..]).is_err());
        // we never send one
        assert_eq!(alloy_rlp::encode(status()).len(), 2 + 71);
    }

    #[test]
    fn non_canonical_and_short_status_rejected() {
        let good = alloy_rlp::encode(status());
        // network id 5551 = 0x15af written with a leading zero byte
        let mut bad = good.clone();
        let at = bad
            .windows(3)
            .position(|w| w == [0x82, 0x15, 0xaf])
            .unwrap();
        bad[at] = 0x83;
        bad.insert(at + 1, 0x00);
        bad[1] += 1;
        assert!(Status62::decode(&mut &bad[..]).is_err());
        // four fields
        assert!(Status62::decode(&mut &good[..good.len() - 33]).is_err());
        assert!(Status62::decode(&mut &[0x80][..]).is_err());
    }
}
