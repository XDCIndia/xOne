// Command oracle prints, for every built-in XDC network and every height a fork rule can flip at,
// what the pinned geth-XDC does there: the active precompile sets, the gas each precompile
// charges for a fixed probe input, the intrinsic gas of fixed messages, and the rule flags.
//
//	oracle precompiles <pin-sha>   active and executing precompile sets, gas probes
//	oracle rules <pin-sha>         rule flags and intrinsic gas
//	oracle touch <pin-sha>         does a STATICCALL touch (and so delete) an empty account?
package main

import (
	"encoding/hex"
	"encoding/json"
	"fmt"
	"math/big"
	"os"
	"reflect"
	"sort"
	"strings"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core"
	"github.com/ethereum/go-ethereum/core/vm"
	"github.com/ethereum/go-ethereum/params"
	"github.com/holiman/uint256"
)

// unscheduled is geth's placeholder for a declared but unscheduled fork.
const unscheduled = 99_999_999_999

type contract struct {
	Address string `json:"address"`
	Name    string `json:"name"`
	// GasProbe is RequiredGas(probe input of the address); absent when the address has no probe.
	GasProbe *uint64 `json:"gasProbe,omitempty"`
}

type intrinsic struct {
	Case string `json:"case"`
	Gas  uint64 `json:"gas"`
}

type row struct {
	Number uint64 `json:"number"`
	// Active is vm.ActivePrecompiles(rules): the addresses geth warms in the access list.
	Active []string `json:"active,omitempty"`
	// Executing is the key set of vm.ActivePrecompiledContracts(rules): what a call really runs.
	Executing []contract      `json:"executing"`
	Intrinsic []intrinsic     `json:"intrinsic"`
	Flags     map[string]bool `json:"flags"`
}

type network struct {
	ChainID uint64 `json:"chainId"`
	Rows    []row  `json:"rows"`
}

// probes: one fixed input per address, so gas formulas that differ between rule sets are visible.
func probes() map[common.Address][]byte {
	word := func(n uint64) []byte { return common.LeftPadBytes(new(big.Int).SetUint64(n).Bytes(), 32) }
	ff := func(n int) []byte { return []byte(strings.Repeat("\xff", n)) }
	modexp := append(append(append(word(64), word(32)...), word(64)...), ff(64+32+64)...)
	pairing := make([]byte, 192*2)
	blake := make([]byte, 213)
	blake[3] = 12 // 12 rounds
	return map[common.Address][]byte{
		common.BytesToAddress([]byte{1}):    make([]byte, 128),
		common.BytesToAddress([]byte{2}):    make([]byte, 100),
		common.BytesToAddress([]byte{3}):    make([]byte, 100),
		common.BytesToAddress([]byte{4}):    make([]byte, 100),
		common.BytesToAddress([]byte{5}):    modexp,
		common.BytesToAddress([]byte{6}):    make([]byte, 128),
		common.BytesToAddress([]byte{7}):    make([]byte, 96),
		common.BytesToAddress([]byte{8}):    pairing,
		common.BytesToAddress([]byte{9}):    blake,
		common.BytesToAddress([]byte{0x0a}): make([]byte, 192),
		common.BytesToAddress([]byte{0x1e}): make([]byte, 64),
		common.BytesToAddress([]byte{0x28}): make([]byte, 64),
		common.BytesToAddress([]byte{0x29}): make([]byte, 64),
		common.BytesToAddress([]byte{0x2a}): make([]byte, 64),
	}
}

func hexs(a common.Address) string { return strings.ToLower(a.Hex()) }

// heights returns every block a rule can flip at, plus its neighbours, from the config itself.
func heights(cfg *params.ChainConfig) []uint64 {
	set := map[uint64]bool{0: true}
	v := reflect.ValueOf(*cfg)
	for i := 0; i < v.NumField(); i++ {
		f := v.Type().Field(i)
		if f.Type != reflect.TypeOf((*big.Int)(nil)) || !strings.HasSuffix(f.Name, "Block") {
			continue
		}
		n, _ := v.Field(i).Interface().(*big.Int)
		if n == nil || !n.IsUint64() || n.Uint64() >= unscheduled {
			continue
		}
		b := n.Uint64()
		set[b] = true
		set[b+1] = true
		if b > 0 {
			set[b-1] = true
		}
	}
	// geth predicates that are not one row: first block of each effective switch.
	for _, p := range []func(*big.Int) bool{cfg.IsIstanbul, cfg.IsPetersburg, cfg.IsEIP1559, cfg.IsTIPXDCXMiner, cfg.IsTIPXDCXCancellationFee} {
		b := firstActive(p)
		if b > 0 {
			set[b-1] = true
		}
		set[b] = true
		set[b+1] = true
	}
	out := make([]uint64, 0, len(set))
	for n := range set {
		out = append(out, n)
	}
	sort.Slice(out, func(i, j int) bool { return out[i] < out[j] })
	return out
}

func firstActive(f func(*big.Int) bool) uint64 {
	lo, hi := uint64(0), uint64(1)<<40
	if f(big.NewInt(0)) {
		return 0
	}
	for hi-lo > 1 {
		mid := (lo + hi) / 2
		if f(new(big.Int).SetUint64(mid)) {
			hi = mid
		} else {
			lo = mid
		}
	}
	return hi
}

func intrinsicCases(cfg *params.ChainConfig, rules params.Rules) []intrinsic {
	// geth core.(*stateTransition).execute: rules copy with IsIstanbul=false when XDCPreEIP2028Calldata.
	intr := rules
	if core.XDCPreEIP2028Calldata(cfg, rules) {
		intr.IsIstanbul = false
	}
	from := common.HexToAddress("0x1111111111111111111111111111111111111111")
	to := common.HexToAddress("0x2222222222222222222222222222222222222222")
	data := func(zeros, nonzero int) []byte {
		return append(make([]byte, zeros), []byte(strings.Repeat("\x01", nonzero))...)
	}
	type c struct {
		name   string
		data   []byte
		create bool
		value  uint64
		self   bool
	}
	cases := []c{
		{"call-empty", nil, false, 0, false},
		{"call-4nz", data(0, 4), false, 0, false},
		{"call-10z-7nz", data(10, 7), false, 0, false},
		{"create-empty", nil, true, 0, false},
		{"create-33z-31nz", data(33, 31), true, 0, false},
		{"create-64nz", data(0, 64), true, 0, false},
		// EIP-2780 prices the base by value, self-transfer and creation (no effect before Amsterdam)
		{"callv-empty", nil, false, 1, false},
		{"self-empty", nil, false, 1, true},
		{"selfz-empty", nil, false, 0, true},
		{"createv-empty", nil, true, 1, false},
	}
	var out []intrinsic
	for _, k := range cases {
		var dst *common.Address
		if !k.create {
			dst = &to
		}
		src := from
		if k.self {
			dst = &src
		}
		g, err := core.IntrinsicGas(k.data, nil, nil, src, dst, uint256.NewInt(k.value), intr)
		if err != nil {
			panic(err)
		}
		out = append(out, intrinsic{k.name + "-" + hex.EncodeToString(k.data), g})
	}
	return out
}

func main() {
	if len(os.Args) != 3 || (os.Args[1] != "precompiles" && os.Args[1] != "rules" && os.Args[1] != "touch") {
		fmt.Fprintln(os.Stderr, "usage: oracle precompiles|rules|touch <pin-sha>")
		os.Exit(2)
	}
	mode, pin := os.Args[1], os.Args[2]
	if mode == "touch" {
		printJSON(touchVectors(pin))
		return
	}
	nets := []struct {
		name string
		cfg  *params.ChainConfig
	}{
		{"mainnet", params.XDCMainnetChainConfig},
		{"apothem", params.XDCApothemChainConfig},
		{"devnet", params.XDCDevnetChainConfig},
		{"devnet551", params.XDCDevnet551ChainConfig},
	}
	// net2040 is not in geth's params: its config is read from the committed genesis file, as a node
	// started on it reads it (same as conformance/exporter).
	raw, err := os.ReadFile("../../../../crates/xone-chainspec/genesis/net2040.json")
	if err != nil {
		panic(err)
	}
	var g2040 core.Genesis
	if err := json.Unmarshal(raw, &g2040); err != nil {
		panic(err)
	}
	nets = append(nets, struct {
		name string
		cfg  *params.ChainConfig
	}{"net2040", g2040.Config})
	// EIP-1559 active at genesis (xOneGo#643): devnet551 with `Eip1559Block = 0`.
	atGenesis := *params.XDCDevnet551ChainConfig
	atGenesis.Eip1559Block = big.NewInt(0)
	nets = append(nets, struct {
		name string
		cfg  *params.ChainConfig
	}{"devnet551-eip1559-at-0", &atGenesis})
	pr := probes()
	out := map[string]any{"pin": pin}
	res := map[string]network{}
	for _, n := range nets {
		// geth reads some TIP rows from process globals; set them as eth/backend.go does, after a
		// reset so one network's values cannot leak into the next (same as conformance/exporter).
		common.CopyXDCConstants(0)
		common.CopyXDCConstants(n.cfg.ChainID.Uint64())
		net := network{ChainID: n.cfg.ChainID.Uint64()}
		for _, h := range heights(n.cfg) {
			num := new(big.Int).SetUint64(h)
			rules := n.cfg.Rules(num, false, 0)
			r := row{Number: h, Flags: map[string]bool{
				"istanbul":         rules.IsIstanbul,
				"petersburg":       rules.IsPetersburg,
				"eip1559":          rules.IsEIP1559,
				"eip2929":          rules.IsEIP2929,
				"eip3860":          rules.IsEIP3860,
				"london":           rules.IsLondon,
				"merge":            rules.IsMerge,
				"shanghai":         rules.IsShanghai,
				"cancun":           rules.IsCancun,
				"prague":           rules.IsPrague,
				"xdcxMinerDisable": rules.IsXDCxMinerDisable,
				"preEip2028":       core.XDCPreEIP2028Calldata(n.cfg, rules),
			}}
			for _, a := range vm.ActivePrecompiles(rules) {
				r.Active = append(r.Active, hexs(a))
			}
			sort.Strings(r.Active)
			set := vm.ActivePrecompiledContracts(rules)
			addrs := make([]common.Address, 0, len(set))
			for a := range set {
				addrs = append(addrs, a)
			}
			sort.Slice(addrs, func(i, j int) bool { return addrs[i].Cmp(addrs[j]) < 0 })
			for _, a := range addrs {
				c := contract{Address: hexs(a), Name: set[a].Name()}
				if in, ok := pr[a]; ok {
					g := set[a].RequiredGas(in)
					c.GasProbe = &g
				}
				r.Executing = append(r.Executing, c)
			}
			r.Intrinsic = intrinsicCases(n.cfg, rules)
			if mode == "precompiles" {
				r.Intrinsic, r.Flags = nil, nil
			} else {
				r.Active, r.Executing = nil, nil
			}
			net.Rows = append(net.Rows, r)
		}
		res[n.name] = net
	}
	out["networks"] = res
	if mode == "precompiles" {
		m := map[string]string{}
		for a, in := range pr {
			m[hexs(a)] = hex.EncodeToString(in)
		}
		out["probes"] = m
	}
	printJSON(out)
}

func printJSON(v any) {
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", " ")
	if err := enc.Encode(v); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
