// Command oracle drives the pinned geth-XDC `core.BlockChain` with block trees and prints, after each
// insertion, the head and the canonical chain it chose. Rust never re-derives these answers.
//
// A scenario has two branches from one shared prefix s1..sF: main m(F+1)..mL and alt a(F+1)..aL. The
// blocks are produced by geth's own `GenerateChainWithGenesis` and inserted one by one with
// `InsertChain` (insertChain -> writeBlockAndSetHead -> keepsV2Head / reorg, blockchain.go:2428-2447,
// :3433). Modes: "v2" (XDPoS V2 switch block 0, every block above it keeps V2 rules) and "v1"
// (no V2 section). `committed` wraps the engine in the `committedBlocker` interface that `reorg` reads
// (blockchain.go:134-140, :3470-3494); the label is the block LatestCommittedBlock returns. `setup` blocks are
// inserted before it is set.
package main

import (
	"encoding/json"
	"fmt"
	"math/big"
	"math/rand"
	"os"
	"strings"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/consensus"
	"github.com/ethereum/go-ethereum/consensus/ethash"
	"github.com/ethereum/go-ethereum/core"
	"github.com/ethereum/go-ethereum/core/rawdb"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/params"
)

func must(err error) {
	if err != nil {
		panic(err)
	}
}

type committedEngine struct {
	consensus.Engine
	committed *types.BlockInfo
}

func (e *committedEngine) LatestCommittedBlock() *types.BlockInfo { return e.committed }

type Event struct {
	Block     string   `json:"block"`
	Err       string   `json:"err"`
	Head      string   `json:"head"`
	Canonical []string `json:"canonical"`
}

type Scenario struct {
	Name      string   `json:"name"`
	Mode      string   `json:"mode"`
	Fork      int      `json:"fork"`
	Main      int      `json:"main"`
	Alt       int      `json:"alt"`
	Setup     []string `json:"setup"`
	Committed string   `json:"committed"`
	Events    []Event  `json:"events"`
}

func config(mode string) *params.ChainConfig {
	cfg := *params.AllEthashProtocolChanges
	cfg.Eip1559Block = big.NewInt(0)
	if mode == "v2" {
		cfg.XDPoS = &params.XDPoSConfig{Epoch: 900, V2: &params.V2{SwitchBlock: big.NewInt(0)}}
	}
	return &cfg
}

// run inserts the prefix and then the events; fills the answers in place.
func run(s *Scenario) {
	gspec := &core.Genesis{Config: config(s.Mode), BaseFee: big.NewInt(params.InitialBaseFee)}
	faker := ethash.NewFaker()
	branch := func(n int, alt bool) []*types.Block {
		_, bs, _ := core.GenerateChainWithGenesis(gspec, faker, n, func(i int, b *core.BlockGen) {
			if alt && i >= s.Fork {
				b.SetCoinbase(common.Address{0x01})
			}
		})
		return bs
	}
	mainB, altB := branch(s.Main, false), branch(s.Alt, true)
	label := map[common.Hash]string{}
	byLabel := map[string]*types.Block{}
	for i, b := range mainB {
		l := fmt.Sprintf("m%d", i+1)
		if i < s.Fork {
			l = fmt.Sprintf("s%d", i+1)
		}
		label[b.Hash()], byLabel[l] = l, b
	}
	for i, b := range altB {
		if i >= s.Fork {
			l := fmt.Sprintf("a%d", i+1)
			label[b.Hash()], byLabel[l] = l, b
		}
	}
	eng := &committedEngine{Engine: faker}
	bc, err := core.NewBlockChain(rawdb.NewMemoryDatabase(), gspec, eng, core.DefaultConfig())
	must(err)
	defer bc.Stop()
	must2 := func(_ int, err error) { must(err) }
	must2(bc.InsertChain(mainB[:s.Fork]))
	// The setup blocks go in before the committed block is set: events start from that state.
	for _, l := range s.Setup {
		must2(bc.InsertChain(types.Blocks{byLabel[l]}))
	}
	if s.Committed != "" {
		b := byLabel[s.Committed]
		eng.committed = &types.BlockInfo{Hash: b.Hash(), Number: b.Number(), Round: 0}
	}
	for i := range s.Events {
		ev := &s.Events[i]
		_, err := bc.InsertChain(types.Blocks{byLabel[ev.Block]})
		if err != nil {
			if strings.Contains(err.Error(), "stop reorg") {
				ev.Err = "stop reorg"
			} else {
				ev.Err = "other: " + err.Error()
			}
		}
		head := bc.CurrentBlock()
		ev.Head = label[head.Hash()]
		for n := uint64(1); n <= head.Number.Uint64(); n++ {
			ev.Canonical = append(ev.Canonical, label[bc.GetCanonicalHash(n)])
		}
	}
}

func labels(prefix string, from, to int) []string {
	var out []string
	for n := from; n <= to; n++ {
		out = append(out, fmt.Sprintf("%s%d", prefix, n))
	}
	return out
}

func events(ls ...string) []Event {
	var out []Event
	for _, l := range ls {
		out = append(out, Event{Block: l})
	}
	return out
}

func main() {
	var all []*Scenario
	add := func(s *Scenario) { run(s); all = append(all, s) }
	// The sibling shapes of the issue (Apothem 85,209,326/327 and 85,214,323, net2040 619,712/713):
	// F is the parent (shared), X = m(F+1) the canonical block, Y = m(F+2) its child, S = a(F+1) the
	// stale sibling, T = a(F+2) a child of the sibling.
	const f = 3
	for _, mode := range []string{"v2", "v1"} {
		for _, sh := range []struct {
			name string
			ev   []string
		}{
			{"stale-head-then-canonical-then-child", []string{"a4", "m4", "m5"}},
			{"canonical-head-then-sibling-then-child", []string{"m4", "a4", "m5"}},
			{"stale-head-then-canonical", []string{"a4", "m4"}},
			{"child-of-sibling-adopts", []string{"m4", "a4", "a5"}},
		} {
			add(&Scenario{Name: "shape/" + sh.name + "/" + mode, Mode: mode, Fork: f, Main: f + 2, Alt: f + 2, Events: events(sh.ev...)})
		}
	}
	// 50 generated sibling pairs: random mode, fork height, branch lengths and arrival order. Both
	// branches get at least the first block above the fork (the siblings); each branch is inserted as
	// a prefix, in order, interleaved at random.
	rng := rand.New(rand.NewSource(20261111))
	for i := 0; i < 50; i++ {
		mode := []string{"v1", "v2"}[rng.Intn(2)]
		fork := 1 + rng.Intn(4)
		km, ka := 1+rng.Intn(3), 1+rng.Intn(3)
		pm, pa := labels("m", fork+1, fork+km), labels("a", fork+1, fork+ka)
		var ev []string
		for len(pm)+len(pa) > 0 {
			if len(pa) == 0 || (len(pm) > 0 && rng.Intn(2) == 0) {
				ev, pm = append(ev, pm[0]), pm[1:]
			} else {
				ev, pa = append(ev, pa[0]), pa[1:]
			}
		}
		add(&Scenario{Name: fmt.Sprintf("gen/%02d/%s", i, mode), Mode: mode, Fork: fork, Main: fork + km, Alt: fork + ka, Events: events(ev...)})
	}
	// Committed-block floor. The main chain is in, with consecutive rounds the 3-chain rule has
	// committed block L-3. A fork from below it is refused where it would become the head; a fork
	// from the committed block or above it goes through.
	// V2 only: a committed block exists only above the V2 switch.
	for _, mode := range []string{"v2"} {
		for _, fork := range []int{1, 2, 5, 6} {
			l := fork + 2
			if fork < 5 {
				l = fork + 5
			}
			committed := fmt.Sprintf("m%d", l-3)
			if l-3 <= fork {
				committed = fmt.Sprintf("s%d", l-3)
			}
			add(&Scenario{
				Name: fmt.Sprintf("floor/fork%d/%s", fork, mode), Mode: mode, Fork: fork, Main: l, Alt: l + 1,
				Setup: labels("m", fork+1, l), Committed: committed, Events: events(labels("a", fork+1, l+1)...),
			})
		}
	}
	out, err := json.MarshalIndent(map[string]any{"pin": strings.TrimSpace(readPin()), "scenarios": all}, "", " ")
	must(err)
	fmt.Println(string(out))
}

func readPin() string {
	b, err := os.ReadFile("PIN")
	must(err)
	return string(b)
}
