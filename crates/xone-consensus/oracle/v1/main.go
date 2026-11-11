// Command v1oracle writes the XDPoS V1 vectors of crates/xone-consensus from an
// archive RPC and the pinned geth-XDC (see PIN). Every expected value is read from
// the archive (checkpoint extra data, header penalties, snapshots) or computed by
// the pinned geth (creator, validator, assigned M2); nothing is typed by hand.
//
//	go run . checkpoints -rpc http://127.0.0.1:17105 -out ../../../../conformance/vectors/consensus/v1/checkpoints.jsonl
//	go run . m1m2        -rpc http://127.0.0.1:17105 -out ../../../../conformance/vectors/consensus/v1/m1m2.jsonl
//	go run . snapshot    -rpc http://127.0.0.1:17105 -out ../../../../conformance/vectors/consensus/v1/snapshot.jsonl
//	go run . snapvotes   -out ../../../../conformance/vectors/consensus/v1/snapvotes.jsonl   (no RPC)
//	go run . gosort      -out ../../../../conformance/vectors/consensus/v1/gosort.jsonl   (no RPC)
//
// Requests are batches, at most 5 requests per second. Both commands resume: a
// number already in the output file is skipped.
package main

import (
	"bufio"
	"bytes"
	"crypto/ecdsa"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"math/big"
	"math/rand"
	"net/http"
	"os"
	"sort"
	"strings"
	"time"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/common/hexutil"
	xdcsort "github.com/ethereum/go-ethereum/common/sort"
	"github.com/ethereum/go-ethereum/consensus"
	"github.com/ethereum/go-ethereum/consensus/XDPoS"
	"github.com/ethereum/go-ethereum/core/rawdb"
	"github.com/ethereum/go-ethereum/core/state"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/ethereum/go-ethereum/params"
	"github.com/ethereum/go-ethereum/rlp"
)

const (
	epoch       = 900
	gap         = 450
	switchBlock = 56828700 // Apothem XDPoS.V2.SwitchBlock; the last V1 block, a checkpoint
	validator88 = "0x0000000000000000000000000000000000000088"
	signers89   = "0x0000000000000000000000000000000000000089"
	slotCands   = 8 // slotValidatorMapping["candidates"], core/state/statedb_utils.go:84
	slotState   = 1 // slotValidatorMapping["validatorsState"], statedb_utils.go:77
	slotSigners = 0 // slotBlockSignerMapping["blockSigners"], statedb_utils.go:17
	mergeSign   = 15
)

func fatal(err error) {
	if err != nil {
		fmt.Fprintln(os.Stderr, "v1oracle:", err)
		os.Exit(1)
	}
}

// client is a rate limited batch JSON-RPC client: one HTTP request every 220 ms.
type client struct {
	url  string
	last time.Time
}

func (c *client) batch(method string, params [][]any) ([]json.RawMessage, error) {
	reqs := make([]map[string]any, len(params))
	for i, p := range params {
		reqs[i] = map[string]any{"jsonrpc": "2.0", "id": i, "method": method, "params": p}
	}
	body, _ := json.Marshal(reqs)
	var lastErr error
	for try := 0; try < 6; try++ {
		if d := 220*time.Millisecond - time.Since(c.last); d > 0 {
			time.Sleep(d)
		}
		resp, err := http.Post(c.url, "application/json", bytes.NewReader(body))
		c.last = time.Now()
		if err != nil {
			lastErr = err
			time.Sleep(time.Duration(try+1) * time.Second)
			continue
		}
		raw, err := io.ReadAll(resp.Body)
		resp.Body.Close()
		if err != nil {
			lastErr = err
			time.Sleep(time.Duration(try+1) * time.Second)
			continue
		}
		var answers []struct {
			ID     int             `json:"id"`
			Result json.RawMessage `json:"result"`
			Error  *struct {
				Message string `json:"message"`
			} `json:"error"`
		}
		if err := json.Unmarshal(raw, &answers); err != nil {
			lastErr = fmt.Errorf("%s: answer %.200s", method, raw)
			time.Sleep(time.Duration(try+1) * time.Second)
			continue
		}
		out := make([]json.RawMessage, len(reqs))
		for _, a := range answers {
			if a.Error != nil {
				return nil, fmt.Errorf("%s %v: %s", method, params[a.ID], a.Error.Message)
			}
			out[a.ID] = a.Result
		}
		return out, nil
	}
	return nil, lastErr
}

func (c *client) rawHeaders(nums []uint64) (map[uint64]*types.Header, error) {
	out := make(map[uint64]*types.Header, len(nums))
	for i := 0; i < len(nums); i += 250 {
		chunk := nums[i:min(i+250, len(nums))]
		ps := make([][]any, len(chunk))
		for j, n := range chunk {
			ps[j] = []any{fmt.Sprintf("0x%x", n)}
		}
		answers, err := c.batch("debug_getRawHeader", ps)
		if err != nil {
			return nil, err
		}
		for j, a := range answers {
			var b hexutil.Bytes
			if err := json.Unmarshal(a, &b); err != nil || len(b) == 0 {
				return nil, fmt.Errorf("header %d: %.100s", chunk[j], a)
			}
			var h types.Header
			if err := rlp.DecodeBytes(b, &h); err != nil {
				return nil, fmt.Errorf("header %d: %v", chunk[j], err)
			}
			if h.Number.Uint64() != chunk[j] {
				return nil, fmt.Errorf("header %d: got number %v", chunk[j], h.Number)
			}
			out[chunk[j]] = &h
		}
	}
	return out, nil
}

// storage reads 32 byte words of one contract at the state after block `at`.
func (c *client) storage(addr string, slots []common.Hash, at uint64) ([]common.Hash, error) {
	out := make([]common.Hash, 0, len(slots))
	for i := 0; i < len(slots); i += 250 {
		chunk := slots[i:min(i+250, len(slots))]
		ps := make([][]any, len(chunk))
		for j, s := range chunk {
			ps[j] = []any{addr, s.Hex(), fmt.Sprintf("0x%x", at)}
		}
		answers, err := c.batch("eth_getStorageAt", ps)
		if err != nil {
			return nil, err
		}
		for _, a := range answers {
			var s string
			fatal(json.Unmarshal(a, &s))
			out = append(out, common.HexToHash(s))
		}
	}
	return out, nil
}

func hexes(as []common.Address) []string {
	out := make([]string, len(as))
	for i, a := range as {
		out[i] = strings.ToLower(a.Hex())
	}
	return out
}

// addressesIn is geth XDPoS extractAddressFromBytes (xdpos.go:2720) of a checkpoint's
// extra data between vanity and seal (GetMasternodesFromCheckpointHeader, xdpos.go:2123).
func extraMasternodes(h *types.Header) []common.Address {
	n := (len(h.Extra) - 32 - 65) / common.AddressLength
	out := make([]common.Address, n)
	for i := range out {
		copy(out[i][:], h.Extra[32+i*common.AddressLength:])
	}
	return out
}

func penaltiesOf(h *types.Header) []common.Address {
	if len(h.Penalties)%common.AddressLength != 0 {
		fatal(fmt.Errorf("header %v: penalties of %d bytes", h.Number, len(h.Penalties)))
	}
	out := make([]common.Address, len(h.Penalties)/common.AddressLength)
	for i := range out {
		copy(out[i][:], h.Penalties[i*common.AddressLength:])
	}
	return out
}

type numberPenalties struct {
	Number    uint64   `json:"number"`
	Penalties []string `json:"penalties"`
}

type word [2]string // slot, value

type cpRecord struct {
	Number uint64 `json:"number"`
	Hash   string `json:"hash"`
	// What the archive's header carries.
	Extra     []string `json:"extra"`
	Penalties []string `json:"penalties"`
	// The geth predicates at this block.
	TIPSigning   bool `json:"tipSigning"`
	TIPRandomize bool `json:"tipRandomize"`
	// Inputs of the derivation.
	PrevMasternodes []string          `json:"prevMasternodes"`
	PrevPenalties   []numberPenalties `json:"prevPenalties"`
	Comeback        *numberPenalties  `json:"comeback"`
	GapNumber       uint64            `json:"gapNumber"`
	CandidateWords  []word            `json:"candidateWords"`
	SnapshotGap     []string          `json:"snapshotGap"`
	SignerBlocks    []numberHash      `json:"signerBlocks,omitempty"`
	SignerWords     []word            `json:"signerWords,omitempty"`
	Creators        [][2]any          `json:"creators,omitempty"`
}

type numberHash struct {
	Number uint64 `json:"number"`
	Hash   string `json:"hash"`
}

func words(slots, vals []common.Hash) []word {
	out := make([]word, len(slots))
	for i := range slots {
		out[i] = word{slots[i].Hex(), vals[i].Hex()}
	}
	return out
}

func checkpoint(c *client, cfg *params.ChainConfig, eng *XDPoS.XDPoS, n uint64) cpRecord {
	rec := cpRecord{Number: n, GapNumber: n - gap, PrevPenalties: []numberPenalties{}}
	num := new(big.Int).SetUint64(n)
	rec.TIPSigning, rec.TIPRandomize = cfg.IsTIPSigning(num), cfg.IsTIPRandomize(num)

	// Headers: the checkpoint, the previous one, the four whose penalties are removed
	// (xdpos.go:1468-1472) and the one five epochs back (hooks.go:200-203).
	want := []uint64{n}
	for i := uint64(1); i <= 5; i++ {
		if n > i*epoch || (i == 1 && n >= epoch) {
			want = append(want, n-i*epoch)
		}
	}
	hs, err := c.rawHeaders(want)
	fatal(err)
	h := hs[n]
	rec.Hash = h.Hash().Hex()
	rec.Extra = hexes(extraMasternodes(h))
	rec.Penalties = hexes(penaltiesOf(h))
	rec.PrevMasternodes = hexes(extraMasternodes(hs[n-epoch]))
	for i := uint64(1); i <= 4; i++ {
		if n > i*epoch {
			rec.PrevPenalties = append(rec.PrevPenalties, numberPenalties{n - i*epoch, hexes(penaltiesOf(hs[n-i*epoch]))})
		}
	}
	if n > 5*epoch {
		rec.Comeback = &numberPenalties{n - 5*epoch, hexes(penaltiesOf(hs[n-5*epoch]))}
	}

	// The snapshot of the gap block and the 0x88 candidates and caps in the state after it.
	snap, err := c.batch("XDPoS_getSnapshot", [][]any{{fmt.Sprintf("0x%x", rec.GapNumber)}})
	fatal(err)
	var s struct {
		Signers map[string]json.RawMessage `json:"signers"`
	}
	fatal(json.Unmarshal(snap[0], &s))
	for a := range s.Signers {
		rec.SnapshotGap = append(rec.SnapshotGap, strings.ToLower(a))
	}
	sort.Strings(rec.SnapshotGap)

	lenSlot := common.BigToHash(big.NewInt(slotCands))
	lv, err := c.storage(validator88, []common.Hash{lenSlot}, rec.GapNumber)
	fatal(err)
	count := lv[0].Big().Uint64()
	slots := []common.Hash{lenSlot}
	for i := uint64(0); i < count; i++ {
		slots = append(slots, state.GetLocDynamicArrAtElement(lenSlot, i, 1))
	}
	vals, err := c.storage(validator88, slots[1:], rec.GapNumber)
	fatal(err)
	vals = append([]common.Hash{lv[0]}, vals...)
	var capSlots []common.Hash
	for _, v := range vals[1:] {
		if v == (common.Hash{}) { // geth GetCandidates skips an empty entry
			continue
		}
		loc := state.GetLocMappingAtKey(common.BytesToHash(common.HexToAddress(v.Hex()).Bytes()), slotState)
		capSlots = append(capSlots, common.BigToHash(loc.Add(loc, big.NewInt(1))))
	}
	capVals, err := c.storage(validator88, capSlots, rec.GapNumber)
	fatal(err)
	rec.CandidateWords = append(words(slots, vals), words(capSlots, capVals)...)

	switch {
	case !rec.TIPSigning:
		// HookPenalty (hooks.go:124): the 0x89 signers of the blocks i in [n-epoch, n) with
		// i % MergeSignRange == 0 or not IsTIP2019(i), read in the state the head had when
		// block n was built, which is the state after n-1.
		var nums []uint64
		for i := n - epoch; i < n; i++ {
			if i%mergeSign == 0 || !cfg.IsTIP2019(new(big.Int).SetUint64(i)) {
				nums = append(nums, i)
			}
		}
		bh, err := c.rawHeaders(nums)
		fatal(err)
		var lens []common.Hash
		for _, i := range nums {
			rec.SignerBlocks = append(rec.SignerBlocks, numberHash{i, bh[i].Hash().Hex()})
			lens = append(lens, common.BigToHash(state.GetLocMappingAtKey(bh[i].Hash(), slotSigners)))
		}
		lv, err := c.storage(signers89, lens, n-1)
		fatal(err)
		var elSlots []common.Hash
		for j, l := range lens {
			for k := uint64(0); k < lv[j].Big().Uint64(); k++ {
				elSlots = append(elSlots, state.GetLocDynamicArrAtElement(l, k, 1))
			}
		}
		ev, err := c.storage(signers89, elSlots, n-1)
		fatal(err)
		rec.SignerWords = append(words(lens, lv), words(elSlots, ev)...)
	case rec.TIPRandomize:
		// HookPenaltyTIPSigning (hooks.go:182-240): the creators of blocks n-1 .. n-(epoch-1).
		var nums []uint64
		for i := n - epoch + 1; i < n; i++ {
			nums = append(nums, i)
		}
		bh, err := c.rawHeaders(nums)
		fatal(err)
		counts := map[common.Address]int{}
		for _, i := range nums {
			miner, err := eng.RecoverSigner(bh[i])
			fatal(err)
			counts[miner]++
		}
		for a, k := range counts {
			rec.Creators = append(rec.Creators, [2]any{strings.ToLower(a.Hex()), k})
		}
		sort.Slice(rec.Creators, func(i, j int) bool { return rec.Creators[i][0].(string) < rec.Creators[j][0].(string) })
	}
	return rec
}

func done(path string, key string) map[uint64]bool {
	seen := map[uint64]bool{}
	f, err := os.Open(path)
	if err != nil {
		return seen
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 1<<20), 1<<26)
	for sc.Scan() {
		var r map[string]json.RawMessage
		if json.Unmarshal(sc.Bytes(), &r) == nil {
			var n uint64
			if json.Unmarshal(r[key], &n) == nil {
				seen[n] = true
			}
		}
	}
	return seen
}

func setup() *params.ChainConfig {
	common.CopyXDCConstants(51)
	XDPoS.SetNetworkConstants(51)
	return params.XDCApothemChainConfig
}

func cmdCheckpoints(args []string) {
	fs := flag.NewFlagSet("checkpoints", flag.ExitOnError)
	rpcURL := fs.String("rpc", "http://127.0.0.1:17105", "archive RPC")
	out := fs.String("out", "checkpoints.jsonl", "output")
	strata := fs.Int("strata", 500, "stratified checkpoints")
	edge := fs.Int("edge", 50, "first and last checkpoints taken in full")
	seed := fs.Int64("seed", 1, "seed")
	fatal(fs.Parse(args))
	cfg := setup()
	eng := XDPoS.New(cfg.XDPoS, nil)
	c := &client{url: *rpcURL}

	// Checkpoint k is block 900*k, k = 1 .. 63143 (the switch block 56,828,700 is k = 63143).
	last := uint64(switchBlock / epoch)
	set := map[uint64]bool{}
	for k := uint64(1); k <= uint64(*edge); k++ {
		set[k] = true
		set[last+1-k] = true
	}
	r := rand.New(rand.NewSource(*seed))
	lo, hi := uint64(*edge)+1, last-uint64(*edge) // the middle, split in equal strata
	for s := 0; s < *strata; s++ {
		a := lo + (hi-lo+1)*uint64(s)/uint64(*strata)
		b := lo + (hi-lo+1)*uint64(s+1)/uint64(*strata)
		set[a+uint64(r.Int63n(int64(b-a)))] = true
	}
	var ks []uint64
	for k := range set {
		ks = append(ks, k)
	}
	sort.Slice(ks, func(i, j int) bool { return ks[i] < ks[j] })

	seen := done(*out, "number")
	f, err := os.OpenFile(*out, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	fatal(err)
	defer f.Close()
	for i, k := range ks {
		n := k * epoch
		if seen[n] {
			continue
		}
		rec := checkpoint(c, cfg, eng, n)
		b, _ := json.Marshal(rec)
		_, err := f.Write(append(b, '\n'))
		fatal(err)
		if i%10 == 0 {
			fmt.Fprintf(os.Stderr, "checkpoint %d/%d (block %d)\n", i+1, len(ks), n)
		}
	}
}

// chain serves the headers the oracle fetched, to geth's own GetValidator.
type chain struct {
	consensus.ChainHeaderReader
	cfg   *params.ChainConfig
	byNum map[uint64]*types.Header
}

func (c chain) Config() *params.ChainConfig              { return c.cfg }
func (c chain) GetHeaderByNumber(n uint64) *types.Header { return c.byNum[n] }

type blockRecord struct {
	Number       uint64  `json:"number"`
	Hash         string  `json:"hash"`
	Header       string  `json:"header"`
	SealHash     string  `json:"sealHash"`
	Creator      string  `json:"creator"`
	Validator    *string `json:"validator"`
	ValidatorErr string  `json:"validatorError,omitempty"`
	Assigned     *string `json:"assigned"`
	AssignedErr  string  `json:"assignedError,omitempty"`
	// What the header verifier of the V1 rules needs besides the block (see the README).
	Parent            string   `json:"parent"`            // rlp of block number-1
	Masternodes       []string `json:"masternodes"`       // XDPoS_getMasternodes(number)
	ParentMasternodes []string `json:"parentMasternodes"` // XDPoS_getMasternodes(number-1): the list of YourTurn
	Signers           []string `json:"signers"`           // XDPoS_getSigners(number-1): the snapshot signers at the parent
}

type epochRecord struct {
	Epoch      uint64        `json:"epoch"`
	Checkpoint *string       `json:"checkpoint"` // rlp(header) of block 900*epoch; null for epoch 0
	Blocks     []blockRecord `json:"blocks"`
}

func dedupe(ns []uint64) []uint64 {
	seen := map[uint64]bool{}
	var out []uint64
	for _, n := range ns {
		if !seen[n] {
			seen[n] = true
			out = append(out, n)
		}
	}
	return out
}

func addrList(raw json.RawMessage) []string {
	var as []common.Address
	fatal(json.Unmarshal(raw, &as))
	return hexes(as)
}

func cmdM1M2(args []string) {
	fs := flag.NewFlagSet("m1m2", flag.ExitOnError)
	rpcURL := fs.String("rpc", "http://127.0.0.1:17105", "archive RPC")
	out := fs.String("out", "m1m2.jsonl", "output")
	strata := fs.Int("strata", 500, "epochs, one per stratum")
	per := fs.Int("per", 4, "blocks per epoch")
	seed := fs.Int64("seed", 1, "seed")
	fatal(fs.Parse(args))
	cfg := setup()
	eng := XDPoS.New(cfg.XDPoS, nil)
	c := &client{url: *rpcURL}

	last := uint64(switchBlock / epoch)
	r := rand.New(rand.NewSource(*seed))
	var eps []uint64
	for s := 0; s < *strata; s++ {
		a := (last + 1) * uint64(s) / uint64(*strata)
		b := (last + 1) * uint64(s+1) / uint64(*strata)
		eps = append(eps, a+uint64(r.Int63n(int64(b-a))))
	}
	eps[0] = 0 // the first epoch: no M2 is assigned there (xdpos_api.go:92)
	eps[len(eps)-1] = last

	seen := done(*out, "epoch")
	f, err := os.OpenFile(*out, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	fatal(err)
	defer f.Close()
	for i, e := range eps {
		picks := map[uint64]bool{}
		for len(picks) < *per {
			lo, hi := max(e*epoch, 1), min(e*epoch+epoch-1, switchBlock)
			if e == last {
				picks[switchBlock] = true
				break
			}
			picks[lo+uint64(r.Int63n(int64(hi-lo+1)))] = true
		}
		if seen[e] {
			continue
		}
		nums := []uint64{}
		for n := range picks {
			nums = append(nums, n)
		}
		sort.Slice(nums, func(i, j int) bool { return nums[i] < nums[j] })
		fetch := append([]uint64{}, nums...)
		for _, n := range nums {
			fetch = append(fetch, n-1)
		}
		if e > 0 {
			fetch = append(fetch, e*epoch)
		}
		fetch = dedupe(fetch)
		hs, err := c.rawHeaders(fetch)
		fatal(err)
		rc := epochRecord{Epoch: e}
		if e > 0 {
			b, _ := rlp.EncodeToBytes(hs[e*epoch])
			s := hexutil.Encode(b)
			rc.Checkpoint = &s
		}
		rd := chain{cfg: cfg, byNum: hs}
		for _, n := range nums {
			h := hs[n]
			raw, _ := rlp.EncodeToBytes(h)
			br := blockRecord{Number: n, Hash: h.Hash().Hex(), Header: hexutil.Encode(raw), SealHash: XDPoS.SealHash(h).Hex()}
			creator, err := eng.RecoverSigner(h)
			fatal(err)
			br.Creator = strings.ToLower(creator.Hex())
			if len(h.Validator) == 0 {
				br.ValidatorErr = "empty"
			} else if v, err := eng.RecoverValidator(h); err != nil {
				br.ValidatorErr = err.Error()
			} else {
				s := strings.ToLower(v.Hex())
				br.Validator = &s
			}
			if a, err := eng.GetValidator(creator, rd, h); err != nil {
				br.AssignedErr = err.Error()
			} else {
				s := strings.ToLower(a.Hex())
				br.Assigned = &s
			}
			praw, _ := rlp.EncodeToBytes(hs[n-1])
			br.Parent = hexutil.Encode(praw)
			rs, err := c.batch("XDPoS_getMasternodes", [][]any{{fmt.Sprintf("0x%x", n)}, {fmt.Sprintf("0x%x", n-1)}})
			fatal(err)
			ss, err := c.batch("XDPoS_getSigners", [][]any{{fmt.Sprintf("0x%x", n-1)}})
			fatal(err)
			br.Masternodes, br.ParentMasternodes, br.Signers = addrList(rs[0]), addrList(rs[1]), addrList(ss[0])
			rc.Blocks = append(rc.Blocks, br)
		}
		b, _ := json.Marshal(rc)
		_, err = f.Write(append(b, '\n'))
		fatal(err)
		if i%25 == 0 {
			fmt.Fprintf(os.Stderr, "epoch %d/%d\n", i+1, len(eps))
		}
	}
}

type sortCase struct {
	Caps  []uint64 `json:"caps"`
	Order []int    `json:"order"` // original indices, in the order geth's frozen sort leaves them
}

// cmdGosort runs the pinned geth's frozen sort (common/sort/slice.go) on inputs with many equal
// keys, the way UpdateMasternodesV1 sorts candidates (xdpos.go:2148): `less` is `>=`.
func cmdGosort(args []string) {
	fs := flag.NewFlagSet("gosort", flag.ExitOnError)
	out := fs.String("out", "gosort.jsonl", "output")
	seed := fs.Int64("seed", 1, "seed")
	fatal(fs.Parse(args))
	r := rand.New(rand.NewSource(*seed))
	var buf bytes.Buffer
	for n := 0; n <= 200; n++ {
		for _, k := range []int{1, 2, 3, 5, 20, 1000} {
			type item struct {
				idx int
				cap uint64
			}
			items := make([]item, n)
			caps := make([]uint64, n)
			for i := range items {
				caps[i] = uint64(r.Intn(k))
				items[i] = item{i, caps[i]}
			}
			xdcsort.Slice(items, func(i, j int) bool { return items[i].cap >= items[j].cap })
			order := make([]int, n)
			for i, it := range items {
				order[i] = it.idx
			}
			b, _ := json.Marshal(sortCase{caps, order})
			buf.Write(append(b, '\n'))
		}
	}
	fatal(os.WriteFile(*out, buf.Bytes(), 0o644))
}

type snapWindow struct {
	Start   json.RawMessage `json:"start"`   // XDPoS_getSnapshot(a)
	End     json.RawMessage `json:"end"`     // XDPoS_getSnapshot(b)
	Headers []string        `json:"headers"` // rlp(header) of a+1 .. b
	// Why: what the window covers. No window but the last one has a gap block inside, where the
	// signer set is replaced by the candidates.
	Why string `json:"why"`
}

// cmdSnapshot writes windows of consecutive headers with the archive's snapshot before and after.
// The snapshot RPC is geth's own `snapshot()`: the gap snapshot, then `apply` over the headers.
func cmdSnapshot(args []string) {
	fs := flag.NewFlagSet("snapshot", flag.ExitOnError)
	rpcURL := fs.String("rpc", "http://127.0.0.1:17105", "archive RPC")
	out := fs.String("out", "snapshot.jsonl", "output")
	seed := fs.Int64("seed", 1, "seed")
	fatal(fs.Parse(args))
	c := &client{url: *rpcURL}
	r := rand.New(rand.NewSource(*seed))
	hasGap := func(a, b uint64) bool { // a gap block in (a, b]
		for n := a + 1; n <= b; n++ {
			if n%epoch == gap {
				return true
			}
		}
		return false
	}
	type win struct {
		a, b uint64
		why  string
	}
	var ws []win
	last := uint64(switchBlock)
	for s := uint64(0); s < 20; s++ { // ordinary windows, one per stratum
		for {
			a := 1 + last*s/20 + uint64(r.Int63n(int64(last/20)))
			if !hasGap(a, a+40) && (a+40)%epoch != gap {
				ws = append(ws, win{a, a + 40, "stratified"})
				break
			}
		}
	}
	for s := uint64(0); s < 8; s++ { // windows over a checkpoint: the votes reset there
		k := 2 + (last/epoch-4)*s/8 + uint64(r.Int63n(int64((last/epoch-4)/8)))
		ws = append(ws, win{k*epoch - 20, k*epoch + 20, "checkpoint"})
	}
	ws = append(ws, win{gap * 3, 2*epoch + 10, "gap 1350 reseeded from the candidates, then the checkpoint 1800"})
	f, err := os.Create(*out)
	fatal(err)
	defer f.Close()
	for _, w := range ws {
		var nums []uint64
		for n := w.a + 1; n <= w.b; n++ {
			nums = append(nums, n)
		}
		hs, err := c.rawHeaders(nums)
		fatal(err)
		sn, err := c.batch("XDPoS_getSnapshot", [][]any{{fmt.Sprintf("0x%x", w.a)}, {fmt.Sprintf("0x%x", w.b)}})
		fatal(err)
		rec := snapWindow{Start: sn[0], End: sn[1], Why: w.why}
		for _, n := range nums {
			raw, _ := rlp.EncodeToBytes(hs[n])
			rec.Headers = append(rec.Headers, hexutil.Encode(raw))
		}
		b, _ := json.Marshal(rec)
		_, err = f.Write(append(b, '\n'))
		fatal(err)
	}
}

// voteChain serves a synthetic chain to geth's own snapshot().
type voteChain struct {
	consensus.ChainHeaderReader
	cfg    *params.ChainConfig
	byNum  map[uint64]*types.Header
	byHash map[common.Hash]*types.Header
}

func (c voteChain) Config() *params.ChainConfig              { return c.cfg }
func (c voteChain) GetHeaderByNumber(n uint64) *types.Header { return c.byNum[n] }
func (c voteChain) GetHeader(h common.Hash, n uint64) *types.Header {
	if x := c.byHash[h]; x != nil && x.Number.Uint64() == n {
		return x
	}
	return nil
}

type voteScenario struct {
	Genesis   string            `json:"genesis"`
	Headers   []string          `json:"headers"` // rlp of blocks 1 ..
	At        []uint64          `json:"at"`
	Snapshots []json.RawMessage `json:"snapshots"` // geth's snapshot at each height of `at`
}

// cmdSnapvotes builds short synthetic chains whose headers carry votes (coinbase and nonce) and
// writes the snapshot the pinned geth computes (XDPoS.GetSnapshot: genesis, then `apply`). The
// archive's own windows hold no vote, so this is what exercises the vote and tally rules. The
// chains have epoch 50 and gap 49 and no signer list in their checkpoint, so geth's gap bootstrap
// (xdpos.go:2345) finds nothing to replace the signers with: only `apply` runs (snapshot.go:163-247),
// including the vote reset at block 50.
func cmdSnapvotes(args []string) {
	fs := flag.NewFlagSet("snapvotes", flag.ExitOnError)
	out := fs.String("out", "snapvotes.jsonl", "output")
	n := fs.Int("scenarios", 24, "scenarios")
	fatal(fs.Parse(args))
	setup()
	var buf bytes.Buffer
	for sc := 0; sc < *n; sc++ {
		r := rand.New(rand.NewSource(int64(sc) + 1))
		var keys []*ecdsaKey
		for i := 0; i < 8; i++ {
			k, err := crypto.ToECDSA(crypto.Keccak256([]byte(fmt.Sprintf("v1 vote scenario %d key %d", sc, i))))
			fatal(err)
			keys = append(keys, &ecdsaKey{k, crypto.PubkeyToAddress(k.PublicKey)})
		}
		initial := 3 + r.Intn(5) // the first `initial` keys are the genesis signers
		genesis := &types.Header{Number: big.NewInt(0), Time: 1000, Difficulty: big.NewInt(1), UncleHash: types.EmptyUncleHash, Extra: make([]byte, 32)}
		for _, k := range keys[:initial] {
			genesis.Extra = append(genesis.Extra, k.addr[:]...)
		}
		genesis.Extra = append(genesis.Extra, make([]byte, 65)...)
		ch := voteChain{cfg: params.XDCApothemChainConfig, byNum: map[uint64]*types.Header{0: genesis}, byHash: map[common.Hash]*types.Header{genesis.Hash(): genesis}}
		rec := voteScenario{Genesis: rlpHex(genesis)}
		prev := genesis
		for i := uint64(1); i <= 99; i++ {
			h := &types.Header{ParentHash: prev.Hash(), Number: new(big.Int).SetUint64(i), Time: 1000 + 2*i, Difficulty: big.NewInt(1), UncleHash: types.EmptyUncleHash, Extra: make([]byte, 32+65)}
			if r.Intn(10) < 6 { // a vote
				h.Coinbase = keys[r.Intn(len(keys))].addr
				if r.Intn(2) == 0 {
					h.Nonce = types.BlockNonce{0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff}
				}
			}
			sig, err := crypto.Sign(XDPoS.SealHash(h).Bytes(), keys[r.Intn(len(keys))].key)
			fatal(err)
			copy(h.Extra[32:], sig)
			ch.byNum[i], ch.byHash[h.Hash()] = h, h
			rec.Headers = append(rec.Headers, rlpHex(h))
			prev = h
		}
		eng := XDPoS.New(&params.XDPoSConfig{Period: 2, Epoch: 50, Gap: 49}, rawdb.NewMemoryDatabase())
		for _, at := range []uint64{3, 10, 25, 49, 50, 51, 75, 99} {
			snap, err := eng.GetSnapshot(ch, ch.byNum[at])
			fatal(err)
			b, err := json.Marshal(snap)
			fatal(err)
			rec.At, rec.Snapshots = append(rec.At, at), append(rec.Snapshots, b)
		}
		b, _ := json.Marshal(rec)
		buf.Write(append(b, '\n'))
	}
	fatal(os.WriteFile(*out, buf.Bytes(), 0o644))
}

type ecdsaKey struct {
	key  *ecdsa.PrivateKey
	addr common.Address
}

func rlpHex(h *types.Header) string {
	b, err := rlp.EncodeToBytes(h)
	fatal(err)
	return hexutil.Encode(b)
}

func main() {
	if len(os.Args) < 2 {
		fatal(fmt.Errorf("usage: v1oracle checkpoints|m1m2|gosort|snapshot|snapvotes [flags]"))
	}
	switch os.Args[1] {
	case "checkpoints":
		cmdCheckpoints(os.Args[2:])
	case "m1m2":
		cmdM1M2(os.Args[2:])
	case "gosort":
		cmdGosort(os.Args[2:])
	case "snapshot":
		cmdSnapshot(os.Args[2:])
	case "snapvotes":
		cmdSnapvotes(os.Args[2:])
	default:
		fatal(fmt.Errorf("unknown command %q", os.Args[1]))
	}
}
