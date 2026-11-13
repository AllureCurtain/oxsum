// Solana wire-format helpers for the exception-queue surface (issue #1142).
// The server is the authority — it re-validates before writing — but the
// refund-address form can refuse a malformed paste before the round-trip.

const BASE58_ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/** Decodes a base58 string, or `null` on a character outside the alphabet. */
export function base58Decode(text: string): Uint8Array | null {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  // Big-int decode: each digit multiplies the accumulator by 58 — exact for
  // the 32/64-byte values Solana uses, and dependency-free. Leading '1's are
  // zero bytes — the big-int drops them, so they are counted separately.
  let leading = 0;
  while (trimmed[leading] === "1") leading += 1;
  let value = 0n;
  for (const ch of trimmed) {
    const digit = BASE58_ALPHABET.indexOf(ch);
    if (digit < 0) return null;
    value = value * 58n + BigInt(digit);
  }
  const hex = value.toString(16);
  const body = hex === "0" ? new Uint8Array(0) : Uint8Array.from(
    (hex.length % 2 === 1 ? `0${hex}` : hex).match(/.{2}/g)!.map((b) => parseInt(b, 16)),
  );
  const decoded = new Uint8Array(leading + body.length);
  decoded.set(body, leading);
  return decoded;
}

/** A base58-encoded 32-byte Solana public key — the refund address's shape. */
export function isSolanaAddress(text: string): boolean {
  const decoded = base58Decode(text);
  return decoded !== null && decoded.length === 32;
}
