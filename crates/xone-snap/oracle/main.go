// Command oracle prints geth-XDC snap/1 vectors as JSON, from the pinned geth-XDC types: the eight
// snap/1 messages (eth/protocols/snap/protocol.go), XDC slim accounts with wide balances
// (core/types/state_account.go), tries built by the pinned trie package, and range-proof cases with
// the verdict of the pinned trie.VerifyRangeProof for each (valid ranges and mutations of them).
// The output is deterministic.
package main

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"math/big"
	"os"
	"sort"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core/rawdb"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/ethereum/go-ethereum/eth/protocols/snap"
	"github.com/ethereum/go-ethereum/rlp"
	"github.com/ethereum/go-ethereum/trie"
	"github.com/ethereum/go-ethereum/trie/trienode"
	"github.com/ethereum/go-ethereum/triedb"
	"github.com/holiman/uint256"
)

func x(b []byte) string { return hex.EncodeToString(b) }

func xs(l [][]byte) []string {
	out := make([]string, len(l))
	for i, b := range l {
		out[i] = x(b)
	}
	return out
}

func must[T any](v T, err error) T {
	if err != nil {
		panic(err)
	}
	return v
}

func enc(v any) string { return x(must(rlp.EncodeToBytes(v))) }

func pow2(n uint) *big.Int { return new(big.Int).Lsh(big.NewInt(1), n) }

func add(a *big.Int, n int64) *big.Int { return new(big.Int).Add(a, big.NewInt(n)) }

func hexBig(s string) *big.Int {
	v, ok := new(big.Int).SetString(s, 16)
	if !ok {
		panic(s)
	}
	return v
}

type kv struct{ k, v []byte }

func sortKV(e []kv) {
	sort.Slice(e, func(i, j int) bool { return bytes.Compare(e[i].k, e[j].k) < 0 })
}

func dec1(h common.Hash) common.Hash {
	for i := 31; i >= 0; i-- {
		h[i]--
		if h[i] != 0xff {
			break
		}
	}
	return h
}

func inc1(h common.Hash) common.Hash {
	for i := 31; i >= 0; i-- {
		h[i]++
		if h[i] != 0 {
			break
		}
	}
	return h
}

func build(entries []kv) (*trie.Trie, common.Hash) {
	tr := trie.NewEmpty(triedb.NewDatabase(rawdb.NewMemoryDatabase(), nil))
	for _, e := range entries {
		tr.MustUpdate(e.k, e.v)
	}
	return tr, tr.Hash()
}

func prove(tr *trie.Trie, keys ...[]byte) [][]byte {
	p := trienode.NewProofSet()
	for _, k := range keys {
		if err := tr.Prove(k, p); err != nil {
			panic(err)
		}
	}
	return p.List()
}

// verify is the check of snap/sync.go OnAccounts (lines 2589-2598) and OnStorage.
func verify(root common.Hash, origin common.Hash, keys, vals, proof [][]byte) (ok bool, more bool, err string) {
	nodes := trienode.ProofList{}
	for _, n := range proof {
		nodes = append(nodes, n)
	}
	firstKey, proofdb := origin[:], nodes.Set()
	if len(nodes) == 0 && origin == (common.Hash{}) {
		more, e := trie.VerifyRangeProof(root, nil, keys, vals, nil)
		if e != nil {
			return false, false, e.Error()
		}
		return true, more, ""
	}
	more, e := trie.VerifyRangeProof(root, firstKey, keys, vals, proofdb)
	if e != nil {
		return false, false, e.Error()
	}
	return true, more, ""
}

type rangeCase struct {
	Trie   string   `json:"trie"`
	Name   string   `json:"name"`
	Root   string   `json:"root"`
	Origin string   `json:"origin"`
	Keys   []string `json:"keys"`
	Values []string `json:"values"`
	Proof  []string `json:"proof"`
	Ok     bool     `json:"ok"`
	More   bool     `json:"more"`
	Err    string   `json:"err"`
}

func mkCase(trname, name string, root common.Hash, origin common.Hash, keys, vals, proof [][]byte) rangeCase {
	ok, more, err := verify(root, origin, keys, vals, proof)
	return rangeCase{trname, name, x(root[:]), x(origin[:]), xs(keys), xs(vals), xs(proof), ok, more, err}
}

func cases(trname string, tr *trie.Trie, root common.Hash, e []kv) []rangeCase {
	n := len(e)
	slice := func(a, b int) ([][]byte, [][]byte) {
		var ks, vs [][]byte
		for _, it := range e[a:b] {
			ks = append(ks, it.k)
			vs = append(vs, it.v)
		}
		return ks, vs
	}
	h := func(i int) common.Hash { return common.BytesToHash(e[i].k) }
	var out []rangeCase
	add := func(name string, origin common.Hash, a, b int, proof [][]byte) {
		ks, vs := slice(a, b)
		out = append(out, mkCase(trname, name, root, origin, ks, vs, proof))
	}
	proofOf := func(origin common.Hash, a, b int) [][]byte {
		if b > a {
			return prove(tr, origin[:], e[b-1].k)
		}
		return prove(tr, origin[:])
	}
	// valid ranges
	add("full_no_proof", common.Hash{}, 0, n, nil)
	add("full_with_proof", common.Hash{}, 0, n, proofOf(common.Hash{}, 0, n))
	add("head", common.Hash{}, 0, 10, proofOf(common.Hash{}, 0, 10))
	add("mid_existing_origin", h(10), 10, 20, proofOf(h(10), 10, 20))
	add("mid_absent_origin", dec1(h(10)), 10, 20, proofOf(dec1(h(10)), 10, 20))
	add("tail", h(n-5), n-5, n, proofOf(h(n-5), n-5, n))
	add("single", h(7), 7, 8, proofOf(h(7), 7, 8))
	add("single_absent_origin", dec1(h(7)), 7, 8, proofOf(dec1(h(7)), 7, 8))
	add("empty_past_end", inc1(h(n-1)), n, n, proofOf(inc1(h(n-1)), n, n))
	add("empty_but_more", inc1(h(3)), 4, 4, proofOf(inc1(h(3)), 4, 4))
	// mutations of mid_absent_origin
	o := dec1(h(10))
	ks, vs := slice(10, 20)
	pr := proofOf(o, 10, 20)
	clone := func(l [][]byte) [][]byte {
		c := make([][]byte, len(l))
		for i := range l {
			c[i] = append([]byte(nil), l[i]...)
		}
		return c
	}
	mut := func(name string, origin common.Hash, r common.Hash, k, v, p [][]byte) {
		out = append(out, mkCase(trname, name, r, origin, k, v, p))
	}
	mut("truncated_proof", o, root, ks, vs, pr[:len(pr)-1])
	mut("proof_without_root", o, root, ks, vs, pr[1:])
	mut("proof_one_node", o, root, ks, vs, pr[:1])
	rev := clone(pr)
	for i, j := 0, len(rev)-1; i < j; i, j = i+1, j-1 {
		rev[i], rev[j] = rev[j], rev[i]
	}
	mut("proof_reordered", o, root, ks, vs, rev)
	k2, v2 := clone(ks), clone(vs)
	k2[2], k2[3] = k2[3], k2[2]
	v2[2], v2[3] = v2[3], v2[2]
	mut("leaves_reordered", o, root, k2, v2, pr)
	bad := root
	bad[0] ^= 1
	mut("wrong_root", o, bad, ks, vs, pr)
	ek, ev := clone(ks), clone(vs)
	fake := inc1(common.BytesToHash(ks[len(ks)-1]))
	ek = append(ek, fake[:])
	ev = append(ev, []byte{0x01})
	mut("extra_leaf", o, root, ek, ev, pr)
	gk, gv := clone(ks), clone(vs)
	gk = append(gk[:4], gk[5:]...)
	gv = append(gv[:4], gv[5:]...)
	mut("missing_leaf", o, root, gk, gv, pr)
	mk, mv := clone(ks), clone(vs)
	mv[3][len(mv[3])-1] ^= 1
	mut("modified_value", o, root, mk, mv, pr)
	dk, dv := clone(ks), clone(vs)
	dk[3] = dk[2]
	mut("duplicate_key", o, root, dk, dv, pr)
	zk, zv := clone(ks), clone(vs)
	zv[2] = []byte{}
	mut("empty_value", o, root, zk, zv, pr)
	mut("partial_no_proof_origin_set", o, root, ks, vs, nil)
	hk, hv := slice(0, 10)
	mut("partial_no_proof_origin_zero", common.Hash{}, root, hk, hv, nil)
	mut("origin_above_first_key", h(12), root, ks, vs, proofOf(h(12), 10, 20))
	mut("proof_of_other_origin", o, root, ks, vs, proofOf(h(30), 30, 31))
	return out
}

func main() {
	pin := "6fc8e8c2088b7f18264f5663036669d04fa7becf"
	hsh := func(tag string, i int) common.Hash { return crypto.Keccak256Hash([]byte(fmt.Sprintf("%s-%d", tag, i))) }

	// ---- wide-balance accounts: full and slim RLP from the pinned types.
	type wide struct {
		Name    string `json:"name"`
		Balance string `json:"balance"`
		Nonce   uint64 `json:"nonce"`
		Root    string `json:"root"`
		Code    string `json:"code"`
		Full    string `json:"full"`
		Slim    string `json:"slim"`
	}
	balances := []struct {
		name string
		v    *big.Int
	}{
		{"zero", big.NewInt(0)},
		{"one", big.NewInt(1)},
		{"2^256-1", add(pow2(256), -1)},
		{"2^256", pow2(256)},
		{"2^256+1", add(pow2(256), 1)},
		{"apothem_2^256+991649269999999999999", func() *big.Int { v, _ := new(big.Int).SetString("991649269999999999999", 10); return new(big.Int).Add(pow2(256), v) }()},
		{"apothem_max_observed", hexBig("2ffffffffffffffffffffffffffffffffffffffe6c1949996b3605008e391759c")},
		{"2^264-1", add(pow2(264), -1)},
		{"2^264", pow2(264)},
	}
	mkAcct := func(nonce uint64, bal *big.Int, root common.Hash, code []byte) types.StateAccount {
		low := new(uint256.Int)
		low.SetFromBig(new(big.Int).And(bal, add(pow2(256), -1)))
		a := types.StateAccount{Nonce: nonce, Balance: low, Root: root, CodeHash: code}
		if bal.Cmp(add(pow2(256), -1)) > 0 {
			a.BigBalance = new(big.Int).Set(bal)
		}
		return a
	}
	var wides []wide
	var entries []kv
	var slims [][]byte
	for i, b := range balances {
		root, code := types.EmptyRootHash, types.EmptyCodeHash.Bytes()
		if i%3 == 1 {
			root, code = hsh("root", i), hsh("code", i).Bytes()
		}
		a := mkAcct(uint64(i), b.v, root, code)
		full := must(rlp.EncodeToBytes(&a))
		slim := types.SlimAccountRLP(a)
		wides = append(wides, wide{b.name, x(b.v.Bytes()), a.Nonce, x(root[:]), x(code), x(full), x(slim)})
		entries = append(entries, kv{hsh("acct", i).Bytes(), full})
		slims = append(slims, slim)
	}
	for i := len(balances); i < 40; i++ {
		root, code := types.EmptyRootHash, types.EmptyCodeHash.Bytes()
		if i%2 == 0 {
			root = hsh("root", i)
		}
		if i%4 == 0 {
			code = hsh("code", i).Bytes()
		}
		a := mkAcct(uint64(i)*7, new(big.Int).Mul(big.NewInt(int64(i)), big.NewInt(1e18)), root, code)
		entries = append(entries, kv{hsh("acct", i).Bytes(), must(rlp.EncodeToBytes(&a))})
		slims = append(slims, types.SlimAccountRLP(a))
	}
	// slims must follow the sorted order of the entries
	idx := make([]int, len(entries))
	for i := range idx {
		idx[i] = i
	}
	sort.Slice(idx, func(a, b int) bool { return bytes.Compare(entries[idx[a]].k, entries[idx[b]].k) < 0 })
	sortedSlims := make([][]byte, len(idx))
	for i, j := range idx {
		sortedSlims[i] = slims[j]
	}
	sortKV(entries)
	atr, aroot := build(entries)

	// ---- storage trie: 40 slots, value = RLP of the trimmed word (as the snapshot stores it).
	var sentries []kv
	for i := 0; i < 40; i++ {
		v := new(big.Int).Add(big.NewInt(int64(i)+1), new(big.Int).Lsh(big.NewInt(int64(i%5)), uint(8*(i%30))))
		sentries = append(sentries, kv{hsh("slot", i).Bytes(), must(rlp.EncodeToBytes(common.TrimLeftZeroes(v.Bytes())))})
	}
	sortKV(sentries)
	str, sroot := build(sentries)

	var rc []rangeCase
	rc = append(rc, cases("account", atr, aroot, entries)...)
	rc = append(rc, cases("storage", str, sroot, sentries)...)

	// ---- messages, from the pinned packet types.
	root := aroot
	var accData []*snap.AccountData
	for i := 3; i < 12; i++ {
		accData = append(accData, &snap.AccountData{Hash: common.BytesToHash(entries[i].k), Body: sortedSlims[i]})
	}
	accProof := prove(atr, entries[3].k, entries[11].k)
	var slotData []*snap.StorageData
	for i := 5; i < 9; i++ {
		slotData = append(slotData, &snap.StorageData{Hash: common.BytesToHash(sentries[i].k), Body: sentries[i].v})
	}
	slotProof := prove(str, sentries[5].k, sentries[8].k)
	code1 := bytes.Repeat([]byte{0x60, 0x01}, 20)
	h1, h2 := crypto.Keccak256Hash(code1), common.Hash{0xaa}
	acc0 := hsh("acct", 0)
	paths := []snap.TrieNodePathSet{
		{{0x00}},
		{{0x1a}},
		{acc0[:], {0x00, 0x99}, {0x3f}},
	}
	// GetTrieNodesPacket.Paths is an rlp.RawList: encode the same bytes through a mirror struct.
	type getTrieNodes struct {
		ID    uint64
		Root  common.Hash
		Paths []snap.TrieNodePathSet
		Bytes uint64
	}
	msgs := map[string]string{
		"GetAccountRange": enc(&snap.GetAccountRangePacket{ID: 1, Root: root, Origin: common.BytesToHash(entries[3].k), Limit: common.MaxHash, Bytes: 524288}),
		"AccountRange":    enc(&snap.AccountRangePacket{ID: 1, Accounts: accData, Proof: accProof}),
		"GetStorageRanges_whole": enc(&snap.GetStorageRangesPacket{ID: 2, Root: root, Accounts: []common.Hash{acc0, hsh("acct", 1)},
			Origin: nil, Limit: nil, Bytes: 65536}),
		"GetStorageRanges_chunk": enc(&snap.GetStorageRangesPacket{ID: 3, Root: root, Accounts: []common.Hash{acc0},
			Origin: sentries[5].k, Limit: common.MaxHash[:], Bytes: 65536}),
		"StorageRanges": enc(&snap.StorageRangesPacket{ID: 3, Slots: [][]*snap.StorageData{slotData}, Proof: slotProof}),
		"StorageRanges_empty": enc(&snap.StorageRangesPacket{ID: 4}),
		"GetByteCodes":  enc(&snap.GetByteCodesPacket{ID: 5, Hashes: []common.Hash{h1, h2}, Bytes: 32768}),
		"ByteCodes":     enc(&snap.ByteCodesPacket{ID: 5, Codes: [][]byte{code1, {}}}),
		"GetTrieNodes":  enc(&getTrieNodes{ID: 6, Root: root, Paths: paths, Bytes: 4096}),
		"TrieNodes":     enc(&snap.TrieNodesPacket{ID: 6, Nodes: [][]byte{accProof[0], accProof[1]}}),
	}
	type msgIn struct {
		AccountHashes []string `json:"account_hashes"`
		AccountBodies []string `json:"account_bodies"`
		AccountProof  []string `json:"account_proof"`
		SlotHashes    []string `json:"slot_hashes"`
		SlotBodies    []string `json:"slot_bodies"`
		SlotProof     []string `json:"slot_proof"`
		Code1         string   `json:"code1"`
		CodeHash1     string   `json:"code_hash1"`
		CodeHash2     string   `json:"code_hash2"`
		Acct0         string   `json:"acct0"`
		Root          string   `json:"root"`
		Origin3       string   `json:"origin3"`
		SlotOrigin5   string   `json:"slot_origin5"`
	}
	in := msgIn{Code1: x(code1), CodeHash1: x(h1[:]), CodeHash2: x(h2[:]), Acct0: x(acc0[:]), Root: x(root[:]),
		Origin3: x(entries[3].k), SlotOrigin5: x(sentries[5].k), AccountProof: xs(accProof), SlotProof: xs(slotProof)}
	for _, a := range accData {
		in.AccountHashes = append(in.AccountHashes, x(a.Hash[:]))
		in.AccountBodies = append(in.AccountBodies, x(a.Body))
	}
	for _, s := range slotData {
		in.SlotHashes = append(in.SlotHashes, x(s.Hash[:]))
		in.SlotBodies = append(in.SlotBodies, x(s.Body))
	}

	var accounts, slots struct {
		Root   string   `json:"root"`
		Keys   []string `json:"keys"`
		Values []string `json:"values"`
		Slim   []string `json:"slim,omitempty"`
	}
	accounts.Root = x(aroot[:])
	for i, e := range entries {
		accounts.Keys = append(accounts.Keys, x(e.k))
		accounts.Values = append(accounts.Values, x(e.v))
		accounts.Slim = append(accounts.Slim, x(sortedSlims[i]))
	}
	slots.Root = x(sroot[:])
	for _, e := range sentries {
		slots.Keys = append(slots.Keys, x(e.k))
		slots.Values = append(slots.Values, x(e.v))
	}

	out := map[string]any{
		"pin":      pin,
		"wide":     wides,
		"accounts": accounts,
		"slots":    slots,
		"inputs":   in,
		"messages": msgs,
		"ranges":   rc,
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", " ")
	if err := enc.Encode(out); err != nil {
		panic(err)
	}
}
