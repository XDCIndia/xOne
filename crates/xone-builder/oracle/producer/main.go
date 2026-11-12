// Command produceroracle prints what the pinned geth-XDC does where the xOne producer must agree: the gas limit
// the XDC miner gives a block (xdcGasLimit, with CalcGasLimit), the base fee tier of that block, the gas gates of
// the sign transaction, the sign transaction itself (CreateTxSign) and the timing rows of the chain configs.
//
//	GETH_SRC=<checkout at oracle/PIN> ./run-producer.sh > ../../../conformance/vectors/producer/oracle.json
//
// Every input is fixed here; every expected value is read from the pinned code (the miner's own functions through
// the overlay export_miner.go.txt, which is added to package miner for the build only).
package main

import (
	"encoding/json"
	"math/big"
	"os"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/consensus/misc/eip1559"
	"github.com/ethereum/go-ethereum/contracts"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/miner"
	"github.com/ethereum/go-ethereum/params"
)

type net struct {
	name string
	cfg  *params.ChainConfig
}

func bi(n uint64) *big.Int { return new(big.Int).SetUint64(n) }

func row(b *big.Int) any {
	if b == nil {
		return nil
	}
	return b.String()
}

func synthetic(id uint64, eip1559, g2500, dyn *big.Int) *params.ChainConfig {
	return &params.ChainConfig{
		ChainID: bi(id), XDPoS: &params.XDPoSConfig{Period: 2, Epoch: 900},
		Eip1559Block: eip1559, Gas2500xBlock: g2500, DynamicGasLimitBlock: dyn,
	}
}

func main() {
	nets := []net{
		{"apothem", params.XDCApothemChainConfig},
		{"mainnet", params.XDCMainnetChainConfig},
		// net2040 rows of conformance/vectors/consensus/net2040.forks.json
		{"net2040", synthetic(2040, bi(12600), bi(12600), bi(16200))},
		// the row declared with geth's far-future placeholder: set, never active
		{"unscheduled", synthetic(1, bi(100), bi(100), bi(99_999_999_999))},
		// no row at all: the 420M plateau rule
		{"none", synthetic(2, bi(100), nil, nil)},
	}
	parents := []uint64{420_000_000, 448_383_716, 60_000_000, 500_000_000, 500_000_001, 5_000, 1_000}
	ceils := []uint64{500_000_000, 60_000_000, 420_000_000}
	var gas, signs, stales, sblocks []map[string]any
	cfgs := map[string]any{}
	for _, n := range nets {
		c := n.cfg
		rows := map[string]any{"eip1559": row(c.Eip1559Block), "gas2500x": row(c.Gas2500xBlock), "dynamicGasLimit": row(c.DynamicGasLimitBlock)}
		cfgs[n.name] = map[string]any{"rows": rows}
		var at []uint64
		for _, r := range []*big.Int{c.DynamicGasLimitBlock, c.Gas2500xBlock, c.Eip1559Block} {
			if r != nil && r.Uint64() < 99_999_999_999 {
				at = append(at, r.Uint64()-1, r.Uint64(), r.Uint64()+1)
			}
		}
		if len(at) == 0 {
			at = []uint64{98_800_199, 98_800_200, 98_800_201, 200_000_000}
		}
		for _, num := range at {
			for _, p := range parents {
				for _, ceil := range ceils {
					var bf any
					if eip1559.FeeMarketActive(c, bi(num)) {
						bf = eip1559.CalcBaseFee(c, &types.Header{Number: bi(num - 1)}).String()
					}
					gas = append(gas, map[string]any{
						"net": n.name, "number": num, "parent_gas_limit": p, "ceil": ceil,
						"gas_limit": miner.OracleGasLimit(c, ceil, num-1, p), "base_fee": bf,
					})
				}
			}
		}
		for _, num := range []uint64{1, 14, 15, 16, 29, 30, 31, 3_000_000, 3_000_015, 83_599_985, 83_600_000, 85_000_001} {
			sblocks = append(sblocks, map[string]any{"net": n.name, "number": num, "sign_block": miner.OracleIsSignBlock(c, num)})
		}
	}
	for _, t := range [][2]uint64{{100, 100}, {100, 130}, {100, 131}, {100, 99}, {0, 30}, {0, 31}} {
		stales = append(stales, map[string]any{"block": t[0], "head": t[1], "stale": miner.OracleIsStale(t[0], t[1])})
	}
	for i, num := range []uint64{15, 30, 83_600_010, 1 << 40} {
		hash := common.BigToHash(bi(uint64(0x1234567 + i*977)))
		nonce := uint64(7 * (i + 1))
		tx := contracts.CreateTxSign(bi(num), hash, nonce, common.BlockSignersBinary)
		signs = append(signs, map[string]any{
			"number": num, "hash": hash.Hex(), "nonce": nonce, "to": tx.To().Hex(),
			"gas": tx.Gas(), "gas_price": tx.GasPrice().String(), "value": tx.Value().String(),
			"data": common.Bytes2Hex(tx.Data()),
		})
	}
	// the timing rows of the real chain configs
	timing := map[string]any{}
	for _, n := range nets[:2] {
		v2 := n.cfg.XDPoS.V2
		v2.BuildConfigIndex() // the engine does this at construction (engine_v2/engine.go:248)
		mp := map[string]any{}
		for _, round := range []uint64{0, 1, 899, 900, 1_000_000, 10_000_000, 25_000_000} {
			if c := v2.Config(round); c != nil {
				mp[big.NewInt(int64(round)).String()] = map[string]any{"minePeriod": c.MinePeriod, "timeoutPeriod": c.TimeoutPeriod}
			}
		}
		timing[n.name] = map[string]any{"period": n.cfg.XDPoS.Period, "epoch": n.cfg.XDPoS.Epoch, "switchBlock": v2.SwitchBlock.String(), "rounds": mp}
	}
	out := map[string]any{"pin": "6fc8e8c2088b7f18264f5663036669d04fa7becf", "configs": cfgs, "gaslimit": gas,
		"signblock": sblocks, "stale": stales, "signtx": signs, "timing": timing}
	e := json.NewEncoder(os.Stdout)
	e.SetIndent("", " ")
	if err := e.Encode(out); err != nil {
		panic(err)
	}
}
