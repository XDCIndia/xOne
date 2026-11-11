//! One deliberately wrong rule per named regression. A named case must replay clean under the real
//! rules and diverge from the archive under its mutant; the sweep only keeps a case that does.

use revm::primitives::hardfork::SpecId;
use xone_evm::spec::XdcRules;

pub type Mutant = fn(&mut XdcRules);

/// The EIP-1559 bundle applied from the London marker window on: Berlin pricing, EIP-3860, 3541.
fn bundle_early(r: &mut XdcRules) {
    r.eip1559_bundle = true;
    r.spec_id = SpecId::SHANGHAI;
    r.marker_opcodes = false;
    r.max_refund_quotient = 5;
}

/// Name prefix of a named case, the mutant, and what the mutant breaks.
pub const MUTANTS: [(&str, Mutant, &str); 9] = [
    (
        "wedge-85197568",
        |r| r.chain_id = 0,
        "CHAINID returns 0 (xOneGo 932d73d2)",
    ),
    (
        "shr-35.4M",
        |r| r.spec_id = SpecId::BYZANTIUM,
        "Istanbul rules keyed on the istanbul row, not TIPXDCXCancellationFee",
    ),
    (
        "sload-58.8M",
        |r| r.spec_id = SpecId::BYZANTIUM,
        "Istanbul rules keyed on the istanbul row (SLOAD at 200)",
    ),
    (
        "delegatecall-62.4M",
        bundle_early,
        "EIP-2929 call pricing before Eip1559Block",
    ),
    (
        "staticcall-69.4M",
        bundle_early,
        "EIP-2929 call pricing before Eip1559Block",
    ),
    (
        "push0-64.0M",
        |r| r.marker_opcodes = false,
        "no PUSH0 between londonBlock and Eip1559Block",
    ),
    (
        "create-62.2M",
        bundle_early,
        "EIP-3860 and EIP-3541 before Eip1559Block",
    ),
    (
        "difficulty-56.4M",
        |r| {
            r.marker_opcodes = true;
            r.prevrandao_is_number_hash = true;
        },
        "PREVRANDAO before londonBlock",
    ),
    (
        "prevrandao-68.7M",
        |r| r.marker_opcodes = false,
        "opcode 0x44 is DIFFICULTY from londonBlock",
    ),
];

pub fn mutant_of(name: &str) -> Option<Mutant> {
    MUTANTS
        .iter()
        .find(|(p, _, _)| name.starts_with(&format!("{p}-")))
        .map(|(_, m, _)| *m)
}

/// Wrong rules that are not tied to one named case: `xone-sweep run <filter> <name>` applies one to
/// the whole sample and the divergences it causes are the evidence that the sweep sees the rule.
pub const GENERAL: [(&str, Mutant); 5] = [
    ("refund-quotient-5", |r| r.max_refund_quotient = 5),
    ("refund-quotient-2", |r| r.max_refund_quotient = 2),
    ("calldata-16", |r| r.non_zero_calldata_gas = 16),
    ("modexp-2565-always", |r| r.modexp_eip2565 = true),
    ("fee-to-coinbase", |r| r.fee_to_owner = false),
];

pub fn general(name: &str) -> Option<Mutant> {
    GENERAL.iter().find(|(n, _)| *n == name).map(|(_, m)| *m)
}
