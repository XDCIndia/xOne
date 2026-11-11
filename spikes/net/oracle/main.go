// Command oracle prints geth-XDC wire vectors (RLP of the eth/100 and eth/101 messages and of the
// XDPoS V2 consensus messages) as JSON, from the pinned geth-XDC types. Nothing in vectors.json is
// edited by hand.
package main

import (
	"encoding/hex"
	"encoding/json"
	"math/big"
	"os"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/eth/protocols/eth"
	"github.com/ethereum/go-ethereum/rlp"
)

func enc(v any) string {
	b, err := rlp.EncodeToBytes(v)
	if err != nil {
		panic(err)
	}
	return hex.EncodeToString(b)
}

func main() {
	h := func(b byte) common.Hash { return common.Hash{b, 0xaa, 0xbb} }
	info := &types.BlockInfo{Hash: h(1), Round: 7, Number: big.NewInt(1234)}
	sig := make([]byte, 65)
	for i := range sig {
		sig[i] = byte(i)
	}
	qc := &types.QuorumCert{ProposedBlockInfo: info, Signatures: []types.Signature{sig, sig[:64]}, GapNumber: 450}
	tc := &types.TimeoutCert{Round: 9, Signatures: []types.Signature{sig}, GapNumber: 450}
	out := map[string]string{
		// eth/100 and eth/101 share StatusPacket62 (eth/protocols/eth/handshake.go:130-132).
		"status62": enc(&eth.StatusPacket62{ProtocolVersion: 101, NetworkID: 5551, TD: big.NewInt(1), Head: h(2), Genesis: h(3)}),
		// eth/100: bare request. eth/101: request id first (peer.go:674-690).
		"get_headers_bare": enc(&eth.GetBlockHeadersRequest{Origin: eth.HashOrNumber{Number: 0}, Amount: 1}),
		"get_headers_rid":  enc(&eth.GetBlockHeadersPacket{RequestId: 0x1122334455, GetBlockHeadersRequest: &eth.GetBlockHeadersRequest{Origin: eth.HashOrNumber{Number: 0}, Amount: 1}}),
		"vote":             enc(&types.Vote{ProposedBlockInfo: info, Signature: sig, GapNumber: 450}),
		"timeout":          enc(&types.Timeout{Round: 9, Signature: sig, GapNumber: 450}),
		"sync_info":        enc(&types.SyncInfo{HighestQuorumCert: qc, HighestTimeoutCert: tc}),
	}
	e := json.NewEncoder(os.Stdout)
	e.SetIndent("", "  ")
	if err := e.Encode(out); err != nil {
		panic(err)
	}
}
