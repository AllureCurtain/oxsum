#!/usr/bin/env python3
"""Sign one x402 V1 `exact` payment authorization (EIP-3009
`TransferWithAuthorization`) against the configured chain's USDC contract and
print the wire `payment` string — base64url'd JSON — ready for
`POST /api/v1/topups/x402`.

Used for live rail exercises (docs/development.md "Real x402 rail exercise")
when a human wants to push a real chain transaction through the deposit path
rather than trusting the test-scheme stand-in. Needs `coincurve` (secp256k1)
and `pycryptodome` (keccak); both pip-installable, nothing to compile.

    PAYER_KEY=0x…        # secp256k1 private key, hex, owns the USDC
    PAY_TO=0x…           # the deployment's receiving address (OXSUM_X402_PAY_TO)
    AMOUNT_MINOR=…       # atomic units — USDC on-chain and oxsum credit share
                         # the 1e6-per-unit scale, so this is also the credit
                         # amount (`5000000` = 5 USDC = 5 credits)
    ASSET=0x…            # optional; defaults to Base Sepolia USDC
    CHAIN_ID=84532       # optional; default is Base Sepolia
    TOKEN_NAME=USDC      # optional; the EIP-712 domain `name` the contract
                         # signed under — see gotcha below
    TOKEN_VERSION=2      # optional; the EIP-712 domain `version`
    NETWORK=base-sepolia # optional; the V1 wire name
    python3 scripts/x402_sign.py

Two details that bit during bring-up and are why the env vars above exist
instead of being hardcoded:

- The USDC Base Sepolia contract's EIP-712 `name` is `"USDC"`, **not**
  `"USD Coin"` — Circle renamed between FiatToken v1 and v2 and the domain
  separator commits to the new string. Verify against
  `eth_call(DOMAIN_SEPARATOR())` if the facilitator rejects an authorization
  with `FiatTokenV2: invalid signature`.
- Ethereum's `ecrecover` wants `v ∈ {27, 28}`. `coincurve` returns the raw
  recovery id `{0, 1}`; the script shifts it. A wrong `v` surfaces as
  `ECRecover: invalid signature 'v' value` from the facilitator.
"""

import base64
import json
import os
import secrets
import sys
import time

try:
    import coincurve
    from Crypto.Hash import keccak
except ImportError:  # pragma: no cover - exercised by running the script
    sys.exit(
        "needs `coincurve` and `pycryptodome`: pip install coincurve pycryptodome"
    )


def keccak256(data: bytes) -> bytes:
    h = keccak.new(digest_bits=256)
    h.update(data)
    return h.digest()


def pad32(b: bytes) -> bytes:
    return b.rjust(32, b"\x00")


def encode_address(addr: str) -> bytes:
    a = addr[2:] if addr.startswith("0x") else addr
    return pad32(bytes.fromhex(a.lower()))


def encode_uint256(n: int) -> bytes:
    return n.to_bytes(32, "big")


def encode_bytes32(b: bytes) -> bytes:
    assert len(b) == 32
    return b


def encode_string(s: str) -> bytes:
    return keccak256(s.encode())


# keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)")
DOMAIN_TYPEHASH = bytes.fromhex(
    "8b73c3c69bb8fe3d512ecc4cf759cc79239f7b179b0ffacaa9a75d522b39400f"
)


def domain_separator(
    token_name: str, token_version: str, chain_id: int, asset: str
) -> bytes:
    return keccak256(
        DOMAIN_TYPEHASH
        + encode_string(token_name)
        + encode_string(token_version)
        + encode_uint256(chain_id)
        + encode_address(asset)
    )


# keccak256("TransferWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)")
TWA_TYPEHASH = bytes.fromhex(
    "7c7c6cdb67a18743f49ec6fa9b35f50d52ed05cbed4cc592e13b44501c1a2267"
)


def build_digest(
    domain: bytes,
    from_addr: str,
    to_addr: str,
    value: int,
    valid_after: int,
    valid_before: int,
    nonce: bytes,
) -> bytes:
    struct_hash = keccak256(
        TWA_TYPEHASH
        + encode_address(from_addr)
        + encode_address(to_addr)
        + encode_uint256(value)
        + encode_uint256(valid_after)
        + encode_uint256(valid_before)
        + encode_bytes32(nonce)
    )
    return keccak256(b"\x19\x01" + domain + struct_hash)


def pubkey_to_address(pubkey_uncompressed: bytes) -> str:
    """64-byte uncompressed pubkey (no 0x04 prefix) → EIP-55 checksummed 0x…."""
    h = keccak256(pubkey_uncompressed)[-20:]
    hexaddr = h.hex()
    digest = keccak256(hexaddr.encode()).hex()
    out = [
        c.upper() if c.isalpha() and int(digest[i], 16) >= 8 else c
        for i, c in enumerate(hexaddr)
    ]
    return "0x" + "".join(out)


def main() -> None:
    payer_key = os.environ["PAYER_KEY"]
    if payer_key.startswith("0x"):
        payer_key = payer_key[2:]
    sk = coincurve.PrivateKey(bytes.fromhex(payer_key))
    from_addr = pubkey_to_address(sk.public_key.format(compressed=False)[1:])

    pay_to = os.environ["PAY_TO"]
    amount_minor = int(os.environ["AMOUNT_MINOR"])
    asset = os.environ.get("ASSET", "0x036CbD53842c5426634e7929541eC2318f3dCF7e")
    chain_id = int(os.environ.get("CHAIN_ID", "84532"))
    token_name = os.environ.get("TOKEN_NAME", "USDC")
    token_version = os.environ.get("TOKEN_VERSION", "2")
    network = os.environ.get("NETWORK", "base-sepolia")

    now = int(time.time())
    valid_after = 0
    valid_before = now + 600  # ten minutes is plenty for the verify+settle pair
    nonce = secrets.token_bytes(32)

    domain = domain_separator(token_name, token_version, chain_id, asset)
    digest = build_digest(
        domain, from_addr, pay_to, amount_minor, valid_after, valid_before, nonce
    )
    sig = sk.sign_recoverable(digest, hasher=None)
    # coincurve returns r‖s‖recovery_id where v ∈ {0,1}; Ethereum's ecrecover
    # wants v ∈ {27,28}.
    sig_fixed = sig[:64] + bytes([sig[64] + 27])

    payload = {
        "x402Version": 1,
        "scheme": "exact",
        "network": network,
        "payload": {
            "signature": "0x" + sig_fixed.hex(),
            "authorization": {
                "from": from_addr,
                "to": pay_to,
                "value": str(amount_minor),
                "validAfter": str(valid_after),
                "validBefore": str(valid_before),
                "nonce": "0x" + nonce.hex(),
            },
        },
    }
    wire = base64.urlsafe_b64encode(
        json.dumps(payload, separators=(",", ":")).encode()
    ).rstrip(b"=").decode()
    print(wire)


if __name__ == "__main__":
    main()
