from eth_keys import keys
from Crypto.Hash import keccak

def _int(x):
    return x.to_bytes((x.bit_length() + 7) // 8, "big") if x else b""

def _len(n, off):
    if n < 56:
        return bytes([off + n])
    b = _int(n)
    return bytes([off + 55 + len(b)]) + b

def _item(b):
    return b if (len(b) == 1 and b[0] < 0x80) else _len(len(b), 0x80) + b

def _list(items):
    p = b"".join(_item(i) for i in items)
    return _len(len(p), 0xC0) + p

def seal_signer(b):
    """V1 sealer: ecrecover over the seal hash (xone-primitives Header::seal_hash, pre-London header)."""
    h = lambda k: bytes.fromhex(b[k][2:])
    extra = h("extraData")
    f = [h("parentHash"), h("sha3Uncles"), h("miner"), h("stateRoot"), h("transactionsRoot"), h("receiptsRoot"), h("logsBloom"),
         _int(int(b["difficulty"], 16)), _int(int(b["number"], 16)), _int(int(b["gasLimit"], 16)), _int(int(b["gasUsed"], 16)),
         _int(int(b["timestamp"], 16)), extra[:-65], h("mixHash"), h("nonce")]
    digest = keccak.new(digest_bits=256, data=_list(f)).digest()
    sig = keys.Signature(signature_bytes=extra[-65:])
    return "0x" + sig.recover_public_key_from_msg_hash(digest).to_address()[2:].lower()
