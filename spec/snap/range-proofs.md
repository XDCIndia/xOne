# snap/1 range proofs: what a range must prove, and when it is refused

## Rule

A range of leaves of the account trie or of a storage trie is accepted only together with the proof that nothing is missing between its origin and its last key. The proof is the nodes along the paths of the two edges. The client never stores a leaf of a range that was not verified against the root, and never accepts a range without its proof, except the one whose leaves alone rebuild the whole trie.

**Terms.** `origin` is the first key of the request; `keys` and `values` are the leaves in the answer (values are the leaf bytes: the full account RLP, or the RLP of the trimmed slot value); `proof` is the list of nodes; `more` says whether the trie has leaves after the last key. All keys are 32-byte hashes, so 64 nibbles.

### What the check does (`proof::verify_range`)

geth reads the two edge paths into a partial trie, unsets everything between them, inserts the leaves and compares the hash (`trie/proof.go:478-590`). xOne reaches the same verdict with alloy-trie's `HashBuilder`:

1. Walk the proof from the root along the paths of `origin` and of the last key. A subtree below the first branch that is not on either path lies entirely left of the range, entirely right of it, or entirely inside; the prefix of its path decides which. A node on a path is decoded and the walk goes on. An extension or a leaf that leaves the path is classified by its whole extent. A node that is on a path and not in the proof is `MissingNode`.
2. Feed the builder, in key order: the hashes of the left subtrees (`add_branch`), the leaves, the hashes of the right subtrees. Subtrees inside the range are skipped: the leaves stand for them.
3. The builder's root must equal the root. `more` is whether any right subtree exists.

A leaf that is missing, added, changed or moved changes the hash, so it is `Mismatch`. The walk uses only nodes that hash back to the root, so a node that is not on a path is ignored: proofs may carry extra nodes and their order does not matter (geth reads the list into a set, `trie/proof.go` `proofToPath`, and accepts both, per the note at `:475-477`).

### Rules, each with the geth line

| Rule | Error | geth |
| --- | --- | --- |
| keys and values have the same count | `Lengths` | `proof.go:479-481` |
| keys strictly increase | `NotIncreasing` | `:489` (the prefix rule at `:492` cannot apply to 32-byte keys) |
| no empty value (a deletion) | `EmptyValue` | `:496` |
| the first key is not below `origin` | `BeforeOrigin` | `:525` |
| an empty proof is accepted only for `origin` zero and leaves that hash to the root; then `more` is false | `NoProof` (origin set), `Mismatch` | `:501-509`; `sync.go:2586-2589` passes no proof only for `origin == 0` |
| no leaves, with a proof: nothing may follow `origin` | `MoreEntries` | `:513-523` |
| one leaf, whether or not `origin` equals its key | no special case: the same walk | `:531-546` |
| the nodes on the edges are in the proof | `MissingNode(hash)` | `proofToPath` `:142` |
| the rebuilt root is the root | `Mismatch` | `:590` |
| a node shape that a trie of 32-byte keys cannot have (a leaf not at depth 64, a branch at depth 64, an embedded node beside the range) | `Shape` | n/a |

`Shape` for an embedded node beside the range: a child node shorter than 32 bytes is embedded in its parent and has no hash to give the builder. With keccak keys that needs about 40 nibbles of shared prefix, which does not occur in the account trie or in a storage trie. Such a proof is refused, not mis-verified.

The check never panics on a proof it is given: the builder's ordering assertions are guarded and the tests flip one bit in a node of a valid proof 2,000 times (`hostile::verify_range_never_panics_on_a_mutated_proof`).

### What the client does with the answer (`client::sync`)

- **AccountRange** (`sync.go:2516-2620`): an empty answer with no proof means the peer does not have this root (`PeerError::Unavailable`; geth marks it stateless, `:2565`). Otherwise each body is read as a slim account, the leaf is `Account::rlp()`, and `verify_range(root, origin, ..)` decides. The next origin is the last key plus one when `more`, and the account range is done otherwise. Proof above 128 nodes is refused (`handlers.go:128`). Every proven leaf must have a legal balance.
- **StorageRanges** (`sync.go:2735-2885`): more sets than accounts is refused (`:2804`). Only the last set can be partial and only it has the proof (`handlers.go:257-290`); the other sets must each hash to their account's storage root with no proof. A proof with no slots means the requested range is empty (`sync.go:2822-2830`): the account is finished when the proof checks. A partial range continues from the last slot plus one, alone, with `origin` and `limit` set. `more` always comes from the proof check, never from the presence of a proof: geth attaches a boundary proof to a last page that was not cut and answers a request past the last slot with no slots and a proof (xOneGo `2daf1e4b`; test `a_storage_request_past_the_last_slot_completes_the_account`).
- **Values.** A slot body must be a canonical RLP integer, not zero, and the verified leaf.
- **ByteCodes** (`sync.go:2635-2735`): each code is matched to the next requested hash it hashes to (the peer skips what it lacks); a code that matches none is refused.
- **Healing** (`GetTrieNodes`, `sync.go:2886-2990`): after the ranges, and after a pivot moved, the tries are walked from the root over the nodes that can be built from the local data plus the fetched ones; each missing node is asked for by path and accepted only if its hash is the one the walk needs; empty strings in the answer are gaps (geth's answer to an absent path). The walk ends when no node is missing; its leaves replace the local data. A moved pivot first reconciles the accounts (an account the new state lacks is dropped with its storage, one whose storage root changed starts over), because a storage request that names an account the server does not have is answered empty by geth (`handlers.go:218-220`). Memory: the final root is streamed (`HashBuilder` over ascending keys, O(depth); xOneGo `16f60a4f`).
- A refused answer is reported to the source (`PeerSource::report`) and the request is retried, up to `max_failures` times in a row. All answers empty gives `SyncError::Unavailable`: replace the pivot with `StateStore::set_root` and run again.

## Tests

`crates/xone-snap/oracle` (Go, pinned) builds an account trie of 40 accounts (9 wide) and a storage trie of 40 slots with the pinned `trie` package, proves ranges with `Trie.Prove`, and records `trie.VerifyRangeProof`'s verdict on each: 25 cases per trie (valid and mutated: truncated proof, proof without its root, one node only, proof reordered, leaves reordered, wrong root, extra leaf, missing leaf, modified value, duplicate key, empty value, partial range with no proof, origin above the first key, the proof of another origin). `tests/vectors.rs` requires the same verdict and the same `more` for all 50 (20 accepted, 30 refused), the same trie roots, and the same proof nodes in the same order.

## Rust

`crates/xone-snap/src/proof.rs`, `mpt.rs` (trie with all nodes, hex-prefix paths, the heal walk), `client.rs`.
