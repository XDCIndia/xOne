package main

// The `headers` subcommand: fetch headers from an archive RPC for a
// deterministic block list and write one JSON object per header.
//
//	go run . headers -network apothem -rpc http://127.0.0.1:17105 -out ../vectors/headers/apothem.jsonl
//
// Every field of the eth_getBlockByNumber answer is kept as returned (values
// are not re-encoded), except the body and derived keys listed in `dropped`.
// The key "xone" is added: the class of the sample and the values the pinned
// geth-XDC computes for that header (seal hash, V2 signature hash,
// HashNoValidator, recovered signer). The canonical hash is the RPC "hash",
// and the exporter stops if geth's own Header.Hash() disagrees with it.

import (
	"bytes"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"math/big"
	"math/rand"
	"net/http"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/consensus/XDPoS"
	"github.com/ethereum/go-ethereum/consensus/XDPoS/engines/engine_v2"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/ethereum/go-ethereum/params"
)

// dropped are the body and derived keys of a block object that are not header
// fields.
var dropped = []string{"transactions", "uncles", "withdrawals", "size", "totalDifficulty"}

const unscheduled = 99999999999 // geth's "never" sentinel for fork blocks

type rpcClient struct {
	url string
}

func (c rpcClient) batch(reqs []map[string]any) ([]json.RawMessage, error) {
	body, _ := json.Marshal(reqs)
	resp, err := http.Post(c.url, "application/json", bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}
	var answers []struct {
		ID     int             `json:"id"`
		Result json.RawMessage `json:"result"`
		Error  *struct {
			Message string `json:"message"`
		} `json:"error"`
	}
	if err := json.Unmarshal(raw, &answers); err != nil {
		return nil, fmt.Errorf("batch answer: %v: %.200s", err, raw)
	}
	out := make([]json.RawMessage, len(reqs))
	for _, a := range answers {
		if a.Error != nil {
			return nil, fmt.Errorf("%v: %s", reqs[a.ID]["method"], a.Error.Message)
		}
		out[a.ID] = a.Result
	}
	return out, nil
}

func (c rpcClient) call(method string, params ...any) (json.RawMessage, error) {
	if params == nil {
		params = []any{}
	}
	r, err := c.batch([]map[string]any{{"jsonrpc": "2.0", "id": 0, "method": method, "params": params}})
	if err != nil {
		return nil, err
	}
	return r[0], nil
}

// chainConfig is the geth config of a network: built in for Apothem, otherwise
// what the node itself reports in admin_nodeInfo.
func chainConfig(network string, c rpcClient) (*params.ChainConfig, error) {
	if network == "apothem" {
		return params.XDCApothemChainConfig, nil
	}
	raw, err := c.call("admin_nodeInfo")
	if err != nil {
		return nil, err
	}
	var info struct {
		Protocols struct {
			Eth struct {
				Config params.ChainConfig `json:"config"`
			} `json:"eth"`
		} `json:"protocols"`
	}
	if err := json.Unmarshal(raw, &info); err != nil {
		return nil, err
	}
	return &info.Protocols.Eth.Config, nil
}

type plan struct {
	class map[uint64]string // first class that asked for a block wins
	head  uint64
	rng   *rand.Rand
}

func (p *plan) add(class string, n uint64) {
	if _, ok := p.class[n]; !ok && n <= p.head {
		p.class[n] = class
	}
}

// random adds up to count unseen blocks drawn uniformly from [lo, hi].
func (p *plan) random(class string, count int, lo, hi uint64) {
	if hi > p.head {
		hi = p.head
	}
	for ; count > 0 && lo <= hi; count-- {
		for tries := 0; tries < 1000; tries++ {
			n := lo + uint64(p.rng.Int63n(int64(hi-lo+1)))
			if _, ok := p.class[n]; !ok {
				p.class[n] = class
				break
			}
		}
	}
}

// forkRows are the fork block numbers of the generated table: the config
// fields ending in "Block", the effective switches, and the V2 switch.
func forkRows(cfg *params.ChainConfig, withEffective bool) []uint64 {
	rows := blocks(cfg)
	if withEffective {
		rows = append(rows, effective(cfg)...)
	}
	rows = append(rows, kv{"v2Switch", dec(cfg.XDPoS.V2.SwitchBlock)})
	var out []uint64
	for _, r := range rows {
		if s, ok := r.V.(string); ok {
			if n, err := strconv.ParseUint(s, 10, 64); err == nil && n != unscheduled {
				out = append(out, n)
			}
		}
	}
	return out
}

// sample builds the block list. The rule is documented in the README next to
// the vectors; keep the two in step.
func sample(network string, cfg *params.ChainConfig, head uint64, v2Switches []uint64, total int, seed int64) *plan {
	p := &plan{class: map[uint64]string{}, head: head, rng: rand.New(rand.NewSource(seed))}
	sw := cfg.XDPoS.V2.SwitchBlock.Uint64()
	epoch := cfg.XDPoS.Epoch
	p.add("genesis", 0)
	for n := uint64(1); n <= 10; n++ {
		p.add("early", n)
	}
	for _, b := range forkRows(cfg, network == "apothem") {
		for _, n := range []uint64{b - 1, b, b + 1} {
			if b > 0 {
				p.add("fork", n)
			}
		}
	}
	// Epoch switches around the V2 switch (20 per side, 10 per side outside
	// Apothem): the V1 checkpoints, which are the multiples of the epoch length
	// at and below the switch block, and the first V2 epoch switches the node
	// lists. V2 epochs are round based, so their block numbers drift.
	span := 10
	if network == "apothem" {
		span = 20
	}
	for k := 0; k <= span && uint64(k)*epoch <= sw; k++ {
		p.add("epoch", sw-uint64(k)*epoch)
	}
	for i := 0; i < span && i < len(v2Switches); i++ {
		p.add("epoch", v2Switches[i])
	}
	for n := head - 49; n <= head; n++ {
		p.add("latest", n)
	}
	randoms := map[string]int{"v1": 200, "v2": 200, "cancun": 200, "prague": 200}
	london, cancun, prague := cfg.Eip1559Block.Uint64(), cfg.CancunBlock.Uint64(), cfg.PragueBlock.Uint64()
	if network != "apothem" {
		randoms = map[string]int{"v1": 40, "v2": 60, "amsterdam": 100}
	}
	p.random("v1", randoms["v1"], 1, sw)
	if network == "apothem" {
		p.random("v2", randoms["v2"], sw+1, london-1)
		p.random("cancun", randoms["cancun"], cancun, prague-1)
		p.random("prague", randoms["prague"], prague, head)
	} else {
		ams := cfg.AmsterdamBlock.Uint64()
		p.random("v2", randoms["v2"], sw+1, ams-1)
		p.random("amsterdam", randoms["amsterdam"], ams, head)
	}
	p.random("filler", total-len(p.class), 1, head)
	return p
}

// oracle holds what pinned geth-XDC computes for one header.
type oracle struct {
	Consensus       string  `json:"consensus"`
	SealHash        string  `json:"sealHash"`
	V2SigHash       string  `json:"v2SigHash"`
	HashNoValidator string  `json:"hashNoValidator"`
	Signer          *string `json:"signer"` // null where geth has no signer for the header
}

func compute(eng *XDPoS.XDPoS, raw json.RawMessage, canonical string) (oracle, error) {
	var h types.Header
	if err := json.Unmarshal(raw, &h); err != nil {
		return oracle{}, err
	}
	if got := h.Hash().Hex(); got != canonical {
		return oracle{}, fmt.Errorf("geth Header.Hash() %s differs from archive hash %s", got, canonical)
	}
	o := oracle{
		Consensus:       "v1",
		SealHash:        XDPoS.SealHash(&h).Hex(),
		V2SigHash:       (*engine_v2.XDPoS_v2)(nil).SignHash(&h).Hex(), // the method does not read its receiver
		HashNoValidator: h.HashNoValidator().Hex(),
	}
	var signer common.Address
	if eng.IsV2Block(&h) {
		// engine_v2.ecrecover: the signature is header.Validator over SignHash.
		o.Consensus = "v2"
		pub, err := crypto.Ecrecover(common.HexToHash(o.V2SigHash).Bytes(), h.Validator)
		if err != nil {
			return o, nil
		}
		copy(signer[:], crypto.Keccak256(pub[1:])[12:])
	} else {
		var err error
		if signer, err = eng.Author(&h); err != nil {
			return o, nil
		}
	}
	s := strings.ToLower(signer.Hex())
	o.Signer = &s
	return o, nil
}

func headers(args []string) {
	fs := flag.NewFlagSet("headers", flag.ExitOnError)
	network := fs.String("network", "", "apothem, or any name for a node that answers admin_nodeInfo")
	url := fs.String("rpc", "", "archive JSON-RPC URL")
	out := fs.String("out", "", "output .jsonl file")
	total := fs.Int("total", 2000, "number of headers")
	seed := fs.Int64("seed", 1, "seed of the random samples")
	headFlag := fs.Uint64("head", 0, "fix the head block (default: ask the node)")
	batchSize := fs.Int("batch", 5, "blocks per JSON-RPC batch")
	interval := fs.Duration("interval", time.Second, "pause between batches")
	fs.Parse(args)
	if *network == "" || *url == "" || *out == "" {
		fatal(fmt.Errorf("-network, -rpc and -out are required"))
	}
	c := rpcClient{*url}

	ver, err := c.call("web3_clientVersion")
	if err != nil { // the net2040 archive does not serve the web3 namespace
		var info struct{ Name string }
		raw, err2 := c.call("admin_nodeInfo")
		fatal(err2)
		fatal(json.Unmarshal(raw, &info))
		ver, _ = json.Marshal(info.Name + " (admin_nodeInfo name; web3_clientVersion is not served)")
	}
	chainID, err := c.call("eth_chainId")
	fatal(err)
	cfg, err := chainConfig(*network, c)
	fatal(err)
	head := *headFlag
	if head == 0 {
		raw, err := c.call("eth_blockNumber")
		fatal(err)
		var s string
		fatal(json.Unmarshal(raw, &s))
		n, ok := new(big.Int).SetString(strings.TrimPrefix(s, "0x"), 16)
		if !ok {
			fatal(fmt.Errorf("bad block number %q", s))
		}
		head = n.Uint64()
	}
	fmt.Fprintf(os.Stderr, "clientVersion %s\nchainId %s\nhead %d\n", ver, chainID, head)

	// XDPoS_getEpochNumbersBetween: the epoch-switch blocks after the V2 switch.
	sw, epoch := cfg.XDPoS.V2.SwitchBlock.Uint64(), cfg.XDPoS.Epoch
	raw, err := c.call("XDPoS_getEpochNumbersBetween", fmt.Sprintf("0x%x", sw+1), fmt.Sprintf("0x%x", min(sw+25*epoch, head)))
	fatal(err)
	var v2Switches []uint64
	fatal(json.Unmarshal(raw, &v2Switches))

	p := sample(*network, cfg, head, v2Switches, *total, *seed)
	nums := make([]uint64, 0, len(p.class))
	for n := range p.class {
		nums = append(nums, n)
	}
	sort.Slice(nums, func(i, j int) bool { return nums[i] < nums[j] })

	eng := XDPoS.New(cfg.XDPoS, nil)
	var buf bytes.Buffer
	counts := map[string]int{}
	for i := 0; i < len(nums); i += *batchSize {
		chunk := nums[i:min(i+*batchSize, len(nums))]
		reqs := make([]map[string]any, len(chunk))
		for j, n := range chunk {
			reqs[j] = map[string]any{"jsonrpc": "2.0", "id": j, "method": "eth_getBlockByNumber", "params": []any{fmt.Sprintf("0x%x", n), false}}
		}
		answers, err := c.batch(reqs)
		fatal(err)
		for j, a := range answers {
			var obj map[string]json.RawMessage
			if err := json.Unmarshal(a, &obj); err != nil || obj == nil {
				fatal(fmt.Errorf("block %d: no block in answer %.100s", chunk[j], a))
			}
			for _, k := range dropped {
				delete(obj, k)
			}
			var hash string
			fatal(json.Unmarshal(obj["hash"], &hash))
			// Unmarshal into geth's Header must not see body keys, so it uses obj.
			hdr, _ := json.Marshal(obj)
			o, err := compute(eng, hdr, hash)
			if err != nil {
				fatal(fmt.Errorf("block %d: %v", chunk[j], err))
			}
			obj["xone"], _ = json.Marshal(struct {
				Class string `json:"class"`
				oracle
			}{p.class[chunk[j]], o})
			line, err := json.Marshal(obj)
			fatal(err)
			buf.Write(line)
			buf.WriteByte('\n')
			counts[p.class[chunk[j]]]++
		}
		time.Sleep(*interval)
	}
	fatal(os.WriteFile(*out, buf.Bytes(), 0o644))
	fmt.Fprintf(os.Stderr, "wrote %d headers, %d bytes, classes %v\n", len(nums), buf.Len(), counts)
}
