package main

import (
	"fmt"
	"math/big"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core/rawdb"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/ethereum/go-ethereum/rlp"
	"github.com/ethereum/go-ethereum/trie"
	"github.com/ethereum/go-ethereum/triedb"
	"github.com/holiman/uint256"
)

// EncVec is one boundary value with every encoding geth-XDC produces for it.
type EncVec struct {
	Name      string `json:"name"`
	True      string `json:"true"`
	Low       string `json:"low"`
	IsWide    bool   `json:"is_wide"`
	BalRLP    string `json:"balance_rlp"`  // rlp.EncodeToBytes(*big.Int)
	AccountRL string `json:"account_rlp"`  // StateAccount.EncodeRLP (trie leaf value)
	SlimRLP   string `json:"slim_rlp"`     // types.SlimAccountRLP (snap AccountRange body item)
	TrieRoot  string `json:"trie_root"`    // root of a one-account state trie
	Decoded   string `json:"decoded_true"` // FullAccount(slim) -> BigBalance or Balance
	DecodedLo string `json:"decoded_low"`
}

const (
	encNonce = 7
	encAddr  = "0x4398241671b3dd484fe3213a4fb7511f30e7d7c0"
)

// encodingVectors derives the boundary values. observedMax is the largest true
// balance seen on Apothem (see REPORT.md).
func encodingVectors(observedMax *big.Int) []EncVec {
	pow := func(n uint) *big.Int { return new(big.Int).Lsh(big.NewInt(1), n) }
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
		{"2^264", pow(264)},
		{"apothem_max_observed", observedMax},
	}
	root := common.HexToHash("0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421")
	codeHash := common.HexToHash("0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470")
	var out []EncVec
	for _, x := range vals {
		low, overflow := uint256.FromBig(x.v)
		acct := types.StateAccount{Nonce: encNonce, Balance: low, Root: root, CodeHash: codeHash[:]}
		if overflow {
			acct.BigBalance = new(big.Int).Set(x.v)
		}
		balRLP, err := rlp.EncodeToBytes(x.v)
		must(err)
		accRLP, err := rlp.EncodeToBytes(&acct)
		must(err)
		slim := types.SlimAccountRLP(acct)
		// Decode both forms back through geth's own decoders.
		var dec types.StateAccount
		must(rlp.DecodeBytes(accRLP, &dec))
		full, err := types.FullAccount(slim)
		must(err)
		if dec.Balance.Cmp(low) != 0 || full.Balance.Cmp(low) != 0 {
			panic("geth decode low mismatch for " + x.n)
		}
		decTrue := new(big.Int).Set(dec.Balance.ToBig())
		if dec.BigBalance != nil {
			decTrue = dec.BigBalance
		}
		if decTrue.Cmp(x.v) != 0 {
			panic("geth decode true mismatch for " + x.n)
		}
		tdb := triedb.NewDatabase(rawdb.NewMemoryDatabase(), nil)
		tr, err := trie.NewStateTrie(trie.StateTrieID(types.EmptyRootHash), tdb)
		must(err)
		must(tr.UpdateAccount(common.HexToAddress(encAddr), &acct, 0))
		out = append(out, EncVec{
			Name: x.n, True: hx(x.v), Low: hx(low.ToBig()), IsWide: overflow,
			BalRLP: fmt.Sprintf("0x%x", balRLP), AccountRL: fmt.Sprintf("0x%x", accRLP),
			SlimRLP: fmt.Sprintf("0x%x", slim), TrieRoot: tr.Hash().Hex(),
			Decoded: hx(decTrue), DecodedLo: hx(full.Balance.ToBig()),
		})
	}
	_ = crypto.Keccak256
	return out
}

func hx(v *big.Int) string { return "0x" + v.Text(16) }

func must(err error) {
	if err != nil {
		panic(err)
	}
}
