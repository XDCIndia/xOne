package main

import (
	"math/big"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core"
	"github.com/ethereum/go-ethereum/core/state"
	"github.com/ethereum/go-ethereum/core/tracing"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/core/vm"
	"github.com/ethereum/go-ethereum/params"
	"github.com/holiman/uint256"
)

type touchCase struct {
	Network string `json:"network"`
	Number  uint64 `json:"number"`
	Sender  string `json:"sender"`
	Caller  string `json:"contract"`
	Code    string `json:"code"`
	Target  string `json:"target"`
	// ExistsAfter: the empty account still exists after the transaction and EIP-161 finalisation.
	ExistsAfter bool `json:"existsAfter"`
}

// touchVectors runs a contract that STATICCALLs an existing empty account, at the heights around
// the first block of geth's Istanbul rules, and reports whether the account survived: core/vm/evm.go:513
// touches it (and EIP-161 deletes it) only from those rules. The archive cannot show this, because
// prestateTracer drops empty accounts.
func touchVectors(pin string) any {
	sender := common.HexToAddress("0x00000000000000000000000000000000000000a1")
	contract := common.HexToAddress("0x00000000000000000000000000000000000000c1")
	target := common.HexToAddress("0x00000000000000000000000000000000000000ee")
	// PUSH1 0 x4 (out size, out offset, in size, in offset), PUSH20 target, PUSH2 gas, STATICCALL, POP, STOP.
	code := append(append([]byte{0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x73}, target.Bytes()...), 0x61, 0xff, 0xff, 0xfa, 0x50, 0x00)
	var cases []touchCase
	for _, n := range []struct {
		name string
		cfg  *params.ChainConfig
	}{{"mainnet", params.XDCMainnetChainConfig}, {"apothem", params.XDCApothemChainConfig}} {
		common.CopyXDCConstants(0)
		common.CopyXDCConstants(n.cfg.ChainID.Uint64())
		b := firstActive(n.cfg.IsIstanbul)
		for _, h := range []uint64{b - 1, b, b + 1} {
			num := new(big.Int).SetUint64(h)
			rules := n.cfg.Rules(num, false, 0)
			db := state.NewDatabaseForTesting()
			sdb, err := state.New(types.EmptyRootHash, db)
			must(err)
			sdb.SetBalance(sender, uint256.NewInt(1_000_000_000_000_000_000), tracing.BalanceChangeUnspecified)
			sdb.SetCode(contract, code, tracing.CodeChangeUnspecified)
			sdb.CreateAccount(target)
			// zero rules: no EIP-161 deletion, so the empty account is in the pre-state.
			root, err := sdb.Commit(params.Rules{}, 0)
			must(err)
			sdb, err = state.New(root, db)
			must(err)
			if !sdb.Exist(target) || !sdb.Empty(target) {
				panic("pre-state: target must be an existing empty account")
			}
			msg := &core.Message{
				From: sender, To: &contract, Nonce: 0, Value: new(uint256.Int), GasLimit: 200_000,
				GasPrice: uint256.NewInt(1), GasFeeCap: uint256.NewInt(1), GasTipCap: uint256.NewInt(1),
			}
			bctx := vm.BlockContext{
				CanTransfer: core.CanTransfer, Transfer: core.Transfer,
				GetHash:  func(uint64) common.Hash { return common.Hash{} },
				Coinbase: common.HexToAddress("0x00000000000000000000000000000000000000c0"),
				GasLimit: 30_000_000, BlockNumber: num, Difficulty: big.NewInt(1), BlobBaseFee: big.NewInt(0),
			}
			if rules.IsEIP1559 {
				bctx.BaseFee = new(big.Int).SetUint64(params.XDCBaseFee)
			}
			evm := vm.NewEVM(bctx, sdb, n.cfg, vm.Config{})
			res, err := core.ApplyMessage(evm, msg, core.NewGasPool(30_000_000))
			must(err)
			if res.Err != nil {
				panic(res.Err)
			}
			sdb.Finalise(rules)
			cases = append(cases, touchCase{
				Network: n.name, Number: h, Sender: sender.Hex(), Caller: contract.Hex(),
				Code: common.Bytes2Hex(code), Target: target.Hex(), ExistsAfter: sdb.Exist(target),
			})
		}
	}
	return map[string]any{"pin": pin, "cases": cases}
}

func must(err error) {
	if err != nil {
		panic(err)
	}
}
