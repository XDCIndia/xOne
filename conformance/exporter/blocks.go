package main

// The `blocks` subcommand: fetch whole blocks (header, body, receipts), the
// parent header and the masternode data the consensus rules need, from an
// archive RPC, and write one JSON object per block. Read by
// crates/xone-consensus/tests/corpus.rs; the layout is in
// conformance/vectors/consensus/README.md.
//
//	go run . blocks -network apothem -rpc http://127.0.0.1:17105 -out ../vectors/consensus/valid/apothem.jsonl
//
// Everything is the archive's answer re-encoded by the pinned geth-XDC types.
// The exporter stops if geth's own hashes of the re-encoded header, transaction
// list, receipt list or withdrawals differ from the header the archive returned.

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"math/big"
	"math/rand"
	"os"
	"sort"
	"strings"
	"time"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/consensus/XDPoS"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/params"
	"github.com/ethereum/go-ethereum/rlp"
	"github.com/ethereum/go-ethereum/trie"
)

type req struct {
	method string
	params []any
}

func (r req) key() string {
	b, _ := json.Marshal(append([]any{r.method}, r.params...))
	return string(b)
}

// fetcher sends requests in batches of `batch` with a pause after each batch
// and remembers every answer, so a block shared by two vectors is fetched once.
type fetcher struct {
	c        rpcClient
	cache    map[string]json.RawMessage
	batch    int
	interval time.Duration
}

func (f *fetcher) do(reqs []req) []json.RawMessage {
	var todo []req
	seen := map[string]bool{}
	for _, r := range reqs {
		if _, ok := f.cache[r.key()]; !ok && !seen[r.key()] {
			seen[r.key()] = true
			todo = append(todo, r)
		}
	}
	for i := 0; i < len(todo); i += f.batch {
		chunk := todo[i:min(i+f.batch, len(todo))]
		wire := make([]map[string]any, len(chunk))
		for j, r := range chunk {
			wire[j] = map[string]any{"jsonrpc": "2.0", "id": j, "method": r.method, "params": r.params}
		}
		answers, err := f.c.batch(wire)
		fatal(err)
		for j, a := range answers {
			f.cache[chunk[j].key()] = a
		}
		time.Sleep(f.interval)
	}
	out := make([]json.RawMessage, len(reqs))
	for i, r := range reqs {
		out[i] = f.cache[r.key()]
	}
	return out
}

func (f *fetcher) one(method string, params ...any) json.RawMessage {
	return f.do([]req{{method, params}})[0]
}

func hexq(n uint64) string { return fmt.Sprintf("0x%x", n) }

func hexb(b []byte) string { return "0x" + hex.EncodeToString(b) }

type blockCtx struct {
	Consensus         string   `json:"consensus"`
	Now               uint64   `json:"now"`
	Masternodes       []string `json:"masternodes"`
	ParentMasternodes []string `json:"parentMasternodes,omitempty"`
	Penalties         []string `json:"penalties"`
	ParentEpochSwitch *uint64  `json:"parentEpochSwitch,omitempty"`
	Signers           []string `json:"signers,omitempty"`
	Checkpoint        string   `json:"checkpoint,omitempty"`
	CheckpointSigners []string `json:"checkpointSigners,omitempty"`
}

type blockBody struct {
	Transactions []string  `json:"transactions"`
	Uncles       []string  `json:"uncles"`
	Withdrawals  *[]string `json:"withdrawals"`
}

type blockVector struct {
	ID       string     `json:"id"`
	Network  string     `json:"network"`
	Class    string     `json:"class"`
	Number   uint64     `json:"number"`
	Hash     string     `json:"hash"`
	Header   string     `json:"header"`
	Parent   string     `json:"parent"`
	Body     blockBody  `json:"body"`
	Receipts []string   `json:"receipts"`
	Context  blockCtx   `json:"context"`
	Oracle   oracle     `json:"oracle"`
	Notes    []string   `json:"notes,omitempty"`
	archive  rawArchive `json:"-"`
}

// rawArchive keeps the archive's own answers while a vector is assembled.
type rawArchive struct{ block, receipts, parent json.RawMessage }

type mnStatus struct {
	Masternodes []string
	Penalty     []string
	Error       any
}

func lower(a []string) []string {
	out := make([]string, len(a))
	for i, s := range a {
		out[i] = strings.ToLower(s)
	}
	return out
}

func decodeHeader(raw json.RawMessage) (*types.Header, string) {
	var obj map[string]json.RawMessage
	fatal(json.Unmarshal(raw, &obj))
	if obj == nil {
		fatal(fmt.Errorf("no block in answer %.100s", raw))
	}
	for _, k := range dropped {
		delete(obj, k)
	}
	var hash string
	fatal(json.Unmarshal(obj["hash"], &hash))
	hdr, _ := json.Marshal(obj)
	var h types.Header
	fatal(json.Unmarshal(hdr, &h))
	if got := h.Hash().Hex(); got != hash {
		fatal(fmt.Errorf("geth Header.Hash() %s differs from archive hash %s", got, hash))
	}
	return &h, hash
}

func headerRLP(h *types.Header) string {
	b, err := rlp.EncodeToBytes(h)
	fatal(err)
	return hexb(b)
}

func blocksCmd(args []string) {
	fs := flag.NewFlagSet("blocks", flag.ExitOnError)
	network := fs.String("network", "", "apothem, or any name for a node that answers admin_nodeInfo")
	url := fs.String("rpc", "", "archive JSON-RPC URL")
	out := fs.String("out", "", "output .jsonl file")
	forksOut := fs.String("forks-out", "", "also write the fork table of this network (not built in) to this file")
	total := fs.Int("total", 300, "number of blocks")
	seed := fs.Int64("seed", 1, "seed of the random samples")
	headFlag := fs.Uint64("head", 0, "fix the head block (default: ask the node)")
	sideNumber := fs.Uint64("side", 0, "block number of a side-branch vector to look up (0: none)")
	nEpoch := fs.Int("v2-epochs", 20, "V2 epoch switches")
	nV1Epoch := fs.Int("v1-epochs", 20, "V1 epoch switches")
	nV1 := fs.Int("v1", 50, "V1 ordinary blocks")
	nSign := fs.Int("sign", 40, "V2 blocks carrying 0x89 signing transactions")
	fs.Parse(args)
	if *network == "" || *url == "" || *out == "" {
		fatal(fmt.Errorf("-network, -rpc and -out are required"))
	}
	f := &fetcher{c: rpcClient{*url}, cache: map[string]json.RawMessage{}, batch: 5, interval: time.Second}

	cfg, err := chainConfig(*network, f.c)
	fatal(err)
	head := *headFlag
	if head == 0 {
		var s string
		fatal(json.Unmarshal(f.one("eth_blockNumber"), &s))
		fmt.Sscanf(s, "0x%x", &head)
	}
	sw, epoch := cfg.XDPoS.V2.SwitchBlock.Uint64(), cfg.XDPoS.Epoch
	fmt.Fprintf(os.Stderr, "head %d switch %d epoch %d\n", head, sw, epoch)

	if *forksOut != "" {
		g, _ := decodeHeader(f.one("eth_getBlockByNumber", "0x0", false))
		table, err := marshalTable(strings.TrimSpace(readPin()), object{{*network, networkOf(cfg, g.Hash().Hex(), g.Root.Hex())}})
		fatal(err)
		fatal(os.WriteFile(*forksOut, table, 0o644))
	}

	p := &plan{class: map[uint64]string{}, head: head, rng: rand.New(rand.NewSource(*seed))}
	if *sideNumber != 0 {
		p.add("side", *sideNumber)
	}
	p.add("v2_switch", sw)
	p.add("v2_switch", sw+1)
	for k := 1; k <= *nV1Epoch && uint64(k)*epoch < sw; k++ {
		p.add("v1_epoch", sw-uint64(k)*epoch)
	}
	// V2 epoch switches: the first ones the node lists, then ones spread over the V2 range.
	var epochs []uint64
	fatal(json.Unmarshal(f.one("XDPoS_getEpochNumbersBetween", hexq(sw+1), hexq(min(sw+10*epoch, head))), &epochs))
	for i := 0; i < len(epochs) && i < *nEpoch/2; i++ {
		p.add("v2_epoch", epochs[i])
	}
	for len(epochs) > 0 && countClass(p, "v2_epoch") < *nEpoch {
		at := sw + 2 + uint64(p.rng.Int63n(int64(head-sw-2*epoch)))
		var near []uint64
		fatal(json.Unmarshal(f.one("XDPoS_getEpochNumbersBetween", hexq(at), hexq(min(at+2*epoch, head))), &near))
		if len(near) > 0 {
			p.add("v2_epoch", near[0])
		}
	}
	// Fork boundaries that change a header rule: row-1, row, row+1.
	for _, row := range []struct {
		name string
		n    uint64
		on   bool
	}{
		{"eip1559", bn(cfg.Eip1559Block), true}, {"gas2500x", bn(cfg.Gas2500xBlock), true},
		{"validatorOrder", bn(cfg.ValidatorOrderBlock), true}, {"signingTxBound", bn(cfg.SigningTxBoundBlock), true},
		{"tipSigning", bn(cfg.TIPSigningBlock), true}, {"tipRandomize", bn(cfg.TIPRandomizeBlock), true},
		{"amsterdam", bn(cfg.AmsterdamBlock), true}, {"dynamicGasLimit", bn(cfg.DynamicGasLimitBlock), true},
	} {
		if row.n > 0 && row.n < unscheduled {
			for _, n := range []uint64{row.n - 1, row.n, row.n + 1} {
				if n > epoch {
					p.add("fork", n)
				}
			}
		}
	}
	// Blocks that carry 0x89 signing transactions are every few blocks; find the
	// phase from the head, then draw blocks of that phase. A drawn block without
	// a signing transaction is relabelled below.
	signers := signPhase(f, head)
	for drawn := 0; drawn < *nSign && signers.period > 0; drawn++ {
		for tries := 0; tries < 1000; tries++ {
			n := sw + 2 + uint64(p.rng.Int63n(int64(head-sw-2)))
			n -= (n % signers.period) - signers.phase
			if _, ok := p.class[n]; !ok && n > sw+1 && n <= head {
				p.class[n] = "sign"
				break
			}
		}
	}
	p.random("v1", *nV1, epoch+1, sw-1)
	p.random("v2", *total-len(p.class), sw+2, head-1)

	nums := make([]uint64, 0, len(p.class))
	for n := range p.class {
		nums = append(nums, n)
	}
	sort.Slice(nums, func(i, j int) bool { return nums[i] < nums[j] })
	fmt.Fprintf(os.Stderr, "%d blocks planned\n", len(nums))

	vecs := collect(f, cfg, p, *network, nums, sw, epoch)

	var buf bytes.Buffer
	counts := map[string]int{}
	for _, v := range vecs {
		line, err := json.Marshal(v)
		fatal(err)
		buf.Write(line)
		buf.WriteByte('\n')
		counts[v.Class]++
	}
	fatal(os.WriteFile(*out, buf.Bytes(), 0o644))
	fmt.Fprintf(os.Stderr, "wrote %d blocks, %d bytes, classes %v\n", len(vecs), buf.Len(), counts)
}

func readPin() string {
	b, err := os.ReadFile("../PIN")
	fatal(err)
	return string(b)
}

func bn(n *big.Int) uint64 {
	if n == nil {
		return 0
	}
	return n.Uint64()
}

func countClass(p *plan, class string) int {
	n := 0
	for _, c := range p.class {
		if c == class {
			n++
		}
	}
	return n
}

type phaseInfo struct{ period, phase uint64 }

// signPhase looks at 40 blocks below the head and returns the spacing and
// offset of the blocks that carry signing transactions (zero if no pattern).
func signPhase(f *fetcher, head uint64) phaseInfo {
	var hits []uint64
	reqs := make([]req, 0, 40)
	for n := head - 40; n < head; n++ {
		reqs = append(reqs, req{"eth_getBlockTransactionCountByNumber", []any{hexq(n)}})
	}
	for i, a := range f.do(reqs) {
		var c string
		fatal(json.Unmarshal(a, &c))
		if c != "0x0" {
			hits = append(hits, head-40+uint64(i))
		}
	}
	if len(hits) < 2 {
		return phaseInfo{}
	}
	d := hits[1] - hits[0]
	return phaseInfo{d, hits[0] % d}
}

func collect(f *fetcher, cfg *params.ChainConfig, p *plan, name string, nums []uint64, sw, epoch uint64) []*blockVector {
	eng := XDPoS.New(cfg.XDPoS, nil)
	// Stage 1: blocks, receipts, parents, masternode data.
	var reqs []req
	for _, n := range nums {
		reqs = append(reqs,
			req{"eth_getBlockByNumber", []any{hexq(n), true}},
			req{"eth_getBlockReceipts", []any{hexq(n)}},
			req{"eth_getBlockByNumber", []any{hexq(n - 1), false}})
		if n > sw {
			reqs = append(reqs, req{"XDPoS_getMasternodesByNumber", []any{hexq(n)}})
			if n-1 > sw {
				reqs = append(reqs,
					req{"XDPoS_getMasternodesByNumber", []any{hexq(n - 1)}},
					req{"XDPoS_getEpochNumbersBetween", []any{hexq(max(sw+1, n-2*epoch)), hexq(n - 1)}})
			}
		} else {
			reqs = append(reqs,
				req{"XDPoS_getMasternodes", []any{hexq(n)}},
				req{"XDPoS_getMasternodes", []any{hexq(n - 1)}})
			if cp := n - n%epoch; n > epoch && n%epoch != 0 {
				reqs = append(reqs, req{"eth_getBlockByNumber", []any{hexq(cp), false}})
			}
		}
	}
	f.do(reqs)
	// Stage 2: V1 snapshot signers, which are asked by block hash.
	var reqs2 []req
	for _, n := range nums {
		if n <= sw {
			par, _ := decodeHeader(f.one("eth_getBlockByNumber", hexq(n-1), false))
			reqs2 = append(reqs2, req{"XDPoS_getSignersAtHash", []any{par.Hash().Hex()}})
			if n%epoch == 0 {
				cur, _ := decodeHeader(f.one("eth_getBlockByNumber", hexq(n), true))
				reqs2 = append(reqs2, req{"XDPoS_getSignersAtHash", []any{cur.Hash().Hex()}})
			}
		}
	}
	f.do(reqs2)

	var vecs []*blockVector
	for _, n := range nums {
		v := assemble(f, eng, cfg, name, p.class[n], n, sw, epoch)
		// The label follows the body: ordinary blocks with a signing transaction
		// are "sign", a block drawn as one that has none is "v2".
		switch signed := hasSign(v); {
		case v.Class == "sign" && !signed:
			v.Class = "v2"
			v.Notes = append(v.Notes, "drawn as a signing-transaction block, has none")
		case (v.Class == "v1" || v.Class == "v2") && signed:
			v.Class = "sign"
		}
		vecs = append(vecs, v)
	}
	return vecs
}

var signTo = strings.ToLower(common.BlockSignersBinary.Hex())

func hasSign(v *blockVector) bool {
	for _, raw := range v.Body.Transactions {
		var tx types.Transaction
		b, _ := hex.DecodeString(strings.TrimPrefix(raw, "0x"))
		if tx.UnmarshalBinary(b) == nil && tx.To() != nil && strings.ToLower(tx.To().Hex()) == signTo {
			return true
		}
	}
	return false
}

func addrs(raw json.RawMessage) []string {
	var a []string
	fatal(json.Unmarshal(raw, &a))
	return lower(a)
}

func assemble(f *fetcher, eng *XDPoS.XDPoS, cfg *params.ChainConfig, name, class string, n, sw, epoch uint64) *blockVector {
	blk := f.one("eth_getBlockByNumber", hexq(n), true)
	hdr, hash := decodeHeader(blk)
	par, _ := decodeHeader(f.one("eth_getBlockByNumber", hexq(n-1), false))
	if hdr.ParentHash != par.Hash() {
		fatal(fmt.Errorf("block %d: the archive's parent is not its parent hash", n))
	}
	v := &blockVector{
		ID:      fmt.Sprintf("%s-%d-%s", name, n, hash[2:10]),
		Network: name, Class: class, Number: n, Hash: hash,
		Header: headerRLP(hdr), Parent: headerRLP(par),
	}
	// Body.
	var body struct {
		Transactions []json.RawMessage  `json:"transactions"`
		Uncles       []string           `json:"uncles"`
		Withdrawals  *[]json.RawMessage `json:"withdrawals"`
	}
	fatal(json.Unmarshal(blk, &body))
	if len(body.Uncles) != 0 {
		fatal(fmt.Errorf("block %d has uncles", n))
	}
	txs := make(types.Transactions, len(body.Transactions))
	v.Body = blockBody{Transactions: []string{}, Uncles: []string{}}
	for i, raw := range body.Transactions {
		txs[i] = new(types.Transaction)
		fatal(json.Unmarshal(raw, txs[i]))
		b, err := txs[i].MarshalBinary()
		fatal(err)
		v.Body.Transactions = append(v.Body.Transactions, hexb(b))
	}
	if got := types.DeriveSha(txs, trie.NewStackTrie(nil)); got != hdr.TxHash {
		fatal(fmt.Errorf("block %d: geth transaction root %s differs from header %s", n, got, hdr.TxHash))
	}
	if body.Withdrawals != nil {
		ws := make(types.Withdrawals, len(*body.Withdrawals))
		list := []string{}
		for i, raw := range *body.Withdrawals {
			ws[i] = new(types.Withdrawal)
			fatal(json.Unmarshal(raw, ws[i]))
			b, err := rlp.EncodeToBytes(ws[i])
			fatal(err)
			list = append(list, hexb(b))
		}
		v.Body.Withdrawals = &list
		if hdr.WithdrawalsHash != nil && types.DeriveSha(ws, trie.NewStackTrie(nil)) != *hdr.WithdrawalsHash {
			fatal(fmt.Errorf("block %d: withdrawals root differs from header", n))
		}
	}
	// Receipts.
	var rcs types.Receipts
	fatal(json.Unmarshal(f.one("eth_getBlockReceipts", hexq(n)), &rcs))
	v.Receipts = []string{}
	for _, r := range rcs {
		b, err := r.MarshalBinary()
		fatal(err)
		v.Receipts = append(v.Receipts, hexb(b))
	}
	if got := types.DeriveSha(rcs, trie.NewStackTrie(nil)); got != hdr.ReceiptHash {
		fatal(fmt.Errorf("block %d: geth receipt root %s differs from header %s", n, got, hdr.ReceiptHash))
	}
	// Oracle values and the rule inputs.
	var obj map[string]json.RawMessage
	fatal(json.Unmarshal(blk, &obj))
	for _, k := range dropped {
		delete(obj, k)
	}
	hj, _ := json.Marshal(obj)
	o, err := compute(eng, hj, hash)
	fatal(err)
	v.Oracle = o

	ctx := blockCtx{Now: hdr.Time, Penalties: []string{}}
	if n > sw {
		ctx.Consensus = "v2"
		var st mnStatus
		fatal(json.Unmarshal(f.one("XDPoS_getMasternodesByNumber", hexq(n)), &st))
		if st.Error != nil {
			fatal(fmt.Errorf("block %d: getMasternodesByNumber: %v", n, st.Error))
		}
		ctx.Masternodes, ctx.Penalties = lower(st.Masternodes), lower(st.Penalty)
		if n-1 > sw {
			var pst mnStatus
			fatal(json.Unmarshal(f.one("XDPoS_getMasternodesByNumber", hexq(n-1)), &pst))
			if pst.Error != nil {
				fatal(fmt.Errorf("block %d: getMasternodesByNumber(parent): %v", n, pst.Error))
			}
			ctx.ParentMasternodes = lower(pst.Masternodes)
			var sws []uint64
			fatal(json.Unmarshal(f.one("XDPoS_getEpochNumbersBetween", hexq(max(sw+1, n-2*epoch)), hexq(n-1)), &sws))
			var at uint64
			for _, s := range sws {
				if s <= n-1 && s > at {
					at = s
				}
			}
			if at == 0 {
				at = sw + 1 // the first V2 block opens the first V2 epoch
				v.Notes = append(v.Notes, "epoch-switch list empty; parent epoch switch taken as the first V2 block")
			}
			ctx.ParentEpochSwitch = &at
		} else {
			at := sw
			ctx.ParentEpochSwitch = &at
		}
	} else {
		ctx.Consensus = "v1"
		ctx.Masternodes = addrs(f.one("XDPoS_getMasternodes", hexq(n)))
		ctx.ParentMasternodes = addrs(f.one("XDPoS_getMasternodes", hexq(n-1)))
		ctx.Signers = addrs(f.one("XDPoS_getSignersAtHash", par.Hash().Hex()))
		if n%epoch == 0 {
			ctx.CheckpointSigners = addrs(f.one("XDPoS_getSignersAtHash", hash))
		} else if cp := n - n%epoch; n > epoch {
			ch, _ := decodeHeader(f.one("eth_getBlockByNumber", hexq(cp), false))
			ctx.Checkpoint = headerRLP(ch)
		}
	}
	v.Context = ctx
	if class == "side" {
		v.Notes = append(v.Notes, sideNote(f, n, hash))
	}
	return v
}

// sideNote records what the archive says about a block that was named as a side-branch case:
// a node only serves the canonical block of a height, and its bad-block list does not hold
// the sibling, so the vector is the canonical block.
func sideNote(f *fetcher, n uint64, hash string) string {
	var bad []struct {
		Hash  string
		Block struct{ Number string }
	}
	fatal(json.Unmarshal(f.one("debug_getBadBlocks"), &bad))
	listed := 0
	for _, b := range bad {
		if b.Block.Number == hexq(n) {
			listed++
		}
	}
	var uncles string
	fatal(json.Unmarshal(f.one("eth_getUncleCountByBlockNumber", hexq(n)), &uncles))
	return fmt.Sprintf("side-branch case %d: the archive serves only the canonical block %s at this height; "+
		"debug_getBadBlocks lists %d blocks at it (%d in all); eth_getUncleCountByBlockNumber is %s; "+
		"a sibling hash is not discoverable from the archive", n, hash, listed, len(bad), uncles)
}
