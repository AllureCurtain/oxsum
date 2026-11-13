import test from "node:test";
import assert from "node:assert/strict";
import { base58Decode, isSolanaAddress } from "../src/lib/solana.ts";

// The exception-queue surface (issue #1142): the refund-address form refuses
// a malformed paste client-side before the server re-validates the same
// shape — base58, exactly 32 bytes for a public key.

test("a real Solana public key decodes to 32 bytes", () => {
  // The system program id — all zeros — is the all-'1's edge case.
  assert.equal(isSolanaAddress("11111111111111111111111111111111"), true);
  // A well-known base58 keypair address.
  assert.equal(isSolanaAddress("4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM"), true);
});

test("shorter or longer decodes are not public keys", () => {
  // 64 decoded bytes is a signature, not an address — a base58 string the
  // length of a payout signature must not pass the address check.
  const signature =
    "299xJhoRJCHgYo6xxU78Joi8sY7uXrsp7kzXMxbVFZVLKHqVAfV1R3dA864pwEhe6z1wYWy1BXDzyvzNZDKHfdMd";
  assert.equal(base58Decode(signature)?.length, 64);
  assert.equal(isSolanaAddress(signature), false);
  // A 32-byte body is the shortest valid key; shorter is never one.
  assert.equal(isSolanaAddress("E6QXmP4nMFtdeSfXGYHqtSewcjw2iNiKwTYCPetBkmfB"), true);
  assert.equal(isSolanaAddress("1"), false);
  assert.equal(isSolanaAddress(""), false);
});

test("characters outside the base58 alphabet are refused", () => {
  // '0', 'O', 'I' and 'l' are not in the alphabet.
  for (const bad of ["0uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM", "OOOO", "Illl"]) {
    assert.equal(base58Decode(bad), null, bad);
    assert.equal(isSolanaAddress(bad), false, bad);
  }
});
