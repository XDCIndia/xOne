// Command v2oracle runs the pinned geth-XDC V2 engine on explicit inputs and prints what it decides.
// The unexported engine functions are reached through an overlay file (export.go.txt, see run.sh), so the
// functions called are the ones the node runs:
//
//	sort    state.GetCandidates + sortCandidatesByStake on a real StateDB   (engine.go:2169)
//	calc    calcMasternodes and GetStandbynodes on a seeded gap snapshot    (engine.go:2023, :3351)
//	epoch   IsEpochSwitch and isEpochSwitchAtRound                          (engine.go:1952, timeout.go:505)
//	cert    verifyQC, verifyTC, VerifySyncInfoMessage and rlp.DecodeBytes    (utils.go:315, timeout.go:373, utils.go:672)
//	commit  commitBlocks                                                    (utils.go:530)
//	midadd  yourturnAligned across a mid-epoch masternode addition          (mining.go:24)
//
//	v2oracle <command> > out.json        (every input is generated from a fixed seed in this file)
package main

import (
	"crypto/ecdsa"
	"encoding/json"
	"fmt"
	"math/big"
	"math/rand"
	"os"
	"sort"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/common/hexutil"
	"github.com/ethereum/go-ethereum/consensus/XDPoS/engines/engine_v2"
	"github.com/ethereum/go-ethereum/core/state"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/ethereum/go-ethereum/params"
	"github.com/ethereum/go-ethereum/rlp"
)

type H = hexutil.Bytes

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

type CfgIn struct {
	Epoch       uint64            `json:"epoch"`
	Gap         uint64            `json:"gap"`
	SwitchBlock uint64            `json:"switch_block"`
	SwitchEpoch uint64            `json:"switch_epoch"`
	Configs     map[uint64]CfgRow `json:"configs"` // by switch round
}
type CfgRow struct {
	MaxMasternodes int     `json:"max_masternodes"`
	CertThreshold  float64 `json:"cert_threshold"`
	MinePeriod     int     `json:"mine_period"`
}

func chainConfig(c CfgIn) *params.ChainConfig {
	all := map[uint64]*params.V2Config{}
	for r, row := range c.Configs {
		all[r] = &params.V2Config{SwitchRound: r, MaxMasternodes: row.MaxMasternodes, CertThreshold: row.CertThreshold, MinePeriod: row.MinePeriod, TimeoutPeriod: 10}
	}
	return &params.ChainConfig{ChainID: big.NewInt(5151), XDPoS: &params.XDPoSConfig{
		Period: 2, Epoch: c.Epoch, Gap: c.Gap,
		V2: &params.V2{SwitchBlock: new(big.Int).SetUint64(c.SwitchBlock), SwitchEpoch: c.SwitchEpoch, AllConfigs: all, CurrentConfig: all[0]},
	}}
}

func addr(r *rand.Rand) common.Address {
	var a common.Address
	r.Read(a[:])
	return a
}

func addrs(l []common.Address) []string {
	out := make([]string, len(l))
	for i, a := range l {
		out[i] = a.Hex()
	}
	return out
}

func v2header(number uint64, parent common.Hash, round uint64, qc *types.QuorumCert, validators, penalties []byte) *types.Header {
	if qc == nil {
		qc = &types.QuorumCert{ProposedBlockInfo: &types.BlockInfo{Number: big.NewInt(0)}, Signatures: []types.Signature{}}
	}
	extra, err := (&types.ExtraFields_v2{Round: types.Round(round), QuorumCert: qc}).EncodeToBytes()
	must(err)
	return &types.Header{Number: new(big.Int).SetUint64(number), ParentHash: parent, Extra: extra, Validators: validators, Penalties: penalties,
		Difficulty: big.NewInt(1), Time: 1000 + number}
}

func pack(l []common.Address) []byte {
	var b []byte
	for _, a := range l {
		b = append(b, a[:]...)
	}
	return b
}

// ---------------------------------------------------------------- sort

// Word is a storage slot and its value.
type Word [2]common.Hash

type SortCase struct {
	ID       string           `json:"id"`
	Kind     string           `json:"kind"`
	Words    []Word           `json:"words"` // the 0x88 storage the case wrote
	Order    []common.Address `json:"array"` // candidates array, zero words included as zero addresses
	Expected []common.Address `json:"expected"`
}

// stateOf writes the candidates array and the caps of 0x88 into a real StateDB, as the contract lays them out
// (state/statedb_utils.go:95-130), and returns the words it wrote.
func stateOf(arr []common.Address, caps map[common.Address]*big.Int) (*state.StateDB, []Word) {
	st, err := state.New(types.EmptyRootHash, state.NewDatabaseForTesting())
	must(err)
	smc := common.MasternodeVotingSMCBinary
	var words []Word
	put := func(k, v common.Hash) {
		st.SetState(smc, k, v)
		words = append(words, Word{k, v})
	}
	slot := common.BigToHash(big.NewInt(8))
	put(slot, common.BigToHash(big.NewInt(int64(len(arr)))))
	for i, a := range arr {
		put(state.GetLocDynamicArrAtElement(slot, uint64(i), 1), common.BytesToHash(a.Bytes()))
		if a == (common.Address{}) {
			continue
		}
		if c, ok := caps[a]; ok {
			loc := state.GetLocMappingAtKey(common.BytesToHash(a.Bytes()), 1)
			put(common.BigToHash(loc.Add(loc, big.NewInt(1))), common.BigToHash(c))
		}
	}
	// the zero-address element has no state of its own; an element word that is zero is a hole in the array
	return st, words
}

func capsOf(r *rand.Rand, arr []common.Address, kind string) map[common.Address]*big.Int {
	caps := map[common.Address]*big.Int{}
	e18 := new(big.Int).Exp(big.NewInt(10), big.NewInt(18), nil)
	for _, a := range arr {
		var units int64
		switch kind {
		case "distinct":
			units = 1 + r.Int63n(1<<40)
		case "ties":
			units = []int64{10_000_000, 10_000_000, 10_000_000, 20_000_000, 50_000_000}[r.Intn(5)]
		default: // mainnet-like: most candidates at the minimum stake or a few multiples of it
			units = 10_000_000 * []int64{1, 1, 1, 1, 1, 1, 2, 2, 3, 5, 10, 25}[r.Intn(12)]
		}
		caps[a] = new(big.Int).Mul(big.NewInt(units), e18)
	}
	return caps
}

func genSort() []SortCase {
	r := rand.New(rand.NewSource(701))
	var out []SortCase
	for _, n := range []int{1, 2, 3, 7, 12, 13, 47, 108, 109, 464} {
		for _, kind := range []string{"distinct", "ties", "mainnet"} {
			arr := make([]common.Address, n)
			for i := range arr {
				arr[i] = addr(r)
			}
			// a hole in the array (a resigned candidate): the reader skips it
			if n >= 7 {
				arr[n/2] = common.Address{}
			}
			st, words := stateOf(arr, capsOf(r, arr, kind))
			got := engine_v2.OracleSort(st, stateCandidates(st))
			out = append(out, SortCase{fmt.Sprintf("n%d-%s", n, kind), kind, words, arr, got})
		}
	}
	return out
}

func stateCandidates(st *state.StateDB) []common.Address { return state.GetCandidates(st) }

// ---------------------------------------------------------------- calc

type CalcCase struct {
	ID         string           `json:"id"`
	Cfg        CfgIn            `json:"cfg"`
	Number     uint64           `json:"number"`
	Round      uint64           `json:"round"`
	Pool       []common.Address `json:"pool"`      // gap snapshot candidates, stake order
	V1List     []common.Address `json:"v1_list"`   // extra data list of the V1 switch block (first V2 block only)
	Penalties  []common.Address `json:"penalties"` // what the penalty hook returns
	Masternode []common.Address `json:"masternodes"`
	PenOut     []common.Address `json:"penalties_out"`
	Standby    []common.Address `json:"standby"`
	Err        *string          `json:"error"`
}

func runCalc(id string, cfg CfgIn, number, round uint64, pool, v1list, pens []common.Address) CalcCase {
	cc := chainConfig(cfg)
	x := engine_v2.OracleEngine(cc)
	chain := engine_v2.NewOracleChain(cc)
	// gap block of the snapshot calcMasternodes reads (engine.go:915-927 for a number that is not a gap number)
	snapNum := number
	if snapNum%cfg.Epoch == cfg.Epoch-cfg.Gap {
		snapNum--
	}
	gapNum := snapNum - snapNum%cfg.Epoch
	if gapNum > cfg.Gap {
		gapNum -= cfg.Gap
	} else {
		gapNum = 0
	}
	gap := &types.Header{Number: new(big.Int).SetUint64(gapNum), Extra: []byte{1}, Difficulty: big.NewInt(1)}
	chain.Add(gap)
	chain.Head = gap
	x.OracleSeed(gap, pool)
	if len(v1list) > 0 { // the V1 switch block carries its masternodes in extra data (vanity, list, seal)
		extra := append(append(make([]byte, 32), pack(v1list)...), make([]byte, 65)...)
		chain.Add(&types.Header{Number: new(big.Int).SetUint64(cfg.SwitchBlock), Extra: extra, Difficulty: big.NewInt(1)})
	}
	parent := common.Hash{1}
	mn, po, _, err := x.OracleCalc(chain, number, parent, round, pens)
	c := CalcCase{ID: id, Cfg: cfg, Number: number, Round: round, Pool: pool, V1List: v1list, Penalties: pens, Masternode: mn, PenOut: po, Err: errStr(err)}
	if err == nil {
		sw := &types.Header{Number: new(big.Int).SetUint64(number), Validators: pack(mn), Penalties: pack(po), Extra: []byte{1}, Difficulty: big.NewInt(1)}
		c.Standby = x.OracleStandby(chain, sw)
	}
	return c
}

func genCalc() []CalcCase {
	r := rand.New(rand.NewSource(675))
	two := map[uint64]CfgRow{0: {MaxMasternodes: 108, CertThreshold: 0.667, MinePeriod: 2}}
	cfg := CfgIn{Epoch: 900, Gap: 450, SwitchBlock: 56_828_700, SwitchEpoch: 63_142, Configs: two}
	cfgRound := CfgIn{Epoch: 900, Gap: 450, SwitchBlock: 56_828_700, SwitchEpoch: 63_142,
		Configs: map[uint64]CfgRow{0: {MaxMasternodes: 50, CertThreshold: 0.667, MinePeriod: 2}, 1000: {MaxMasternodes: 30, CertThreshold: 0.667, MinePeriod: 2}, 5000: {MaxMasternodes: 90, CertThreshold: 0.667, MinePeriod: 2}}}
	pool := func(n int) []common.Address {
		p := make([]common.Address, n)
		for i := range p {
			p[i] = addr(r)
		}
		return p
	}
	pick := func(p []common.Address, k int) []common.Address {
		idx := r.Perm(len(p))[:k]
		sort.Ints(idx)
		out := make([]common.Address, k)
		for i, j := range idx {
			out[i] = p[j]
		}
		return out
	}
	big464, p47, p20 := pool(464), pool(47), pool(20)
	S := uint64(60_000_123) // an epoch switch that is not at a block multiple of the epoch
	var out []CalcCase
	add := func(id string, cf CfgIn, n, round uint64, p, v1, pens []common.Address) {
		out = append(out, runCalc(id, cf, n, round, p, v1, pens))
	}
	add("no-penalties-under-cap", cfg, S, 2700, p47, nil, nil)
	add("penalties-under-cap", cfg, S, 2700, p47, nil, pick(p47, 14))
	add("cap-binds-no-penalties", cfg, S, 2700, big464, nil, nil)
	add("cap-binds-with-penalties", cfg, S, 2700, big464, nil, pick(big464[:150], 30))
	add("cap-binds-penalty-below-cap", cfg, S, 2700, big464, nil, pick(big464[200:], 20))
	add("penalty-not-in-pool", cfg, S, 2700, p47, nil, pool(3))
	add("every-candidate-penalised", cfg, S, 2700, p20, nil, p20)
	add("empty-pool", cfg, S, 2700, nil, nil, nil)
	add("cap-config-round-0", cfgRound, S, 999, big464, nil, nil)
	add("cap-config-round-1000", cfgRound, S, 1000, big464, nil, nil)
	add("cap-config-round-4999", cfgRound, S, 4999, big464, nil, nil)
	add("cap-config-round-5000", cfgRound, S, 5000, big464, nil, nil)
	add("cap-config-round-1000-with-penalties", cfgRound, S, 1000, big464, nil, pick(big464[:40], 12))
	// an epoch-switch block whose number is itself a gap number: calcMasternodes reads the parent's gap (engine.go:2030)
	add("switch-at-gap-number", cfg, 60_000_450, 2700, p47, nil, pick(p47, 5))
	add("first-v2-block-under-cap", cfg, cfg.SwitchBlock+1, 1, nil, p20, nil)
	// an epoch switch inside the first V2 epoch: its pool is the V1 list of the switch block, penalties come off it
	add("first-epoch-switch-with-penalties", cfg, cfg.SwitchBlock+493, 900, nil, p20, pick(p20, 4))
	add("first-epoch-switch-cap-binds", CfgIn{Epoch: 900, Gap: 450, SwitchBlock: 56_828_700, SwitchEpoch: 63_142, Configs: map[uint64]CfgRow{0: {MaxMasternodes: 10, CertThreshold: 0.667, MinePeriod: 2}}}, cfg.SwitchBlock+493, 900, nil, p20, pick(p20[:12], 3))
	small := CfgIn{Epoch: 900, Gap: 450, SwitchBlock: 56_828_700, SwitchEpoch: 63_142, Configs: map[uint64]CfgRow{0: {MaxMasternodes: 10, CertThreshold: 0.667, MinePeriod: 2}}}
	add("first-v2-block-cap-binds", small, cfg.SwitchBlock+1, 1, nil, p20, nil)
	add("first-v2-block-empty-list", cfg, cfg.SwitchBlock+1, 1, nil, nil, nil)
	return out
}

// ---------------------------------------------------------------- epoch

type EpochCase struct {
	Number   uint64  `json:"number"`
	Round    uint64  `json:"round"`
	QCRound  uint64  `json:"qc_round"`
	QCNumber uint64  `json:"qc_number"`
	IsSwitch bool    `json:"is_switch"`
	EpochNum uint64  `json:"epoch_num"`
	Err      *string `json:"error"`
}
type AtRoundCase struct {
	Round       uint64  `json:"round"`
	ParentNum   uint64  `json:"parent_number"`
	ParentRound uint64  `json:"parent_round"`
	IsSwitch    bool    `json:"is_switch"`
	EpochNum    uint64  `json:"epoch_num"`
	Err         *string `json:"error"`
}
type EpochOut struct {
	Cfg     CfgIn         `json:"cfg"`
	Headers []EpochCase   `json:"headers"`
	AtRound []AtRoundCase `json:"at_round"`
}

func genEpoch() EpochOut {
	cfg := CfgIn{Epoch: 900, Gap: 450, SwitchBlock: 56_828_700, SwitchEpoch: 63_142, Configs: map[uint64]CfgRow{0: {MaxMasternodes: 108, CertThreshold: 0.667, MinePeriod: 2}}}
	x := engine_v2.OracleEngine(chainConfig(cfg))
	sw := cfg.SwitchBlock
	out := EpochOut{Cfg: cfg}
	rounds := [][2]uint64{ // round, qc round
		{1, 0}, {2, 1}, {899, 898}, {900, 899}, {901, 900}, {900, 898}, {901, 899}, {1799, 1798}, {1800, 1799}, {1800, 1700}, {1801, 1800},
		{2700, 899}, {2699, 899}, {2701, 2700}, {2701, 899}, {1000, 900}, {1000, 899}, {1799, 900}, {900, 0}, {0, 0}, {5400, 5399}, {5401, 5399},
	}
	for _, num := range []uint64{sw - 1, sw, sw + 1, sw + 2, sw + 450, 60_000_000} {
		for _, rq := range rounds {
			for _, qn := range []uint64{num - 1, sw} {
				if qn > num {
					continue
				}
				qc := &types.QuorumCert{ProposedBlockInfo: &types.BlockInfo{Round: types.Round(rq[1]), Number: new(big.Int).SetUint64(qn)}, Signatures: []types.Signature{}}
				h := v2header(num, common.Hash{}, rq[0], qc, nil, nil)
				ok, en, err := x.OracleIsEpochSwitch(h)
				out.Headers = append(out.Headers, EpochCase{num, rq[0], rq[1], qn, ok, en, errStr(err)})
			}
		}
	}
	for _, pn := range []uint64{sw, sw + 1, sw + 500, 60_000_000} {
		for _, pr := range []uint64{0, 1, 10, 899, 900, 901, 1799, 1800, 2699} {
			for _, rd := range []uint64{0, 1, 10, 899, 900, 901, 1799, 1800, 1801, 2700, 2701} {
				parent := v2header(pn, common.Hash{}, pr, nil, nil, nil)
				ok, en, err := x.OracleIsEpochSwitchAtRound(rd, parent)
				out.AtRound = append(out.AtRound, AtRoundCase{rd, pn, pr, ok, en, errStr(err)})
			}
		}
	}
	return out
}

// ---------------------------------------------------------------- cert

type Key struct {
	k *ecdsa.PrivateKey
	a common.Address
}

func keys(tag string, n int) []Key {
	out := make([]Key, n)
	for i := range out {
		k, err := crypto.ToECDSA(crypto.Keccak256([]byte(fmt.Sprintf("v2oracle/%s/%d", tag, i))))
		must(err)
		out[i] = Key{k, crypto.PubkeyToAddress(k.PublicKey)}
	}
	return out
}

func sign(k Key, h common.Hash) types.Signature {
	s, err := crypto.Sign(h[:], k.k)
	must(err)
	return s
}

// signK signs with a nonce taken from r: a different valid low-S signature of the same key and message each call.
func signK(r *rand.Rand, k Key, h common.Hash) types.Signature {
	for {
		rr, s, err := ecdsa.Sign(readerOf{r}, k.k, h[:])
		must(err)
		n := crypto.S256().Params().N
		if s.Cmp(new(big.Int).Rsh(n, 1)) > 0 {
			s.Sub(n, s)
		}
		sig := make([]byte, 65)
		rr.FillBytes(sig[:32])
		s.FillBytes(sig[32:64])
		for v := byte(0); v < 2; v++ {
			sig[64] = v
			if pub, err := crypto.SigToPub(h[:], sig); err == nil && crypto.PubkeyToAddress(*pub) == k.a {
				return sig
			}
		}
	}
}

type readerOf struct{ r *rand.Rand }

func (x readerOf) Read(p []byte) (int, error) { return x.r.Read(p) }

// highS is the malleated twin of a signature: (r, N-s) with the other recovery id. It recovers the same address.
func highS(sig types.Signature) types.Signature {
	n := crypto.S256().Params().N
	s := new(big.Int).Sub(n, new(big.Int).SetBytes(sig[32:64]))
	out := append(types.Signature{}, sig...)
	s.FillBytes(out[32:64])
	out[64] ^= 1
	return out
}

type CertCase struct {
	ID          string           `json:"id"`
	Kind        string           `json:"kind"` // qc, tc, syncinfo
	Label       string           `json:"label"`
	Masternodes []common.Address `json:"masternodes"`
	Threshold   float64          `json:"threshold"`
	Epoch       uint64           `json:"epoch"`
	Gap         uint64           `json:"gap"`
	SwitchBlock uint64           `json:"switch_block"`
	SwitchRound uint64           `json:"epoch_switch_round"` // round of the epoch-switch block the masternodes belong to
	EpochSwitch uint64           `json:"epoch_switch_number"`
	Parent      H                `json:"parent"` // rlp(header) of the QC block (qc only)
	QC          H                `json:"qc"`     // rlp(QuorumCert)
	TC          H                `json:"tc"`     // rlp(TimeoutCert)
	SnapshotN   int              `json:"snapshot_candidates"`
	Accept      bool             `json:"accept"`
	Err         *string          `json:"error"`
}

type DecodeCase struct {
	ID     string  `json:"id"`
	Type   string  `json:"type"` // tc, qc
	Rlp    H       `json:"rlp"`
	Accept bool    `json:"accept"`
	Err    *string `json:"error"`
}

type CertOut struct {
	Cases  []CertCase   `json:"cases"`
	Decode []DecodeCase `json:"decode"`
	// float64(n) * threshold as Go computes it, for the rounding cases
	Edges []Edge `json:"threshold_edges"`
}

type Edge struct {
	N         int     `json:"n"`
	Threshold float64 `json:"threshold"`
	Product   float64 `json:"product"`
	Need      int     `json:"min_signers"` // smallest k with float64(k) >= product
}

func minSigners(n int, thr float64) int {
	p := float64(n) * thr
	k := 0
	for float64(k) < p {
		k++
	}
	return k
}

func genCert() CertOut {
	r := rand.New(rand.NewSource(19525))
	out := CertOut{}
	cfgFor := func(thr float64) CfgIn {
		return CfgIn{Epoch: 900, Gap: 450, SwitchBlock: 1800, SwitchEpoch: 2, Configs: map[uint64]CfgRow{0: {MaxMasternodes: 108, CertThreshold: thr, MinePeriod: 2}, 100000: {MaxMasternodes: 108, CertThreshold: 0.9, MinePeriod: 2}}}
	}
	outsiders := keys("outsider", 4)
	// (n, threshold) pairs: the ones where float64(n)*threshold falls just off an integer are the rounding edges.
	type nt struct {
		n   int
		thr float64
	}
	var pairs []nt
	for _, thr := range []float64{0.667, 2.0 / 3.0, 0.7, 0.75, 0.9, 0.6, 0.5, 0.95} {
		for n := 1; n <= 130; n++ {
			p := float64(n) * thr
			frac := p - float64(int64(p))
			if len(pairs) < 24 && (n%11 == 0 || n == 1 || n == 3 || n == 108 || n == 30 || n == 10 || (frac != 0 && (frac < 1e-9 || frac > 1-1e-9))) {
				pairs = append(pairs, nt{n, thr})
			}
		}
	}
	for _, p := range pairs {
		out.Edges = append(out.Edges, Edge{p.n, p.thr, float64(p.n) * p.thr, minSigners(p.n, p.thr)})
	}
	pairs = pairs[:0]
	for _, e := range out.Edges {
		pairs = append(pairs, nt{e.N, e.Threshold})
	}

	mk := func(id, label string, n int, thr float64, qcRound uint64, build func(mn []Key, digest common.Hash, need int) []types.Signature) {
		cfg := cfgFor(thr)
		cc := chainConfig(cfg)
		mn := keys(fmt.Sprintf("mn%d", n), n)
		list := make([]common.Address, n)
		for i, k := range mn {
			list[i] = k.a
		}
		// QC for a block of the epoch whose switch block is es; parent is that QC block
		es := uint64(2700) // epoch switch number; gap = 2700 - 2700%900 - 450 = 2250
		gapN := es - es%cfg.Epoch - cfg.Gap
		parent := v2header(2800, common.Hash{9}, qcRound, nil, nil, nil)
		info := &types.BlockInfo{Hash: parent.Hash(), Round: types.Round(qcRound), Number: parent.Number}
		qc := &types.QuorumCert{ProposedBlockInfo: info, GapNumber: gapN}
		digest := types.VoteSigHash(&types.VoteForSign{ProposedBlockInfo: info, GapNumber: gapN})
		need := minSigners(n, thr)
		qc.Signatures = build(mn, digest, need)
		x := engine_v2.OracleEngine(cc)
		chain := engine_v2.NewOracleChain(cc)
		chain.Add(parent)
		x.OracleSeedEpoch(parent.Hash(), list, &types.BlockInfo{Number: new(big.Int).SetUint64(es), Round: 2700})
		err := x.OracleVerifyQC(chain, qc, parent)
		enc, e2 := rlp.EncodeToBytes(qc)
		must(e2)
		pe, e3 := rlp.EncodeToBytes(parent)
		must(e3)
		out.Cases = append(out.Cases, CertCase{ID: id, Kind: "qc", Label: label, Masternodes: list, Threshold: thr, Epoch: cfg.Epoch, Gap: cfg.Gap, SwitchBlock: cfg.SwitchBlock,
			SwitchRound: 2700, EpochSwitch: es, Parent: pe, QC: enc, Accept: err == nil, Err: errStr(err)})
	}
	signMembers := func(mn []Key, d common.Hash, k int) []types.Signature {
		var s []types.Signature
		for i := 0; i < k && i < len(mn); i++ {
			s = append(s, sign(mn[i], d))
		}
		return s
	}
	for i, p := range pairs {
		n, thr := p.n, p.thr
		tag := fmt.Sprintf("n%d-t%v", n, thr)
		mk(fmt.Sprintf("qc-%d-exact-min", i), "accept:"+tag, n, thr, 5000, func(mn []Key, d common.Hash, need int) []types.Signature { return signMembers(mn, d, need) })
		if minSigners(n, thr) > 1 {
			mk(fmt.Sprintf("qc-%d-one-below", i), "below_threshold:"+tag, n, thr, 5000, func(mn []Key, d common.Hash, need int) []types.Signature { return signMembers(mn, d, need-1) })
			mk(fmt.Sprintf("qc-%d-dups-pad", i), "below_threshold:"+tag, n, thr, 5000, func(mn []Key, d common.Hash, need int) []types.Signature {
				s := signMembers(mn, d, need-1)
				return append(s, s...) // each signature twice: counted once
			})
			mk(fmt.Sprintf("qc-%d-one-signer-many-k", i), "below_threshold:"+tag, n, thr, 5000, func(mn []Key, d common.Hash, need int) []types.Signature {
				var s []types.Signature
				for j := 0; j < need+3; j++ {
					s = append(s, signK(r, mn[0], d))
				}
				return s
			})
		}
		mk(fmt.Sprintf("qc-%d-all-members-dups-ok", i), "accept:"+tag, n, thr, 5000, func(mn []Key, d common.Hash, need int) []types.Signature {
			s := signMembers(mn, d, need)
			return append(s, s[0], s[0]) // padding with repeats of a counted signer does not hurt
		})
		mk(fmt.Sprintf("qc-%d-nonmember-extra", i), "non_member:"+tag, n, thr, 5000, func(mn []Key, d common.Hash, need int) []types.Signature {
			return append(signMembers(mn, d, need), sign(outsiders[0], d))
		})
		mk(fmt.Sprintf("qc-%d-high-s-twin", i), "high_s:"+tag, n, thr, 5000, func(mn []Key, d common.Hash, need int) []types.Signature {
			s := signMembers(mn, d, need)
			return append(s, highS(s[0]))
		})
	}
	// single cases
	mk("qc-garbage", "unrecoverable", 10, 0.667, 5000, func(mn []Key, d common.Hash, need int) []types.Signature {
		return append(signMembers(mn, d, need), make(types.Signature, 65))
	})
	mk("qc-short-signature", "unrecoverable", 10, 0.667, 5000, func(mn []Key, d common.Hash, need int) []types.Signature {
		return append(signMembers(mn, d, need), make(types.Signature, 64))
	})
	mk("qc-hacken-one-signer-all-k", "below_threshold", 9, 0.667, 5000, func(mn []Key, d common.Hash, need int) []types.Signature {
		var s []types.Signature
		for j := 0; j < 9; j++ {
			s = append(s, signK(r, mn[3], d))
		}
		return s
	})
	mk("qc-round0-no-signatures", "accept", 9, 0.667, 0, func(mn []Key, d common.Hash, need int) []types.Signature { return nil })
	mk("qc-round0-nonmember", "non_member", 9, 0.667, 0, func(mn []Key, d common.Hash, need int) []types.Signature {
		return []types.Signature{sign(outsiders[1], d)}
	})
	mk("qc-round0-high-s", "high_s", 9, 0.667, 0, func(mn []Key, d common.Hash, need int) []types.Signature {
		return []types.Signature{highS(sign(mn[0], d))}
	})
	mk("qc-round1-no-signatures", "below_threshold", 9, 0.667, 1, func(mn []Key, d common.Hash, need int) []types.Signature { return nil })
	mk("qc-high-threshold-round", "below_threshold", 10, 0.667, 100000, func(mn []Key, d common.Hash, need int) []types.Signature {
		return signMembers(mn, d, need) // round 100000 uses the 0.9 row
	})
	mk("qc-high-threshold-round-ok", "accept", 10, 0.667, 100000, func(mn []Key, d common.Hash, need int) []types.Signature {
		return signMembers(mn, d, 9)
	})

	// TC and sync info
	// epochRound is the round of the epoch-switch block of the epoch the TC is checked in; a TC of an earlier
	// round makes geth walk back to an earlier epoch, which the driver does (not this rule)
	var epochRound *uint64
	mkTC := func(id, label string, kind string, n int, thr float64, tcRound, tcGap uint64, snapN int, build func(mn []Key, d common.Hash, need int) []types.Signature, qcOK bool) {
		cfg := cfgFor(thr)
		cc := chainConfig(cfg)
		mn := keys(fmt.Sprintf("mn%d", n), n)
		list := make([]common.Address, n)
		for i, k := range mn {
			list[i] = k.a
		}
		es := uint64(2700)
		gapN := es - es%cfg.Epoch - cfg.Gap
		if tcGap != 1<<63 {
			gapN = tcGap
		}
		d := types.TimeoutSigHash(&types.TimeoutForSign{Round: types.Round(tcRound), GapNumber: gapN})
		tc := &types.TimeoutCert{Round: types.Round(tcRound), GapNumber: gapN}
		need := minSigners(n, thr)
		tc.Signatures = build(mn, d, need)
		if tc.Signatures == nil {
			tc.Signatures = []types.Signature{}
		}
		x := engine_v2.OracleEngine(cc)
		chain := engine_v2.NewOracleChain(cc)
		head := v2header(2800, common.Hash{9}, 2900, nil, nil, nil)
		chain.Add(head)
		chain.Head = head
		gapH := &types.Header{Number: new(big.Int).SetUint64(gapN), Extra: []byte{1}, Difficulty: big.NewInt(1)}
		chain.Add(gapH)
		pool := make([]common.Address, snapN)
		copy(pool, list)
		x.OracleSeed(gapH, pool)
		swRound := uint64(2700)
		if epochRound != nil {
			swRound = *epochRound
		}
		swInfo := &types.BlockInfo{Hash: common.Hash{7}, Number: new(big.Int).SetUint64(es), Round: types.Round(swRound)}
		x.OracleSeedEpoch(head.Hash(), list, swInfo)
		x.OracleSeedEpoch(swInfo.Hash, list, swInfo)
		var err error
		cse := CertCase{ID: id, Kind: kind, Label: label, Masternodes: list, Threshold: thr, Epoch: cfg.Epoch, Gap: cfg.Gap, SwitchBlock: cfg.SwitchBlock, SwitchRound: 2700, EpochSwitch: es, SnapshotN: snapN}
		enc, e2 := rlp.EncodeToBytes(tc)
		must(e2)
		cse.TC = enc
		if kind == "tc" {
			err = x.OracleVerifyTC(chain, tc)
		} else { // syncinfo: a valid QC of round 5000 next to the TC
			parent := v2header(2800, common.Hash{9}, 5000, nil, nil, nil)
			chain.Add(parent)
			info := &types.BlockInfo{Hash: parent.Hash(), Round: 5000, Number: parent.Number}
			qc := &types.QuorumCert{ProposedBlockInfo: info, GapNumber: gapN}
			qd := types.VoteSigHash(&types.VoteForSign{ProposedBlockInfo: info, GapNumber: gapN})
			if qcOK {
				qc.Signatures = signMembers(mn, qd, need)
			} else {
				qc.Signatures = signMembers(mn, qd, 0)
			}
			x.OracleSeedEpoch(parent.Hash(), list, swInfo)
			_, err = x.OracleSyncInfo(chain, &types.SyncInfo{HighestQuorumCert: qc, HighestTimeoutCert: tc})
			qe, e4 := rlp.EncodeToBytes(qc)
			must(e4)
			cse.QC = qe
			pe, e5 := rlp.EncodeToBytes(parent)
			must(e5)
			cse.Parent = pe
		}
		cse.Accept, cse.Err = err == nil, errStr(err)
		out.Cases = append(out.Cases, cse)
	}
	const anyGap = 1 << 63
	for i, p := range pairs[:8] {
		n, thr := p.n, p.thr
		tag := fmt.Sprintf("n%d-t%v", n, thr)
		mkTC(fmt.Sprintf("tc-%d-exact-min", i), "accept:"+tag, "tc", n, thr, 2750, anyGap, n, func(mn []Key, d common.Hash, need int) []types.Signature { return signMembers(mn, d, need) }, true)
		if minSigners(n, thr) > 1 {
			mkTC(fmt.Sprintf("tc-%d-one-below", i), "below_threshold:"+tag, "tc", n, thr, 2750, anyGap, n, func(mn []Key, d common.Hash, need int) []types.Signature { return signMembers(mn, d, need-1) }, true)
			mkTC(fmt.Sprintf("tc-%d-one-signer-many-k", i), "below_threshold:"+tag, "tc", n, thr, 2750, anyGap, n, func(mn []Key, d common.Hash, need int) []types.Signature {
				var s []types.Signature
				for j := 0; j < need+3; j++ {
					s = append(s, signK(r, mn[0], d))
				}
				return s
			}, true)
		}
		mkTC(fmt.Sprintf("tc-%d-nonmember", i), "non_member:"+tag, "tc", n, thr, 2750, anyGap, n, func(mn []Key, d common.Hash, need int) []types.Signature {
			return append(signMembers(mn, d, need), sign(outsiders[2], d))
		}, true)
		mkTC(fmt.Sprintf("tc-%d-high-s", i), "high_s:"+tag, "tc", n, thr, 2750, anyGap, n, func(mn []Key, d common.Hash, need int) []types.Signature {
			s := signMembers(mn, d, need)
			return append(s, highS(s[0]))
		}, true)
	}
	zero := uint64(0)
	epochRound = &zero
	none := func(mn []Key, d common.Hash, need int) []types.Signature { return nil }
	ok := func(mn []Key, d common.Hash, need int) []types.Signature { return signMembers(mn, d, need) }
	mkTC("tc-blank-direct", "below_threshold", "tc", 9, 0.667, 0, anyGap, 9, none, true)
	mkTC("tc-round0-valid-signatures", "accept", "tc", 9, 0.667, 0, anyGap, 9, ok, true)
	epochRound = nil
	mkTC("tc-snapshot-empty", "snapshot_missing", "tc", 9, 0.667, 2750, anyGap, 0, ok, true)
	mkTC("tc-no-signatures", "below_threshold", "tc", 9, 0.667, 2700, anyGap, 9, none, true)
	mkTC("si-blank", "accept", "syncinfo", 9, 0.667, 0, 0, 9, none, true)
	mkTC("si-blank-gap-set", "accept", "syncinfo", 9, 0.667, 0, 2250, 9, none, true)
	mkTC("si-blank-qc-bad", "qc", "syncinfo", 9, 0.667, 0, 0, 9, none, false)
	mkTC("si-round0-one-signature", "tc_below", "syncinfo", 9, 0.667, 0, 0, 9, func(mn []Key, d common.Hash, need int) []types.Signature { return signMembers(mn, d, 1) }, true)
	mkTC("si-round7-no-signatures", "tc_below", "syncinfo", 9, 0.667, 7, 2250, 9, none, true)
	mkTC("si-round7-valid", "accept", "syncinfo", 9, 0.667, 2750, 2250, 9, ok, true)
	mkTC("si-round7-one-below", "tc_below", "syncinfo", 9, 0.667, 2750, 2250, 9, func(mn []Key, d common.Hash, need int) []types.Signature { return signMembers(mn, d, need-1) }, true)

	// decoding: rlp.DecodeBytes, exactly one value
	vtc := &types.TimeoutCert{Round: 2750, GapNumber: 2250, Signatures: []types.Signature{sign(keys("d", 1)[0], common.Hash{1}), sign(keys("d", 2)[1], common.Hash{1})}}
	vqc := &types.QuorumCert{ProposedBlockInfo: &types.BlockInfo{Hash: common.Hash{3}, Round: 5, Number: big.NewInt(77)}, Signatures: vtc.Signatures, GapNumber: 2250}
	tcb, _ := rlp.EncodeToBytes(vtc)
	qcb, _ := rlp.EncodeToBytes(vqc)
	dec := func(kind string, id string, b []byte) {
		var err error
		if kind == "tc" {
			err = rlp.DecodeBytes(b, new(types.TimeoutCert))
		} else {
			err = rlp.DecodeBytes(b, new(types.QuorumCert))
		}
		out.Decode = append(out.Decode, DecodeCase{id, kind, b, err == nil, errStr(err)})
	}
	for kind, b := range map[string][]byte{"tc": tcb, "qc": qcb} {
		dec(kind, kind+"-valid", b)
		dec(kind, kind+"-trailing-1", append(append([]byte{}, b...), 0))
		dec(kind, kind+"-trailing-80", append(append([]byte{}, b...), 0x80))
		dec(kind, kind+"-trailing-copy", append(append([]byte{}, b...), b...))
		dec(kind, kind+"-truncated", b[:len(b)-1])
		dec(kind, kind+"-empty", nil)
		dec(kind, kind+"-empty-list", []byte{0xc0})
	}
	// list with an extra element and with one missing
	var items []rlp.RawValue
	it, _, err := rlp.SplitList(tcb)
	must(err)
	for len(it) > 0 {
		_, _, rest, err := rlp.Split(it)
		must(err)
		items = append(items, it[:len(it)-len(rest)])
		it = rest
	}
	join := func(parts ...[]byte) []byte {
		var body []byte
		for _, p := range parts {
			body = append(body, p...)
		}
		b, err := rlp.EncodeToBytes(rlp.RawValue(append(listHeader(len(body)), body...)))
		must(err)
		return b
	}
	dec("tc", "tc-extra-element", join(items[0], items[1], items[2], []byte{0x01}))
	dec("tc", "tc-missing-element", join(items[0], items[1]))
	dec("tc", "tc-round-leading-zero", join([]byte{0x82, 0x00, 0x05}, items[1], items[2]))
	dec("tc", "tc-signatures-not-a-list", join(items[0], []byte{0x83, 1, 2, 3}, items[2]))
	dec("tc", "tc-round-as-list", join([]byte{0xc0}, items[1], items[2]))
	dec("tc", "tc-gap-leading-zero", join(items[0], items[1], []byte{0x82, 0x00, 0x01}))
	dec("tc", "tc-signature-as-list", join(items[0], []byte{0xc1, 0xc0}, items[2]))
	dec("tc", "tc-blank", mustEnc(&types.TimeoutCert{Round: 0, Signatures: []types.Signature{}, GapNumber: 0}))
	return out
}

func listHeader(n int) []byte {
	if n < 56 {
		return []byte{0xc0 + byte(n)}
	}
	var l []byte
	for x := n; x > 0; x >>= 8 {
		l = append([]byte{byte(x)}, l...)
	}
	return append([]byte{0xf7 + byte(len(l))}, l...)
}

func mustEnc(v interface{}) []byte {
	b, err := rlp.EncodeToBytes(v)
	must(err)
	return b
}

// ---------------------------------------------------------------- commit

type Link struct {
	Number uint64      `json:"number"`
	Round  uint64      `json:"round"`
	Hash   common.Hash `json:"hash"`
	Parent common.Hash `json:"parent"`
}
type CommitCase struct {
	ID       string  `json:"id"`
	Chain    int     `json:"chain"`
	Highest  *Link   `json:"highest"` // already committed, none when null
	Proposed Link    `json:"proposed"`
	Ok       bool    `json:"committed"`
	After    *Link   `json:"after"`
	Err      *string `json:"error"`
}
type CommitOut struct {
	SwitchBlock uint64       `json:"switch_block"`
	Chains      [][]Link     `json:"chains"`
	Cases       []CommitCase `json:"cases"`
}

func genCommit() CommitOut {
	r := rand.New(rand.NewSource(3))
	cfg := CfgIn{Epoch: 900, Gap: 450, SwitchBlock: 100, SwitchEpoch: 1, Configs: map[uint64]CfgRow{0: {MaxMasternodes: 10, CertThreshold: 0.667, MinePeriod: 2}}}
	cc := chainConfig(cfg)
	out := CommitOut{SwitchBlock: cfg.SwitchBlock}
	for c := 0; c < 40; c++ {
		x := engine_v2.OracleEngine(cc)
		chain := engine_v2.NewOracleChain(cc)
		var links []Link
		hs := map[uint64]*types.Header{}
		// the switch block and a block before it, then V2 blocks whose rounds sometimes skip
		prev := common.Hash{}
		round := uint64(0)
		for n := cfg.SwitchBlock - 1; n <= cfg.SwitchBlock+8; n++ {
			var h *types.Header
			switch {
			case n < cfg.SwitchBlock:
				h = &types.Header{Number: new(big.Int).SetUint64(n), ParentHash: prev, Extra: []byte{1}, Difficulty: big.NewInt(1)}
			case n == cfg.SwitchBlock:
				h = v2header(n, prev, 0, nil, nil, nil) // the switch block's own round is 0
			default:
				round++
				if r.Intn(3) == 0 {
					round += uint64(r.Intn(3))
				}
				h = v2header(n, prev, round, nil, nil, nil)
			}
			chain.Add(h)
			hs[n] = h
			rr := uint64(0)
			if n >= cfg.SwitchBlock {
				rr = round
			}
			links = append(links, Link{n, rr, h.Hash(), prev})
			prev = h.Hash()
		}
		out.Chains = append(out.Chains, links)
		for i, l := range links {
			if l.Number <= cfg.SwitchBlock {
				continue
			}
			for hi := -1; hi <= 3; hi++ { // already committed: none, or the block hi places below the proposed one
				var hb *types.BlockInfo
				var hl *Link
				if hi >= 0 {
					if i-hi < 0 {
						continue
					}
					p := links[i-hi]
					hb = &types.BlockInfo{Hash: p.Hash, Round: types.Round(p.Round), Number: new(big.Int).SetUint64(p.Number)}
					hl = &p
				}
				got, ok, err := x.OracleCommit(chain, hs[l.Number], l.Round, hb)
				cs := CommitCase{ID: fmt.Sprintf("c%d-b%d-h%d", c, l.Number, hi), Chain: c, Highest: hl, Proposed: l, Ok: ok, Err: errStr(err)}
				if got != nil {
					cs.After = &Link{Number: got.Number.Uint64(), Round: uint64(got.Round), Hash: got.Hash}
				}
				out.Cases = append(out.Cases, cs)
			}
		}
		// a proposed block whose parent the chain does not hold
		orphan := v2header(cfg.SwitchBlock+20, common.Hash{0xde, 0xad}, round+5, nil, nil, nil)
		chain.Add(orphan)
		_, ok, err := x.OracleCommit(chain, orphan, round+5, nil)
		out.Cases = append(out.Cases, CommitCase{ID: fmt.Sprintf("c%d-orphan", c), Chain: c, Proposed: Link{orphan.Number.Uint64(), round + 5, orphan.Hash(), orphan.ParentHash}, Ok: ok, Err: errStr(err)})
	}
	return out
}

// ---------------------------------------------------------------- midadd

type MidCase struct {
	Round    uint64         `json:"round"`
	Parent   uint64         `json:"parent_round"`
	ParentNo uint64         `json:"parent_number"`
	Switch   bool           `json:"epoch_switch"`
	Leader   common.Address `json:"leader"`
	ListSize int            `json:"list_size"`
}
type MidOut struct {
	Cfg      CfgIn            `json:"cfg"`
	Words8   []Word           `json:"words_before"` // 0x88 at the gap block of the epoch that ends, 8 candidates
	Words9   []Word           `json:"words_after"`  // 0x88 at the gap block of the next epoch: the newcomer is in
	Newcomer common.Address   `json:"newcomer"`
	List8    []common.Address `json:"list_epoch_k"`
	List9    []common.Address `json:"list_epoch_k1"`
	Pool9    []common.Address `json:"pool_epoch_k1"`
	Cases    []MidCase        `json:"cases"`
}

func genMidAdd() MidOut {
	r := rand.New(rand.NewSource(5151))
	cfg := CfgIn{Epoch: 900, Gap: 450, SwitchBlock: 1800, SwitchEpoch: 2, Configs: map[uint64]CfgRow{0: {MaxMasternodes: 108, CertThreshold: 0.667, MinePeriod: 2}}}
	cc := chainConfig(cfg)
	arr := make([]common.Address, 8)
	for i := range arr {
		arr[i] = addr(r)
	}
	newcomer := addr(r)
	caps8 := capsOf(r, arr, "distinct")
	arr9 := append(append([]common.Address{}, arr...), newcomer)
	caps9 := map[common.Address]*big.Int{}
	for k, v := range caps8 {
		caps9[k] = v
	}
	e18 := new(big.Int).Exp(big.NewInt(10), big.NewInt(18), nil)
	caps9[newcomer] = new(big.Int).Mul(big.NewInt(10_000_000), e18)
	st8, w8 := stateOf(arr, caps8)
	st9, w9 := stateOf(arr9, caps9)
	pool8 := engine_v2.OracleSort(st8, state.GetCandidates(st8))
	pool9 := engine_v2.OracleSort(st9, state.GetCandidates(st9))

	// The switch of epoch k+1 is block 4500 (gap block 4050); its parent is block 4499.
	newChain := func() (*engine_v2.XDPoS_v2, *engine_v2.OracleChain) {
		x := engine_v2.OracleEngine(cc)
		chain := engine_v2.NewOracleChain(cc)
		gap := &types.Header{Number: big.NewInt(4050), Extra: []byte{1}, Difficulty: big.NewInt(1)}
		chain.Add(gap)
		x.OracleSeed(gap, pool9)
		return x, chain
	}
	x0, c0 := newChain()
	list9, _, _, err := x0.OracleCalc(c0, 4500, common.Hash{1}, 3600, nil)
	must(err)
	out := MidOut{Cfg: cfg, Words8: w8, Words9: w9, Newcomer: newcomer, List8: pool8, List9: list9, Pool9: pool9}
	for _, s := range [][2]uint64{ // parent round, round
		{2700, 2701}, {2701, 2702}, {2702, 2703}, {2703, 2704}, {3000, 3001}, {3590, 3592}, {3596, 3597}, {3597, 3599},
		{3599, 3600}, {3599, 3601}, {3599, 3612}, {3599, 3605}, {3600, 3601}, {3601, 3602}, {3601, 3610}, {3610, 3611}, {3611, 3695},
	} {
		x, chain := newChain()
		parentNum := 3600 + s[0] - 2700
		if s[0] >= 3600 {
			parentNum = 4500 + s[0] - 3600
		}
		parent := v2header(parentNum, common.Hash{byte(s[0])}, s[0], nil, nil, nil)
		chain.Add(parent)
		list := pool8
		if s[0] >= 3600 { // parent is in epoch k+1: its list is the one the switch computed
			list = list9
		}
		sw, _, _ := x.OracleIsEpochSwitchAtRound(s[1], parent)
		if !sw {
			x.OracleSeedEpoch(parent.Hash(), list, &types.BlockInfo{Number: big.NewInt(3600), Round: 2700})
		}
		var leader common.Address
		for _, c := range pool9 {
			if ok, err := x.OracleYourTurn(chain, s[1], parent, c); err == nil && ok {
				leader = c
			}
		}
		size := len(list)
		if sw {
			size = len(list9)
		}
		out.Cases = append(out.Cases, MidCase{s[1], s[0], parentNum, sw, leader, size})
	}
	return out
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: v2oracle sort|calc|epoch|cert|commit|midadd")
		os.Exit(2)
	}
	var v interface{}
	switch os.Args[1] {
	case "sort":
		v = genSort()
	case "calc":
		v = genCalc()
	case "epoch":
		v = genEpoch()
	case "cert":
		v = genCert()
	case "commit":
		v = genCommit()
	case "midadd":
		v = genMidAdd()
	default:
		panic(os.Args[1])
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", " ")
	must(enc.Encode(v))
}
