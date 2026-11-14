# Operator runbook: Solana deposits into a Squads v4 multisig

How to point the Solana rail's receiving address at a Squads v4 multisig
vault, what the boot does and does not verify, and who holds the keys.

The one rule the whole setup serves: **the server holds no spend key.**
`OXSUM_SOLANA_PAY_TO` names where customer top-ups land; that address should
be a *vault* — a program-derived account whose spending authority is a
multisig's threshold of signers — never a wallet whose seed the deployment,
or anyone who compromises it, could move. The server never signs a
transaction; receiving is all it does.

## The shape

A Squads v4 multisig carries a deterministic **vault PDA** — the account that
actually receives funds — derived from the multisig account:

```
vault[0] = find_program_address(
    ["multisig", <multisig account>, "vault", 0],
    SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf
)
```

`vault index 0` is the default vault every multisig has (the Squads
"authority index" — not a member, not a threshold). `PAY_TO` is that vault
address; `MULTISIG_ACCOUNT` is the multisig account itself (the multisig PDA
the `getMultisigPda` derivation returns — not a signer and not the
`createKey`).

```bash
OXSUM_SOLANA_PAY_TO=<vault index 0 PDA>            # where deposits land
OXSUM_SOLANA_MULTISIG_ACCOUNT=<multisig account>   # the multisig PDA
```

## What the boot verifies — and what it does not

When the rail is configured **and** `OXSUM_SOLANA_MULTISIG_ACCOUNT` is set,
the boot derives `vault index 0` from the multisig account **offline** and
refuses to start unless `PAY_TO` equals it. That catches the dangerous
misconfiguration: a `PAY_TO` that is *not* the vault — a pasted hot-wallet, a
vault of a different index, a multisig that was re-created so its PDAs moved.
A mismatch fails startup and names both the configured `PAY_TO` and the
vault the multisig implies.

When `OXSUM_SOLANA_MULTISIG_ACCOUNT` is **unset**, the answer depends on the
endpoint's shape. On a **devnet/testnet/localhost** endpoint the rail still
boots — a plain wallet is a legitimate `PAY_TO` there — but the boot logs a
loud warning that `PAY_TO` is an unverified **hot wallet**. On a
**mainnet-shaped** endpoint (issue #1189) the boot **refuses to start**: a
hot wallet holding real customer funds is a custody risk, not a warning. The
only way past is the deliberate opt-out `OXSUM_SOLANA_ALLOW_HOT_WALLET=1`
(`1`/`true`; unset/`0`/`false` is off, anything else is a startup error) —
the same explicit-opt-out pattern the sanctions gate holds — which boots
under an even louder warn that names the accepted risk. Treat either warning
as the deployment telling you its deposits land on a single key.

**The residual, on the record.** The check is a PDA *derivation*, not
on-chain introspection. It proves `PAY_TO` is the vault the multisig account
implies — it does **not** read the multisig's threshold or member set, so it
cannot tell a 1-of-1 vault from a 3-of-5 one. Whether the signers are the
people you intend is a setup question, verified in the Squads UI, not a boot
check the rail runs. The derivation also cannot detect that a multisig's
*policy* changed after `PAY_TO` was pinned — the vault address is stable
across member/threshold edits, so a drained-policy multisig keeps the same
valid vault. Review signer membership out-of-band; the boot only proves the
address is the vault.

## Setting up the multisig

1. Create a Squads v4 multisig (app.squads.so, or the `@sqds/multisig` SDK).
   Record the **multisig account address** — the PDA Squads shows as the
   multisig itself — into `OXSUM_SOLANA_MULTISIG_ACCOUNT`.
2. Read the multisig's **vault index 0** address (Squads calls it the default
   vault; `getVaultPda(multisigPda, 0)` in the SDK) into `OXSUM_SOLANA_PAY_TO`.
3. Set the rest of the `OXSUM_SOLANA_*` set (RPC, rate, mint/decimals).
4. Boot. A matching pair starts clean; a mismatch refuses and prints the
   vault the multisig derives — paste that. On mainnet, running an
   unverified hot wallet instead now requires the deliberate
   `OXSUM_SOLANA_ALLOW_HOT_WALLET=1` opt-out (issue #1189); on devnet,
   unsetting `MULTISIG_ACCOUNT` is enough.

## Signer roles — who holds the keys

The multisig's signers are the only spend authority over the vault. Keep them
deliberate and separated:

- **Operations lead** — the day-to-day signer; proposes and approves routine
  vault movements.
- **Finance** — the second approver on every spend; the check that keeps one
  person from moving deposits alone.
- **One offline backup key** — held cold (hardware wallet or an air-gapped
  signer), used only when an online signer is lost or unavailable; it makes a
  2-of-3 recoverable instead of fragile.

A **2-of-3** threshold across those three is the recommended shape: no single
key can spend, any two can act, and the vault survives losing one. More
signers (a director, a second ops) trade liveness for spread — go to 3-of-5
only if the business genuinely needs the wider quorum. Never a 1-of-1
"multisig": that is a hot wallet with extra steps and defeats the custody
the boot check exists to prove.

## Refunds and consolidation are manual multisig ops

The rail **never** signs. Every outgoing transfer — a deposit refund
(`docs/operator-solana-refunds.md`), a consolidation that sweeps the vault
into a treasury account, a withdrawal — is a **manual multisig operation**:
a signer proposes the spend in the Squads UI, the threshold approves it, the
transaction executes on-chain. There is no "the service refunds" path and no
API that moves vault funds; a refund is the operator's signature, not the
server's. The refund runbook's "on-chain return" step is exactly this — done
by a signer against the vault, then recorded on the deposit row for the
books.
