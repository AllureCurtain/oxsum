//! End-to-end Solana deposits against a local `solana-test-validator`
//! (issue #1152): the real `SolanaRpcClient` — not a `ChainLookup` stub —
//! finds and validates the paying transfer, and `credit_solana_deposit` books
//! it on the scratch database. The covered cases:
//!
//! - SOL happy path: airdrop → a reference-carrying `system` transfer →
//!   `credited`.
//! - SPL/USDC happy path: a test mint created on the validator →
//!   `transferChecked` carrying the reference → `credited`.
//! - underpay → `underpaid`; overpay → `overpaid` (the opened amount still
//!   credits).
//! - post-expiry arrival: a transfer whose on-chain `blockTime` lands
//!   at/after `expiresAt` → `late`.
//! - same-signature replay: a second confirm resolves the standing receipt —
//!   exactly one credit.
//! - reference drowned by noise: dishonoring transfers touching the marker
//!   cannot mask the honoring one.
//!
//! The tests skip cleanly when no validator is reachable: set
//! `OXSUM_SOLANA_E2E_RPC` (default `http://127.0.0.1:8899`) and run
//! `solana-test-validator --reset`. DATABASE_URL is needed for the credit
//! path, like every other suite.
//!
//! Transactions are built and signed by hand — the crate deliberately keeps
//! no `solana-sdk` dependency (the rail speaks two JSON-RPC calls), so the
//! e2e pays its transfers with a ~200-line wire-format builder rather than
//! pulling the SDK in as a dev-dependency. Everything a payment needs is
//! stable wire format: the message header, account-key ordering, shortvec
//! lengths, Ed25519 signatures over the serialized message.
//!
//! The SPL half runs the validator's built-in Token and Associated Token
//! Account programs (genesis programs on `solana-test-validator`): a fresh
//! mint is created per run, the payer gets an ATA minted full, the deposit
//! address gets its ATA, and the payment is a `transferChecked` carrying the
//! intent's reference as a read-only non-signer — the exact shape
//! `validate_transfer`'s `token_delta` reads under `jsonParsed`.

use std::time::Duration;

use ed25519_dalek::{Signer, SigningKey};
use oxsum_core::{
    AssetTerms, Db, LockedRate, NewUser, SolanaCredit, SolanaRailVerifier, SolanaTerms,
    USD_PEGGED_RATE_MICRO, solana_status,
};
use oxsum_integrations::solana::{MintWhitelistEntry, SolanaConfig, SolanaRpcClient};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

/// The RPC the suite talks to: `OXSUM_SOLANA_E2E_RPC`, or the
/// `solana-test-validator` default.
fn rpc_url() -> String {
    let _ = dotenvy::dotenv();
    std::env::var("OXSUM_SOLANA_E2E_RPC")
        .ok()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:8899".to_owned())
}

/// The suite's world: a reachable validator plus a scratch database, or
/// `None` so the test skips. The reachability probe is `getHealth` — the
/// cheapest call the JSON-RPC offers — answered under a short timeout so a
/// missing validator fails the probe fast rather than hanging the suite.
async fn world(slug: &str) -> Option<(Db, Chain)> {
    let _ = dotenvy::dotenv();
    // Probe the validator first — the scratch database is a side effect a
    // validator-less run should never pay for.
    let chain = Chain::new(&rpc_url());
    if !chain.reachable().await {
        eprintln!("solana e2e: no validator at {} — skipping", chain.url);
        return None;
    }
    let url = oxsum_testkit::suite_db_url(slug)?;
    let db = Db::connect(&url, 5).await.unwrap();
    db.migrate().await.unwrap();
    Some((db, chain))
}

/// A fresh organization's id and the wallet `credit_solana_deposit` books
/// into — the fixture every case shares.
async fn organization(db: &Db, slug: &str) -> (Uuid, oxsum_core::Wallet) {
    let registration = db
        .register(NewUser {
            email: format!(
                "solana-e2e-{slug}-{}@example.test",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "solana-private-password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = registration.organization.id;
    let tenant_id: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &tenant_id)
        .await
        .unwrap();
    (org, wallet)
}

/// The deposit address every e2e intent pays: a fresh keypair per test run,
/// so `payTo` is a real base58 address the validator can receive lamports and
/// token accounts for.
fn pay_to() -> Keypair {
    Keypair::generate()
}

/// The deployment's SOL-only terms: `rate_micro = 1_000_000_000` means one
/// whole SOL buys one credit, so `amount_minor` minor-credits converts to
/// `amount_minor` lamports exactly — the e2e's figures stay readable.
fn sol_terms(pay_to: &Keypair) -> SolanaTerms {
    SolanaTerms::new(
        pay_to.address(),
        vec![AssetTerms {
            key: "sol".to_owned(),
            spl_token_mint: None,
            asset_decimals: 9,
            rate_micro: 1_000_000_000,
        }],
        "sol".to_owned(),
        600,
        1,
        i64::MAX,
        0,
    )
}

/// A two-asset whitelist for the SPL case: SOL plus the test mint at USD
/// parity (6 decimals like real USDC), `usdc` default.
fn spl_terms(pay_to: &Keypair, mint: &Pubkey) -> SolanaTerms {
    SolanaTerms::new(
        pay_to.address(),
        vec![
            AssetTerms {
                key: "sol".to_owned(),
                spl_token_mint: None,
                asset_decimals: 9,
                rate_micro: 1_000_000_000,
            },
            AssetTerms {
                key: "usdc".to_owned(),
                spl_token_mint: Some(mint.to_base58()),
                asset_decimals: 6,
                rate_micro: USD_PEGGED_RATE_MICRO,
            },
        ],
        "usdc".to_owned(),
        600,
        1,
        i64::MAX,
        0,
    )
}

/// The `LockedRate` `resolve_rate` answers under `SolanaPricing::Static` for
/// the named whitelist asset — `open_solana_deposit` carries it.
fn rate(terms: &SolanaTerms, asset: &str) -> LockedRate {
    LockedRate::Static {
        rate_micro: terms
            .terms_for_asset(Some(asset))
            .expect("the test asset is on the whitelist")
            .rate_micro,
    }
}

/// The `SolanaRailVerifier` bound to the REAL client on the validator — the
/// piece that makes this e2e: `find_reference`/`validate_transfer` are the
/// production JSON-RPC calls, exercised end-to-end.
fn verifier(
    chain: &Chain,
    pay_to: &Keypair,
    terms: &SolanaTerms,
    mints: Vec<MintWhitelistEntry>,
) -> SolanaRailVerifier {
    let config = SolanaConfig {
        rpc_urls: vec![chain.url.clone()],
        pay_to: pay_to.address(),
        mints,
        commitment: "confirmed".to_owned(),
        rpc_auth_header: None,
    };
    let client = SolanaRpcClient::try_new(vec![chain.url.clone()], reqwest::Client::new(), config)
        .expect("the e2e endpoint carries a host");
    SolanaRailVerifier::new(Arc::new(client), terms.clone())
}

/// The deposit row's status.
async fn status(db: &Db, deposit_id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
        .bind(deposit_id)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

/// The deposit row's meta.
async fn meta(db: &Db, deposit_id: Uuid) -> Value {
    sqlx::query_scalar("SELECT meta FROM oxsum.deposits WHERE deposit_id = $1")
        .bind(deposit_id)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

/// Opens a SOL intent of `amount_minor` minor-credits (== lamports at the
/// e2e's 1e9 rate) and returns `(deposit_id, reference, expected_lamports)`.
async fn open_sol(
    db: &Db,
    org: Uuid,
    terms: &SolanaTerms,
    amount_minor: i64,
) -> (Uuid, Pubkey, i64) {
    let deposit_id = Uuid::new_v4();
    let (reference, lamports, _expires) = db
        .open_solana_deposit(
            org,
            deposit_id,
            amount_minor,
            terms,
            Some("sol"),
            &rate(terms, "sol"),
        )
        .await
        .unwrap()
        .expect("a fresh intent inserts");
    (deposit_id, Pubkey::from_base58(&reference), lamports)
}

// ---------------------------------------------------------------------------
// The chain half: keys, the wire-format transaction builder and a JSON-RPC
// submitter — everything a payment needs, nothing of solana-sdk.
// ---------------------------------------------------------------------------

/// A 32-byte Solana public key.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Pubkey([u8; 32]);

impl Pubkey {
    /// The base58 spelling `meta`/`ExpectedTransfer` carry.
    fn to_base58(self) -> String {
        bs58::encode(self.0).into_string()
    }

    /// Decodes a base58 address; panics on the malformed shapes a test bug
    /// would feed it — a test builder need not be gentle.
    fn from_base58(address: &str) -> Self {
        let bytes = bs58::decode(address).into_vec().expect("base58 decodes");
        let bytes: [u8; 32] = bytes.try_into().expect("a pubkey is 32 bytes");
        Self(bytes)
    }
}

/// A keypair: the `SigningKey` signs, the address receives.
struct Keypair {
    signing: SigningKey,
    pubkey: Pubkey,
}

impl Keypair {
    /// A fresh random keypair.
    fn generate() -> Self {
        use rand::Rng as _;
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
        let signing = SigningKey::from_bytes(&seed);
        let pubkey = Pubkey(signing.verifying_key().to_bytes());
        Self { signing, pubkey }
    }

    /// The account's base58 address.
    fn address(&self) -> String {
        self.pubkey.to_base58()
    }
}

/// One account's role inside an instruction — the `AccountMeta` a compiled
/// instruction names. The transaction builder re-derives the global key table
/// from these flags (Solana's ordering: writable signers, readonly signers,
/// writable non-signers, readonly non-signers).
#[derive(Clone, Copy)]
struct AccountMeta {
    pubkey: Pubkey,
    signer: bool,
    writable: bool,
}

impl AccountMeta {
    fn signer_writable(pubkey: Pubkey) -> Self {
        Self {
            pubkey,
            signer: true,
            writable: true,
        }
    }

    fn writable(pubkey: Pubkey) -> Self {
        Self {
            pubkey,
            signer: false,
            writable: true,
        }
    }

    fn readonly(pubkey: Pubkey) -> Self {
        Self {
            pubkey,
            signer: false,
            writable: false,
        }
    }
}

/// An instruction as the builder sees it: the program's id plus the accounts
/// and data. The program id lands in the account table as a readonly
/// non-signer, the same shape the wire keeps it in.
struct Instruction {
    program: Pubkey,
    accounts: Vec<AccountMeta>,
    data: Vec<u8>,
}

/// A Solana transaction: the message plus the signatures that sign it. Built
/// by [`Transaction::compile`], which orders the account table the wire's
/// way and signs the serialized message with every required signer.
struct Transaction {
    wire: Vec<u8>,
}

impl Transaction {
    /// Compiles `instructions` into the wire-format transaction: builds the
    /// deduplicated account table (writable signers, readonly signers,
    /// writable non-signers, readonly non-signers — privilege unions applied
    /// when an account repeats), compiles the message, and signs it with
    /// `payer` plus every additional `signers` entry. `payer` is the fee
    /// payer — `accountKeys[0]` — and must be a signer.
    fn compile(
        payer: &Keypair,
        signers: &[&Keypair],
        instructions: &[Instruction],
        blockhash: &[u8; 32],
    ) -> Self {
        // Union each account's flags across the instructions it appears on.
        // The fee payer is always a writable signer (it pays the fee even
        // when no instruction names it).
        let mut table: Vec<AccountMeta> = Vec::new();
        let mut upsert = |meta: AccountMeta| {
            if let Some(existing) = table.iter_mut().find(|m| m.pubkey == meta.pubkey) {
                existing.signer |= meta.signer;
                existing.writable |= meta.writable;
            } else {
                table.push(meta);
            }
        };
        upsert(AccountMeta::signer_writable(payer.pubkey));
        for signer in signers {
            upsert(AccountMeta::signer_writable(signer.pubkey));
        }
        for instruction in instructions {
            for meta in &instruction.accounts {
                upsert(*meta);
            }
            upsert(AccountMeta::readonly(instruction.program));
        }
        // Order: writable signers, readonly signers, writable non-signers,
        // readonly non-signers — stable inside each bucket so the program id
        // lands after the accounts it acts on, the usual wire shape.
        table.sort_by_key(|m| (!m.signer as u8, !m.writable as u8));
        let index_of = |pubkey: &Pubkey| -> u8 {
            u8::try_from(
                table
                    .iter()
                    .position(|m| &m.pubkey == pubkey)
                    .expect("every key is in the table"),
            )
            .expect("the table fits in a u8")
        };
        let signer_count = table.iter().filter(|m| m.signer).count();
        let readonly_signers = table.iter().filter(|m| m.signer && !m.writable).count();
        let readonly_unsigned = table.iter().filter(|m| !m.signer && !m.writable).count();
        let mut message = Vec::new();
        // Header: num signatures, num readonly signers, num readonly unsigned.
        message.extend_from_slice(&[
            u8::try_from(signer_count).unwrap(),
            u8::try_from(readonly_signers).unwrap(),
            u8::try_from(readonly_unsigned).unwrap(),
        ]);
        push_shortvec(&mut message, table.len());
        for meta in &table {
            message.extend_from_slice(&meta.pubkey.0);
        }
        message.extend_from_slice(blockhash);
        push_shortvec(&mut message, instructions.len());
        for instruction in instructions {
            message.push(index_of(&instruction.program));
            push_shortvec(&mut message, instruction.accounts.len());
            for meta in &instruction.accounts {
                message.push(index_of(&meta.pubkey));
            }
            push_shortvec(&mut message, instruction.data.len());
            message.extend_from_slice(&instruction.data);
        }
        // Sign the message with every required signer, in account-table
        // order — signature[i] belongs to accountKeys[i], the wire's rule.
        let mut wire = Vec::new();
        push_shortvec(&mut wire, signer_count);
        let signer_keys: Vec<Pubkey> = table
            .iter()
            .filter(|m| m.signer)
            .map(|m| m.pubkey)
            .collect();
        let mut available: Vec<&Keypair> = vec![payer];
        available.extend(signers.iter().copied());
        for key in &signer_keys {
            let keypair = available
                .iter()
                .find(|k| k.pubkey == *key)
                .copied()
                .expect("every required signer is supplied");
            let signature = keypair.signing.sign(&message);
            wire.extend_from_slice(&signature.to_bytes());
        }
        wire.extend_from_slice(&message);
        Self { wire }
    }
}

/// Solana's shortvec length prefix: 7-bit groups, little-endian, the high bit
/// of every byte but the last set. Enough for any table the tests build.
fn push_shortvec(out: &mut Vec<u8>, mut len: usize) {
    loop {
        let mut byte = (len & 0x7f) as u8;
        len >>= 7;
        if len > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
}

// --- Program ids (genesis programs on solana-test-validator). --------------

/// `11111111111111111111111111111111` — the System program.
fn system_program() -> Pubkey {
    Pubkey([0u8; 32])
}

/// The SPL Token program.
fn token_program() -> Pubkey {
    Pubkey::from_base58("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA")
}

/// The Associated Token Account program.
fn ata_program() -> Pubkey {
    Pubkey::from_base58("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL")
}

/// The rent sysvar `InitializeMint` names.
fn rent_sysvar() -> Pubkey {
    Pubkey::from_base58("SysvarRent111111111111111111111111111111111")
}

// --- Instruction builders ---------------------------------------------------

/// `SystemProgram::createAccount` — the mint account's allocation. Discriminant
/// 0: `[u32le 0, u64le lamports, u64le space, owner (32)]`.
fn create_account(
    from: &Pubkey,
    new: &Pubkey,
    lamports: u64,
    space: u64,
    owner: &Pubkey,
) -> Instruction {
    let mut data = Vec::with_capacity(4 + 8 + 8 + 32);
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&lamports.to_le_bytes());
    data.extend_from_slice(&space.to_le_bytes());
    data.extend_from_slice(&owner.0);
    Instruction {
        program: system_program(),
        accounts: vec![
            AccountMeta::signer_writable(*from),
            AccountMeta::signer_writable(*new),
        ],
        data,
    }
}

/// `SystemProgram::transfer`, optionally carrying a Solana Pay `reference`
/// marker as a read-only non-signer account — the shape `find_reference`
/// keys on.
fn transfer(from: &Pubkey, to: &Pubkey, lamports: u64, reference: Option<&Pubkey>) -> Instruction {
    let mut data = Vec::with_capacity(12);
    data.extend_from_slice(&2u32.to_le_bytes());
    data.extend_from_slice(&lamports.to_le_bytes());
    let mut accounts = vec![
        AccountMeta::signer_writable(*from),
        AccountMeta::writable(*to),
    ];
    if let Some(reference) = reference {
        accounts.push(AccountMeta::readonly(*reference));
    }
    Instruction {
        program: system_program(),
        accounts,
        data,
    }
}

/// `TokenInstruction::InitializeMint`: `[0, decimals, mint_authority (32),
/// COption<freeze_authority>]` — `freeze_authority` absent is the 4-byte zero
/// tag. The mint account must already be allocated to the Token program.
fn initialize_mint(mint: &Pubkey, authority: &Pubkey, decimals: u8) -> Instruction {
    let mut data = Vec::with_capacity(38);
    data.push(0u8);
    data.push(decimals);
    data.extend_from_slice(&authority.0);
    data.extend_from_slice(&0u32.to_le_bytes()); // no freeze authority
    Instruction {
        program: token_program(),
        accounts: vec![
            AccountMeta::writable(*mint),
            AccountMeta::readonly(rent_sysvar()),
        ],
        data,
    }
}

/// `TokenInstruction::MintTo` — discriminant 7: `[7, u64le amount]`.
fn mint_to(mint: &Pubkey, dest: &Pubkey, authority: &Pubkey, amount: u64) -> Instruction {
    let mut data = Vec::with_capacity(9);
    data.push(7u8);
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program: token_program(),
        accounts: vec![
            AccountMeta::writable(*mint),
            AccountMeta::writable(*dest),
            AccountMeta {
                pubkey: *authority,
                signer: true,
                writable: false,
            },
        ],
        data,
    }
}

/// `TokenInstruction::TransferChecked` — discriminant 12:
/// `[12, u64le amount, decimals]`, optionally carrying the reference marker.
fn transfer_checked(
    source: &Pubkey,
    mint: &Pubkey,
    dest: &Pubkey,
    owner: &Pubkey,
    amount: u64,
    decimals: u8,
    reference: Option<&Pubkey>,
) -> Instruction {
    let mut data = Vec::with_capacity(10);
    data.push(12u8);
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(decimals);
    let mut accounts = vec![
        AccountMeta::writable(*source),
        AccountMeta::readonly(*mint),
        AccountMeta::writable(*dest),
        AccountMeta {
            pubkey: *owner,
            signer: true,
            writable: false,
        },
    ];
    if let Some(reference) = reference {
        accounts.push(AccountMeta::readonly(*reference));
    }
    Instruction {
        program: token_program(),
        accounts,
        data,
    }
}

/// `AssociatedTokenAccountInstruction::Create` — no data, the program derives
/// the ATA from (owner, token program, mint) itself.
fn create_ata(payer: &Pubkey, ata: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> Instruction {
    Instruction {
        program: ata_program(),
        accounts: vec![
            AccountMeta::signer_writable(*payer),
            AccountMeta::writable(*ata),
            AccountMeta::readonly(*owner),
            AccountMeta::readonly(*mint),
            AccountMeta::readonly(system_program()),
            AccountMeta::readonly(token_program()),
        ],
        data: Vec::new(),
    }
}

/// The Associated Token Account PDA for `(owner, mint)` on the classic Token
/// program: seeds `[owner, token_program, mint]` under `ata_program`, the
/// same `find_program_address` derivation `crates/integrations::squads`
/// documents — sha256 over seeds + bump + program + "ProgramDerivedAddress",
/// the first descending bump whose digest is off the ed25519 curve.
fn associated_token_address(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    let token = token_program();
    let ata = ata_program();
    for bump in (0u8..=u8::MAX).rev() {
        let mut hasher = sha2::Sha256::new();
        use sha2::Digest as _;
        hasher.update(owner.0);
        hasher.update(token.0);
        hasher.update(mint.0);
        hasher.update([bump]);
        hasher.update(ata.0);
        hasher.update(b"ProgramDerivedAddress");
        let digest: [u8; 32] = hasher.finalize().into();
        if curve25519_dalek::edwards::CompressedEdwardsY(digest)
            .decompress()
            .is_none()
        {
            return Pubkey(digest);
        }
    }
    panic!("no ATA bump derives an off-curve address")
}

// --- The JSON-RPC submitter --------------------------------------------------

/// The minimal JSON-RPC client the suite submits through: airdrops, blockhash
/// reads, transaction sends and confirmation polls. `SolanaRpcClient` itself
/// is what the tests verify — submitting through it would conflate the
/// fixture with the subject, so the suite speaks its own 4-call RPC.
struct Chain {
    url: String,
    http: reqwest::Client,
}

impl Chain {
    fn new(url: &str) -> Self {
        Self {
            url: url.to_owned(),
            http: reqwest::Client::new(),
        }
    }

    /// `getHealth` — the reachability probe the skip gate runs. A valid
    /// probe is a JSON-RPC `result` answer, not just a 200: a CI runner that
    /// happens to serve something else on the port must not trick the suite
    /// into running against it.
    async fn reachable(&self) -> bool {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "getHealth"});
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            self.http.post(&self.url).json(&body).send(),
        )
        .await;
        let Ok(Ok(response)) = response else {
            return false;
        };
        response
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v["result"].as_str().map(str::to_owned))
            .as_deref()
            == Some("ok")
    }

    /// One call, unwrapping the JSON-RPC envelope.
    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": method, "params": params,
        });
        let response: Value = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("{method} transport: {e}"))?
            .json()
            .await
            .map_err(|e| format!("{method} unreadable: {e}"))?;
        if let Some(error) = response.get("error") {
            return Err(format!("{method} refused: {error}"));
        }
        Ok(response["result"].clone())
    }

    /// `requestAirdrop` — the test-validator faucet, confirmed.
    async fn airdrop(&self, to: &Pubkey, lamports: u64) {
        let signature = self
            .call("requestAirdrop", json!([to.to_base58(), lamports]))
            .await
            .expect("the faucet answers")
            .as_str()
            .expect("a signature")
            .to_owned();
        self.confirm(&signature).await;
    }

    /// `getLatestBlockhash` — the recent blockhash every transaction needs.
    /// `processed` gives the freshest hash on the single-validator fork, the
    /// longest window before it ages out.
    async fn blockhash(&self) -> [u8; 32] {
        let result = self
            .call("getLatestBlockhash", json!([{"commitment": "processed"}]))
            .await
            .expect("a blockhash answers");
        let b58 = result["value"]["blockhash"].as_str().expect("a blockhash");
        let bytes = bs58::decode(b58).into_vec().expect("base58 decodes");
        bytes.try_into().expect("a blockhash is 32 bytes")
    }

    /// `sendTransaction` — submits the wire bytes base64-encoded and returns
    /// the signature. `skipPreflight` because the confirmation is awaited
    /// through `getSignatureStatuses` anyway — and because the validator's
    /// preflight bank can lag the `confirmed` blockhash queue enough to
    /// answer `BlockhashNotFound` on a perfectly valid blockhash.
    async fn send(&self, tx: &Transaction) -> String {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&tx.wire);
        self.call(
            "sendTransaction",
            json!([encoded, {"encoding": "base64", "skipPreflight": true}]),
        )
        .await
        .expect("sendTransaction answers")
        .as_str()
        .expect("a signature")
        .to_owned()
    }

    /// `send` + `confirm` — the usual submit shape.
    async fn submit(&self, tx: &Transaction) -> String {
        let signature = self.send(tx).await;
        self.confirm(&signature).await;
        signature
    }

    /// Waits for `signature` to reach `confirmed`: `getSignatureStatuses`
    /// polled on a short interval — the local validator lands a slot in
    /// ~400 ms, so a handful of polls is plenty.
    async fn confirm(&self, signature: &str) {
        for _ in 0..40 {
            let result = self
                .call(
                    "getSignatureStatuses",
                    json!([[signature], {"searchTransactionHistory": true}]),
                )
                .await
                .expect("status answers");
            let status = &result["value"][0];
            if let Some(err) = status.get("err").filter(|e| !e.is_null()) {
                panic!("the transaction failed on-chain: {err}");
            }
            if status["confirmationStatus"].as_str() == Some("confirmed")
                || status["confirmationStatus"].as_str() == Some("finalized")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        panic!("{signature} never confirmed on the validator");
    }

    /// Builds, signs and submits a transaction — the one-call payment shape
    /// every test uses.
    async fn transact(
        &self,
        payer: &Keypair,
        signers: &[&Keypair],
        instructions: &[Instruction],
    ) -> String {
        let blockhash = self.blockhash().await;
        let tx = Transaction::compile(payer, signers, instructions, &blockhash);
        self.submit(&tx).await
    }

    /// A SOL payment honoring an intent: `payer` transfers `lamports` to
    /// `pay_to`, carrying `reference` as the read-only non-signer marker.
    async fn pay_sol(
        &self,
        payer: &Keypair,
        pay_to: &Keypair,
        reference: &Pubkey,
        lamports: u64,
    ) -> String {
        self.transact(
            payer,
            &[],
            &[transfer(
                &payer.pubkey,
                &pay_to.pubkey,
                lamports,
                Some(reference),
            )],
        )
        .await
    }

    /// A noise transaction on the marker: carries `reference` but pays the
    /// payer itself — the dishonoring shape `find_reference` must see past.
    /// A self-transfer needs no rent-exempt funding for a fresh recipient
    /// (the account already holds lamports), so the noise is one cheap send
    /// apiece. `nonce` only names the call site for debugging.
    async fn noise_on(&self, payer: &Keypair, reference: &Pubkey, nonce: u64) {
        let _ = nonce;
        self.transact(
            payer,
            &[],
            &[transfer(
                &payer.pubkey,
                &payer.pubkey,
                5_000,
                Some(reference),
            )],
        )
        .await;
    }
}

/// The fixture a funded payer needs: a fresh keypair airdropped enough for the
/// payment plus fees. `fund_lamports` covers the transfer amount; ~0.01 SOL
/// covers fees several times over.
async fn funded_payer(chain: &Chain, fund_lamports: u64) -> Keypair {
    let payer = Keypair::generate();
    chain
        .airdrop(&payer.pubkey, fund_lamports + 20_000_000)
        .await;
    payer
}

/// The SPL fixture: a fresh 6-decimal mint (authority = the payer), the
/// payer's ATA minted `supply` base units, and the deposit address's ATA.
/// Returns `(mint, payer_ata, payto_ata, payer, pay_to)` — the deposit
/// address's keypair rides out so the terms and verifier can name it. Mint
/// account: 82 bytes, its rent-exempt figure read off the chain.
async fn spl_fixture(chain: &Chain, supply: u64) -> (Keypair, Pubkey, Pubkey, Keypair, Keypair) {
    let payer = funded_payer(chain, 0).await;
    let mint = Keypair::generate();
    let pay_to = pay_to();
    let payer_ata = associated_token_address(&payer.pubkey, &mint.pubkey);
    let payto_ata = associated_token_address(&pay_to.pubkey, &mint.pubkey);
    // Rent-exempt minimum for the 82-byte mint account — read off the chain
    // rather than hardcoded so a rent change cannot break the fixture.
    let rent = chain
        .call(
            "getMinimumBalanceForRentExemption",
            json!([82, {"commitment": "confirmed"}]),
        )
        .await
        .expect("rent answers")
        .as_u64()
        .expect("a lamport figure");
    chain
        .transact(
            &payer,
            &[&mint],
            &[
                create_account(&payer.pubkey, &mint.pubkey, rent, 82, &token_program()),
                initialize_mint(&mint.pubkey, &payer.pubkey, 6),
            ],
        )
        .await;
    chain
        .transact(
            &payer,
            &[],
            &[create_ata(
                &payer.pubkey,
                &payer_ata,
                &payer.pubkey,
                &mint.pubkey,
            )],
        )
        .await;
    chain
        .transact(
            &payer,
            &[],
            &[mint_to(&mint.pubkey, &payer_ata, &payer.pubkey, supply)],
        )
        .await;
    chain
        .transact(
            &payer,
            &[],
            &[create_ata(
                &payer.pubkey,
                &payto_ata,
                &pay_to.pubkey,
                &mint.pubkey,
            )],
        )
        .await;
    (mint, payer_ata, payto_ata, payer, pay_to)
}

// ---------------------------------------------------------------------------
// The cases (issue #1152's required coverage).
// ---------------------------------------------------------------------------

/// SOL happy path: the paying transfer carrying the intent's `reference`
/// credits the opened `amount_minor` — `credited`, `entry_id` set, the wallet
/// holding exactly the opened figure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sol_happy_path_credits_the_opened_amount() {
    let Some((db, chain)) = world("solana_e2e_sol").await else {
        return;
    };
    let (org, wallet) = organization(&db, "sol").await;
    let pay_to = pay_to();
    let terms = sol_terms(&pay_to);
    // 0.02 SOL intent: 20_000_000 lamports at the 1e9 parity rate.
    let (deposit_id, reference, lamports) = open_sol(&db, org, &terms, 20_000_000).await;
    assert_eq!(lamports, 20_000_000);
    let payer = funded_payer(&chain, 30_000_000).await;
    chain
        .pay_sol(&payer, &pay_to, &reference, lamports as u64)
        .await;
    let verifier = verifier(
        &chain,
        &pay_to,
        &terms,
        vec![MintWhitelistEntry {
            key: "sol".into(),
            mint: None,
        }],
    );
    let credit = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    let SolanaCredit::Credited(receipt) = credit else {
        panic!("the SOL payment credits: {credit:?}");
    };
    assert!(receipt.is_new);
    assert_eq!(wallet.available().await.unwrap(), 20_000_000);
    assert_eq!(status(&db, deposit_id).await, "credited");
    let meta = meta(&db, deposit_id).await;
    assert_eq!(meta["delivered"].as_i64().unwrap(), 20_000_000);
    assert_eq!(meta["expected"].as_i64().unwrap(), 20_000_000);
}

/// SPL/USDC happy path: a test mint on the validator, a `transferChecked`
/// paying the deposit address's ATA and carrying the reference — `credited`,
/// the wallet holding the opened amount.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usdc_happy_path_credits_the_opened_amount() {
    let Some((db, chain)) = world("solana_e2e_usdc").await else {
        return;
    };
    let (org, wallet) = organization(&db, "usdc").await;
    let (mint, payer_ata, payto_ata, payer, pay_to) = spl_fixture(&chain, 100_000_000).await;
    let terms = spl_terms(&pay_to, &mint.pubkey);
    let deposit_id = Uuid::new_v4();
    let (reference, units, _expires) = db
        .open_solana_deposit(
            org,
            deposit_id,
            25_000_000,
            &terms,
            Some("usdc"),
            &rate(&terms, "usdc"),
        )
        .await
        .unwrap()
        .expect("a fresh intent inserts");
    assert_eq!(units, 25_000_000);
    let reference = Pubkey::from_base58(&reference);
    chain
        .transact(
            &payer,
            &[],
            &[transfer_checked(
                &payer_ata,
                &mint.pubkey,
                &payto_ata,
                &payer.pubkey,
                units as u64,
                6,
                Some(&reference),
            )],
        )
        .await;
    let verifier = verifier(
        &chain,
        &pay_to,
        &terms,
        vec![
            MintWhitelistEntry {
                key: "sol".into(),
                mint: None,
            },
            MintWhitelistEntry {
                key: "usdc".into(),
                mint: Some(mint.pubkey.to_base58()),
            },
        ],
    );
    let credit = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    let SolanaCredit::Credited(receipt) = credit else {
        panic!("the USDC payment credits: {credit:?}");
    };
    assert!(receipt.is_new);
    assert_eq!(wallet.available().await.unwrap(), 25_000_000);
    assert_eq!(status(&db, deposit_id).await, "credited");
}

/// Underpay: delivering under the asked amount marks `underpaid`, no credit
/// runs, the figures land on `meta` for the queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_underpaying_transfer_marks_underpaid() {
    let Some((db, chain)) = world("solana_e2e_under").await else {
        return;
    };
    let (org, wallet) = organization(&db, "under").await;
    let pay_to = pay_to();
    let terms = sol_terms(&pay_to);
    let (deposit_id, reference, lamports) = open_sol(&db, org, &terms, 20_000_000).await;
    let payer = funded_payer(&chain, 30_000_000).await;
    // Deliver 15_000_000 of the asked 20_000_000.
    chain.pay_sol(&payer, &pay_to, &reference, 15_000_000).await;
    let _ = lamports;
    let verifier = verifier(
        &chain,
        &pay_to,
        &terms,
        vec![MintWhitelistEntry {
            key: "sol".into(),
            mint: None,
        }],
    );
    let verdict = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    assert!(matches!(verdict, SolanaCredit::Underpaid), "{verdict:?}");
    assert_eq!(status(&db, deposit_id).await, solana_status::UNDERPAID);
    assert_eq!(wallet.available().await.unwrap(), 0);
    let meta = meta(&db, deposit_id).await;
    assert_eq!(meta["delivered"].as_i64().unwrap(), 15_000_000);
    assert_eq!(meta["expected"].as_i64().unwrap(), 20_000_000);
}

/// Overpay: delivering over the asked amount still credits the opened amount
/// — the row lands `overpaid` with `entry_id` proving the credit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_overpaying_transfer_credits_once_and_marks_overpaid() {
    let Some((db, chain)) = world("solana_e2e_over").await else {
        return;
    };
    let (org, wallet) = organization(&db, "over").await;
    let pay_to = pay_to();
    let terms = sol_terms(&pay_to);
    let (deposit_id, reference, _lamports) = open_sol(&db, org, &terms, 20_000_000).await;
    let payer = funded_payer(&chain, 40_000_000).await;
    // Deliver 30_000_000 against the asked 20_000_000.
    chain.pay_sol(&payer, &pay_to, &reference, 30_000_000).await;
    let verifier = verifier(
        &chain,
        &pay_to,
        &terms,
        vec![MintWhitelistEntry {
            key: "sol".into(),
            mint: None,
        }],
    );
    let credit = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    let SolanaCredit::Credited(receipt) = credit else {
        panic!("the over-delivery credits: {credit:?}");
    };
    assert!(receipt.is_new);
    // The opened amount credits — never the surplus.
    assert_eq!(wallet.available().await.unwrap(), 20_000_000);
    assert_eq!(status(&db, deposit_id).await, solana_status::OVERPAID);
    let meta = meta(&db, deposit_id).await;
    assert!(meta["overpaid"].as_bool().unwrap());
    assert_eq!(meta["delivered"].as_i64().unwrap(), 30_000_000);
}

/// Post-expiry arrival: the paying transfer's on-chain `blockTime` lands
/// at/after `expiresAt` → `late`. The intent opens with a 1-second TTL and
/// the payment is delayed past it — judged on the chain's own clock, not the
/// server's (issue #1128).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_post_expiry_payment_marks_late() {
    let Some((db, chain)) = world("solana_e2e_late").await else {
        return;
    };
    let (org, wallet) = organization(&db, "late").await;
    let pay_to = pay_to();
    let terms = SolanaTerms {
        intent_ttl_secs: 1,
        ..sol_terms(&pay_to)
    };
    let (deposit_id, reference, lamports) = open_sol(&db, org, &terms, 20_000_000).await;
    let payer = funded_payer(&chain, 30_000_000).await;
    // Sleep past the 1 s TTL so the transfer's blockTime is at/after expiry.
    tokio::time::sleep(Duration::from_secs(3)).await;
    chain
        .pay_sol(&payer, &pay_to, &reference, lamports as u64)
        .await;
    let verifier = verifier(
        &chain,
        &pay_to,
        &terms,
        vec![MintWhitelistEntry {
            key: "sol".into(),
            mint: None,
        }],
    );
    let verdict = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    assert!(matches!(verdict, SolanaCredit::Late), "{verdict:?}");
    assert_eq!(status(&db, deposit_id).await, solana_status::LATE);
    assert_eq!(wallet.available().await.unwrap(), 0);
}

/// Same-signature replay: confirming a second time resolves the standing
/// receipt — the ledger books exactly once (`solana:<signature>` idempotency
/// + `deposits_solana_tx_hash`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replayed_confirm_credits_exactly_once() {
    let Some((db, chain)) = world("solana_e2e_replay").await else {
        return;
    };
    let (org, wallet) = organization(&db, "replay").await;
    let pay_to = pay_to();
    let terms = sol_terms(&pay_to);
    let (deposit_id, reference, lamports) = open_sol(&db, org, &terms, 20_000_000).await;
    let payer = funded_payer(&chain, 30_000_000).await;
    chain
        .pay_sol(&payer, &pay_to, &reference, lamports as u64)
        .await;
    let verifier = verifier(
        &chain,
        &pay_to,
        &terms,
        vec![MintWhitelistEntry {
            key: "sol".into(),
            mint: None,
        }],
    );
    let first = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    let SolanaCredit::Credited(receipt) = first else {
        panic!("the first confirm credits: {first:?}");
    };
    assert!(receipt.is_new);
    // The replay sees the row non-pending and answers `Settled` — the
    // signature cannot re-credit.
    let second = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    assert!(matches!(second, SolanaCredit::Settled), "{second:?}");
    assert_eq!(wallet.available().await.unwrap(), 20_000_000);
}

/// Reference drowned by noise: several dishonoring transfers touching the
/// marker (paying elsewhere) land on `getSignaturesForAddress` beside the real
/// payment — `find_reference` still locates the honoring transfer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_payment_survives_noise_on_the_marker() {
    let Some((db, chain)) = world("solana_e2e_noise").await else {
        return;
    };
    let (org, wallet) = organization(&db, "noise").await;
    let pay_to = pay_to();
    let terms = sol_terms(&pay_to);
    let (deposit_id, reference, lamports) = open_sol(&db, org, &terms, 20_000_000).await;
    let payer = funded_payer(&chain, 60_000_000).await;
    // Noise before and around the real payment: transfers carrying the marker
    // but paying elsewhere (and one under-delivery to payTo — a noise shape
    // that dishonors on amount, so the honoring transfer must still win).
    chain.noise_on(&payer, &reference, 1).await;
    chain.noise_on(&payer, &reference, 2).await;
    chain
        .pay_sol(&payer, &pay_to, &reference, lamports as u64)
        .await;
    chain.noise_on(&payer, &reference, 3).await;
    chain.noise_on(&payer, &reference, 4).await;
    let verifier = verifier(
        &chain,
        &pay_to,
        &terms,
        vec![MintWhitelistEntry {
            key: "sol".into(),
            mint: None,
        }],
    );
    let credit = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    let SolanaCredit::Credited(receipt) = credit else {
        panic!("the honoring transfer is found through the noise: {credit:?}");
    };
    assert!(receipt.is_new);
    assert_eq!(wallet.available().await.unwrap(), 20_000_000);
    assert_eq!(status(&db, deposit_id).await, "credited");
}
