//! The XDC transaction denylist (geth-XDC `common.blacklist`, `common.BlackListHFNumber`).
//!
//! From the denylist block on, a transaction whose sender or recipient is listed is refused by the
//! pool (`core/txpool/legacypool/legacypool.go:640-655`) and makes its block invalid
//! (`core/state_processor.go:218-229`). The activation is the genesis `denylistBlock` when it is
//! set (`eth/backend.go:263-266`, rule A7), else the per-chain `BlackListHFNumber` of
//! `common.CopyXDCConstants` (`common/types.go:218-372`). The 61 addresses are the geth-XDC list at
//! `conformance/PIN` (`common/types.go:142`), equal to XinFinOrg/XDPoSChain
//! `common/constants.shared.go:11-75` at `cb1ef790`.

use alloy_primitives::{address, Address};

use crate::{ChainSpec, ForkBlock};

/// geth-XDC `common.blacklist`, sorted so [`is_denylisted`] can binary-search it.
pub const DENYLIST: [Address; 61] = [
    address!("0000000000000000000000000000000000000011"),
    address!("04dd29ce5c253377a9a3796103ea0d9a9e514153"),
    address!("0579337873c97c4ba051310236ea847f5be41bc0"),
    address!("06869dbd0e3a2ea37ddef832e20fa005c6f0ca39"),
    address!("07048d51d9e6179578a6e3b9ee28cdc183b865e4"),
    address!("0832517654c7b7e36b1ef45d76de70326b09e2c7"),
    address!("09c4f991a41e7ca0645d7dfbfee160b55e562ea4"),
    address!("0d07a6cbbe9fa5c4f154e5623bfe47fb4d857d8e"),
    address!("0dfdcebf80006dc9ab7aae8c216b51c6b6759e86"),
    address!("0e7c48c085b6b0aa7ca6e4cbcc8b9a92dc270eb4"),
    address!("10ba49c1caa97d74b22b3e74493032b180cebe01"),
    address!("10f01a27cf9b29d02ce53497312b96037357a361"),
    address!("175d728b0e0f1facb5822a2e0c03bde93596e324"),
    address!("1d1f909f6600b23ce05004f5500ab98564717996"),
    address!("1f9a78534d61732367cbb43fc6c89266af67c989"),
    address!("206e6508462033ef8425edc6c10789d241d49acb"),
    address!("29a79f00f16900999d61b6e171e44596af4fb5ae"),
    address!("2b373890a28e5e46197fbc04f303bbfdd344056f"),
    address!("2b4b56846eaf05c1fd762b5e1ac802efd0ab871c"),
    address!("3538a544021c07869c16b764424c5987409cba48"),
    address!("3969f86acb733526cd61e3c6e3b4660589f32bc6"),
    address!("3ba623300cf9e48729039b3c9e0dee9b785d636e"),
    address!("402f2cfc9c8942f5e7a12c70c625d07a5d52fe29"),
    address!("492d26d852a0a0a2982bb40ec86fe394488c419e"),
    address!("4b899001d73c7b4ec404a771d37d9be13b8983de"),
    address!("4f3d18136fe2b5665c29bdaf74591fc6625ef427"),
    address!("5248bfb72fd4f234e062d3e9bb76f08643004fcd"),
    address!("5888dc1ceb0ff632713486b9418e59743af0fd20"),
    address!("5ac26105b35ea8935be382863a70281ec7a985e9"),
    address!("652ce195a23035114849f7642b0e06647d13e57a"),
    address!("67615413d7cdadb2c435a946aec713a9a9794d39"),
    address!("693dd49b0ed70f162d733cf20b6c43dc2a2b4d95"),
    address!("741277a8952128d5c2ffe0550f5001e4c8247674"),
    address!("7710e7b7682f26cb5a1202e1cff094fbf7777758"),
    address!("7c3b1fa91df55ff7af0cad9e0399384dc5c6641b"),
    address!("7ec1e48a582475f5f2b7448a86c4ea7a26ea36f8"),
    address!("82e48bc7e2c93d89125428578fb405947764ad7c"),
    address!("85cb320a9007f26b7652c19a2a65db1da2d0016f"),
    address!("8b635ef2e4c8fe21fc2bda027eb5f371d6aa2fc1"),
    address!("8b94db158b5e78a6c032c7e7c9423dec62c8b11c"),
    address!("8bcfb0caf41f0aa1b548cae76dcdd02e33866a1b"),
    address!("a512fa1c735fc3cc635624d591dd9ea1ce339ca5"),
    address!("a8a3ef3dc5d8e36aee76f3671ec501ec31e28254"),
    address!("abfef22b92366d3074676e77ea911ccaabfb64c1"),
    address!("b3157bbc5b401a45d6f60b106728bb82ebaa585b"),
    address!("b835710c9901d5fe940ef1b99ed918902e293e35"),
    address!("c8793633a537938cb49cdbbffd45428f10e45b64"),
    address!("ca14e3c4c78bafb60819a78ff6e6f0f709d2aea7"),
    address!("cb06f949313b46bbf53b8e6b2868a0c260ff9385"),
    address!("cc4df7a32faf3efba32c9688def5ccf9fefe443d"),
    address!("ce5c7635d02dc4e1d6b46c256cae6323be294a32"),
    address!("d4080b289da95f70a586610c38268d8d4cf1e4c4"),
    address!("d575c2611984fcd79513b80ab94f59dc5bab4916"),
    address!("d62358d42afbde095a4ca868581d85f9adcc3d61"),
    address!("e0bec72d1c2a7a7fb0532cdfac44ebab9f6f41ee"),
    address!("e187cf86c2274b1f16e8225a7da9a75aba4f1f5f"),
    address!("e3de67289080f63b0c2612844256a25bb99ac0ad"),
    address!("ed12a519cc15b286920fc15fd86106b3e6a16218"),
    address!("f884e43533f61dc2997c0e19a6eff33481920c00"),
    address!("f9fd1c2b0af0d91b0b6754e55639e3f8478dd04a"),
    address!("fe685f43acc62f92ab01a8da80d76455d39d3cb3"),
];

/// True when `a` is on the denylist (geth `common.IsInBlacklist`). A contract creation has no
/// recipient and is never listed.
pub fn is_denylisted(a: &Address) -> bool {
    DENYLIST.binary_search(a).is_ok()
}

/// geth-XDC `BlackListHFNumber` for a chain whose genesis has no `denylistBlock`
/// (`common.CopyXDCConstants`): mainnet 38,383,838, Apothem 23,779,191, the devnets and the
/// puppeth chain 0, and the mainnet base for every other chain ID.
pub fn default_denylist_block(chain_id: u64) -> u64 {
    match chain_id {
        51 => 23_779_191,
        551 | 5050 | 5551 | 99_999 => 0,
        _ => 38_383_838,
    }
}

impl ChainSpec {
    /// The first block that applies the denylist: the genesis `denylistBlock` row when it is set
    /// (rule A7), else [`default_denylist_block`]. `None` when the row holds geth's far-future
    /// placeholder.
    pub fn denylist_block(&self) -> Option<u64> {
        match self.denylist {
            ForkBlock::At(n) => Some(n),
            ForkBlock::Unscheduled => None,
            ForkBlock::Never => Some(default_denylist_block(self.chain_id)),
        }
    }

    /// The denylist applies to block `n` (geth `blockNumber >= common.BlackListHFNumber`).
    pub fn is_denylist(&self, n: u64) -> bool {
        self.denylist_block().is_some_and(|b| n >= b)
    }

    /// The denylist is active at block `n` and `a` is listed.
    pub fn is_denylisted(&self, n: u64, a: &Address) -> bool {
        self.is_denylist(n) && is_denylisted(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_sorted_unique_and_complete() {
        assert!(DENYLIST.windows(2).all(|w| w[0] < w[1]));
        assert!(is_denylisted(&address!("5248bfb72fd4f234e062d3e9bb76f08643004fcd")));
        assert!(is_denylisted(&address!("0000000000000000000000000000000000000011")));
        assert!(!is_denylisted(&Address::ZERO));
    }

    #[test]
    fn default_blocks_follow_copy_xdc_constants() {
        assert_eq!(default_denylist_block(50), 38_383_838);
        assert_eq!(default_denylist_block(51), 23_779_191);
        assert_eq!(default_denylist_block(551), 0);
        assert_eq!(default_denylist_block(2040), 38_383_838);
    }
}
