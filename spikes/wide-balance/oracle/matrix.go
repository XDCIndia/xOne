package main

import (
	"fmt"
	"math/big"
	"sort"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core"
	"github.com/ethereum/go-ethereum/core/rawdb"
	"github.com/ethereum/go-ethereum/core/state"
	"github.com/ethereum/go-ethereum/core/tracing"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/core/vm"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/ethereum/go-ethereum/params"
	"github.com/ethereum/go-ethereum/rlp"
	"github.com/ethereum/go-ethereum/trie"
	"github.com/ethereum/go-ethereum/triedb"
	"github.com/holiman/uint256"
)

// Scenario input. Both the Go oracle and the Rust spike read these fields.
type AcctIn struct {
	Balance string            `json:"balance"` // true balance, hex
	Nonce   uint64            `json:"nonce,omitempty"`
	Code    string            `json:"code,omitempty"`
	Storage map[string]string `json:"storage,omitempty"`
}

type Op struct {
	Op       string `json:"op"` // add, sub, set_balance, snapshot, revert, tx
	Addr     string `json:"addr,omitempty"`
	Amount   string `json:"amount,omitempty"`
	Name     string `json:"name,omitempty"`
	From     string `json:"from,omitempty"`
	To       string `json:"to,omitempty"` // empty: contract creation
	Value    string `json:"value,omitempty"`
	Gas      uint64 `json:"gas,omitempty"`
	GasPrice string `json:"gas_price,omitempty"`
	Data     string `json:"data,omitempty"`
	Coinbase string `json:"coinbase,omitempty"`
	Owner    string `json:"owner,omitempty"` // TRC21 fee owner; default: the coinbase
}

type TxOut struct {
	ConsensusErr string `json:"consensus_err"`
	VMErr        string `json:"vm_err"`
	GasUsed      uint64 `json:"gas_used"`
}

type AcctOut struct {
	Exists      bool              `json:"exists"`
	Nonce       uint64            `json:"nonce"`
	Low         string            `json:"low"`
	True        string            `json:"true"`
	Wide        bool              `json:"wide"`
	CodeHash    string            `json:"code_hash"`
	StorageRoot string            `json:"storage_root"`
	Storage     map[string]string `json:"storage,omitempty"`
	Rlp         string            `json:"rlp"`
}

type Expect struct {
	StateRoot string             `json:"state_root"`
	Txs       []TxOut            `json:"txs"`
	Accounts  map[string]AcctOut `json:"accounts"`
}

type Row struct {
	ID       string              `json:"id"`
	Matrix   string              `json:"matrix_row"`
	Rule     string              `json:"rule"` // legacy: Apothem block 40M; cancun: block 72M; replay: Block
	Block    uint64              `json:"block,omitempty"`
	Time     uint64              `json:"time,omitempty"` // block timestamp (replay)
	Accounts map[string]AcctIn   `json:"accounts"`
	Ops      []Op                `json:"ops"`
	Watch    []string            `json:"watch"`
	Slots    map[string][]string `json:"slots,omitempty"`
	Expect   Expect              `json:"expect"`
}

var ruleBlock = map[string]uint64{"legacy": 40_000_000, "cancun": 72_000_000}

func pw(n uint) *big.Int { return new(big.Int).Lsh(big.NewInt(1), n) }
func big10(s string) *big.Int {
	v, ok := new(big.Int).SetString(s, 10)
	if !ok {
		panic(s)
	}
	return v
}
func addr(n byte) string { return fmt.Sprintf("0x%040x", n) }

// asm is a tiny EVM assembler.
type asm struct{ b []byte }

func (a *asm) op(o ...vm.OpCode) *asm {
	for _, x := range o {
		a.b = append(a.b, byte(x))
	}
	return a
}
func (a *asm) push(v *big.Int) *asm {
	bs := v.Bytes()
	if len(bs) == 0 {
		bs = []byte{0}
	}
	a.b = append(a.b, byte(vm.PUSH1)+byte(len(bs)-1))
	a.b = append(a.b, bs...)
	return a
}
func (a *asm) pushN(n int64) *asm { return a.push(big.NewInt(n)) }
func (a *asm) pushAddr(s string) *asm {
	a.b = append(a.b, byte(vm.PUSH20))
	a.b = append(a.b, common.HexToAddress(s).Bytes()...)
	return a
}

// call leaves the CALL result on the stack. gas < 0 means GAS (all).
func (a *asm) call(to string, value *big.Int, gas int64) *asm {
	a.pushN(0).pushN(0).pushN(0).pushN(0).push(value).pushAddr(to)
	if gas < 0 {
		a.op(vm.GAS)
	} else {
		a.pushN(gas)
	}
	return a.op(vm.CALL)
}
func (a *asm) sstore(slot int64) *asm { return a.pushN(slot).op(vm.SSTORE) }
func (a *asm) hex() string            { return fmt.Sprintf("0x%x", a.b) }

func codeOf(a *asm) string { return a.hex() }

// emptyInit returns empty runtime: PUSH1 0 PUSH1 0 RETURN.
var emptyInit = []byte{0x60, 0, 0x60, 0, 0xf3}

func create(opc vm.OpCode, value *big.Int) *asm {
	// memory[0..5] = init code (copied from the tail of this contract), then CREATE.
	a := &asm{}
	build := func(off int64) *asm {
		a := &asm{}
		a.pushN(int64(len(emptyInit))).pushN(off).pushN(0).op(vm.CODECOPY)
		if opc == vm.CREATE2 {
			a.pushN(0x5a1) // salt
		}
		a.pushN(int64(len(emptyInit))).pushN(0).push(value).op(opc)
		a.sstore(0).op(vm.STOP)
		return a
	}
	off := int64(len(build(0).b))
	a = build(off)
	a.b = append(a.b, emptyInit...)
	return a
}

func ethW(n int64) *big.Int { return new(big.Int).Mul(big.NewInt(n), big10("1000000000000000000")) }

const (
	realWide = "0x1000000000000000000000000000000000000000000000035c1e5ed14fd6d5fff" // Apothem 0x4398.. at head
	wideEOA  = "0x4398241671b3dd484fe3213a4fb7511f30e7d7c0"
	coinbase = "0x00000000000000000000000000000000000000c0"
)

func H(v *big.Int) string { return hx(v) }

func rowsFor() []Row {
	var rows []Row
	vBig := ethW(5000)                                          // 5e21 > low part of realWide
	narrowNearMax := new(big.Int).Sub(pw(256), big.NewInt(100)) // 2^256-100
	wideSmallLow := new(big.Int).Add(pw(256), big.NewInt(1000)) // low = 1000
	realW, _ := new(big.Int).SetString(realWide[2:], 16)

	eoa := func(n byte) string { return addr(0xa0 + n) }
	e1, e2, e3 := eoa(1), eoa(2), eoa(3)
	big1e30 := big10("1000000000000000000000000000000")

	add := func(id, m string, accts map[string]AcctIn, ops []Op, watch []string, slots map[string][]string, rules ...string) {
		if len(rules) == 0 {
			rules = []string{"legacy", "cancun"}
		}
		for _, r := range rules {
			rows = append(rows, Row{ID: id + "." + r, Matrix: m, Rule: r, Accounts: accts, Ops: ops, Watch: watch, Slots: slots})
		}
	}
	bal := func(v *big.Int) AcctIn { return AcctIn{Balance: H(v)} }
	code := func(v *big.Int, nonce uint64, a *asm) AcctIn {
		return AcctIn{Balance: H(v), Nonce: nonce, Code: codeOf(a)}
	}
	tx := func(from, to string, value *big.Int, gas uint64, price int64, data string) Op {
		return Op{Op: "tx", From: from, To: to, Value: H(value), Gas: gas, GasPrice: H(big.NewInt(price)), Data: data, Coinbase: coinbase}
	}

	// 1. credit to wide (narrow -> wide) and wide -> wide (block reward).
	add("credit_to_wide", "credit to wide", map[string]AcctIn{e1: bal(narrowNearMax)},
		[]Op{{Op: "add", Addr: e1, Amount: H(big.NewInt(300))}}, []string{e1}, nil, "legacy")
	add("block_reward", "block reward", map[string]AcctIn{wideEOA: bal(realW)},
		[]Op{{Op: "add", Addr: wideEOA, Amount: H(ethW(5000))}}, []string{wideEOA}, nil, "legacy")
	// 2. debit crossing back below 2^256, and debit that stays wide.
	add("debit_crossing_back", "debit crossing back below 2^256", map[string]AcctIn{e1: bal(wideSmallLow), e2: bal(wideSmallLow)},
		[]Op{{Op: "sub", Addr: e1, Amount: H(big.NewInt(1500))}, {Op: "sub", Addr: e2, Amount: H(big.NewInt(100))}},
		[]string{e1, e2}, nil, "legacy")
	// 3. SetBalance clears BigBalance.
	add("set_balance_clears", "SetBalance clearing", map[string]AcctIn{e1: bal(wideSmallLow)},
		[]Op{{Op: "set_balance", Addr: e1, Amount: H(big.NewInt(12345))}}, []string{e1}, nil, "legacy")
	// 4. journal revert after wide transitions.
	jr := []Op{
		{Op: "snapshot", Name: "s1"},
		{Op: "add", Addr: e1, Amount: H(big.NewInt(300))}, // narrow -> wide
		{Op: "snapshot", Name: "s2"},
		{Op: "set_balance", Addr: e1, Amount: H(big.NewInt(1))}, // wide -> narrow (clear)
		{Op: "revert", Name: "s2"},                              // back to wide
		{Op: "sub", Addr: e1, Amount: H(big.NewInt(250))},       // wide -> narrow
		{Op: "snapshot", Name: "s3"},
		{Op: "add", Addr: e1, Amount: H(big.NewInt(400))}, // narrow -> wide again
	}
	add("journal_revert_inner", "journal revert after a wide transition", map[string]AcctIn{e1: bal(narrowNearMax)},
		append(append([]Op{}, jr...), Op{Op: "revert", Name: "s3"}), []string{e1}, nil, "legacy")
	add("journal_revert_all", "journal revert after a wide transition", map[string]AcctIn{e1: bal(narrowNearMax)},
		append(append([]Op{}, jr...), Op{Op: "revert", Name: "s1"}), []string{e1}, nil, "legacy")
	add("journal_revert_wide_source", "journal revert after a wide transition", map[string]AcctIn{e1: bal(wideSmallLow)},
		[]Op{{Op: "snapshot", Name: "s1"}, {Op: "sub", Addr: e1, Amount: H(big.NewInt(1500))}, {Op: "revert", Name: "s1"}},
		[]string{e1}, nil, "legacy")

	// 3b. A wide value whose wrapped part is zero is "empty" to EIP-161 (geth checks Balance only).
	add("wide_low_zero_empty", "SetBalance clearing", map[string]AcctIn{e1: bal(new(big.Int).Sub(pw(256), big.NewInt(5)))},
		[]Op{{Op: "add", Addr: e1, Amount: H(big.NewInt(5))}}, []string{e1}, nil, "legacy")

	// 5. BALANCE / SELFBALANCE.
	cBal := &asm{}
	cBal.pushAddr(wideEOA).op(vm.BALANCE).sstore(0).op(vm.ADDRESS, vm.BALANCE).sstore(1).op(vm.SELFBALANCE).sstore(2).op(vm.STOP)
	cB := addr(0xc1)
	add("balance_selfbalance", "BALANCE, SELFBALANCE", map[string]AcctIn{
		e1: bal(big1e30), wideEOA: bal(realW), cB: code(realW, 1, cBal)},
		[]Op{tx(e1, cB, big.NewInt(0), 1_000_000, 0, "")}, []string{cB, wideEOA}, map[string][]string{cB: {"0x0", "0x1", "0x2"}})

	// 6. CanTransfer: top-level value > wrapped balance, and CALL value from a wide contract.
	add("can_transfer_top", "CanTransfer", map[string]AcctIn{wideEOA: bal(realW), e2: bal(big.NewInt(1))},
		[]Op{tx(wideEOA, e2, vBig, 100_000, 0, "")}, []string{wideEOA, e2}, nil)
	cCall := addr(0xc2)
	add("call_value_wide_contract", "CALL value", map[string]AcctIn{
		e1: bal(big1e30), cCall: code(realW, 1, (&asm{}).call(e2, vBig, -1).sstore(0).op(vm.STOP)), e2: bal(big.NewInt(1))},
		[]Op{tx(e1, cCall, big.NewInt(0), 1_000_000, 0, "")}, []string{cCall, e2}, map[string][]string{cCall: {"0x0"}})

	// 7. buyGas affordability (gas*price > wrapped balance, wide sender) and fee refund.
	add("buy_gas_wide_sender", "buyGas / affordability", map[string]AcctIn{wideEOA: bal(realW), e2: bal(big.NewInt(1))},
		[]Op{tx(wideEOA, e2, big.NewInt(0), 21_000, 500_000_000_000_000_000, "")}, []string{wideEOA, e2, coinbase}, nil)
	add("fee_refund_crosses_up", "fee refund", map[string]AcctIn{e1: bal(new(big.Int).Add(pw(256), big.NewInt(50_000))), e2: bal(big.NewInt(1))},
		[]Op{tx(e1, e2, big.NewInt(0), 100_000, 1, "")}, []string{e1, e2, coinbase}, nil)
	add("coinbase_wide_fee", "block reward", map[string]AcctIn{e1: bal(big1e30), e2: bal(big.NewInt(1)), coinbase: bal(wideSmallLow)},
		[]Op{tx(e1, e2, big.NewInt(1), 21_000, 3, "")}, []string{e1, e2, coinbase}, nil)

	// 8. CREATE / CREATE2 endowment from a wide contract, and creation tx from a wide EOA.
	cCr, cCr2 := addr(0xc3), addr(0xc4)
	add("create_endowment", "CREATE/CREATE2 endowment", map[string]AcctIn{
		e1: bal(big1e30), cCr: code(realW, 1, create(vm.CREATE, vBig)), cCr2: code(realW, 1, create(vm.CREATE2, vBig))},
		[]Op{tx(e1, cCr, big.NewInt(0), 1_000_000, 0, ""), tx(e1, cCr2, big.NewInt(0), 1_000_000, 0, "")},
		[]string{cCr, cCr2, e1}, map[string][]string{cCr: {"0x0"}, cCr2: {"0x0"}})
	add("create_tx_wide_eoa", "CREATE/CREATE2 endowment", map[string]AcctIn{wideEOA: bal(realW)},
		[]Op{tx(wideEOA, "", vBig, 1_000_000, 0, fmt.Sprintf("0x%x", emptyInit))}, []string{wideEOA}, nil)

	// 9. SELFDESTRUCT of a wide contract (beneficiary narrow, and wide).
	sd := func(b string) *asm { return (&asm{}).pushAddr(b).op(vm.SELFDESTRUCT) }
	cSd := addr(0xc5)
	add("selfdestruct_wide", "SELFDESTRUCT of a wide contract", map[string]AcctIn{
		e1: bal(big1e30), cSd: code(realW, 1, sd(e2)), e2: bal(big.NewInt(5))},
		[]Op{tx(e1, cSd, big.NewInt(0), 1_000_000, 0, "")}, []string{cSd, e2}, nil)
	add("selfdestruct_to_wide", "SELFDESTRUCT of a wide contract", map[string]AcctIn{
		e1: bal(big1e30), cSd: code(realW, 1, sd(e3)), e3: bal(wideSmallLow)},
		[]Op{tx(e1, cSd, big.NewInt(0), 1_000_000, 0, "")}, []string{cSd, e3}, nil)
	add("selfdestruct_to_self", "SELFDESTRUCT of a wide contract", map[string]AcctIn{
		e1: bal(big1e30), cSd: code(realW, 1, sd(cSd))},
		[]Op{tx(e1, cSd, big.NewInt(0), 1_000_000, 0, "")}, []string{cSd}, nil)

	// 10. Nested revert at depth 2: P -> Q -> R -> X, R reverts after crediting X across 2^256.
	P, Q, R, X := addr(0xd1), addr(0xd2), addr(0xd3), addr(0xd4)
	progR := (&asm{}).call(X, big.NewInt(200), -1).op(vm.POP).pushN(0).pushN(0).op(vm.REVERT)
	progQ := (&asm{}).call(R, big.NewInt(7), -1).sstore(0).call(X, big.NewInt(150), -1).sstore(1).op(vm.STOP)
	progP := (&asm{}).call(Q, big.NewInt(9), -1).sstore(0).op(vm.STOP)
	add("nested_revert_depth2", "nested revert at depth >= 2", map[string]AcctIn{
		e1: bal(big1e30), P: code(big.NewInt(1000), 1, progP), Q: code(big.NewInt(1000), 1, progQ),
		R: code(big.NewInt(500), 1, progR), X: bal(narrowNearMax)},
		[]Op{tx(e1, P, big.NewInt(1000), 2_000_000, 0, "")}, []string{P, Q, R, X},
		map[string][]string{P: {"0x0"}, Q: {"0x0", "0x1"}})

	// 11. Out-of-gas revert: child loops forever with limited gas; and a top-level OOG from a wide sender.
	T, W2 := addr(0xe1), addr(0xe2)
	loop := []byte{0x5b, 0x60, 0x00, 0x56}
	add("oog_child_revert", "out-of-gas revert", map[string]AcctIn{
		e1: bal(big1e30), T: {Balance: "0x1", Nonce: 1, Code: fmt.Sprintf("0x%x", loop)},
		W2: code(realW, 1, (&asm{}).call(T, vBig, 50_000).sstore(0).op(vm.STOP))},
		[]Op{tx(e1, W2, big.NewInt(0), 1_000_000, 0, "")}, []string{W2, T}, map[string][]string{W2: {"0x0"}})
	add("oog_top_wide_sender", "out-of-gas revert", map[string]AcctIn{wideEOA: bal(realW), T: {Balance: "0x1", Nonce: 1, Code: fmt.Sprintf("0x%x", loop)}},
		[]Op{tx(wideEOA, T, vBig, 100_000, 0, "")}, []string{wideEOA, T}, nil)

	// 11b. A wide contract debits itself across 2^256 in a frame that then reverts.
	W4 := addr(0xe4)
	add("revert_after_wide_debit", "journal revert after a wide transition", map[string]AcctIn{
		e1: bal(big1e30), W4: code(realW, 1, (&asm{}).call(e2, vBig, -1).op(vm.POP).pushN(0).pushN(0).op(vm.REVERT)), e2: bal(big.NewInt(1))},
		[]Op{tx(e1, W4, big.NewInt(0), 1_000_000, 0, "")}, []string{W4, e2}, nil)

	// 12. Precompile call with value from a wide contract (identity, 0x04).
	W3 := addr(0xe3)
	add("precompile_value", "precompile calls", map[string]AcctIn{
		e1: bal(big1e30), W3: code(realW, 1, (&asm{}).call(addr(4), vBig, -1).sstore(0).op(vm.STOP))},
		[]Op{tx(e1, W3, big.NewInt(0), 1_000_000, 0, "")}, []string{W3, addr(4)}, map[string][]string{W3: {"0x0"}})
	return rows
}

func rulesFor(rule string, block uint64) (*params.ChainConfig, params.Rules, *big.Int) {
	cfg := params.XDCApothemChainConfig
	if block == 0 {
		block = ruleBlock[rule]
	}
	bn := new(big.Int).SetUint64(block)
	return cfg, cfg.Rules(bn, false, 0), bn
}

func pb(s string) *big.Int {
	if s == "" {
		return new(big.Int)
	}
	v, ok := new(big.Int).SetString(s[2:], 16)
	if !ok {
		panic(s)
	}
	return v
}

func u256(v *big.Int) *uint256.Int {
	u, o := uint256.FromBig(v)
	if o {
		panic("amount exceeds uint256")
	}
	return u
}

func runRow(r *Row) {
	common.CopyXDCConstants(51)
	cfg, rules, bn := rulesFor(r.Rule, r.Block)
	db := state.NewDatabaseForTesting()
	sdb, err := state.New(types.EmptyRootHash, db)
	must(err)
	// Pre-state: build wide balances with the same AddBalance path geth uses.
	for a, in := range r.Accounts {
		ad := common.HexToAddress(a)
		want := pb(in.Balance)
		first := want
		if want.Cmp(new(big.Int).Sub(pw(256), big.NewInt(1))) > 0 {
			first = new(big.Int).Sub(pw(256), big.NewInt(1))
		}
		sdb.SetBalance(ad, u256(first), tracing.BalanceChangeUnspecified)
		for rest := new(big.Int).Sub(want, first); rest.Sign() > 0; {
			step := rest
			if step.Cmp(new(big.Int).Sub(pw(256), big.NewInt(1))) > 0 {
				step = new(big.Int).Sub(pw(256), big.NewInt(1))
			}
			sdb.AddBalance(ad, u256(step), tracing.BalanceChangeUnspecified)
			rest = new(big.Int).Sub(rest, step)
		}
		sdb.SetNonce(ad, in.Nonce, tracing.NonceChangeUnspecified)
		if in.Code != "" {
			sdb.SetCode(ad, common.FromHex(in.Code), tracing.CodeChangeUnspecified)
		}
		for k, v := range in.Storage {
			sdb.SetState(ad, common.HexToHash(k), common.HexToHash(v))
		}
	}
	root, err := sdb.Commit(rules, 0)
	must(err)
	sdb, err = state.New(root, db)
	must(err)

	snaps := map[string]int{}
	r.Expect.Txs = []TxOut{}
	for _, op := range r.Ops {
		switch op.Op {
		case "add":
			sdb.AddBalance(common.HexToAddress(op.Addr), u256(pb(op.Amount)), tracing.BalanceChangeUnspecified)
		case "sub":
			sdb.SubBalance(common.HexToAddress(op.Addr), u256(pb(op.Amount)), tracing.BalanceChangeUnspecified)
		case "set_balance":
			sdb.SetBalance(common.HexToAddress(op.Addr), u256(pb(op.Amount)), tracing.BalanceChangeUnspecified)
		case "snapshot":
			snaps[op.Name] = sdb.Snapshot()
		case "revert":
			sdb.RevertToSnapshot(snaps[op.Name])
		case "tx":
			r.Expect.Txs = append(r.Expect.Txs, runTx(sdb, cfg, rules, bn, r.Rule, r.Time, op))
		default:
			panic("unknown op " + op.Op)
		}
	}
	final, err := sdb.Commit(rules, 1)
	must(err)
	r.Expect.StateRoot = final.Hex()
	r.Expect.Accounts = map[string]AcctOut{}
	post, err := state.New(final, db)
	must(err)
	tr, err := db.OpenTrie(final)
	must(err)
	for _, a := range r.Watch {
		ad := common.HexToAddress(a)
		out := AcctOut{Exists: post.Exist(ad)}
		if out.Exists {
			low := post.GetBalance(ad)
			tru := low.ToBig()
			if bb := post.GetBigBalance(ad); bb != nil {
				tru, out.Wide = bb, true
			}
			out.Nonce, out.Low, out.True = post.GetNonce(ad), H(low.ToBig()), H(tru)
			out.CodeHash, out.StorageRoot = post.GetCodeHash(ad).Hex(), post.GetStorageRoot(ad).Hex()
			if slots := r.Slots[a]; len(slots) > 0 {
				out.Storage = map[string]string{}
				for _, k := range slots {
					out.Storage[k] = post.GetState(ad, common.HexToHash(k)).Hex()
				}
			}
			acc, err := tr.(*trie.StateTrie).GetAccount(ad)
			must(err)
			raw, err := rlp.EncodeToBytes(acc)
			must(err)
			out.Rlp = fmt.Sprintf("0x%x", raw)
			if acc.BigBalance != nil && !out.Wide || acc.BigBalance == nil && out.Wide {
				panic("trie/state wide disagreement")
			}
		}
		r.Expect.Accounts[a] = out
	}
	_ = crypto.Keccak256
	_ = rawdb.NewMemoryDatabase
	_ = triedb.NewDatabase
}

func runTx(sdb *state.StateDB, cfg *params.ChainConfig, rules params.Rules, bn *big.Int, rule string, time uint64, op Op) TxOut {
	from := common.HexToAddress(op.From)
	msg := &core.Message{
		From: from, Nonce: sdb.GetNonce(from), Value: u256(pb(op.Value)), GasLimit: op.Gas,
		GasPrice: u256(pb(op.GasPrice)), GasFeeCap: u256(pb(op.GasPrice)), GasTipCap: u256(pb(op.GasPrice)),
		Data: common.FromHex(op.Data),
	}
	if op.To != "" {
		t := common.HexToAddress(op.To)
		msg.To = &t
	}
	cb := common.HexToAddress(op.Coinbase)
	bctx := vm.BlockContext{
		CanTransfer: core.CanTransfer, Transfer: core.Transfer,
		GetHash:  func(uint64) common.Hash { return common.Hash{} },
		Coinbase: cb, GasLimit: 30_000_000, BlockNumber: bn, Time: time, Difficulty: big.NewInt(1),
		BlobBaseFee: big.NewInt(0),
	}
	if rule == "cancun" {
		bctx.BaseFee = big.NewInt(0)
	}
	evm := vm.NewEVM(bctx, sdb, cfg, vm.Config{})
	// XDC pays the fee to the coinbase owner after TIPTRC21Fee; the coinbase is its own owner here.
	owner := cb
	if op.Owner != "" {
		owner = common.HexToAddress(op.Owner)
	}
	res, err := core.ApplyMessage(evm, msg, core.NewGasPool(30_000_000), owner)
	out := TxOut{}
	if err != nil {
		out.ConsensusErr = err.Error()
	} else {
		out.GasUsed = res.UsedGas
		if res.Err != nil {
			out.VMErr = res.Err.Error()
		}
	}
	sdb.Finalise(rules)
	return out
}

func matrixVectors() any {
	rows := rowsFor()
	sort.SliceStable(rows, func(i, j int) bool { return rows[i].ID < rows[j].ID })
	for i := range rows {
		runRow(&rows[i])
	}
	return rows
}
