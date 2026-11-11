// Command exporter writes the fork table of the built-in XDC networks, read
// from the pinned geth-XDC `params` and `core` packages, to a JSON file that
// crates/xone-chainspec includes at build time.
//
// Run it from this directory:
//
//	go run .          write ../../crates/xone-chainspec/generated/forks.json
//	go run . -check   regenerate in memory; exit 1 with a diff if the file differs
//	go run . headers  write header vectors from an archive RPC (see headers.go)
package main

import (
	"bytes"
	"encoding/json"
	"flag"
	"fmt"
	"math/big"
	"os"
	"os/exec"
	"reflect"
	"sort"
	"strings"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core"
	"github.com/ethereum/go-ethereum/params"
)

// kv is one member of a JSON object whose order is fixed by the exporter.
type kv struct {
	K string
	V any
}

type object []kv

func (o object) MarshalJSON() ([]byte, error) {
	var b bytes.Buffer
	b.WriteByte('{')
	for i, e := range o {
		if i > 0 {
			b.WriteByte(',')
		}
		k, _ := json.Marshal(e.K)
		v, err := json.Marshal(e.V)
		if err != nil {
			return nil, err
		}
		b.Write(k)
		b.WriteByte(':')
		b.Write(v)
	}
	b.WriteByte('}')
	return b.Bytes(), nil
}

type expTimeout struct {
	Base        float64 `json:"base"`
	MaxExponent uint8   `json:"maxExponent"`
}

// v2Config lists every geth V2Config field, zero values included.
type v2Config struct {
	SwitchRound               uint64     `json:"switchRound"`
	MinePeriod                int        `json:"minePeriod"`
	TimeoutSyncThreshold      int        `json:"timeoutSyncThreshold"`
	TimeoutPeriod             int        `json:"timeoutPeriod"`
	CertThreshold             float64    `json:"certificateThreshold"`
	MaxMasternodes            int        `json:"maxMasternodes"`
	MaxProtectorNodes         int        `json:"maxProtectorNodes"`
	MaxObserverNodes          int        `json:"maxObserverNodes"`
	MasternodeReward          float64    `json:"masternodeReward"`
	ProtectorReward           float64    `json:"protectorReward"`
	ObserverReward            float64    `json:"observerReward"`
	MinimumMinerBlockPerEpoch int        `json:"minimumMinerBlockPerEpoch"`
	LimitPenaltyEpoch         int        `json:"limitPenaltyEpoch"`
	MinimumSigningTx          int        `json:"minimumSigningTx"`
	ExpTimeoutConfig          expTimeout `json:"expTimeoutConfig"`
}

type v2 struct {
	SwitchBlock string `json:"switchBlock"`
	SwitchEpoch uint64 `json:"switchEpoch"`
	AllConfigs  object `json:"allConfigs"` // keyed by round, numeric order
}

type xdpos struct {
	Period              uint64 `json:"period"`
	Epoch               uint64 `json:"epoch"`
	Reward              uint64 `json:"reward"`
	RewardCheckpoint    uint64 `json:"rewardCheckpoint"`
	Gap                 uint64 `json:"gap"`
	FoudationWalletAddr string `json:"foudationWalletAddr"`
	MaxMasternodesV2    int    `json:"maxMasternodesV2"`
	V2                  v2     `json:"v2"`
}

type genesis struct {
	Hash      string `json:"hash"`
	StateRoot string `json:"stateRoot"`
}

type network struct {
	ChainID   string  `json:"chainId"`
	Blocks    object  `json:"blocks"`
	Effective object  `json:"effective"`
	XDPoS     xdpos   `json:"xdpos"`
	Genesis   genesis `json:"genesis"`
}

type file struct {
	Pin      string `json:"pin"`
	Networks object `json:"networks"`
}

// dec renders a big.Int as a decimal string; nil is JSON null.
func dec(n *big.Int) any {
	if n == nil {
		return nil
	}
	return n.String()
}

// blocks emits every *big.Int field of ChainConfig whose name ends in "Block".
func blocks(cfg *params.ChainConfig) object {
	var out object
	v := reflect.ValueOf(*cfg)
	for i := 0; i < v.NumField(); i++ {
		f := v.Type().Field(i)
		if f.Type != reflect.TypeOf((*big.Int)(nil)) || !strings.HasSuffix(f.Name, "Block") {
			continue
		}
		key, _, _ := strings.Cut(f.Tag.Get("json"), ",")
		n, _ := v.Field(i).Interface().(*big.Int)
		out = append(out, kv{key, dec(n)})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].K < out[j].K })
	return out
}

// firstActive returns the lowest block at which the geth predicate holds, or
// null if it never holds. It treats the predicate as monotone, which every
// predicate below is (they are all "at or after block N").
func firstActive(f func(*big.Int) bool) any {
	big62 := new(big.Int).Lsh(big.NewInt(1), 62)
	if !f(big62) {
		return nil
	}
	lo, hi := big.NewInt(-1), big62 // f(lo) false, f(hi) true
	for new(big.Int).Sub(hi, lo).Cmp(big.NewInt(1)) > 0 {
		mid := new(big.Int).Add(lo, hi)
		mid.Rsh(mid, 1)
		if mid.Sign() < 0 || !f(mid) {
			lo = mid
		} else {
			hi = mid
		}
	}
	return hi.String()
}

// effective evaluates geth's own predicates, so switches that are not plain
// params rows (global TIP constants, fallbacks inside IsIstanbul) are exported
// as the blocks geth really uses. CopyXDCConstants is called first, as
// eth/backend.go does at start-up for an XDPoS chain. Those constants are
// process globals and some chain cases leave a few untouched, so reset them to
// the default profile first; otherwise one network's values leak into the next.
func effective(cfg *params.ChainConfig) object {
	common.CopyXDCConstants(0)
	common.CopyXDCConstants(cfg.ChainID.Uint64())
	return object{
		{"constantinople", firstActive(cfg.IsConstantinople)},
		{"eip1559", firstActive(cfg.IsEIP1559)},
		{"gas50x", firstActive(cfg.IsGas50x)},
		{"istanbul", firstActive(cfg.IsIstanbul)},
		{"petersburg", firstActive(cfg.IsPetersburg)},
		{"tip2019", firstActive(cfg.IsTIP2019)},
		{"tipNoHalvingMNReward", firstActive(cfg.IsTIPNoHalvingMNReward)},
		{"tipRandomize", firstActive(cfg.IsTIPRandomize)},
		{"tipSigning", firstActive(cfg.IsTIPSigning)},
		{"tipTRC21Fee", firstActive(cfg.IsTIPTRC21Fee)},
		{"tipXDCX", firstActive(cfg.IsTIPXDCX)},
		{"tipXDCXCancellationFee", firstActive(cfg.IsTIPXDCXCancellationFee)},
		{"tipXDCXMinerDisable", firstActive(cfg.IsTIPXDCXMiner)},
	}
}

func xdposOf(c *params.XDPoSConfig) xdpos {
	rounds := make([]uint64, 0, len(c.V2.AllConfigs))
	for r := range c.V2.AllConfigs {
		rounds = append(rounds, r)
	}
	sort.Slice(rounds, func(i, j int) bool { return rounds[i] < rounds[j] })
	var all object
	for _, r := range rounds {
		p := c.V2.AllConfigs[r]
		all = append(all, kv{fmt.Sprint(r), v2Config{
			p.SwitchRound, p.MinePeriod, p.TimeoutSyncThreshold, p.TimeoutPeriod,
			p.CertThreshold, p.MaxMasternodes, p.MaxProtectorNodes, p.MaxObserverNodes,
			p.MasternodeReward, p.ProtectorReward, p.ObserverReward,
			p.MinimumMinerBlockPerEpoch, p.LimitPenaltyEpoch, p.MinimumSigningTx,
			expTimeout{p.ExpTimeoutConfig.Base, p.ExpTimeoutConfig.MaxExponent},
		}})
	}
	return xdpos{
		Period: c.Period, Epoch: c.Epoch, Reward: c.Reward,
		RewardCheckpoint: c.RewardCheckpoint, Gap: c.Gap,
		FoudationWalletAddr: strings.ToLower(c.FoudationWalletAddr.Hex()),
		MaxMasternodesV2:    c.MaxMasternodesV2,
		V2:                  v2{SwitchBlock: c.V2.SwitchBlock.String(), SwitchEpoch: c.V2.SwitchEpoch, AllConfigs: all},
	}
}

func build(pin string) ([]byte, error) {
	nets := []struct {
		name string
		cfg  *params.ChainConfig
		gen  *core.Genesis
	}{
		{"apothem", params.XDCApothemChainConfig, core.DefaultXDCApothemGenesisBlock()},
		{"devnet", params.XDCDevnetChainConfig, core.DefaultXDCDevnetGenesisBlock()},
		{"devnet551", params.XDCDevnet551ChainConfig, core.DefaultXDCDevnet551GenesisBlock()},
		{"mainnet", params.XDCMainnetChainConfig, core.DefaultXDCMainnetGenesisBlock()},
	}
	var out object
	for _, n := range nets {
		b := n.gen.ToBlock()
		out = append(out, kv{n.name, network{
			ChainID:   n.cfg.ChainID.String(),
			Blocks:    blocks(n.cfg),
			Effective: effective(n.cfg),
			XDPoS:     xdposOf(n.cfg.XDPoS),
			Genesis:   genesis{b.Hash().Hex(), b.Root().Hex()},
		}})
	}
	raw, err := json.Marshal(file{Pin: pin, Networks: out})
	if err != nil {
		return nil, err
	}
	var indented bytes.Buffer
	if err := json.Indent(&indented, raw, "", "  "); err != nil {
		return nil, err
	}
	indented.WriteByte('\n')
	return indented.Bytes(), nil
}

func main() {
	if len(os.Args) > 1 && os.Args[1] == "headers" {
		headers(os.Args[2:])
		return
	}
	out := flag.String("out", "../../crates/xone-chainspec/generated/forks.json", "generated file")
	pinFile := flag.String("pin", "../PIN", "file holding the geth-XDC commit SHA")
	check := flag.Bool("check", false, "compare with the committed file instead of writing it")
	flag.Parse()

	pin, err := os.ReadFile(*pinFile)
	if err != nil {
		fatal(err)
	}
	got, err := build(strings.TrimSpace(string(pin)))
	if err != nil {
		fatal(err)
	}
	if !*check {
		fatal(os.WriteFile(*out, got, 0o644))
		return
	}
	want, err := os.ReadFile(*out)
	if err != nil {
		fatal(err)
	}
	if bytes.Equal(got, want) {
		return
	}
	cmd := exec.Command("diff", "-u", *out, "-")
	cmd.Stdin = bytes.NewReader(got)
	diff, _ := cmd.CombinedOutput()
	fmt.Fprintf(os.Stderr, "%s is out of date with the pinned geth-XDC; regenerate with `go run .`\n%s", *out, diff)
	os.Exit(1)
}

func fatal(err error) {
	if err != nil {
		fmt.Fprintln(os.Stderr, "exporter:", err)
		os.Exit(1)
	}
}
