//! Squads v4 multisig custody for the Solana deposit rail (issue #1143).
//!
//! The server holds **no** spend key: `OXSUM_SOLANA_PAY_TO` names the
//! receiving address, and on a production deployment that address should be a
//! Squads v4 multisig's *vault PDA* — the deterministic account that actually
//! receives funds, controlled by the multisig's threshold of signers, not by
//! any key the service process could be made to leak.
//!
//! This module derives that vault PDA offline so the boot can check the
//! configured `payTo` *is* the vault the multisig account implies, rather than
//! an unrelated hot wallet the operator pasted in by mistake. The check is
//! deliberately narrow: it proves `payTo` equals
//! `find_program_address(["multisig", multisig, "vault", 0], program)` — the
//! account a Squads v4 multisig's default vault occupies. It does **not** read
//! the multisig's threshold or member set on-chain (that introspection is the
//! documented residual — see `docs/self-hosted-solana-multisig.md`), so a
//! matching derivation proves the address is the vault, not that the vault's
//! policy is the one the deployment intends.
//!
//! The derivation is hand-rolled — the repo deliberately has no `solana-sdk`
//! (`docs/decisions.md`): a PDA is `sha256(seeds ‖ bump ‖ program ‖
//! "ProgramDerivedAddress")` iterated over a descending bump until the digest
//! is *not* an ed25519 curve point, which is the only curve arithmetic the
//! check needs and the only reason `curve25519-dalek` is a dependency.

use sha2::Digest as _;

/// The Squads v4 multisig program id (mainnet + devnet deploys the same
/// program). Verified against the `@sqds/multisig` SDK's `PROGRAM_ID`
/// (issue #1143); the derivation below reproduces `getVaultPda`'s answer.
pub const SQUADS_V4_PROGRAM_ID: &str = "SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf";

/// The seed prefix every Squads v4 PDA leads with (`SEED_PREFIX` in the
/// program's `pda.ts`).
const SEED_PREFIX: &[u8] = b"multisig";
/// The seed that names the vault PDA under the multisig account.
const SEED_VAULT: &[u8] = b"vault";
/// The PDA derivation's domain tag — appended last in the hashed preimage.
const PDA_MARKER: &[u8] = b"ProgramDerivedAddress";

/// The multisig-account + vault-index pair the boot validates against. The
/// `multisig_account` is what `OXSUM_SOLANA_MULTISIG_ACCOUNT` names (the
/// multisig PDA itself — not a signer, not a create key); `vault_index` is the
/// Squads "authority index" the deposit vault occupies, always `0` for the
/// default vault this rail checks.
#[derive(Debug, Clone, Copy)]
pub struct MultisigVault<'a> {
    /// The Squads multisig account address (base58), whose `index`-th vault
    /// PDA is derived.
    pub multisig_account: &'a str,
    /// The vault/authority index — `0` for the multisig's default vault.
    pub vault_index: u8,
}

/// What a failed Squads-vault derivation or mismatch is. The boot maps the
/// variants to its own startup-error wording; the variants stay granular so a
/// misconfiguration names which half is wrong — a malformed address, a
/// derivation that found no valid bump, or a `payTo` that is not the vault.
#[derive(Debug, PartialEq, Eq)]
pub enum VaultCheckError {
    /// An address did not decode to a 32-byte base58 public key (the multisig
    /// account or the program id — the program id is a constant, so in
    /// practice this is the configured multisig).
    InvalidAddress(&'static str),
    /// `find_program_address` found no bump in `255..=0` that yields an
    /// off-curve digest — astronomically unlikely for a real multisig, and
    /// the answer a wrong-program derivation would also produce.
    NoValidBump,
    /// The configured `payTo` is not the vault the multisig implies — the
    /// misconfiguration the boot check exists to catch (a pasted hot-wallet,
    /// a vault of a different index, a multisig that was re-created).
    /// Carries the derived vault so the startup error can name both.
    PayToIsNotVault {
        /// The vault PDA the multisig account + index actually derive.
        derived: String,
        /// The `payTo` that was configured instead.
        configured: String,
    },
}

impl std::fmt::Display for VaultCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultCheckError::InvalidAddress(which) => {
                write!(f, "{which} is not a 32-byte base58 public key")
            }
            VaultCheckError::NoValidBump => {
                write!(f, "no PDA bump derives a valid vault address")
            }
            VaultCheckError::PayToIsNotVault {
                derived,
                configured,
            } => write!(
                f,
                "the configured payTo {configured} is not the multisig's vault {derived}"
            ),
        }
    }
}

impl std::error::Error for VaultCheckError {}

/// Decodes a base58 public key into its 32 bytes. Solana addresses are always
/// 32 bytes; a longer or shorter decode is not an address.
fn decode_address(raw: &str, which: &'static str) -> Result<[u8; 32], VaultCheckError> {
    let bytes = bs58::decode(raw.trim())
        .into_vec()
        .map_err(|_| VaultCheckError::InvalidAddress(which))?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| VaultCheckError::InvalidAddress(which))
}

/// Whether `candidate` lands *on* the ed25519 curve — the PDA validity test's
/// negative half. A Solana program-derived address must be a point no private
/// key can sign for, i.e. one whose 32-byte compressed-Y encoding fails
/// decompression. `true` means the digest is a real curve point and the bump
/// is unusable.
fn is_on_curve(candidate: &[u8; 32]) -> bool {
    curve25519_dalek::edwards::CompressedEdwardsY(*candidate)
        .decompress()
        .is_some()
}

/// One PDA hash: `sha256(seed₁ ‖ … ‖ seedₙ ‖ bump ‖ program ‖
/// "ProgramDerivedAddress")`. `bump` is appended as a single byte the way
/// `solana-sdk`'s `create_program_address` does.
fn pda_hash(seeds: &[&[u8]], bump: u8, program_id: &[u8; 32]) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    for seed in seeds {
        hasher.update(seed);
    }
    hasher.update([bump]);
    hasher.update(program_id);
    hasher.update(PDA_MARKER);
    hasher.finalize().into()
}

/// Solana's `find_program_address`: try bumps 255 → 0, return the first whose
/// digest is off-curve, plus that bump. `None` when every bump produced a
/// curve point — the `NoValidBump` case.
fn find_program_address(seeds: &[&[u8]], program_id: &[u8; 32]) -> Option<([u8; 32], u8)> {
    for bump in (0u8..=u8::MAX).rev() {
        let candidate = pda_hash(seeds, bump, program_id);
        if !is_on_curve(&candidate) {
            return Some((candidate, bump));
        }
    }
    None
}

/// Derives the Squads v4 vault PDA for `multisig_account` at `vault_index`
/// and returns it base58-encoded — the address `OXSUM_SOLANA_PAY_TO` should
/// equal when the deployment's receiving account is that multisig's vault.
///
/// The seeds mirror the program's `getVaultPda` (`src/pda.ts`):
/// `[SEED_PREFIX, multisigPda, SEED_VAULT, u8(index)]` — that is, the literal
/// strings `"multisig"` and `"vault"` framing the multisig account's 32 bytes
/// and the single-byte index. The `vault_index` is the Squads *authority
/// index*: `0` is the default vault every multisig carries and the only one
/// this check validates; a nonzero index is a different vault and a different
/// address.
///
/// # Errors
///
/// `VaultCheckError::InvalidAddress` when an address does not decode, and
/// `VaultCheckError::NoValidBump` when no bump derives an off-curve point.
pub fn derive_vault_pda(
    multisig_account: &str,
    vault_index: u8,
) -> Result<String, VaultCheckError> {
    let multisig = decode_address(multisig_account, "the multisig account")?;
    let program = decode_address(SQUADS_V4_PROGRAM_ID, "the Squads program id")?;
    let seeds: [&[u8]; 4] = [SEED_PREFIX, &multisig, SEED_VAULT, &[vault_index]];
    let (vault, _bump) =
        find_program_address(&seeds, &program).ok_or(VaultCheckError::NoValidBump)?;
    Ok(bs58::encode(vault).into_string())
}

/// Compares a configured `payTo` against the derived vault: `Ok(())` when
/// `pay_to` is exactly the `vault_index` vault of `multisig_account`. The
/// comparison is the point of the boot check — a `payTo` that is not the
/// derived vault is the misconfiguration (a pasted hot-wallet, a stale vault
/// from a multisig that was re-created, an index other than the one intended).
///
/// # Errors
///
/// The [`VaultCheckError`] the derivation produces, surfaced unchanged.
pub fn check_pay_to_is_vault(
    multisig: MultisigVault<'_>,
    pay_to: &str,
) -> Result<(), VaultCheckError> {
    let derived = derive_vault_pda(multisig.multisig_account, multisig.vault_index)?;
    if derived == pay_to.trim() {
        Ok(())
    } else {
        Err(VaultCheckError::PayToIsNotVault {
            derived,
            configured: pay_to.trim().to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// The known-answer pair `getVaultPda` produced for the multisig PDA
    /// `getMultisigPda(createKey)` derives — create key
    /// `5WrWJhPxeU6rZ6v5e2hK6c5T2qQ2mE7v6b6v2f3gHk1a`, multisig
    /// `C46NEXuBLRJjJhfAsKBUxUYdxSBQUPc3SXQFRBTXfupC`. The derivation is
    /// pinned to the `@sqds/multisig` SDK's answer, not recomputed.
    const MULTISIG: &str = "C46NEXuBLRJjJhfAsKBUxUYdxSBQUPc3SXQFRBTXfupC";
    /// `getVaultPda(multisig, index 0)` — the default vault's known answer.
    const VAULT_0: &str = "65kaU3KWZJCEz11vrCQfUZMc5AWSTzkFybwKD5BckLH8";
    /// `getVaultPda(multisig, index 1)` — a different authority index is a
    /// different vault.
    const VAULT_1: &str = "7Kaw2CR9P7dzKmExvUV7yd6ik5i8ErWK7WVM3585PiDC";

    #[test]
    fn the_known_multisig_derives_its_known_vault() {
        assert_eq!(derive_vault_pda(MULTISIG, 0).unwrap(), VAULT_0);
        assert_eq!(derive_vault_pda(MULTISIG, 1).unwrap(), VAULT_1);
    }

    #[test]
    fn a_different_index_is_a_different_vault() {
        assert_ne!(
            derive_vault_pda(MULTISIG, 0).unwrap(),
            derive_vault_pda(MULTISIG, 1).unwrap()
        );
    }

    #[test]
    fn a_different_multisig_is_a_different_vault() {
        // The program id itself as a stand-in multisig — a valid 32-byte key
        // whose vault is not C46NEX…'s.
        let other = SQUADS_V4_PROGRAM_ID;
        assert_ne!(derive_vault_pda(other, 0).unwrap(), VAULT_0);
    }

    #[test]
    fn a_malformed_multisig_address_is_refused() {
        assert_eq!(
            derive_vault_pda("not-base58!!!", 0),
            Err(VaultCheckError::InvalidAddress("the multisig account"))
        );
        // A base58 string that is not 32 bytes (an 8-byte decode) is refused.
        assert_eq!(
            derive_vault_pda("abc", 0),
            Err(VaultCheckError::InvalidAddress("the multisig account"))
        );
    }

    #[test]
    fn pay_to_is_vault_accepts_the_matching_address() {
        let vault = MultisigVault {
            multisig_account: MULTISIG,
            vault_index: 0,
        };
        assert!(check_pay_to_is_vault(vault, VAULT_0).is_ok());
        // Surrounding whitespace on the configured address is tolerated.
        assert!(
            check_pay_to_is_vault(vault, "  65kaU3KWZJCEz11vrCQfUZMc5AWSTzkFybwKD5BckLH8  ")
                .is_ok()
        );
    }

    #[test]
    fn pay_to_is_vault_refuses_a_mismatch() {
        let vault = MultisigVault {
            multisig_account: MULTISIG,
            vault_index: 0,
        };
        // A different index's vault is a mismatch for index 0.
        assert!(check_pay_to_is_vault(vault, VAULT_1).is_err());
        // A hot-wallet / arbitrary address is a mismatch.
        assert!(
            check_pay_to_is_vault(vault, "SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf").is_err()
        );
    }
}
