// Command oracle prints geth-XDC state-commitment vectors as JSON.
//
//	oracle mutations -> 50 seeded account/storage mutations, with the state root after each
//	oracle accounts  -> account leaf and slim encodings for boundary balances and root/code variants
//
// The roots come from geth's own trie.StateTrie (secure trie) and types.StateAccount codec at the pin.
package main

import (
	"encoding/json"
	"fmt"
	"math/big"
	"math/rand"
	"os"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core/rawdb"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/ethereum/go-ethereum/rlp"
	"github.com/ethereum/go-ethereum/trie"
	"github.com/ethereum/go-ethereum/triedb"
	"github.com/holiman/uint256"
)

func must(err error) {
	if err != nil {
		panic(err)
	}
}

func hx(v *big.Int) string { return "0x" + v.Text(16) }

func pow(n uint) *big.Int { return new(big.Int).Lsh(big.NewInt(1), n) }

// account converts a true balance into geth's StateAccount: Balance wrapped, BigBalance above 2^256-1.
func account(nonce uint64, bal *big.Int, root, codeHash common.Hash) *types.StateAccount {
	low, overflow := uint256.FromBig(bal)
	a := &types.StateAccount{Nonce: nonce, Balance: low, Root: root, CodeHash: codeHash[:]}
	if overflow {
		a.BigBalance = new(big.Int).Set(bal)
	}
	return a
}

// ---- accounts mode ----

type AccVec struct {
	Name     string `json:"name"`
	Nonce    uint64 `json:"nonce"`
	Balance  string `json:"balance"`
	Root     string `json:"root"`
	CodeHash string `json:"code_hash"`
	Full     string `json:"full_rlp"`
	Slim     string `json:"slim_rlp"`
	// FullAccountRLP(slim): geth's own slim -> full conversion
	SlimToFull string `json:"slim_to_full_rlp"`
}

func accountVectors() []AccVec {
	one := big.NewInt(1)
	vals := []struct {
		n string
		v *big.Int
	}{
		{"0", big.NewInt(0)},
		{"1", big.NewInt(1)},
		{"2^256-1", new(big.Int).Sub(pow(256), one)},
		{"2^256", pow(256)},
		{"2^256+1", new(big.Int).Add(pow(256), one)},
		{"2^264-1", new(big.Int).Sub(pow(264), one)},
	}
	roots := map[string]common.Hash{
		"empty-root":   types.EmptyRootHash,
		"storage-root": crypto.Keccak256Hash([]byte("storage")),
	}
	codes := map[string]common.Hash{
		"empty-code": types.EmptyCodeHash,
		"code":       crypto.Keccak256Hash([]byte("code")),
	}
	var out []AccVec
	for _, x := range vals {
		for _, rn := range []string{"empty-root", "storage-root"} {
			for _, cn := range []string{"empty-code", "code"} {
				a := account(7, x.v, roots[rn], codes[cn])
				full, err := rlp.EncodeToBytes(a)
				must(err)
				slim := types.SlimAccountRLP(*a)
				s2f, err := types.FullAccountRLP(slim)
				must(err)
				out = append(out, AccVec{
					Name: x.n + "/" + rn + "/" + cn, Nonce: 7, Balance: hx(x.v),
					Root: roots[rn].Hex(), CodeHash: codes[cn].Hex(),
					Full: fmt.Sprintf("0x%x", full), Slim: fmt.Sprintf("0x%x", slim),
					SlimToFull: fmt.Sprintf("0x%x", s2f),
				})
			}
		}
	}
	return out
}

// ---- mutations mode ----

type Step struct {
	N        int    `json:"n"`
	Op       string `json:"op"` // put_account | put_slot | del_account
	Addr     string `json:"addr"`
	Nonce    uint64 `json:"nonce,omitempty"`
	Balance  string `json:"balance,omitempty"`
	CodeHash string `json:"code_hash,omitempty"`
	Slot     string `json:"slot,omitempty"`
	Value    string `json:"value,omitempty"`        // 32-byte word; all zero deletes the slot
	AcctRoot string `json:"storage_root,omitempty"` // storage root of the account after the step
	Root     string `json:"root"`                   // state root after the step
}

type acct struct {
	nonce uint64
	bal   *big.Int
	code  common.Hash
	st    *trie.StateTrie
}

type model struct {
	tdb   *triedb.Database
	state *trie.StateTrie
	accts map[common.Address]*acct
}

func newTrie(tdb *triedb.Database) *trie.StateTrie {
	t, err := trie.NewStateTrie(trie.StateTrieID(types.EmptyRootHash), tdb)
	must(err)
	return t
}

func (m *model) write(addr common.Address) {
	a := m.accts[addr]
	must(m.state.UpdateAccount(addr, account(a.nonce, a.bal, a.st.Hash(), a.code), 0))
}

func mutations() []Step {
	rng := rand.New(rand.NewSource(0x2040))
	tdb := triedb.NewDatabase(rawdb.NewMemoryDatabase(), nil)
	m := &model{tdb: tdb, state: newTrie(tdb), accts: map[common.Address]*acct{}}
	pool := make([]common.Address, 8)
	for i := range pool {
		pool[i] = common.BytesToAddress(crypto.Keccak256([]byte{byte(i), 0xa1}))
	}
	slotOf := func(i int) common.Hash { return common.BigToHash(big.NewInt(int64(i))) }
	one := big.NewInt(1)
	special := []*big.Int{new(big.Int).Sub(pow(256), one), pow(256), new(big.Int).Add(pow(256), one)}
	randBal := func() *big.Int {
		switch rng.Intn(6) {
		case 0:
			return big.NewInt(0)
		case 1:
			return new(big.Int).SetUint64(rng.Uint64())
		case 2:
			return special[rng.Intn(3)]
		case 3: // up to 2^264-1
			return new(big.Int).Rand(rng, pow(264))
		default:
			return new(big.Int).Rand(rng, pow(256))
		}
	}
	codes := []common.Hash{types.EmptyCodeHash, crypto.Keccak256Hash([]byte{1}), crypto.Keccak256Hash([]byte{2})}
	var steps []Step
	for n := 0; n < 50; n++ {
		addr := pool[rng.Intn(len(pool))]
		s := Step{N: n, Addr: addr.Hex()}
		a := m.accts[addr]
		switch {
		case n < 3: // the three boundary balances first, on three accounts
			addr = pool[n]
			s.Addr = addr.Hex()
			fallthrough
		case a == nil || rng.Intn(4) == 0:
			bal := randBal()
			if n < 3 {
				bal = special[n]
			}
			s.Op, s.Nonce, s.Balance = "put_account", uint64(rng.Intn(5)), hx(bal)
			c := codes[rng.Intn(len(codes))]
			s.CodeHash = c.Hex()
			if m.accts[addr] == nil {
				m.accts[addr] = &acct{st: newTrie(tdb)}
			}
			x := m.accts[addr]
			x.nonce, x.bal, x.code = s.Nonce, bal, c
			m.write(addr)
		case rng.Intn(8) == 0:
			s.Op = "del_account"
			delete(m.accts, addr)
			must(m.state.DeleteAccount(addr))
		default:
			slot := slotOf(rng.Intn(6))
			var val common.Hash
			if rng.Intn(4) != 0 {
				val = common.BigToHash(new(big.Int).Rand(rng, pow(256)))
			}
			s.Op, s.Slot, s.Value = "put_slot", slot.Hex(), val.Hex()
			if val == (common.Hash{}) {
				must(a.st.DeleteStorage(addr, slot[:]))
			} else {
				must(a.st.UpdateStorage(addr, slot[:], common.TrimLeftZeroes(val[:])))
			}
			m.write(addr)
		}
		if x := m.accts[addr]; x != nil {
			s.AcctRoot = x.st.Hash().Hex()
		}
		s.Root = m.state.Hash().Hex()
		steps = append(steps, s)
	}
	return steps
}

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: oracle mutations|accounts")
		os.Exit(2)
	}
	var v any
	switch os.Args[1] {
	case "mutations":
		v = mutations()
	case "accounts":
		v = accountVectors()
	default:
		fmt.Fprintln(os.Stderr, "unknown mode")
		os.Exit(2)
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", " ")
	must(enc.Encode(v))
}
