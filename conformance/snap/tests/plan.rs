//! The completeness check against `xone_sync::bootstrap::pivot_plan`: a snap-synced node is not
//! done when the pivot state is verified; it must also hold the headers, bodies, receipts and
//! gap snapshots below the pivot that the first post-pivot block needs.

use alloy_primitives::B256;
use std::collections::BTreeSet;
use xone_chainspec::Network;
use xone_snap::{
    client::{
        finish, sync, Config, History, Incomplete, Kind, Need, PeerError, StateStore, SyncError,
    },
    server::{Limits, Server, Snapshot},
    testkit::{flat_state, Loopback},
};
use xone_state::trie::flat_root;
use xone_sync::bootstrap::{pivot_plan, Anchor, Class, Provenance, FORMAT_VERSION};

fn kind(c: Class) -> Kind {
    match c {
        Class::Header => Kind::Header,
        Class::Body => Kind::Body,
        Class::Receipt => Kind::Receipt,
        Class::GapSnapshot => Kind::GapSnapshot,
        Class::State => Kind::State,
    }
}

fn anchor(number: u64, state_root: B256) -> Anchor {
    Anchor {
        version: FORMAT_VERSION,
        chain_id: 51,
        genesis_hash: B256::ZERO,
        number,
        hash: B256::ZERO,
        state_root,
        created_at: 0,
        max_age: 0,
        provenance: Provenance {
            switch_number: 0,
            switch_hash: B256::ZERO,
            validators: vec![],
        },
        headers: vec![],
    }
}

#[derive(Default)]
struct Held {
    have: BTreeSet<(Kind, u64)>,
    can_fetch: bool,
    asked: Vec<Need>,
}

impl History for Held {
    fn has(&self, k: Kind, n: u64) -> bool {
        self.have.contains(&(k, n))
    }
    fn fetch(&mut self, k: Kind, first: u64, last: u64) -> Result<(), PeerError> {
        self.asked.push(Need {
            kind: k,
            first,
            last,
        });
        if self.can_fetch {
            self.have.extend((first..=last).map(|n| (k, n)));
            Ok(())
        } else {
            Err(PeerError::Unavailable)
        }
    }
}

#[test]
fn a_snap_synced_node_is_complete_only_with_what_the_plan_needs() {
    let spec = Network::Apothem.spec();
    let pivot = 83_600_000;
    let (flat, codes) = flat_state(300);
    let root = flat_root(&flat);
    let plan = pivot_plan(&spec, &anchor(pivot, root)).unwrap();
    let needs: Vec<Need> = plan
        .needs
        .iter()
        .map(|n| Need {
            kind: kind(n.class),
            first: n.first,
            last: n.last,
        })
        .collect();
    let of = |k: Kind| {
        needs
            .iter()
            .filter(move |n| n.kind == k)
            .copied()
            .collect::<Vec<_>>()
    };
    // block pivot + 1 can open an epoch: 4,500 headers, 1,799 bodies, 4 gap snapshots, the state
    assert_eq!(
        (of(Kind::Header)[0].first, of(Kind::Header)[0].last),
        (pivot + 1 - 4500, pivot)
    );
    assert_eq!(
        (of(Kind::Body)[0].first, of(Kind::Body)[0].last),
        (pivot + 1 - 1799, pivot)
    );
    assert_eq!(of(Kind::GapSnapshot).len(), 4);
    assert_eq!(
        of(Kind::State),
        vec![Need {
            kind: Kind::State,
            first: pivot,
            last: pivot
        }]
    );

    let mut store = StateStore::new(root);
    let server = Server::new(Snapshot::from_flat(&flat, codes), Limits::default());
    sync(&mut store, &mut Loopback::new(server), &Config::default()).unwrap();

    // the state is verified, nothing below the pivot is held and nothing can be fetched
    let mut held = Held::default();
    let e = finish(&store, root, &needs, &mut held).unwrap_err();
    let SyncError::Incomplete(Incomplete(missing)) = e else {
        panic!("{e:?}")
    };
    let wanted: Vec<Need> = needs
        .iter()
        .filter(|n| n.kind != Kind::State)
        .copied()
        .collect();
    assert_eq!(
        missing, wanted,
        "every pre-pivot range is refused as missing"
    );
    assert_eq!(held.asked, wanted, "and each was asked for first");
    println!("refused: {} ranges missing ({} headers, {} bodies, {} gap snapshots) with the state verified",
        missing.len(), 4500, 1799, of(Kind::GapSnapshot).len());

    // headers and receipts held, bodies for the last 100 blocks only: the bodies are the gap
    let mut held = Held::default();
    for n in &needs {
        if n.kind != Kind::State && n.kind != Kind::Body {
            held.have.extend((n.first..=n.last).map(|b| (n.kind, b)));
        }
    }
    held.have
        .extend((pivot - 99..=pivot).map(|b| (Kind::Body, b)));
    let e = finish(&store, root, &needs, &mut held).unwrap_err();
    assert_eq!(
        e,
        SyncError::Incomplete(Incomplete(vec![Need {
            kind: Kind::Body,
            first: pivot + 1 - 1799,
            last: pivot - 100
        }]))
    );

    // the same, with a source for bodies: fetched, then complete
    held.can_fetch = true;
    assert_eq!(finish(&store, root, &needs, &mut held), Ok(()));
    assert_eq!(
        held.asked.last(),
        Some(&Need {
            kind: Kind::Body,
            first: pivot + 1 - 1799,
            last: pivot - 100
        })
    );

    // and a pivot whose state is not the verified one is not complete, whatever else is held
    assert!(matches!(
        finish(&store, B256::repeat_byte(5), &needs, &mut held),
        Err(SyncError::Incomplete(Incomplete(v))) if v == vec![Need { kind: Kind::State, first: pivot, last: pivot }]
    ));
}
