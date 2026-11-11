// Command bftoracle runs the pinned geth-XDC V2 BFT code on explicit inputs and prints what it decides.
// The unexported engine functions are reached through overlay files (export.go.txt, countdown.go.txt; see
// run.sh), so the functions called are the ones the node runs:
//
//	timer      setNewRound + ExpCountDown.Reset/GetTimeout/onTimeout    (engine.go:3220, countdown.go:75-148)
//	rule       verifyVotingRule and isExtendingFromAncestor             (vote.go:404-462)
//	forensics  ForensicsMonitoring: ProcessForensics, SetCommittedQCs, SendForensicProof, generateForensicsId (forensics.go:50-431)
//
//	bftoracle timer <forks.json>... > out.json    (every other input is generated from a fixed seed in this file)
//	bftoracle rule > out.json
//	bftoracle forensics > out.json
package main

import (
	"encoding/json"
	"fmt"
	"math/big"
	"math/rand"
	"os"
	"sort"
	"time"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/consensus/XDPoS/engines/engine_v2"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/ethereum/go-ethereum/params"
)

func must(err error) {
	if err != nil {
		panic(err)
	}
}

func errStr(err error) *string {
	if err == nil {
		return nil
	}
	s := err.Error()
	return &s
}

func v2header(number uint64, parent common.Hash, round uint64, qc *types.QuorumCert) *types.Header {
	if qc == nil {
		qc = &types.QuorumCert{ProposedBlockInfo: &types.BlockInfo{Number: big.NewInt(0)}, Signatures: []types.Signature{}}
	}
	extra, err := (&types.ExtraFields_v2{Round: types.Round(round), QuorumCert: qc}).EncodeToBytes()
	must(err)
	return &types.Header{Number: new(big.Int).SetUint64(number), ParentHash: parent, Extra: extra,
		Difficulty: big.NewInt(1), Time: 1000 + number}
}

func qcOf(h *types.Header, round uint64) *types.QuorumCert {
	return &types.QuorumCert{ProposedBlockInfo: &types.BlockInfo{Hash: h.Hash(), Round: types.Round(round), Number: new(big.Int).Set(h.Number)},
		Signatures: []types.Signature{}}
}

func info(hash common.Hash, round, number uint64) *types.BlockInfo {
	return &types.BlockInfo{Hash: hash, Round: types.Round(round), Number: new(big.Int).SetUint64(number)}
}

func config(rows map[uint64]*params.V2Config) *params.ChainConfig {
	return &params.ChainConfig{ChainID: big.NewInt(5151), XDPoS: &params.XDPoSConfig{
		Period: 2, Epoch: 900, Gap: 450,
		V2: &params.V2{SwitchBlock: big.NewInt(0), SwitchEpoch: 1, AllConfigs: rows, CurrentConfig: rows[0]},
	}}
}

// ---------------------------------------------------------------- timer

type Row struct {
	Key          uint64  `json:"key"`
	Period       int     `json:"timeout_period"`
	Base         float64 `json:"base"`
	MaxExponent  uint8   `json:"max_exponent"`
	SyncThresold int     `json:"timeout_sync_threshold"`
	MinePeriod   int     `json:"mine_period"`
}

type Step struct {
	Round uint64 `json:"round"`
	Qc    uint64 `json:"qc_round"`
	// Delays in nanoseconds: the timer after setNewRound, then after each expiry.
	Delays []int64 `json:"delays_ns"`
}

type TimerSet struct {
	Name      string `json:"name"`
	Rows      []Row  `json:"rows"`
	InitialNs int64  `json:"initial_ns"`
	Steps     []Step `json:"steps"`
}

func rowsOf(rows []Row) map[uint64]*params.V2Config {
	m := map[uint64]*params.V2Config{}
	for _, r := range rows {
		m[r.Key] = &params.V2Config{SwitchRound: r.Key, TimeoutPeriod: r.Period, TimeoutSyncThreshold: r.SyncThresold, MinePeriod: r.MinePeriod,
			CertThreshold: 0.667, MaxMasternodes: 108, ExpTimeoutConfig: params.ExpTimeoutConfig{Base: r.Base, MaxExponent: r.MaxExponent}}
	}
	return m
}

// scenario runs setNewRound for each (round, qc) in order on one engine and fires the timer `fires` times after each.
func scenario(name string, rows []Row, plan [][2]uint64, fires int) TimerSet {
	x := engine_v2.OracleEngine(config(rowsOf(rows)))
	out := TimerSet{Name: name, Rows: rows, InitialNs: int64(x.OracleInitial())}
	for _, p := range plan {
		s := Step{Round: p[0], Qc: p[1]}
		s.Delays = append(s.Delays, int64(x.OracleNewRound(p[0], p[1])))
		for i := 0; i < fires; i++ {
			s.Delays = append(s.Delays, int64(x.OracleFire()))
		}
		out.Steps = append(out.Steps, s)
	}
	return out
}

type forksFile struct {
	Networks map[string]struct {
		Xdpos struct {
			V2 struct {
				AllConfigs map[string]struct {
					MinePeriod           int `json:"minePeriod"`
					TimeoutSyncThreshold int `json:"timeoutSyncThreshold"`
					TimeoutPeriod        int `json:"timeoutPeriod"`
					ExpTimeoutConfig     struct {
						Base        float64 `json:"base"`
						MaxExponent uint8   `json:"maxExponent"`
					} `json:"expTimeoutConfig"`
				} `json:"allConfigs"`
			} `json:"v2"`
		} `json:"xdpos"`
	} `json:"networks"`
}

func genTimer(paths []string) []TimerSet {
	var out []TimerSet
	for _, p := range paths {
		b, err := os.ReadFile(p)
		must(err)
		var f forksFile
		must(json.Unmarshal(b, &f))
		names := []string{}
		for n := range f.Networks {
			names = append(names, n)
		}
		sort.Strings(names)
		for _, n := range names {
			var rows []Row
			for k, c := range f.Networks[n].Xdpos.V2.AllConfigs {
				var key uint64
				_, err := fmt.Sscan(k, &key)
				must(err)
				rows = append(rows, Row{key, c.TimeoutPeriod, c.ExpTimeoutConfig.Base, c.ExpTimeoutConfig.MaxExponent, c.TimeoutSyncThreshold, c.MinePeriod})
			}
			sort.Slice(rows, func(i, j int) bool { return rows[i].Key < rows[j].Key })
			// every row: its first round, one after, and a later one, with QCs 1, 2, 5 and 300 rounds behind
			var plan [][2]uint64
			for _, r := range rows {
				for _, d := range []uint64{0, 1, 7} {
					for _, back := range []uint64{1, 2, 5, 300} {
						round := r.Key + d + 1
						plan = append(plan, [2]uint64{round, round - back})
					}
				}
			}
			out = append(out, scenario(n, rows, plan, 5))
		}
	}
	// Synthetic rows: exponential bases, a base below 1 (geth keeps the old base), an unset base,
	// a QC above the round, and the exponent cap.
	syn := []Row{
		{0, 10, 1.0, 0, 3, 2}, {100, 10, 2.0, 6, 3, 2}, {200, 7, 1.5, 4, 2, 1}, {300, 60, 0.5, 3, 3, 2},
		{400, 60, 0, 3, 3, 2}, {500, 5, 1.2, 10, 3, 2}, {600, 3, 1.0, 0, 3, 2}, {700, 20, 1.7, 5, 2, 2},
		{800, 30, 2.0, 8, 2, 2}, {900, 1, 1.1, 20, 2, 1},
	}
	var plan [][2]uint64
	for _, r := range syn {
		for _, back := range []uint64{0, 1, 2, 3, 5, 9, 21} {
			plan = append(plan, [2]uint64{r.Key + 30, r.Key + 30 - back})
		}
		plan = append(plan, [2]uint64{r.Key + 30, r.Key + 40}) // QC above the round
	}
	out = append(out, scenario("synthetic", syn, plan, 12))
	// the same rows with the unset first row: the engine starts with base 2 and max exponent 6
	syn2 := []Row{{0, 10, 0, 0, 3, 2}, {50, 10, 0, 0, 3, 2}, {60, 10, 3.0, 0, 3, 2}}
	out = append(out, scenario("unset-first-row", syn2, [][2]uint64{{5, 1}, {60, 55}, {61, 55}, {70, 1}}, 8))
	return out
}

// ---------------------------------------------------------------- rule

type Ref struct {
	Hash   common.Hash `json:"hash"`
	Round  uint64      `json:"round"`
	Number uint64      `json:"number"`
}

type Hdr struct {
	Hash   common.Hash `json:"hash"`
	Number uint64      `json:"number"`
	Parent common.Hash `json:"parent"`
}

type RuleCase struct {
	ID           string  `json:"id"`
	Round        uint64  `json:"round"`
	HighestVoted uint64  `json:"highest_voted_round"`
	Lock         *Ref    `json:"lock"`
	Block        Ref     `json:"block"`
	QcRound      uint64  `json:"qc_round"`
	Vote         bool    `json:"vote"`
	Err          *string `json:"err"`
}

type RuleOut struct {
	Headers []Hdr      `json:"headers"`
	Cases   []RuleCase `json:"cases"`
	// The net2040 shape of xOneGo ee9b750f: N@R timed out, N'@R+1 at the same height.
	Sibling SiblingOut `json:"sibling"`
}

type SiblingOut struct {
	Headers []Hdr      `json:"headers"`
	Names   []string   `json:"names"` // header name for each entry of Headers
	Cases   []RuleCase `json:"cases"`
}

func ruleCase(x *engine_v2.XDPoS_v2, chain *engine_v2.OracleChain, id string, round, hv uint64, lock *Ref, block Ref, qcRound uint64) RuleCase {
	var l *types.QuorumCert
	if lock != nil {
		l = &types.QuorumCert{ProposedBlockInfo: info(lock.Hash, lock.Round, lock.Number)}
	}
	ok, err := x.OracleVotingRule(chain, round, hv, l, info(block.Hash, block.Round, block.Number), &types.QuorumCert{ProposedBlockInfo: info(common.Hash{}, qcRound, 0)})
	return RuleCase{ID: id, Round: round, HighestVoted: hv, Lock: lock, Block: block, QcRound: qcRound, Vote: ok, Err: errStr(err)}
}

func genRule() RuleOut {
	cfg := config(map[uint64]*params.V2Config{0: {MaxMasternodes: 10, CertThreshold: 0.667, MinePeriod: 2, TimeoutPeriod: 10, TimeoutSyncThreshold: 3}})
	x := engine_v2.OracleEngine(cfg)
	chain := engine_v2.NewOracleChain(cfg)
	r := rand.New(rand.NewSource(15))
	// a main chain of 24 blocks and four forks from it
	var all []*types.Header
	var hdrs []Hdr
	add := func(h *types.Header) {
		chain.Add(h)
		all = append(all, h)
		hdrs = append(hdrs, Hdr{h.Hash(), h.Number.Uint64(), h.ParentHash})
	}
	var main []*types.Header
	prev := common.Hash{}
	for n := uint64(100); n < 124; n++ {
		h := v2header(n, prev, n-99, nil)
		add(h)
		main = append(main, h)
		prev = h.Hash()
	}
	for f := 0; f < 4; f++ {
		base := main[3+f*5]
		p := base.Hash()
		for n := base.Number.Uint64() + 1; n < base.Number.Uint64()+1+uint64(3+f); n++ {
			h := v2header(n, p, n-99+uint64(f)+1, nil)
			h.Time += uint64(f) + 1 // another hash than the main chain's block of that height
			add(h)
			p = h.Hash()
		}
	}
	ref := func(h *types.Header, round uint64) Ref { return Ref{h.Hash(), round, h.Number.Uint64()} }
	var out RuleOut
	out.Headers = hdrs
	for i := 0; i < 3000; i++ {
		blk := all[r.Intn(len(all))]
		round := uint64(3 + r.Intn(37))
		hv := round - 1 - uint64(r.Intn(3)) // below the round, so most cases reach the lock
		if r.Intn(6) == 0 {
			hv = uint64(r.Intn(40))
		}
		b := ref(blk, round)
		if r.Intn(10) == 0 {
			b.Round = uint64(r.Intn(40)) // the block's round is not the current round
		}
		var lock *Ref
		if r.Intn(5) != 0 {
			lh := all[r.Intn(len(all))]
			l := ref(lh, uint64(r.Intn(40)))
			lock = &l
		}
		if r.Intn(40) == 0 {
			b.Hash = common.Hash{byte(i), 0xee} // a block the chain does not hold
		}
		out.Cases = append(out.Cases, ruleCase(x, chain, fmt.Sprintf("r%d", i), round, hv, lock, b, uint64(r.Intn(40))))
	}
	// edges of each comparison, on a fixed block and lock
	blk, lk := main[10], main[6]
	for _, round := range []uint64{4, 5, 6} {
		for _, hv := range []uint64{4, 5, 6} {
			for _, qc := range []uint64{4, 5, 6} {
				out.Cases = append(out.Cases, ruleCase(x, chain, fmt.Sprintf("edge-%d-%d-%d", round, hv, qc), round, hv, &Ref{lk.Hash(), 5, lk.Number.Uint64()}, ref(blk, 5), qc))
			}
		}
	}
	out.Sibling = genSibling()
	return out
}

// genSibling is the shape of the net2040 incident at heights 619,712 and 619,713: block A at height 619,712
// in round R is voted; its round times out; block A' at the same height, with the same parent, comes in R+1
// and is voted; a child of A' follows in R+2. Heights are the real ones, rounds and hashes are made up.
func genSibling() SiblingOut {
	cfg := config(map[uint64]*params.V2Config{0: {MaxMasternodes: 10, CertThreshold: 0.667, MinePeriod: 2, TimeoutPeriod: 10, TimeoutSyncThreshold: 3}})
	x := engine_v2.OracleEngine(cfg)
	chain := engine_v2.NewOracleChain(cfg)
	const R = uint64(1000)
	var out SiblingOut
	hs := map[string]*types.Header{}
	add := func(name string, h *types.Header) {
		chain.Add(h)
		hs[name] = h
		out.Headers = append(out.Headers, Hdr{h.Hash(), h.Number.Uint64(), h.ParentHash})
		out.Names = append(out.Names, name)
	}
	g := v2header(619710, common.Hash{1}, R-3, nil)
	add("grand", g)
	p := v2header(619711, g.Hash(), R-2, nil)
	add("parent", p)
	a := v2header(619712, p.Hash(), R, nil) // R-1 timed out between the parent and A
	add("A", a)
	a2 := v2header(619712, p.Hash(), R+1, nil)
	a2.Time += 1
	add("A'", a2)
	a3 := v2header(619712, p.Hash(), R+1, nil)
	a3.Time += 2
	add("A''", a3)
	c := v2header(619713, a2.Hash(), R+2, nil)
	add("child of A'", c)
	other := v2header(619711, g.Hash(), R-2, nil) // a fork below the parent
	other.Time += 3
	add("fork parent", other)
	f := v2header(619712, other.Hash(), R+1, nil)
	add("fork block", f)
	ref := func(n string, round uint64) Ref { h := hs[n]; return Ref{h.Hash(), round, h.Number.Uint64()} }
	lockP := ref("parent", R-2)
	lockA := ref("A", R)
	cases := []struct {
		id        string
		round, hv uint64
		lock      *Ref
		block     Ref
		qc        uint64
	}{
		{"A in its round, never voted", R, R - 2, &lockP, ref("A", R), R - 2},
		{"A' after the TC of R: voted A, timed out, round R+1", R + 1, R, &lockP, ref("A'", R+1), R - 2},
		{"A' arrives before the TC: current round still R", R, R, &lockP, ref("A'", R+1), R - 2},
		{"A'' after A' was voted in R+1", R + 1, R + 1, &lockP, ref("A''", R+1), R - 2},
		{"child of A' in R+2, QC of A'", R + 2, R + 1, &lockP, ref("child of A'", R+2), R + 1},
		{"A' with the lock on A: it does not extend A, its QC is not above", R + 1, R, &lockA, ref("A'", R+1), R - 2},
		{"A' with the lock on A and a QC above it", R + 1, R, &lockA, ref("A'", R+1), R + 1},
		{"fork block, lock on the parent of A: it does not extend it", R + 1, R, &lockP, ref("fork block", R+1), R - 2},
		{"fork block, no lock yet", R + 1, R, nil, ref("fork block", R+1), R - 2},
	}
	for _, k := range cases {
		out.Cases = append(out.Cases, ruleCase(x, chain, k.id, k.round, k.hv, k.lock, k.block, k.qc))
	}
	return out
}

// ---------------------------------------------------------------- forensics

type QcRec struct {
	Hash   common.Hash `json:"hash"`
	Round  uint64      `json:"round"`
	Number uint64      `json:"number"`
	Gap    uint64      `json:"gap"`
}

type FHdr struct {
	Hash   common.Hash `json:"hash"`
	Number uint64      `json:"number"`
	Parent common.Hash `json:"parent"`
	Round  uint64      `json:"round"`
	Qc     QcRec       `json:"qc"`
	// The epoch switch block of the block's epoch.
	Switch common.Hash `json:"switch"`
}

type Commit struct {
	Parent   common.Hash `json:"parent"`   // header of the block below the proposed one
	Proposed common.Hash `json:"proposed"` // header of the block the QC certifies
	Incoming QcRec       `json:"incoming"`
}

type Proof struct {
	ID              string      `json:"id"`
	Type            string      `json:"type"`
	DivergingHash   string      `json:"diverging_hash"`
	DivergingNumber uint64      `json:"diverging_number"`
	AcrossEpoch     bool        `json:"across_epoch"`
	SmallerPath     []string    `json:"smaller_path"`
	LargerPath      []string    `json:"larger_path"`
	SmallerQc       common.Hash `json:"smaller_qc_hash"`
	LargerQc        common.Hash `json:"larger_qc_hash"`
}

type FCase struct {
	ID     string  `json:"id"`
	First  Commit  `json:"first"`
	Second Commit  `json:"second"`
	Proofs []Proof `json:"proofs"` // events after the second commit
	First_ []Proof `json:"first_proofs"`
}

type FOut struct {
	Chains [][]FHdr `json:"chains"`
	Cases  []FCase  `json:"cases"`
}

func capture(ch chan types.ForensicsEvent) []Proof {
	var out []Proof
	for {
		select {
		case ev := <-ch:
			p := ev.ForensicsProof
			var c types.ForensicsContent
			must(json.Unmarshal([]byte(p.Content), &c))
			out = append(out, Proof{p.Id, p.ForensicsType, c.DivergingBlockHash, c.DivergingBlockNumber, c.AcrossEpoch,
				c.SmallerRoundInfo.HashPath, c.LargerRoundInfo.HashPath, c.SmallerRoundInfo.QuorumCert.ProposedBlockInfo.Hash, c.LargerRoundInfo.QuorumCert.ProposedBlockInfo.Hash})
		case <-time.After(150 * time.Millisecond):
			return out
		}
	}
}

func genForensics() FOut {
	cfg := config(map[uint64]*params.V2Config{0: {MaxMasternodes: 10, CertThreshold: 0.667, MinePeriod: 2, TimeoutPeriod: 10, TimeoutSyncThreshold: 3}})
	r := rand.New(rand.NewSource(7))
	var out FOut
	for c := 0; c < 150; c++ {
		x := engine_v2.OracleEngine(cfg)
		chain := engine_v2.NewOracleChain(cfg)
		// the main chain and a fork off it; a block's QC certifies its parent
		type blk struct {
			h     *types.Header
			round uint64
		}
		var chains [][]blk
		mk := func(parent *blk, number, round uint64, salt uint64) blk {
			var qc *types.QuorumCert
			p := common.Hash{}
			if parent != nil {
				qc = qcOf(parent.h, parent.round)
				p = parent.h.Hash()
			}
			h := v2header(number, p, round, qc)
			h.Time += salt
			chain.Add(h)
			return blk{h, round}
		}
		var main []blk
		round := uint64(1)
		for n := uint64(500); n < 520; n++ {
			round += 1
			if r.Intn(3) == 0 {
				round += uint64(r.Intn(3))
			}
			var par *blk
			if len(main) > 0 {
				par = &main[len(main)-1]
			}
			main = append(main, mk(par, n, round, 0))
		}
		chains = append(chains, main)
		fi := 3 + r.Intn(10)
		var fork []blk
		par := main[fi]
		fround := par.round
		for n := par.h.Number.Uint64() + 1; n < par.h.Number.Uint64()+1+uint64(4+r.Intn(8)); n++ {
			fround += 1
			if r.Intn(2) == 0 {
				fround += uint64(r.Intn(4))
			}
			p := par
			if len(fork) > 0 {
				p = fork[len(fork)-1]
			}
			fork = append(fork, mk(&p, n, fround, 77))
		}
		chains = append(chains, fork)
		// seed the epoch info of every block: epochs of 8 blocks, the switch block of an epoch is its first block
		var fh []FHdr
		epoch := func(n uint64) *types.BlockInfo {
			sn := n - n%8
			return info(crypto.Keccak256Hash([]byte(fmt.Sprintf("switch-%d", sn))), sn, sn)
		}
		for _, ch := range chains {
			for _, b := range ch {
				e := epoch(b.h.Number.Uint64())
				x.OracleSeedEpoch(b.h.Hash(), []common.Address{{1}}, e)
				q, _, _, _ := engineQC(b.h)
				fh = append(fh, FHdr{b.h.Hash(), b.h.Number.Uint64(), b.h.ParentHash, b.round, q, e.Hash})
			}
		}
		out.Chains = append(out.Chains, fh)
		commitOf := func(ch []blk, i int) (Commit, []types.Header, types.QuorumCert) {
			parent, prop := ch[i-1], ch[i]
			qc := qcOf(prop.h, prop.round)
			return Commit{parent.h.Hash(), prop.h.Hash(), QcRec{qc.ProposedBlockInfo.Hash, prop.round, prop.h.Number.Uint64(), 0}}, []types.Header{*parent.h, *prop.h}, *qc
		}
		run := func(f *engine_v2.Forensics, ch []blk, i int, sub chan types.ForensicsEvent) (Commit, []Proof) {
			cm, hs, qc := commitOf(ch, i)
			_ = f.ForensicsMonitoring(chain, x, hs, qc)
			return cm, capture(sub)
		}
		f := engine_v2.NewForensics()
		sub := make(chan types.ForensicsEvent, 8)
		f.SubscribeForensicsEvent(sub)
		i1 := fi + 3 + r.Intn(len(main)-fi-3) // the oldest QC of the commit is above the fork point, so the fork conflicts with it
		first, fp := run(f, main, i1, sub)
		var second Commit
		var sp []Proof
		switch r.Intn(6) {
		case 0: // the same chain, later or earlier
			second, sp = run(f, main, 3+r.Intn(len(main)-3), sub)
		default: // the fork: the oldest QC of its commit is above the fork point
			second, sp = run(f, fork, 2+r.Intn(len(fork)-2), sub)
		}
		// the same conflict again: still one event per commit (the Rust machine reports an id once)
		out.Cases = append(out.Cases, FCase{ID: fmt.Sprintf("f%d", c), First: first, Second: second, Proofs: sp, First_: fp})
	}
	return out
}

func engineQC(h *types.Header) (QcRec, uint64, bool, error) {
	var d types.ExtraFields_v2
	if err := engine_v2.DecodeExtraFields(h.Extra, &d); err != nil {
		return QcRec{}, 0, false, err
	}
	b := d.QuorumCert.ProposedBlockInfo
	return QcRec{b.Hash, uint64(b.Round), b.Number.Uint64(), d.QuorumCert.GapNumber}, uint64(d.Round), true, nil
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: bftoracle timer <forks.json>... | rule | forensics")
		os.Exit(2)
	}
	var v interface{}
	switch os.Args[1] {
	case "timer":
		v = genTimer(os.Args[2:])
	case "rule":
		v = genRule()
	case "forensics":
		v = genForensics()
	default:
		panic(os.Args[1])
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", " ")
	must(enc.Encode(v))
}
