//! Required blocks: a height whose canonical hash a peer or a chain must have (geth
//! `--eth.requiredblocks`). A peer or chain that disagrees is on another branch.

use std::collections::BTreeMap;

use alloy_primitives::{b256, B256};

use crate::{Error, Network};

/// The first canonical Apothem block after the September 2026 reset (geth-XDC `cmd/utils/flags.go`,
/// `apothemResetBlockNum`). Stale v2.8.2 peers still serve the abandoned pre-reset branch.
const APOTHEM: [(u64, B256); 1] = [(
    83_665_617,
    b256!("2432ce6433608954ef6eaf2feba8f9a2fe90f5a913401eb07d5fa2f1bb499c3f"),
)];

impl Network {
    /// The entries every node of this network checks unless the operator replaces them.
    pub fn required_blocks(self) -> &'static [(u64, B256)] {
        match self {
            Self::Apothem => &APOTHEM,
            _ => &[],
        }
    }
}

/// `<n>=<0x hash>[,<n>=<0x hash>...]` as geth reads `--eth.requiredblocks`.
pub fn parse_required_blocks(s: &str) -> Result<Vec<(u64, B256)>, Error> {
    let bad = |v: &str| Error::InvalidValue {
        field: "required block (want <number>=<0x hash>)",
        value: v.to_owned(),
    };
    let mut out = Vec::new();
    for item in s.split(',').map(str::trim).filter(|i| !i.is_empty()) {
        let (n, h) = item.split_once('=').ok_or_else(|| bad(item))?;
        let n = n.trim().parse::<u64>().map_err(|_| bad(item))?;
        let h = h.trim().parse::<B256>().map_err(|_| bad(item))?;
        out.push((n, h));
    }
    Ok(out)
}

/// The built-in entries with the operator's laid over them: an operator entry replaces a built-in
/// one at the same height. Sorted by height.
pub fn merge_required_blocks(builtin: &[(u64, B256)], user: &[(u64, B256)]) -> Vec<(u64, B256)> {
    let mut m: BTreeMap<u64, B256> = builtin.iter().copied().collect();
    m.extend(user.iter().copied());
    m.into_iter().collect()
}

/// A block at a required height with another hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("required block {number}: want {want}, got {got}")]
pub struct RequiredBlockMismatch {
    pub number: u64,
    pub want: B256,
    pub got: B256,
}

/// `Err` if `(number, hash)` sits at a required height with another hash.
pub fn check_required(
    required: &[(u64, B256)],
    number: u64,
    hash: B256,
) -> Result<(), RequiredBlockMismatch> {
    match required.iter().find(|(n, _)| *n == number) {
        Some(&(_, want)) if want != hash => Err(RequiredBlockMismatch {
            number,
            want,
            got: hash,
        }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: B256 = B256::repeat_byte(0xaa);
    const B: B256 = B256::repeat_byte(0xbb);

    #[test]
    fn only_apothem_has_a_built_in_anchor() {
        for n in Network::ALL {
            let want = usize::from(n == Network::Apothem);
            assert_eq!(n.required_blocks().len(), want, "{}", n.name());
        }
        assert_eq!(Network::Apothem.required_blocks()[0].0, 83_665_617);
    }

    #[test]
    fn parses_geth_syntax_and_rejects_the_rest() {
        let s = format!("5={A},  7={B} ,");
        assert_eq!(parse_required_blocks(&s).unwrap(), vec![(5, A), (7, B)]);
        assert_eq!(parse_required_blocks("").unwrap(), vec![]);
        for bad in [
            "5",
            "x=0x00",
            &format!("-1={A}"),
            "5=0x12",
            &format!("5:{A}"),
        ] {
            assert!(parse_required_blocks(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_operator_entry_replaces_the_built_in_one_at_its_height() {
        let m = merge_required_blocks(&[(9, A), (3, A)], &[(9, B), (4, B)]);
        assert_eq!(m, vec![(3, A), (4, B), (9, B)]);
    }

    #[test]
    fn a_wrong_hash_at_a_required_height_is_a_typed_error() {
        let r = [(10, A)];
        assert_eq!(check_required(&r, 10, A), Ok(()));
        assert_eq!(check_required(&r, 11, B), Ok(()));
        assert_eq!(
            check_required(&r, 10, B),
            Err(RequiredBlockMismatch {
                number: 10,
                want: A,
                got: B
            })
        );
    }
}
