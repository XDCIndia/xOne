// Command oracle prints geth-XDC wire vectors as JSON, from the pinned geth-XDC types: the
// eth/100 and eth/101 messages, the XDPoS V2 consensus messages, the base p2p Hello and
// Disconnect, and the discv4 packets with the XDC ping (packet type 5). The signing key of
// the discv4 packets is fixed, so the output is deterministic.
package main

import (
	"encoding/hex"
	"encoding/json"
	"math/big"
	"net"
	"os"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"
	"github.com/ethereum/go-ethereum/eth/protocols/eth"
	"github.com/ethereum/go-ethereum/p2p"
	"github.com/ethereum/go-ethereum/p2p/discover/v4wire"
	"github.com/ethereum/go-ethereum/rlp"
)

func enc(v any) string {
	b, err := rlp.EncodeToBytes(v)
	if err != nil {
		panic(err)
	}
	return hex.EncodeToString(b)
}

func must[T any](v T, err error) T {
	if err != nil {
		panic(err)
	}
	return v
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

	// An XDC header: validators, validator and penalties are part of the RLP; no optional tail.
	header := &types.Header{
		ParentHash: h(10), UncleHash: types.EmptyUncleHash, Coinbase: common.Address{0x11, 0x22},
		Root: h(11), TxHash: h(12), ReceiptHash: h(13), Bloom: types.Bloom{1, 2, 3},
		Difficulty: big.NewInt(1), Number: big.NewInt(1234), GasLimit: 420000000, GasUsed: 21000,
		Time: 1700000000, Extra: []byte{2, 0xc0}, MixDigest: common.Hash{},
		Nonce: types.BlockNonce{}, Validators: []byte{0xaa, 0xbb}, Validator: sig, Penalties: []byte{},
	}
	to := common.Address{0x33}
	legacy := types.NewTx(&types.LegacyTx{Nonce: 1, GasPrice: big.NewInt(2), Gas: 21000, To: &to, Value: big.NewInt(3), Data: []byte{1, 2}, V: big.NewInt(27), R: big.NewInt(4), S: big.NewInt(5)})
	typed := types.NewTx(&types.AccessListTx{ChainID: big.NewInt(5551), Nonce: 2, GasPrice: big.NewInt(2), Gas: 21000, To: &to, Value: big.NewInt(3), V: big.NewInt(1), R: big.NewInt(4), S: big.NewInt(5)})
	txs := []*types.Transaction{legacy, typed}
	block := types.NewBlockWithHeader(header).WithBody(types.Body{Transactions: txs, Uncles: nil})

	hashes := []common.Hash{h(1), h(2)}
	rlist := types.Receipts{
		&types.Receipt{Type: 0, Status: 1, CumulativeGasUsed: 21000, Bloom: types.Bloom{9},
			Logs: []*types.Log{{Address: to, Topics: []common.Hash{h(3)}, Data: []byte{7, 8}}}},
		&types.Receipt{Type: 1, Status: 0, CumulativeGasUsed: 42000, Logs: []*types.Log{}},
	}
	// eth/100: bare, consensus receipts (handleReceiptsXDPOS2). eth/101: eth/70 shape with the
	// network ReceiptList (handlers.go:812-843).
	var net70 rlp.RawList[*eth.ReceiptList]
	if err := net70.AppendRaw(must(rlp.EncodeToBytes(eth.NewReceiptList(rlist)))); err != nil {
		panic(err)
	}
	body := []rlp.RawValue{must(rlp.EncodeToBytes(&types.Body{Transactions: txs, Uncles: []*types.Header{header}}))}
	hdrRaw := []rlp.RawValue{must(rlp.EncodeToBytes(header)), must(rlp.EncodeToBytes(header))}

	// discv4 with the XDC ping. Fixed key, so the signature is deterministic.
	key := must(crypto.ToECDSA(common.FromHex("b71c71a67e1177ad4e901695e1b4b9ee17ae16c6668d313eac2f96dbcda3f291")))
	from := v4wire.Endpoint{IP: net.IPv4(10, 0, 0, 1).To4(), UDP: 30303, TCP: 30303}
	toEp := v4wire.Endpoint{IP: net.IPv4(10, 0, 0, 2).To4(), UDP: 30304, TCP: 30305}
	ping := &v4wire.Ping{Version: 4, From: from, To: toEp, Expiration: 1700000020}
	pingPkt, pingHash, err := v4wire.Encode(key, ping)
	if err != nil {
		panic(err)
	}
	pong := &v4wire.Pong{To: from, ReplyTok: pingHash, Expiration: 1700000020}
	findnode := &v4wire.Findnode{Target: v4wire.EncodePubkey(&key.PublicKey), Expiration: 1700000020}
	nb := &v4wire.Neighbors{Expiration: 1700000020, Nodes: []v4wire.Node{
		{IP: net.IPv4(10, 0, 0, 3).To4(), UDP: 30303, TCP: 30303, ID: v4wire.EncodePubkey(&key.PublicKey)},
		{IP: net.ParseIP("2001:db8::1"), UDP: 30304, TCP: 30305, ID: v4wire.EncodePubkey(&key.PublicKey)},
	}}
	pk := func(p v4wire.Packet) string {
		b, _, err := v4wire.Encode(key, p)
		if err != nil {
			panic(err)
		}
		return hex.EncodeToString(b)
	}

	hello := must(rlp.EncodeToBytes(&struct {
		Version    uint64
		Name       string
		Caps       []p2p.Cap
		ListenPort uint64
		ID         []byte
	}{5, "XDC/v1.17.10-xdc.21/linux-amd64/go1.25.2", []p2p.Cap{{Name: "eth", Version: 101}, {Name: "eth", Version: 100}}, 0, make([]byte, 64)}))

	out := map[string]string{
		// eth/100 and eth/101 share StatusPacket62 (handshake.go:115-132).
		"status62":             enc(&eth.StatusPacket62{ProtocolVersion: 101, NetworkID: 5551, TD: big.NewInt(1), Head: h(2), Genesis: h(3)}),
		"status62_td_wide":     enc(&eth.StatusPacket62{ProtocolVersion: 100, NetworkID: 50, TD: new(big.Int).Lsh(big.NewInt(1), 70), Head: h(2), Genesis: h(3)}),
		"new_block_hashes":     enc(&eth.NewBlockHashesPacket{{Hash: h(1), Number: 5}, {Hash: h(2), Number: 6}}),
		"transactions":         enc(txs),
		"get_headers_bare":     enc(&eth.GetBlockHeadersRequest{Origin: eth.HashOrNumber{Number: 0}, Amount: 1}),
		"get_headers_rid":      enc(&eth.GetBlockHeadersPacket{RequestId: 0x1122334455, GetBlockHeadersRequest: &eth.GetBlockHeadersRequest{Origin: eth.HashOrNumber{Number: 0}, Amount: 1}}),
		"get_headers_hash_rid": enc(&eth.GetBlockHeadersPacket{RequestId: 1, GetBlockHeadersRequest: &eth.GetBlockHeadersRequest{Origin: eth.HashOrNumber{Hash: h(4)}, Amount: 192, Skip: 3, Reverse: true}}),
		"block_headers_bare":   enc(eth.BlockHeadersRLPResponse(hdrRaw)),
		"block_headers_rid":    enc(&eth.BlockHeadersRLPPacket{RequestId: 7, BlockHeadersRLPResponse: hdrRaw}),
		"get_bodies_bare":      enc(eth.GetBlockBodiesRequest(hashes)),
		"get_bodies_rid":       enc(&eth.GetBlockBodiesPacket{RequestId: 8, GetBlockBodiesRequest: hashes}),
		"block_bodies_bare":    enc(eth.BlockBodiesRLPResponse(body)),
		"block_bodies_rid":     enc(&eth.BlockBodiesRLPPacket{RequestId: 9, BlockBodiesRLPResponse: body}),
		"new_block":            enc(&eth.NewBlockPacket{Block: block, TD: big.NewInt(1235)}),
		"get_receipts_bare":    enc(eth.GetReceiptsRequest(hashes)),
		"get_receipts_rid":     enc(&eth.GetReceiptsPacket70{RequestId: 10, FirstBlockReceiptIndex: 2, GetReceiptsRequest: hashes}),
		"receipts_bare":        enc([]rlp.RawValue{must(rlp.EncodeToBytes(rlist))}),
		"receipts_rid":         enc(&eth.ReceiptsPacket70{RequestId: 11, LastBlockIncomplete: true, List: net70}),
		"vote":                 enc(&types.Vote{ProposedBlockInfo: info, Signature: sig, GapNumber: 450}),
		"timeout":              enc(&types.Timeout{Round: 9, Signature: sig, GapNumber: 450}),
		"sync_info":            enc(&types.SyncInfo{HighestQuorumCert: qc, HighestTimeoutCert: tc}),
		// base p2p (p2p/peer.go:60-67, peer_error.go).
		"hello":                hex.EncodeToString(hello),
		"disconnect_quit":      enc([]p2p.DiscReason{p2p.DiscQuitting}),
		"disconnect_requested": enc([]p2p.DiscReason{p2p.DiscRequested}),
		// discv4 (v4wire.go:44-53: ping is type 5). Whole datagrams: hash, signature, type, body.
		"discv4_ping_xdc":  hex.EncodeToString(pingPkt),
		"discv4_pong":      pk(pong),
		"discv4_findnode":  pk(findnode),
		"discv4_neighbors": pk(nb),
		"discv4_key":       "b71c71a67e1177ad4e901695e1b4b9ee17ae16c6668d313eac2f96dbcda3f291",
		"discv4_pubkey":    hex.EncodeToString(crypto.FromECDSAPub(&key.PublicKey)[1:]),
	}
	e := json.NewEncoder(os.Stdout)
	e.SetIndent("", "  ")
	if err := e.Encode(out); err != nil {
		panic(err)
	}
}
