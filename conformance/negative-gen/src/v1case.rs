//! The line format of `conformance/vectors/consensus/v1/negative.jsonl` and the function that
//! runs one input through the V1 rule it names. Included by the generator
//! (`src/bin/v1.rs`) and by `crates/xone-consensus/tests/v1_negative.rs`, so both read and run the
//! same shape. One line is a pair made from a real archive block or checkpoint: `accept` is the
//! input as the archive has it, `reject` the same input with one change.

use alloy_primitives::{Address, Bytes};
use serde::{Deserialize, Serialize};
use xone_chainspec::Network;
use xone_consensus::v1::{
    checkpoint::{verify_checkpoint_list, verify_checkpoint_penalties},
    double_validation::check_double_validation,
    switch::check_switch_block,
    turn::{authorize, check_difficulty},
};
use xone_consensus::verify::HeaderError;
use xone_primitives::Header;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    /// `rlp(header)`, its creator and the M2 the checkpoint assigns to that creator.
    DoubleValidation {
        header: Bytes,
        creator: Address,
        assigned: Address,
    },
    #[serde(rename_all = "camelCase")]
    Authorize {
        creator: Address,
        signers: Vec<Address>,
        masternodes: Vec<Address>,
        recent_creator: Option<Address>,
        number: u64,
        epoch: u64,
    },
    /// `want` is what geth's `calcDifficulty` gives; `have` is the header's.
    Difficulty { have: i64, want: i64 },
    CheckpointList {
        listed: Vec<Address>,
        expected: Vec<Address>,
    },
    /// The `Penalties` bytes of a checkpoint header and the list computed for it.
    CheckpointPenalties {
        header: Bytes,
        computed: Vec<Address>,
    },
    /// The Apothem chain spec with its V2 switch block replaced.
    SwitchBlock { switch_block: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Case {
    pub id: String,
    pub rule: String,
    pub recipe: String,
    /// The variant name of the typed error `reject` must return.
    pub expect: String,
    pub accept: Input,
    pub reject: Input,
}

impl Input {
    pub fn run(&self) -> Result<(), HeaderError> {
        match self {
            Input::DoubleValidation {
                header,
                creator,
                assigned,
            } => {
                let h: Header = alloy_rlp::decode_exact(header).unwrap();
                check_double_validation(&h, *creator, || Ok(*assigned)).map(|_| ())
            }
            Input::Authorize {
                creator,
                signers,
                masternodes,
                recent_creator,
                number,
                epoch,
            } => authorize(
                *creator,
                signers,
                masternodes,
                *recent_creator,
                *number,
                *epoch,
            ),
            Input::Difficulty { have, want } => check_difficulty(*have, *want),
            Input::CheckpointList { listed, expected } => verify_checkpoint_list(listed, expected),
            Input::CheckpointPenalties { header, computed } => {
                verify_checkpoint_penalties(header, computed)
            }
            Input::SwitchBlock { switch_block } => {
                let mut spec = Network::Apothem.spec();
                spec.xdpos.v2.as_mut().unwrap().switch_block = *switch_block;
                check_switch_block(&spec.xdpos)
            }
        }
    }
}
