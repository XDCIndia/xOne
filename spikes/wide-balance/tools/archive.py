"""Read-only access to the xdc02 archives (JSON-RPC over ssh, at most 5 requests per second)
and verified account leaves: eth_getProof nodes are hashed back to the block's stateRoot, so
the true (unwrapped) balance is read from the canonical trie, not from the RPC balance field.

Used for the "Legal range" and "Legacy adjudication" sections of REPORT.md.
"""
import json
import subprocess
import time

HOST = "xdc02"
APOTHEM, NET2040 = 21105, 10071


def rpc(port, method, params):
    time.sleep(0.25)
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    out = subprocess.run(
        ["ssh", HOST, "curl", "-s", "-H", "'content-type: application/json'", "-d", "'%s'" % body, "http://127.0.0.1:%d" % port],
        capture_output=True, text=True, check=True).stdout
    return json.loads(out)


def rlp_dec(b, i=0):
    c = b[i]
    if c < 0x80:
        return b[i:i + 1], i + 1
    if c < 0xb8:
        n = c - 0x80
        return b[i + 1:i + 1 + n], i + 1 + n
    if c < 0xc0:
        ll = c - 0xb7
        n = int.from_bytes(b[i + 1:i + 1 + ll], "big")
        s = i + 1 + ll
        return b[s:s + n], s + n
    if c < 0xf8:
        n, s = c - 0xc0, i + 1
    else:
        ll = c - 0xf7
        n = int.from_bytes(b[i + 1:i + 1 + ll], "big")
        s = i + 1 + ll
    out, e = [], s + n
    while s < e:
        x, s = rlp_dec(b, s)
        out.append(x)
    return out, e


RC=[0x0000000000000001,0x0000000000008082,0x800000000000808A,0x8000000080008000,0x000000000000808B,0x0000000080000001,0x8000000080008081,0x8000000000008009,0x000000000000008A,0x0000000000000088,0x0000000080008009,0x000000008000000A,0x000000008000808B,0x800000000000008B,0x8000000000008089,0x8000000000008003,0x8000000000008002,0x8000000000000080,0x000000000000800A,0x800000008000000A,0x8000000080008081,0x8000000000008080,0x0000000080000001,0x8000000080008008]
ROT=[[0,36,3,41,18],[1,44,10,45,2],[62,6,43,15,61],[28,55,25,21,56],[27,20,39,8,14]]
M=(1<<64)-1
def rol(x,n): return ((x<<n)|(x>>(64-n)))&M if n else x
def f(A):
    for rc in RC:
        C=[A[x][0]^A[x][1]^A[x][2]^A[x][3]^A[x][4] for x in range(5)]
        D=[C[(x-1)%5]^rol(C[(x+1)%5],1) for x in range(5)]
        A=[[A[x][y]^D[x] for y in range(5)] for x in range(5)]
        B=[[0]*5 for _ in range(5)]
        for x in range(5):
            for y in range(5): B[y][(2*x+3*y)%5]=rol(A[x][y],ROT[x][y])
        A=[[B[x][y]^((~B[(x+1)%5][y])&B[(x+2)%5][y]) for y in range(5)] for x in range(5)]
        A[0][0]^=rc
    return A
def keccak256(data: bytes) -> bytes:
    rate=136; p=bytearray(data); p.append(0x01)
    while len(p)%rate: p.append(0)
    p[-1]|=0x80
    A=[[0]*5 for _ in range(5)]
    for off in range(0,len(p),rate):
        blk=p[off:off+rate]
        for i in range(rate//8):
            A[i%5][i//5]^=int.from_bytes(blk[8*i:8*i+8],'little')
        A=f(A)
    out=b''.join(A[i%5][i//5].to_bytes(8,'little') for i in range(4))
    return out

def nib(b):
    return [x for c in b for x in (c >> 4, c & 15)]


def verify(root, addr, proofs):
    """Walk the proof from the state root (hex) to the account leaf of addr; returns the leaf value."""
    key = nib(keccak256(bytes.fromhex(addr[2:])))
    exp, idx = bytes.fromhex(root[2:]), 0
    for h in proofs:
        node = bytes.fromhex(h[2:])
        assert keccak256(node) == exp, "hash chain broken"
        items, _ = rlp_dec(node)
        if len(items) == 17:
            exp = items[key[idx]]
            idx += 1
        else:
            n = nib(items[0])
            flag = n[0]
            n = n[2:] if flag & 1 == 0 else n[1:]
            if flag >= 2:
                assert key[idx:] == n, "leaf path"
                return items[1]
            idx += len(n)
            exp = items[1]
    raise AssertionError("no leaf")


def header(port, b):
    return rpc(port, "eth_getBlockByNumber", [hex(b), False])["result"]


def leaf_at(port, addr, blk, check=True):
    """Account at block blk from its proof: nonce, true balance, wrapped balance.

    check=True hashes the proof back to the block's stateRoot (the canonical leaf). check=False only
    decodes the last node and compares it with the RPC fields; it is for scans, not for evidence.
    """
    r = rpc(port, "eth_getProof", [addr, [], hex(blk)])["result"]
    if check:
        val = verify(header(port, blk)["stateRoot"], addr, r["accountProof"])
    else:
        node, _ = rlp_dec(bytes.fromhex(r["accountProof"][-1][2:]))
        assert len(node) == 2, "absent account"
        val = node[1]
    acct, _ = rlp_dec(val)
    assert len(acct) == 4, "absent account"
    nonce, bal, sroot, chash = acct
    out = dict(nonce=int.from_bytes(nonce, "big"), true=int.from_bytes(bal, "big"), low=int(r["balance"], 16),
               rlp=val.hex(), storageHash=r["storageHash"], codeHash=r["codeHash"])
    assert out["true"] % 2**256 == out["low"], "leaf does not match the RPC balance"
    return out
