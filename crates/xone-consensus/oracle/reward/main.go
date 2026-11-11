// Command oracle runs the pinned geth-XDC reward functions on explicit inputs and prints the
// reward map (address -> amount, the foundation and zero amounts included) of each case.
//
//	oracle cases.json [net2040.forks.json]
//
// A case holds what the driver would collect: the per-signer counts of the window (flat rule or
// the three tiers with the sign slots), the 0x88 data of the paid masternodes (owner, voters and
// their stake), the block number and, for the tiered rule, the round. The counts themselves are
// not computed here: the window walk needs a chain and an engine, so the count is checked against
// the archive's state change instead. What the oracle calls:
//
//	eth/hooks/engine_v2_hooks.go:117-212   HookReward, the lines that turn a config reward into a chain reward
//	consensus/XDPoS/reward.go:228,260,289  CalculateRewardForSigner, ...Fixed, ...Proportional
//	consensus/XDPoS/reward.go:466,333      GetRewardBalancesRate, GetRewardBalancesRateUpgrade
//	eth/util/util.go:18                    RewardInflation
package main

import (
	"encoding/json"
	"fmt"
	"math/big"
	"os"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/consensus"
	"github.com/ethereum/go-ethereum/consensus/XDPoS"
	"github.com/ethereum/go-ethereum/core/state"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/ethereum/go-ethereum/eth/util"
	"github.com/ethereum/go-ethereum/params"
)

type Holder struct {
	Owner  string      `json:"owner"`
	Voters [][2]string `json:"voters"`
}

type Case struct {
	ID      string `json:"id"`
	Network string `json:"network"`
	Rule    string `json:"rule"` // v1, v2, tiered
	Number  uint64 `json:"number"`
	Round   uint64 `json:"round"`
	// flat rule
	Total uint64            `json:"total"`
	Signs map[string]uint64 `json:"signs"`
	// tiered rule
	Slots     uint64            `json:"slots"`
	Master    map[string]uint64 `json:"masternode"`
	Protector map[string]uint64 `json:"protector"`
	Observer  map[string]uint64 `json:"observer"`
	Holders   map[string]Holder `json:"holders"`
}

type Result struct {
	ID      string            `json:"id"`
	Rewards map[string]string `json:"rewards"`
}

func must(err error) {
	if err != nil {
		panic(err)
	}
}

// chainCfg gives RewardInflation the chain config it reads, nothing else.
type chainCfg struct {
	consensus.ChainHeaderReader
	cfg *params.ChainConfig
}

func (c chainCfg) Config() *params.ChainConfig { return c.cfg }

func config(network, forks string) *params.ChainConfig {
	// The exporter evaluates every predicate the same way (conformance/exporter/main.go:169-171).
	common.CopyXDCConstants(0)
	switch network {
	case "apothem":
		common.CopyXDCConstants(51)
		params.XDCApothemChainConfig.XDPoS.V2.BuildConfigIndex()
		return params.XDCApothemChainConfig
	case "mainnet":
		common.CopyXDCConstants(50)
		params.XDCMainnetChainConfig.XDPoS.V2.BuildConfigIndex()
		return params.XDCMainnetChainConfig
	case "net2040":
		raw, err := os.ReadFile(forks)
		must(err)
		var f struct {
			Networks map[string]struct {
				Blocks map[string]*string `json:"blocks"`
				XDPoS  json.RawMessage    `json:"xdpos"`
			} `json:"networks"`
		}
		must(json.Unmarshal(raw, &f))
		n := f.Networks["net2040"]
		common.CopyXDCConstants(2040)
		cc := &params.ChainConfig{ChainID: big.NewInt(2040), XDPoS: &params.XDPoSConfig{}}
		for key, ptr := range map[string]**big.Int{
			"tipFixedRewardBlock":        &cc.TIPFixedRewardBlock,
			"tipProportionalRewardBlock": &cc.TIPProportionalRewardBlock,
			"tipUpgradePenaltyBlock":     &cc.TIPUpgradePenaltyBlock,
			"tipNoHalvingMNRewardBlock":  &cc.TIPNoHalvingMNRewardBlock,
		} {
			if v := n.Blocks[key]; v != nil {
				b, ok := new(big.Int).SetString(*v, 10)
				if !ok {
					panic(key)
				}
				*ptr = b
			}
		}
		var x struct {
			Period     uint64 `json:"period"`
			Epoch      uint64 `json:"epoch"`
			Reward     uint64 `json:"reward"`
			Foundation string `json:"foudationWalletAddr"`
			V2         struct {
				SwitchBlock string `json:"switchBlock"`
				All         map[uint64]struct {
					MasternodeReward float64 `json:"masternodeReward"`
					ProtectorReward  float64 `json:"protectorReward"`
					ObserverReward   float64 `json:"observerReward"`
				} `json:"allConfigs"`
			} `json:"v2"`
		}
		must(json.Unmarshal(n.XDPoS, &x))
		cc.XDPoS.Period, cc.XDPoS.Epoch, cc.XDPoS.Reward = x.Period, x.Epoch, x.Reward
		cc.XDPoS.FoudationWalletAddr = common.HexToAddress(x.Foundation)
		sw, _ := new(big.Int).SetString(x.V2.SwitchBlock, 10)
		cc.XDPoS.V2 = &params.V2{SwitchBlock: sw, AllConfigs: map[uint64]*params.V2Config{}}
		for round, c := range x.V2.All {
			cc.XDPoS.V2.AllConfigs[round] = &params.V2Config{MasternodeReward: c.MasternodeReward, ProtectorReward: c.ProtectorReward, ObserverReward: c.ObserverReward}
		}
		cc.XDPoS.V2.BuildConfigIndex()
		return cc
	}
	panic("network " + network)
}

// parentState holds the 0x88 data of the cases (the contract layout of core/state/statedb_utils.go:75-157).
func parentState(holders map[string]Holder) *state.StateDB {
	st, err := state.New(types.EmptyRootHash, state.NewDatabaseForTesting())
	must(err)
	smc := common.MasternodeVotingSMCBinary
	put := func(slot common.Hash, v common.Hash) { st.SetState(smc, slot, v) }
	for node, h := range holders {
		n := common.HexToAddress(node)
		state_ := state.GetLocMappingAtKey(common.BytesToHash(n.Bytes()), 1)
		put(common.BigToHash(state_), common.BytesToHash(common.HexToAddress(h.Owner).Bytes()))
		locVoters := common.BigToHash(state.GetLocMappingAtKey(common.BytesToHash(n.Bytes()), 2))
		put(locVoters, common.BigToHash(big.NewInt(int64(len(h.Voters)))))
		capBase := common.BigToHash(new(big.Int).Add(state_, big.NewInt(2)))
		for i, v := range h.Voters {
			va := common.HexToAddress(v[0])
			put(state.GetLocDynamicArrAtElement(locVoters, uint64(i), 1), common.BytesToHash(va.Bytes()))
			c, _ := new(big.Int).SetString(v[1], 10)
			put(common.BytesToHash(crypto.Keccak256(common.BytesToHash(va.Bytes()).Bytes(), capBase.Bytes())), common.BigToHash(c))
		}
	}
	return st
}

func logs(m map[string]uint64) map[common.Address]*XDPoS.RewardLog {
	out := map[common.Address]*XDPoS.RewardLog{}
	for a, n := range m {
		out[common.HexToAddress(a)] = &XDPoS.RewardLog{Sign: n, Reward: new(big.Int)}
	}
	return out
}

func add(out map[common.Address]*big.Int, m map[common.Address]*big.Int) {
	for a, r := range m {
		if out[a] == nil {
			out[a] = new(big.Int)
		}
		out[a].Add(out[a], r)
	}
}

func run(c Case, cc *params.ChainConfig) map[common.Address]*big.Int {
	out := map[common.Address]*big.Int{}
	st := parentState(c.Holders)
	foundation := cc.XDPoS.FoudationWalletAddr
	chain := chainCfg{cfg: cc}
	num := new(big.Int).SetUint64(c.Number)
	if c.Rule != "tiered" {
		// engine_v2_hooks.go:219-249 (the flat path of both V1 and V2 checkpoints)
		chainReward := new(big.Int).Mul(new(big.Int).SetUint64(cc.XDPoS.Reward), new(big.Int).SetUint64(params.Ether))
		chainReward = util.RewardInflation(chain, chainReward, c.Number, cc.XDCBlocksPerYear())
		rs, err := XDPoS.CalculateRewardForSigner(chainReward, logs(c.Signs), c.Total)
		must(err)
		for signer, calc := range rs {
			h, err := XDPoS.GetRewardBalancesRate(foundation, st, signer, calc, c.Number)
			must(err)
			add(out, h)
		}
		return out
	}
	// engine_v2_hooks.go:117-170 (the tiered path)
	v2 := cc.XDPoS.V2.Config(c.Round)
	proportional := cc.IsTIPProportionalReward(num)
	type tier struct {
		r       float64
		signers map[string]uint64
	}
	for _, t := range []tier{{v2.MasternodeReward, c.Master}, {v2.ProtectorReward, c.Protector}, {v2.ObserverReward, c.Observer}} {
		f := new(big.Float).Mul(new(big.Float).SetFloat64(t.r), new(big.Float).SetUint64(params.Ether))
		orig, _ := f.Int(nil)
		chainReward := util.RewardInflation(chain, orig, c.Number, cc.XDCBlocksPerYear())
		var rs map[common.Address]*big.Int
		var err error
		if proportional {
			rs, err = XDPoS.CalculateRewardForSignerProportional(chainReward, logs(t.signers), c.Slots)
		} else {
			rs, err = XDPoS.CalculateRewardForSignerFixed(chainReward, logs(t.signers))
		}
		must(err)
		for signer, calc := range rs {
			h, err := XDPoS.GetRewardBalancesRateUpgrade(cc, foundation, st, signer, calc, num)
			must(err)
			add(out, h)
		}
	}
	return out
}

// floats prints floor(reward * 1e18) for each reward given as a decimal string, the exact lines of
// engine_v2_hooks.go:136-138.
func floats(args []string) {
	type row struct {
		In  string `json:"reward"`
		Out string `json:"amount"`
	}
	var rows []row
	for _, a := range args {
		var r float64
		must(json.Unmarshal([]byte(a), &r))
		f := new(big.Float).Mul(new(big.Float).SetFloat64(r), new(big.Float).SetUint64(params.Ether))
		i, _ := f.Int(nil)
		rows = append(rows, row{a, i.String()})
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", " ")
	must(enc.Encode(rows))
}

func main() {
	if os.Args[1] == "floats" {
		floats(os.Args[2:])
		return
	}
	raw, err := os.ReadFile(os.Args[1])
	must(err)
	var cases []Case
	must(json.Unmarshal(raw, &cases))
	forks := ""
	if len(os.Args) > 2 {
		forks = os.Args[2]
	}
	var results []Result
	for _, c := range cases {
		res := Result{ID: c.ID, Rewards: map[string]string{}}
		for a, r := range run(c, config(c.Network, forks)) {
			res.Rewards[a.Hex()] = r.String()
		}
		results = append(results, res)
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", " ")
	must(enc.Encode(results))
	fmt.Fprintln(os.Stderr, "oracle: ", len(results), "cases")
}
