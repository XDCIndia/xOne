// Command txpooloracle drives the pinned geth-XDC legacy transaction pool (core/txpool/legacypool) on scripted and
// seeded scenarios and prints, for every step, what the pool answered and what it holds afterwards.
//
//	txpooloracle > scenarios.json
//
// Every input is built here from fixed seeds; every expected value is read from the pool. A scenario whose result
// differs between repeated runs (Go map order, equal-price ties) is reported on stderr and left out of the file, so
// the file holds only outcomes the pinned pool decides deterministically. The real clock is not scriptable: queue
// heartbeats are set with OracleAge (export.go.txt) and the pool's own eviction body is run with OracleEvict.
package main

import (
	"bytes"
	"crypto/ecdsa"
	"encoding/json"
	"fmt"
	"math/big"
	"math/rand"
	"os"
	"os/exec"
	"strings"
	"sync"
	"time"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core/state"
	"github.com/ethereum/go-ethereum/core/tracing"
	"github.com/ethereum/go-ethereum/core/txpool/legacypool"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/ethereum/go-ethereum/params"
	"github.com/holiman/uint256"
)

const (
	h1 = 85_000_000 // Apothem after Prague and Gas2500x: 625 gwei tier, 16 gas per calldata byte
	h2 = 80_000_000 // Eip1559 and the 12.5 gwei tier, before Gas2500x and Prague: 68 gas per calldata byte
	h3 = 65_000_000 // London, before Eip1559: legacy transactions only
	gl = 50_000_000
)

var (
	signers   = common.HexToAddress("0x0000000000000000000000000000000000000089")
	randomize = common.HexToAddress("0x0000000000000000000000000000000000000090")
	ether     = new(big.Int).Exp(big.NewInt(10), big.NewInt(24), nil)
	gwei      = big.NewInt(1_000_000_000)
	keys      []*ecdsa.PrivateKey
	addrs     []common.Address
)

func init() {
	for i := 0; i < 40; i++ {
		k, err := crypto.ToECDSA(crypto.Keccak256([]byte(fmt.Sprintf("xone-txpool-oracle-%d", i))))
		if err != nil {
			panic(err)
		}
		keys = append(keys, k)
		addrs = append(addrs, crypto.PubkeyToAddress(k.PublicKey))
	}
}

type Acct struct {
	Nonce   uint64 `json:"nonce"`
	Balance string `json:"balance"`
	Code    string `json:"code,omitempty"`
}

type PoolCfg struct {
	PriceBump    uint64 `json:"price_bump"`
	AccountSlots uint64 `json:"account_slots"`
	GlobalSlots  uint64 `json:"global_slots"`
	AccountQueue uint64 `json:"account_queue"`
	GlobalQueue  uint64 `json:"global_queue"`
	LifetimeSecs uint64 `json:"lifetime_secs"`
}

type Step struct {
	Op       string          `json:"op"`
	Txs      []string        `json:"txs,omitempty"`
	Number   uint64          `json:"number,omitempty"`
	GasLimit uint64          `json:"gas_limit,omitempty"`
	Accounts map[string]Acct `json:"accounts,omitempty"`
	Addr     string          `json:"addr,omitempty"`
	Secs     uint64          `json:"secs,omitempty"`
	Tip      uint64          `json:"tip,omitempty"`
	Addrs    []string        `json:"addrs,omitempty"`
	Syncing  bool            `json:"syncing,omitempty"`
}

type Expect struct {
	Errs    []string            `json:"errs,omitempty"`
	Pending map[string][]string `json:"pending"`
	Queued  map[string][]string `json:"queued"`
	Nonces  map[string]uint64   `json:"nonces"`
}

type Scenario struct {
	ID         string          `json:"id"`
	Note       string          `json:"note"`
	Number     uint64          `json:"number"`
	GasLimit   uint64          `json:"gas_limit"`
	Config     PoolCfg         `json:"config"`
	MinTip     uint64          `json:"min_tip"`
	Syncing    bool            `json:"syncing"`
	Authorised []string        `json:"authorised"`
	Accounts   map[string]Acct `json:"accounts"`
	Steps      []Step          `json:"steps"`
	Expect     []Expect        `json:"expect,omitempty"`
}

func ac(n uint64, bal *big.Int) Acct { return Acct{Nonce: n, Balance: bal.String()} }

func hx(a common.Address) string { return strings.ToLower(a.Hex()) }

func defaultCfg() PoolCfg {
	d := legacypool.DefaultConfig
	return PoolCfg{d.PriceBump, d.AccountSlots, d.GlobalSlots, d.AccountQueue, d.GlobalQueue, uint64(d.Lifetime / time.Second)}
}

// ---- scenario building ------------------------------------------------------------------------------------------

type B struct {
	sc    *Scenario
	cfg   *params.ChainConfig
	used  map[string]bool
	loose bool
}

func apothem() *params.ChainConfig {
	c := *params.XDCApothemChainConfig
	return &c
}

func newB(id, note string, number uint64) *B {
	b := &B{sc: &Scenario{ID: id, Note: note, Number: number, GasLimit: gl, Config: defaultCfg(), MinTip: 1, Accounts: map[string]Acct{}}, cfg: apothem(), used: map[string]bool{}}
	return b
}

func (b *B) fund(i int, nonce uint64, bal *big.Int) *B {
	b.sc.Accounts[hx(addrs[i])] = Acct{Nonce: nonce, Balance: bal.String()}
	return b
}

func (b *B) rich(n int) *B {
	for i := 0; i < n; i++ {
		b.fund(i, 0, ether)
	}
	return b
}

func (b *B) signers(idx ...int) *B {
	b.sc.Authorised = []string{}
	for _, i := range idx {
		b.sc.Authorised = append(b.sc.Authorised, hx(addrs[i]))
	}
	return b
}

func (b *B) cfgOpt(f func(*PoolCfg)) *B { f(&b.sc.Config); return b }

func raw(tx *types.Transaction) string {
	bin, err := tx.MarshalBinary()
	if err != nil {
		panic(err)
	}
	return fmt.Sprintf("0x%x", bin)
}

// add sends each transaction as its own step; batch sends them as one Add call.
func (b *B) add(txs ...*types.Transaction) *B {
	for _, t := range txs {
		b.batch(t)
	}
	return b
}

func (b *B) batch(txs ...*types.Transaction) *B {
	s := Step{Op: "add"}
	for _, t := range txs {
		s.Txs = append(s.Txs, raw(t))
	}
	b.sc.Steps = append(b.sc.Steps, s)
	return b
}

func (b *B) reset(number uint64, gasLimit uint64, accts map[int]Acct) *B {
	m := map[string]Acct{}
	for i, a := range accts {
		m[hx(addrs[i])] = a
	}
	b.sc.Steps = append(b.sc.Steps, Step{Op: "reset", Number: number, GasLimit: gasLimit, Accounts: m})
	return b
}

func (b *B) age(i int, secs uint64) *B {
	b.sc.Steps = append(b.sc.Steps, Step{Op: "age", Addr: hx(addrs[i]), Secs: secs})
	return b
}

func (b *B) evict() *B { b.sc.Steps = append(b.sc.Steps, Step{Op: "evict"}); return b }

func (b *B) gasTip(t uint64) *B {
	b.sc.Steps = append(b.sc.Steps, Step{Op: "set_gas_tip", Tip: t})
	return b
}

func (b *B) setSigners(syncing bool, idx ...int) *B {
	s := Step{Op: "signers", Syncing: syncing, Addrs: []string{}}
	for _, i := range idx {
		s.Addrs = append(s.Addrs, hx(addrs[i]))
	}
	b.sc.Steps = append(b.sc.Steps, s)
	return b
}

// floor is the XDC tier floor of validation.go:409-420 for a head.
func floor(c *params.ChainConfig, head uint64) *big.Int {
	n := new(big.Int).SetUint64(head + 1)
	if c.IsGas50x(n) {
		return params.XDCBaseFeeForBlock(c, n)
	}
	return big.NewInt(common.DefaultMinGasPrice)
}

func signer(c *params.ChainConfig) types.Signer { return types.LatestSignerForChainID(c.ChainID) }

func (b *B) leg(i int, nonce, gas uint64, price *big.Int, to *common.Address, val int64, data []byte) *types.Transaction {
	b.uniq(price)
	t, err := types.SignTx(types.NewTx(&types.LegacyTx{Nonce: nonce, GasPrice: price, Gas: gas, To: to, Value: big.NewInt(val), Data: data}), types.NewEIP155Signer(b.cfg.ChainID), keys[i])
	if err != nil {
		panic(err)
	}
	return t
}

func (b *B) homestead(i int, nonce, gas uint64, price *big.Int) *types.Transaction {
	t, err := types.SignTx(types.NewTransaction(nonce, common.Address{1}, big.NewInt(1), gas, price, nil), types.HomesteadSigner{}, keys[i])
	if err != nil {
		panic(err)
	}
	return t
}

func (b *B) t1(i int, nonce, gas uint64, price *big.Int) *types.Transaction {
	t, err := types.SignNewTx(keys[i], signer(b.cfg), &types.AccessListTx{ChainID: b.cfg.ChainID, Nonce: nonce, GasPrice: price, Gas: gas, To: &common.Address{1}, Value: big.NewInt(1),
		AccessList: types.AccessList{{Address: common.Address{2}, StorageKeys: []common.Hash{{3}}}}})
	if err != nil {
		panic(err)
	}
	return t
}

func (b *B) t2(i int, nonce, gas uint64, feeCap, tip *big.Int) *types.Transaction {
	t, err := types.SignNewTx(keys[i], signer(b.cfg), &types.DynamicFeeTx{ChainID: b.cfg.ChainID, Nonce: nonce, GasTipCap: tip, GasFeeCap: feeCap, Gas: gas, To: &common.Address{1}, Value: big.NewInt(1)})
	if err != nil {
		panic(err)
	}
	return t
}

func (b *B) uniq(p *big.Int) {
	if p.Sign() == 0 || b.loose {
		return
	}
	k := p.String()
	if b.used[k] && strings.HasPrefix(b.sc.ID, "rnd-") {
		panic("price collision " + k + " in " + b.sc.ID)
	}
	b.used[k] = true
}

func signData(target uint64) []byte {
	data := common.Hex2Bytes(common.HexSignMethod)
	data = append(data, common.LeftPadBytes(new(big.Int).SetUint64(target).Bytes(), 32)...)
	var h common.Hash
	h[0] = byte(target)
	return append(data, h[:]...)
}

// special builds a sign transaction (contracts.CreateTxSign): selector, block number, block hash; gas price 0.
func (b *B) special(i int, nonce, target uint64) *types.Transaction {
	return b.leg(i, nonce, 200000, new(big.Int), &signers, 0, signData(target))
}

func (b *B) specialRaw(i int, nonce uint64, to common.Address, data []byte) *types.Transaction {
	return b.leg(i, nonce, 200000, new(big.Int), &to, 0, data)
}

func p(base *big.Int, k int64) *big.Int { return new(big.Int).Add(base, big.NewInt(k)) }

func pct(x *big.Int, n int64) *big.Int {
	return new(big.Int).Div(new(big.Int).Mul(x, big.NewInt(100+n)), big.NewInt(100))
}

func ad(i int) *common.Address { a := addrs[i]; return &a }

// ---- running ----------------------------------------------------------------------------------------------------

type chain struct {
	cfg  *params.ChainConfig
	head *types.Header
	st   *state.StateDB
}

func (c *chain) Config() *params.ChainConfig                                   { return c.cfg }
func (c *chain) CurrentBlock() *types.Header                                   { return c.head }
func (c *chain) GetBlock(common.Hash, uint64) *types.Block                     { return nil }
func (c *chain) StateAt(common.Hash, *big.Int, uint64) (*state.StateDB, error) { return c.st, nil }

type reserver struct{}

func (reserver) Hold(common.Address) error    { return nil }
func (reserver) Release(common.Address) error { return nil }
func (reserver) Has(common.Address) bool      { return false }

func header(number, gasLimit uint64) *types.Header {
	return &types.Header{Number: new(big.Int).SetUint64(number), Difficulty: big.NewInt(1), GasLimit: gasLimit}
}

func build(accts map[string]Acct) *state.StateDB {
	st, _ := state.New(types.EmptyRootHash, state.NewDatabaseForTesting())
	for a, v := range accts {
		bal, _ := new(big.Int).SetString(v.Balance, 10)
		addr := common.HexToAddress(a)
		st.SetNonce(addr, v.Nonce, tracing.NonceChangeUnspecified)
		st.SetBalance(addr, uint256.MustFromBig(bal), tracing.BalanceChangeUnspecified)
		if v.Code != "" {
			st.SetCode(addr, common.FromHex(v.Code), tracing.CodeChangeUnspecified)
		}
	}
	return st
}

func errName(err error) string {
	if err == nil {
		return ""
	}
	m := err.Error()
	if i := strings.Index(m, ": "); i >= 0 {
		m = m[:i]
	}
	return m
}

func run(sc *Scenario) []Expect {
	cfg := apothem()
	ch := &chain{cfg: cfg, head: header(sc.Number, sc.GasLimit), st: build(sc.Accounts)}
	pc := legacypool.DefaultConfig
	pc.Journal = ""
	pc.PriceBump, pc.AccountSlots, pc.GlobalSlots, pc.AccountQueue, pc.GlobalQueue = sc.Config.PriceBump, sc.Config.AccountSlots, sc.Config.GlobalSlots, sc.Config.AccountQueue, sc.Config.GlobalQueue
	pc.Lifetime = time.Duration(sc.Config.LifetimeSecs) * time.Second
	pool := legacypool.New(pc, ch)
	var mu sync.Mutex
	auth := map[common.Address]bool{}
	syncing := sc.Syncing
	for _, a := range sc.Authorised {
		auth[common.HexToAddress(a)] = true
	}
	if sc.Authorised != nil || sc.Syncing {
		pool.SetIsSigner(func(a common.Address) bool { mu.Lock(); defer mu.Unlock(); return !syncing && auth[a] })
	}
	if err := pool.Init(sc.MinTip, ch.head, reserver{}); err != nil {
		panic(err)
	}
	defer pool.Close()
	var out []Expect
	for _, s := range sc.Steps {
		var e Expect
		switch s.Op {
		case "add":
			txs := make([]*types.Transaction, len(s.Txs))
			for i, r := range s.Txs {
				tx := new(types.Transaction)
				if err := tx.UnmarshalBinary(common.FromHex(r)); err != nil {
					panic(err)
				}
				txs[i] = tx
			}
			for _, err := range pool.Add(txs, true) {
				e.Errs = append(e.Errs, errName(err))
			}
		case "reset":
			ch.head, ch.st = header(s.Number, s.GasLimit), build(s.Accounts)
			pool.Reset(nil, ch.head)
		case "age":
			pool.OracleAge(common.HexToAddress(s.Addr), time.Duration(s.Secs)*time.Second)
		case "evict":
			pool.OracleEvict()
		case "set_gas_tip":
			pool.SetGasTip(new(big.Int).SetUint64(s.Tip))
		case "signers":
			mu.Lock()
			auth = map[common.Address]bool{}
			for _, a := range s.Addrs {
				auth[common.HexToAddress(a)] = true
			}
			syncing = s.Syncing
			mu.Unlock()
		default:
			panic("op " + s.Op)
		}
		pend, que := pool.Content()
		e.Pending, e.Queued, e.Nonces = map[string][]string{}, map[string][]string{}, map[string]uint64{}
		fill := func(dst map[string][]string, src map[common.Address][]*types.Transaction) {
			for a, l := range src {
				for _, t := range l {
					dst[hx(a)] = append(dst[hx(a)], t.Hash().Hex())
				}
			}
		}
		fill(e.Pending, pend)
		fill(e.Queued, que)
		for _, a := range addrs {
			if n := pool.Nonce(a); n != 0 { // absent means 0
				e.Nonces[hx(a)] = n
			}
		}
		out = append(out, e)
	}
	return out
}

// ---- scripted scenarios -----------------------------------------------------------------------------------------

func scripted() []*B {
	var all []*B
	mk := func(id, note string, head uint64) *B {
		b := newB(id, note, head).rich(20)
		all = append(all, b)
		return b
	}
	cfg := apothem()
	f1, f2, f3 := floor(cfg, h1), floor(cfg, h2), floor(cfg, h3)
	to := ad(30)

	b := mk("adm-basic-pending", "an executable transaction goes to pending, a gapped one to queued, the gap filler promotes both", h1)
	b.add(b.leg(0, 0, 21000, p(f1, 1), to, 1, nil)).add(b.leg(0, 2, 21000, p(f1, 2), to, 1, nil)).add(b.leg(0, 1, 21000, p(f1, 3), to, 1, nil))

	b = mk("adm-nonce-too-low", "state nonce 5: nonce 4 is refused, 5 accepted", h1)
	b.fund(0, 5, ether).add(b.leg(0, 4, 21000, p(f1, 1), to, 1, nil), b.leg(0, 5, 21000, p(f1, 2), to, 1, nil))

	b = mk("adm-balance-boundary", "cost equal to the balance passes, one wei more fails", h1)
	price := p(f1, 7)
	cost := new(big.Int).Add(new(big.Int).Mul(price, big.NewInt(21000)), big.NewInt(1000))
	b.fund(1, 0, cost).fund(2, 0, new(big.Int).Sub(cost, big.NewInt(1)))
	b.add(b.leg(1, 0, 21000, price, to, 1000, nil), b.leg(2, 0, 21000, price, to, 1000, nil))

	b = mk("adm-cumulative-spend", "queued and pending costs add up against the balance", h1)
	c1 := new(big.Int).Mul(p(f1, 1), big.NewInt(21000))
	b.fund(0, 0, new(big.Int).Add(new(big.Int).Mul(c1, big.NewInt(2)), big.NewInt(500)))
	b.add(b.leg(0, 0, 21000, p(f1, 1), to, 0, nil), b.leg(0, 1, 21000, p(f1, 2), to, 0, nil), b.leg(0, 2, 21000, p(f1, 3), to, 0, nil))

	nz := bytes.Repeat([]byte{1}, 100)
	b = mk("gas-intrinsic-prague", "head after Prague: 16 gas per non-zero byte, floor 40 per byte (EIP-7623): the floor is the minimum", h1)
	b.add(b.leg(0, 0, 21000+16*100, p(f1, 1), to, 0, nz), b.leg(0, 0, 21000+40*100-1, p(f1, 2), to, 0, nz), b.leg(0, 0, 21000+40*100, p(f1, 3), to, 0, nz))

	b = mk("gas-intrinsic-pre-prague", "head before Prague: 68 gas per non-zero byte", h2)
	b.add(b.leg(0, 0, 21000+68*100-1, p(f2, 1), to, 0, nz), b.leg(0, 0, 21000+68*100, p(f2, 2), to, 0, nz))

	b = mk("gas-create", "creation: 53000 before Eip1559 would not matter on this head; 32000 extra, init-code size from Eip1559", h1)
	big49 := bytes.Repeat([]byte{0}, 49153)
	b.add(b.leg(0, 0, 5_000_000, p(f1, 1), nil, 0, big49), b.leg(0, 0, 5_000_000, p(f1, 2), nil, 0, big49[:49152]))

	b = mk("gas-create-pre-eip1559", "head before Eip1559: no init-code limit", h3)
	b.add(b.leg(0, 0, 5_000_000, p(f3, 1), nil, 0, big49))

	b = mk("gas-over-block-limit", "gas above the head's gas limit is refused; equal is accepted", h1)
	b.fund(0, 0, new(big.Int).Mul(ether, big.NewInt(100)))
	b.add(b.leg(0, 0, gl+1, p(f1, 1), to, 0, nil), b.leg(0, 0, gl, p(f1, 2), to, 0, nil))

	b = mk("size-oversize", "128 KiB boundary: the size is the encoded size", h1)
	var okd, badd []byte
	for l := 130800; l < 131100; l++ {
		t := b.leg(0, 0, 4_000_000, p(f1, 1), to, 0, make([]byte, l))
		if t.Size() == 131072 && okd == nil {
			okd = make([]byte, l)
		}
		if t.Size() == 131073 && badd == nil {
			badd = make([]byte, l)
		}
	}
	b.fund(0, 0, new(big.Int).Mul(ether, big.NewInt(100)))
	b.add(b.leg(0, 0, 4_000_000, p(f1, 2), to, 0, okd), b.leg(0, 1, 4_000_000, p(f1, 3), to, 0, badd))

	b = mk("price-min-tip", "a gas price of 0 is below the pool minimum; the tier floor is the next check", h1)
	b.add(b.leg(0, 0, 21000, new(big.Int), to, 0, nil))

	b = mk("floor-tier-2500x", "625 gwei floor at the Gas2500x head: one wei under is refused", h1)
	b.add(b.leg(0, 0, 21000, p(f1, -1), to, 0, nil), b.leg(0, 0, 21000, f1, to, 0, nil))

	b = mk("floor-tier-50x", "12.5 gwei floor before Gas2500x", h2)
	b.add(b.leg(0, 0, 21000, p(f2, -1), to, 0, nil), b.leg(0, 0, 21000, f2, to, 0, nil))

	b = mk("floor-before-50x", "0.25 gwei floor (DefaultMinGasPrice) before Gas50x, if the head is before it", 40_000_000)
	b.add(b.leg(0, 0, 21000, p(floor(cfg, 40_000_000), -1), to, 0, nil), b.leg(0, 0, 21000, floor(cfg, 40_000_000), to, 0, nil))

	b = mk("floor-dynamic-fee-uses-fee-cap", "the floor reads the fee cap of a type 2 transaction, not the tip", h1)
	b.add(b.t2(0, 0, 21000, p(f1, -1), big.NewInt(1)), b.t2(0, 0, 21000, f1, big.NewInt(1)))

	b = mk("type-gating-pre-eip1559", "types 1 and 2 are refused before Eip1559 on XDC", h3)
	b.add(b.t1(0, 0, 30000, p(f3, 1)), b.t2(0, 0, 30000, p(f3, 2), big.NewInt(1)), b.leg(0, 0, 21000, p(f3, 3), to, 0, nil))

	b = mk("type-gating-eip1559", "types 1 and 2 are accepted from Eip1559", h2)
	b.add(b.t1(0, 0, 30000, p(f2, 1)), b.t2(1, 0, 21000, p(f2, 2), big.NewInt(1)))

	b = mk("type-tip-above-cap", "tip above fee cap", h1)
	b.add(b.t2(0, 0, 21000, p(f1, 1), p(f1, 2)))

	b = mk("tx-homestead-signature", "an unprotected legacy signature is accepted", h1)
	b.add(b.homestead(0, 0, 21000, p(f1, 1)))

	// ---- special transactions
	b = mk("special-signer-fast-path", "a sign transaction from a signer at the pending nonce is promoted at price 0, below every floor", h1)
	b.signers(0, 1).fund(0, 0, big.NewInt(0)).fund(1, 3, big.NewInt(0))
	b.add(b.special(0, 0, h1-5), b.special(1, 3, h1-4), b.special(1, 4, h1-3))

	b = mk("special-non-signer", "a sign transaction from an address outside the set is refused", h1)
	b.signers(0).add(b.special(1, 0, h1-5), b.special(0, 0, h1-5))

	b = mk("special-syncing", "while syncing nobody is a signer: a special transaction is refused", h1)
	b.signers(0)
	b.sc.Syncing = true
	b.add(b.special(0, 0, h1-5)).setSigners(false, 0).add(b.special(0, 0, h1-6))

	b = mk("special-nonce-gap", "a special transaction at a gapped nonce does not take the fast path: it queues", h1)
	b.signers(0).add(b.special(0, 3, h1-5), b.special(0, 0, h1-5), b.special(0, 1, h1-5), b.special(0, 2, h1-5))

	b = mk("special-duplicate", "a second special transaction at a pending nonce is a duplicate", h1)
	b.signers(0).add(b.special(0, 0, h1-5), b.special(0, 0, h1-6), b.special(0, 1, h1-7))

	b = mk("special-expired-replace", "a signing slot expired (head > block + 2 epochs) is replaced by a fresh sign transaction", h1)
	b.signers(0)
	b.add(b.special(0, 0, h1-2000), b.special(0, 1, h1-5), b.special(0, 0, h1-6), b.special(0, 1, h1-6))

	b = mk("special-expired-boundary", "expiry is strict: head == block + 2*epoch is not expired", h1)
	b.signers(0).add(b.special(0, 0, h1-1800), b.special(0, 0, h1-1))
	b2 := mk("special-expired-boundary-2", "head == block + 2*epoch + 1 is expired", h1)
	b2.signers(0).add(b2.special(0, 0, h1-1801), b2.special(0, 0, h1-1))

	b = mk("special-bad-calldata", "wrong calldata from a signer takes the normal path: tip is exempt, the tier floor is exempt, slots apply", h1)
	b.signers(0)
	good := common.Hex2Bytes(common.HexSignMethod)
	b.add(b.specialRaw(0, 0, signers, good), b.specialRaw(0, 0, signers, append(append([]byte{}, good...), make([]byte, 65)...)),
		b.specialRaw(0, 1, signers, bytes.Repeat([]byte{0xe3}, 68)))

	b = mk("special-bad-length-full-pool", "a full pool refuses a signer's sign call of 69 bytes (not the sign shape, no fast path) and takes the exact 68 bytes", h1)
	b.cfgOpt(func(c *PoolCfg) { c.GlobalSlots, c.GlobalQueue = 2, 1 })
	b.signers(7)
	b.add(b.leg(0, 0, 21000, p(f1, 100), to, 0, nil), b.leg(1, 0, 21000, p(f1, 101), to, 0, nil), b.leg(2, 0, 21000, p(f1, 102), to, 0, nil),
		b.specialRaw(7, 0, signers, append(signData(h1-5), 0)), b.special(7, 0, h1-5))

	b = mk("special-randomize", "0x90 with a setSecret or setOpening selector is special; other calldata is not on the fast path", h1)
	b.signers(0, 1, 2)
	b.add(b.specialRaw(0, 0, randomize, append(common.Hex2Bytes(common.HexSetSecret), make([]byte, 64)...)),
		b.specialRaw(1, 0, randomize, append(common.Hex2Bytes(common.HexSetOpening), make([]byte, 32)...)),
		b.specialRaw(2, 0, randomize, []byte{1, 2, 3, 4}))

	b = mk("special-regular-cannot-replace", "a regular transaction does not replace a pending special one at its nonce", h1)
	b.signers(0)
	b.add(b.special(0, 0, h1-5), b.leg(0, 0, 21000, p(f1, 1), to, 0, nil))

	b = mk("special-regular-first-then-special", "a special transaction takes a regular one's nonce without a price bump", h1)
	b.signers(0)
	b.add(b.leg(0, 0, 21000, p(f1, 1), to, 0, nil), b.special(0, 0, h1-5))

	b = mk("special-ignores-full-pool", "a full pool refuses a regular transaction but still takes a signer's special one", h1)
	b.cfgOpt(func(c *PoolCfg) { c.GlobalSlots, c.GlobalQueue = 2, 1 })
	b.signers(7)
	b.add(b.leg(0, 0, 21000, p(f1, 100), to, 0, nil), b.leg(1, 0, 21000, p(f1, 101), to, 0, nil), b.leg(2, 0, 21000, p(f1, 102), to, 0, nil),
		b.leg(3, 0, 21000, p(f1, 1), to, 0, nil), b.special(7, 0, h1-5))

	// ---- replacement
	b = mk("repl-pending-bump", "10 percent bump on a pending transaction: exact threshold passes, one wei less fails", h1)
	old := p(f1, 1000)
	b.add(b.leg(0, 0, 21000, old, to, 0, nil), b.leg(0, 0, 21000, p(pct(old, 10), -1), to, 0, nil), b.leg(0, 0, 21000, pct(old, 10), to, 0, nil))

	b = mk("repl-queued-bump", "the same rule in the future queue", h1)
	b.add(b.leg(0, 5, 21000, old, to, 0, nil), b.leg(0, 5, 21000, p(pct(old, 10), -1), to, 0, nil), b.leg(0, 5, 21000, pct(old, 10), to, 0, nil))

	b = mk("repl-dynamic-both-bumped", "a type 2 replacement needs both the fee cap and the tip bumped", h1)
	cap0, tip0 := p(f1, 1000), big.NewInt(1_000_000)
	b.add(b.t2(0, 0, 21000, cap0, tip0), b.t2(0, 0, 21000, pct(cap0, 10), tip0), b.t2(0, 0, 21000, pct(cap0, 10), pct(tip0, 10)))

	b = mk("repl-funds", "a replacement is checked against the balance with the old cost taken out", h1)
	c0 := new(big.Int).Mul(p(f1, 1000), big.NewInt(21000))
	b.fund(0, 0, new(big.Int).Add(c0, big.NewInt(100)))
	b.add(b.leg(0, 0, 21000, p(f1, 1000), to, 0, nil), b.leg(0, 0, 21000, pct(p(f1, 1000), 10), to, 0, nil))

	b = mk("repl-custom-bump", "price bump 50", h1)
	b.cfgOpt(func(c *PoolCfg) { c.PriceBump = 50 })
	b.add(b.leg(0, 0, 21000, old, to, 0, nil), b.leg(0, 0, 21000, pct(old, 10), to, 0, nil), b.leg(0, 0, 21000, pct(old, 50), to, 0, nil))

	b = mk("repl-known", "the same transaction twice is already known", h1)
	t := b.leg(0, 0, 21000, p(f1, 1), to, 0, nil)
	b.add(t, t)

	// ---- reset: promote, demote
	b = mk("reset-included", "after a block that included nonces 0 and 1 the pool drops them and promotes the queue", h1)
	b.add(b.leg(0, 0, 21000, p(f1, 1), to, 0, nil), b.leg(0, 1, 21000, p(f1, 2), to, 0, nil), b.leg(0, 2, 21000, p(f1, 3), to, 0, nil), b.leg(0, 4, 21000, p(f1, 4), to, 0, nil), b.leg(0, 5, 21000, p(f1, 5), to, 0, nil))
	b.reset(h1+1, gl, map[int]Acct{0: ac(2, ether)}).reset(h1+2, gl, map[int]Acct{0: ac(4, ether)})

	b = mk("reset-demote-balance", "the balance after a reset covers nonces 0, 2 and 3 but not 1: 1 is dropped, 2 and 3 go back to the queue", h1)
	b.add(b.leg(0, 0, 21000, p(f1, 1000), to, 0, nil), b.leg(0, 1, 21000, p(f1, 5000), to, 0, nil), b.leg(0, 2, 21000, p(f1, 10), to, 0, nil), b.leg(0, 3, 21000, p(f1, 11), to, 0, nil))
	b.reset(h1+1, gl, map[int]Acct{0: ac(0, new(big.Int).Mul(p(f1, 1000), big.NewInt(21000)))})
	b.reset(h1+2, gl, map[int]Acct{0: ac(0, ether)})

	b = mk("reset-gas-limit", "a lower gas limit drops the transactions above it", h1)
	b.fund(0, 0, new(big.Int).Mul(ether, big.NewInt(100)))
	b.add(b.leg(0, 0, 21000, p(f1, 1), to, 0, nil), b.leg(0, 1, 3_000_000, p(f1, 2), to, 0, nil), b.leg(0, 2, 21000, p(f1, 3), to, 0, nil))
	b.reset(h1+1, 2_000_000, map[int]Acct{0: ac(0, new(big.Int).Mul(ether, big.NewInt(100)))})

	b = mk("reset-base-fee-tier", "the head moves across the Gas2500x block: the pending base fee changes the urgent order, no tx is dropped", 83_599_990)
	b.add(b.leg(0, 0, 21000, p(floor(cfg, 83_599_990), 1), to, 0, nil), b.leg(1, 0, 21000, p(floor(cfg, 83_599_990), 2), to, 0, nil))
	b.reset(83_599_999, gl, map[int]Acct{0: ac(0, ether), 1: ac(0, ether), 2: ac(0, ether)}).reset(83_600_001, gl, map[int]Acct{0: ac(0, ether), 1: ac(0, ether), 2: ac(0, ether)})
	b.add(b.leg(2, 0, 21000, p(f1, 1), to, 0, nil))

	b = mk("queue-lifetime", "a queued account idle longer than the lifetime is evicted; a younger one stays; pending never expires", h1)
	b.add(b.leg(0, 5, 21000, p(f1, 1), to, 0, nil), b.leg(1, 5, 21000, p(f1, 2), to, 0, nil), b.leg(2, 0, 21000, p(f1, 3), to, 0, nil))
	b.age(0, 4*3600).age(1, 3600).age(2, 100*3600).evict()
	b.age(1, 3*3600+5).evict()

	b = mk("queue-account-cap", "an account's queue is cut to AccountQueue by dropping the highest nonces", h1)
	b.cfgOpt(func(c *PoolCfg) { c.AccountQueue = 3 })
	b.add(b.leg(0, 5, 21000, p(f1, 1), to, 0, nil), b.leg(0, 6, 21000, p(f1, 2), to, 0, nil), b.leg(0, 7, 21000, p(f1, 3), to, 0, nil), b.leg(0, 8, 21000, p(f1, 4), to, 0, nil), b.leg(0, 9, 21000, p(f1, 5), to, 0, nil))

	// ---- bounds, eviction
	small := func(c *PoolCfg) { c.GlobalSlots, c.GlobalQueue, c.AccountSlots, c.AccountQueue = 4, 2, 2, 2 }
	b = mk("bound-underpriced", "pool full (4+2 slots): a cheaper transaction is refused, a dearer one evicts the cheapest", h1)
	b.cfgOpt(small)
	for i := 0; i < 6; i++ {
		b.add(b.leg(i, 0, 21000, p(f1, int64(100+10*i)), to, 0, nil))
	}
	b.add(b.leg(6, 0, 21000, p(f1, 50), to, 0, nil), b.leg(6, 0, 21000, p(f1, 500), to, 0, nil))

	b = mk("bound-future-replaces-pending", "a gapped transaction never evicts a pending one", h1)
	b.cfgOpt(small)
	for i := 0; i < 6; i++ {
		b.add(b.leg(i, 0, 21000, p(f1, int64(100+10*i)), to, 0, nil))
	}
	b.add(b.leg(6, 3, 21000, p(f1, 900), to, 0, nil), b.leg(6, 0, 21000, p(f1, 901), to, 0, nil))

	b = mk("bound-truncate-pending", "global pending cap: accounts over AccountSlots lose their highest nonces, largest first", h1)
	b.cfgOpt(func(c *PoolCfg) { c.GlobalSlots, c.GlobalQueue, c.AccountSlots, c.AccountQueue = 8, 8, 2, 8 })
	n := int64(0)
	for g, cnt := range []int{5, 4, 3} {
		for i := 0; i < cnt; i++ {
			n++
			b.add(b.leg(g, uint64(i), 21000, p(f1, 1000+n), to, 0, nil))
		}
	}

	b = mk("bound-truncate-queue", "global queue cap drops the oldest account's transactions first", h1)
	b.cfgOpt(func(c *PoolCfg) { c.GlobalSlots, c.GlobalQueue, c.AccountSlots, c.AccountQueue = 20, 4, 4, 4 })
	b.add(b.leg(0, 5, 21000, p(f1, 1), to, 0, nil), b.leg(0, 6, 21000, p(f1, 2), to, 0, nil), b.leg(1, 5, 21000, p(f1, 3), to, 0, nil), b.leg(1, 6, 21000, p(f1, 4), to, 0, nil), b.leg(2, 5, 21000, p(f1, 5), to, 0, nil), b.leg(2, 6, 21000, p(f1, 6), to, 0, nil))

	b = mk("bound-big-tx-slots", "a transaction over 32 KiB takes two slots", h1)
	b.cfgOpt(func(c *PoolCfg) { c.GlobalSlots, c.GlobalQueue = 3, 1 })
	b.fund(9, 0, new(big.Int).Mul(ether, big.NewInt(100)))
	b.add(b.leg(0, 0, 21000, p(f1, 1), to, 0, nil), b.leg(1, 0, 21000, p(f1, 2), to, 0, nil), b.leg(9, 0, 3_000_000, p(f1, 50), to, 0, make([]byte, 40000)),
		b.leg(2, 0, 21000, p(f1, 3), to, 0, nil), b.leg(3, 0, 21000, p(f1, 60), to, 0, nil))

	b = mk("bound-throttle", "more evictions in one batch than a quarter of the global slots: overflow", h1)
	b.cfgOpt(func(c *PoolCfg) { c.GlobalSlots, c.GlobalQueue, c.AccountSlots, c.AccountQueue = 8, 0+1, 4, 4 })
	for i := 0; i < 9; i++ {
		b.add(b.leg(i, 0, 21000, p(f1, int64(100+i)), to, 0, nil))
	}
	b.batch(b.leg(10, 0, 21000, p(f1, 300), to, 0, nil), b.leg(11, 0, 21000, p(f1, 301), to, 0, nil), b.leg(12, 0, 21000, p(f1, 302), to, 0, nil), b.leg(13, 0, 21000, p(f1, 303), to, 0, nil))

	b = mk("batch-mixed", "one batch with a duplicate, an invalid and a valid transaction", h1)
	t = b.leg(0, 0, 21000, p(f1, 1), to, 0, nil)
	b.batch(t, t, b.leg(1, 0, 21000, p(f1, -5), to, 0, nil), b.leg(2, 0, 21000, p(f1, 2), to, 0, nil))

	b = mk("gas-tip-raise", "raising the pool minimum tip drops pending transactions below it", h1)
	b.add(b.leg(0, 0, 21000, p(f1, 1), to, 0, nil), b.leg(1, 0, 21000, p(f1, 100), to, 0, nil)).gasTip(f1.Uint64() + 50)

	b = mk("delegated-sender", "an account that has code is limited to one in-flight transaction (legacypool.go:728-756)", h1)
	b.sc.Accounts[hx(addrs[0])] = Acct{Nonce: 0, Balance: ether.String(), Code: "0xef0100" + strings.Repeat("11", 20)}
	b.add(b.leg(0, 3, 21000, p(f1, 1), to, 0, nil), b.leg(0, 0, 21000, p(f1, 2), to, 0, nil), b.leg(0, 1, 21000, p(f1, 3), to, 0, nil), b.leg(0, 0, 21000, pct(p(f1, 2), 10), to, 0, nil))

	return all
}

// ---- seeded scenarios -------------------------------------------------------------------------------------------

func random(seed int64) *B {
	r := rand.New(rand.NewSource(seed))
	cfg := apothem()
	f1 := floor(cfg, h1)
	b := newB(fmt.Sprintf("rnd-%03d", seed), "seeded mix of adds, replacements, special transactions, resets and queue ageing on a small pool", h1).rich(10)
	b.cfgOpt(func(c *PoolCfg) {
		c.GlobalSlots = uint64(3 + r.Intn(6))
		c.GlobalQueue = uint64(1 + r.Intn(5))
		c.AccountSlots = uint64(1 + r.Intn(3))
		c.AccountQueue = uint64(2 + r.Intn(4))
	})
	b.signers(8, 9)
	nonces := make([]uint64, 10)
	last := map[[2]int]*big.Int{}
	offs := r.Perm(4000)
	next := 0
	price := func() *big.Int { next++; return p(f1, int64(offs[next])*1_000_000) }
	steps := 25 + r.Intn(20)
	to := ad(30)
	for s := 0; s < steps; s++ {
		switch x := r.Intn(100); {
		case x < 58:
			i := r.Intn(8)
			nonce := nonces[i] + uint64(r.Intn(3))
			var data []byte
			if r.Intn(30) == 0 {
				data = make([]byte, 32700)
			}
			gas := uint64(21000)
			if data != nil {
				gas = 400_000
			}
			pr := price()
			if old, ok := last[[2]int{i, int(nonce)}]; ok && r.Intn(2) == 0 {
				pr = p(pct(old, 10), int64(offs[next])+1_000+int64(next))
			}
			last[[2]int{i, int(nonce)}] = pr
			b.add(b.leg(i, nonce, gas, pr, to, 1, data))
		case x < 66:
			i := 8 + r.Intn(2)
			b.add(b.special(i, nonces[i]+uint64(r.Intn(2)), h1-uint64(r.Intn(2500))))
		case x < 80:
			m := map[int]Acct{}
			for i := 0; i < 10; i++ {
				if r.Intn(3) == 0 {
					nonces[i] += uint64(1 + r.Intn(3))
				}
				bal := ether
				if r.Intn(8) == 0 {
					bal = new(big.Int).Mul(f1, big.NewInt(int64(21000*(1+r.Intn(3)))))
				}
				m[i] = Acct{Nonce: nonces[i], Balance: bal.String()}
			}
			gl2 := uint64(gl)
			if r.Intn(6) == 0 {
				gl2 = 300_000
			}
			b.reset(h1+uint64(s)+1, gl2, m)
		case x < 90:
			i := r.Intn(10)
			b.age(i, uint64(r.Intn(6))*3600+60).evict() // never exactly on the lifetime: the real clock moves on
		default:
			b.loose = true
			b.add(b.leg(r.Intn(8), nonces[r.Intn(8)], 21000, p(f1, int64(-1-r.Intn(5))), to, 1, nil))
			b.loose = false
		}
	}
	return b
}

// probe is a sign transaction with a non-zero value from a signer, then a block that includes it. The pinned
// promoteSpecialTx leaves the transaction's cost out of the list's total (legacypool.go:827-834), and the removal
// of the included transaction subtracts it (list.go:496-505): `totalcost underflow` is a panic, in the reorg
// goroutine, so it ends the process.
func probe() *B {
	b := newB("probe-special-value", "a sign transaction with value 1 from a signer, then a head that includes it", h1).rich(4)
	b.signers(0)
	data := common.Hex2Bytes(common.HexSignMethod)
	data = append(data, make([]byte, 64)...)
	b.add(b.leg(0, 0, 200000, new(big.Int), &signers, 1, data))
	b.reset(h1+1, gl, map[int]Acct{0: ac(1, ether)})
	return b
}

// ---- main -------------------------------------------------------------------------------------------------------

func main() {
	common.CopyXDCConstants(51)
	if len(os.Args) > 1 && os.Args[1] == "probe" {
		run(probe().sc)
		fmt.Println("survived")
		return
	} // the node does this at start (eth/backend.go); Gas50x and the TIPs are globals
	var out []*Scenario
	scenarios := scripted()
	for seed := int64(1); seed <= 60; seed++ {
		scenarios = append(scenarios, random(seed))
	}
	dropped := 0
	for _, b := range scenarios {
		sc := b.sc
		first := run(sc)
		enc, _ := json.Marshal(first)
		stable := true
		for k := 0; k < 6 && stable; k++ {
			again, _ := json.Marshal(run(sc))
			stable = bytes.Equal(enc, again)
		}
		if !stable {
			dropped++
			fmt.Fprintln(os.Stderr, "nondeterministic, left out:", sc.ID)
			continue
		}
		sc.Expect = first
		out = append(out, sc)
	}
	fmt.Fprintf(os.Stderr, "%d scenarios kept, %d left out\n", len(out), dropped)
	exe, _ := os.Executable()
	pout, perr := exec.Command(exe, "probe").CombinedOutput()
	result := "survived"
	if perr != nil {
		result = "crashed"
		for _, l := range strings.Split(string(pout), "\n") {
			if strings.HasPrefix(l, "panic:") {
				result = "crashed, " + l
				break
			}
		}
	}
	ps := probe().sc
	ps.Expect = nil
	doc := map[string]interface{}{"pin": "6fc8e8c2088b7f18264f5663036669d04fa7becf", "chain_id": 51, "scenarios": out, "pin_crash": map[string]interface{}{"scenario": ps, "pinned_pool": result}}
	e := json.NewEncoder(os.Stdout)
	e.SetEscapeHTML(false)
	if err := e.Encode(doc); err != nil {
		panic(err)
	}
}
