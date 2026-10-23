//! CCTP against Circle's real programs, shared by the CCTP tests and the fuzz: devnet
//! MessageTransmitterV2 and TokenMessengerMinterV2 (`fixtures/cctp_*.so`) with their state as dumped
//! 2026-10-22, and the pool on Circle's devnet USDC. The one change: Circle's two attester keys are
//! swapped for two test keys, so the tests can attest their own burns.
#![allow(dead_code)]

use super::*;
use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{InstructionData, ToAccountMetas};
use base64::Engine;
use k256::ecdsa::SigningKey;
use pool_core::cctp as c;
use safu_pool::constants::*;
use safu_pool::instructions::OwnerSignature;
use safu_pool::state::CctpAccount;
use sha3::{Digest, Keccak256};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_signer::Signer;

pub const MT: Pubkey = safu_pool::constants::CCTP_MESSAGE_TRANSMITTER_ID;
pub const TMM: Pubkey = safu_pool::constants::CCTP_TOKEN_MESSENGER_MINTER_ID;
pub const CIRCLE_USDC: Pubkey =
    anchor_lang::prelude::pubkey!("4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU");
/// Sepolia USDC (the Ethereum token pair) and Stellar testnet USDC (the Stellar one).
pub const SEPOLIA_USDC: &str = "1c7D4B196Cb0C7B01d743Fbc6116a902379C7238";
pub const STELLAR_USDC: &str = "CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA";
/// `MessageTransmitter`: discriminator, 4 keys, paused, local domain, version, threshold, then the
/// attesters vector (u32 length, 32-byte entries holding a left-padded Ethereum address).
pub const ATTESTERS_AT: usize = 8 + 4 * 32 + 1 + 4 + 4 + 4 + 4;
/// `RemoteTokenMessenger`: discriminator, domain, then the remote messenger's address.
pub const REMOTE_MESSENGER_AT: usize = 8 + 4;
/// `finality_threshold_executed` for a finalized burn.
pub const FINALIZED: u32 = 2000;

pub fn fixture(name: &str) -> Account {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let j: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let a = &j["account"];
    Account {
        lamports: a["lamports"].as_u64().unwrap(),
        data: base64::engine::general_purpose::STANDARD
            .decode(a["data"][0].as_str().unwrap())
            .unwrap(),
        owner: a["owner"].as_str().unwrap().parse().unwrap(),
        executable: false,
        rent_epoch: 0,
    }
}

pub fn mt_pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &MT).0
}
pub fn tmm_pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &TMM).0
}

pub fn padded(hex: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..20 {
        out[12 + i] = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

pub fn burn_token(domain: u32) -> [u8; 32] {
    if domain == c::domain::ETHEREUM {
        padded(SEPOLIA_USDC)
    } else {
        strkey_payload(STELLAR_USDC)
    }
}

pub fn keccak(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Keccak256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// A fresh secp256k1 key (entropy from a new Solana keypair's secret).
pub fn random_eth_key() -> SigningKey {
    SigningKey::from_slice(&Keypair::new().to_bytes()[..32]).unwrap()
}

pub fn eth_address(key: &SigningKey) -> [u8; 20] {
    let point = key.verifying_key().to_encoded_point(false);
    keccak(&[&point.as_bytes()[1..]])[12..].try_into().unwrap()
}

/// r || s || v (27/28) over a 32-byte hash.
pub fn eth_sign(key: &SigningKey, hash: &[u8; 32]) -> [u8; 65] {
    let (sig, rec) = key.sign_prehash_recoverable(hash).unwrap();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = rec.to_byte() + 27;
    out
}

/// A home-chain owner: an Ethereum key or a Stellar (Ed25519) key.
pub enum HomeKey {
    Eth(SigningKey),
    Stellar(Keypair),
}

impl HomeKey {
    pub fn eth() -> HomeKey {
        HomeKey::Eth(random_eth_key())
    }
    pub fn stellar() -> HomeKey {
        HomeKey::Stellar(Keypair::new())
    }
    pub fn domain(&self) -> u32 {
        match self {
            HomeKey::Eth(_) => c::domain::ETHEREUM,
            HomeKey::Stellar(_) => c::domain::STELLAR,
        }
    }
    pub fn bytes32(&self) -> [u8; 32] {
        match self {
            HomeKey::Eth(k) => {
                let mut out = [0u8; 32];
                out[12..].copy_from_slice(&eth_address(k));
                out
            }
            HomeKey::Stellar(k) => k.pubkey().to_bytes(),
        }
    }
}

pub struct Cctp {
    pub env: Env,
    pub attesters: Vec<SigningKey>,
    pub relayer: Keypair,
    pub nonces: u64,
    /// Sign for this cluster instead of the pool's (a signature made for another network).
    pub sign_cluster: Option<u8>,
}

/// Instruction accounts as the program sees them: a key is a signer if any instruction signs it or
/// it pays the fee, writable if any instruction writes it.
pub fn seen(tx: &[Instruction], payer: &Pubkey, metas: &[AccountMeta]) -> Vec<AccountMeta> {
    metas
        .iter()
        .map(|m| {
            let all = tx
                .iter()
                .flat_map(|i| i.accounts.iter())
                .filter(|a| a.pubkey == m.pubkey);
            let (s, w) = all.fold((m.pubkey == *payer, m.pubkey == *payer), |(s, w), a| {
                (s || a.is_signer, w || a.is_writable)
            });
            AccountMeta {
                pubkey: m.pubkey,
                is_signer: s,
                is_writable: w,
            }
        })
        .collect()
}

/// `instructions::cctp::call_hash` over metas.
pub fn call_hash(data: &[u8], metas: &[AccountMeta]) -> [u8; 32] {
    let mut parts: Vec<Vec<u8>> = vec![
        (data.len() as u32).to_le_bytes().to_vec(),
        data.to_vec(),
        vec![metas.len() as u8],
    ];
    for m in metas {
        let mut p = m.pubkey.to_bytes().to_vec();
        p.push(u8::from(m.is_writable));
        p.push(u8::from(m.is_signer));
        parts.push(p);
    }
    let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    solana_sha256_hasher::hashv(&refs).to_bytes()
}

impl Cctp {
    pub fn new() -> Cctp {
        Cctp::install(Env::new_at(CIRCLE_USDC, START))
    }

    /// Circle's programs and state into `env` (a pool on Circle's devnet USDC), test attesters, and
    /// both inboxes open.
    pub fn install(mut env: Env) -> Cctp {
        env.svm
            .add_program(
                MT,
                include_bytes!("../fixtures/cctp_message_transmitter.so"),
            )
            .unwrap();
        env.svm
            .add_program(
                TMM,
                include_bytes!("../fixtures/cctp_token_messenger_minter.so"),
            )
            .unwrap();
        let fee_recipient = {
            let tm = fixture("cctp_token_messenger");
            Pubkey::try_from(&tm.data[109..141]).unwrap()
        };
        for (key, name) in [
            (
                mt_pda(&[b"message_transmitter"]),
                "cctp_message_transmitter",
            ),
            (tmm_pda(&[b"token_messenger"]), "cctp_token_messenger"),
            (tmm_pda(&[b"token_minter"]), "cctp_token_minter"),
            (
                tmm_pda(&[b"local_token", CIRCLE_USDC.as_ref()]),
                "cctp_local_token",
            ),
            (
                tmm_pda(&[b"remote_token_messenger", b"0"]),
                "cctp_remote_messenger_0",
            ),
            (
                tmm_pda(&[b"remote_token_messenger", b"27"]),
                "cctp_remote_messenger_27",
            ),
            (tmm_pda(&[b"custody", CIRCLE_USDC.as_ref()]), "cctp_custody"),
            (
                tmm_pda(&[b"token_pair", b"0", &burn_token(c::domain::ETHEREUM)]),
                "cctp_token_pair_0",
            ),
            (
                tmm_pda(&[b"token_pair", b"27", &burn_token(c::domain::STELLAR)]),
                "cctp_token_pair_27",
            ),
            (CIRCLE_USDC, "cctp_usdc_mint"),
            (
                Pubkey::find_program_address(
                    &[fee_recipient.as_ref(), TOKEN.as_ref(), CIRCLE_USDC.as_ref()],
                    &ATA_PROGRAM,
                )
                .0,
                "cctp_fee_recipient_ata",
            ),
        ] {
            env.svm.set_account(key, fixture(name)).unwrap();
        }
        // Two test attesters in place of Circle's, in increasing address order as Circle requires.
        let mut attesters: Vec<SigningKey> = (0..2).map(|_| random_eth_key()).collect();
        attesters.sort_by_key(eth_address);
        let state = mt_pda(&[b"message_transmitter"]);
        let mut a = env.svm.get_account(&state).unwrap();
        assert_eq!(&a.data[ATTESTERS_AT - 4..ATTESTERS_AT], &2u32.to_le_bytes());
        for (i, k) in attesters.iter().enumerate() {
            let at = ATTESTERS_AT + 32 * i;
            a.data[at..at + 12].fill(0);
            a.data[at + 12..at + 32].copy_from_slice(&eth_address(k));
        }
        env.svm.set_account(state, a).unwrap();

        let relayer = Keypair::new();
        env.svm.airdrop(&relayer.pubkey(), 100 * SOL).unwrap();
        let mut t = Cctp {
            env,
            attesters,
            relayer,
            nonces: 0,
            sign_cluster: None,
        };
        for mode in [c::MODE_STAKE, c::MODE_BACK] {
            let ix = t.open_inbox_ix(mode);
            let r = t.relayer.insecure_clone();
            t.env.ok(&[ix], &[&r]);
        }
        t
    }

    pub fn pool(&self) -> Pubkey {
        self.env.pool()
    }
    pub fn authority(&self) -> Pubkey {
        pda(&[SEED_CCTP_AUTHORITY, self.pool().as_ref()])
    }
    pub fn inbox(&self, mode: u8) -> Pubkey {
        pda(&[SEED_CCTP_INBOX, self.pool().as_ref(), &[mode]])
    }
    pub fn identity(&self, mode: u8, owner: &HomeKey) -> Pubkey {
        pda(&[
            SEED_CCTP_IDENTITY,
            self.pool().as_ref(),
            &[mode],
            &owner.domain().to_be_bytes(),
            &owner.bytes32(),
        ])
    }
    pub fn account(&self, identity: &Pubkey) -> Pubkey {
        pda(&[SEED_CCTP_ACCOUNT, identity.as_ref()])
    }
    pub fn held(&self, identity: &Pubkey) -> Pubkey {
        pda(&[SEED_CCTP_HELD, identity.as_ref()])
    }
    pub fn state(&self, identity: &Pubkey) -> CctpAccount {
        self.env.read(&self.account(identity))
    }

    pub fn open_inbox_ix(&self, mode: u8) -> Instruction {
        self.env.ix(
            safu_pool::accounts::CctpOpenInbox {
                payer: self.relayer.pubkey(),
                pool: self.pool(),
                authority: self.authority(),
                inbox: self.inbox(mode),
                usdc_mint: CIRCLE_USDC,
                token_program: TOKEN,
                system_program: SYSTEM,
            },
            safu_pool::instruction::CctpOpenInbox { mode },
        )
    }

    /// A finalized v2 burn from `owner`'s chain to this pool's `mode` inbox.
    pub fn burn(&mut self, owner: &HomeKey, mode: u8, amount: u64) -> Vec<u8> {
        let (caller, recipient) = (self.authority().to_bytes(), self.inbox(mode).to_bytes());
        self.burn_to(owner, owner.domain(), amount, caller, recipient)
    }

    pub fn burn_to(
        &mut self,
        owner: &HomeKey,
        source: u32,
        amount: u64,
        destination_caller: [u8; 32],
        mint_recipient: [u8; 32],
    ) -> Vec<u8> {
        self.nonces += 1;
        let mut nonce = [0u8; 32];
        nonce[24..].copy_from_slice(&self.nonces.to_be_bytes());
        // Circle's messenger on the source chain (none for Solana, which is not a home chain).
        let sender: [u8; 32] = if source == c::domain::SOLANA {
            [0; 32]
        } else {
            let remote = fixture(&format!("cctp_remote_messenger_{source}"));
            read_32(&remote.data, REMOTE_MESSENGER_AT)
        };
        let mut m = Vec::new();
        m.extend_from_slice(&c::MESSAGE_VERSION.to_be_bytes());
        m.extend_from_slice(&source.to_be_bytes());
        m.extend_from_slice(&c::domain::SOLANA.to_be_bytes());
        m.extend_from_slice(&nonce);
        m.extend_from_slice(&sender);
        m.extend_from_slice(TMM.as_ref());
        m.extend_from_slice(&destination_caller);
        m.extend_from_slice(&FINALIZED.to_be_bytes());
        m.extend_from_slice(&FINALIZED.to_be_bytes());
        assert_eq!(m.len(), c::header::BODY);
        let mut amount32 = [0u8; 32];
        amount32[24..].copy_from_slice(&amount.to_be_bytes());
        m.extend_from_slice(&c::BURN_MESSAGE_VERSION.to_be_bytes());
        m.extend_from_slice(&burn_token(source));
        m.extend_from_slice(&mint_recipient);
        m.extend_from_slice(&amount32);
        m.extend_from_slice(&owner.bytes32());
        m.extend_from_slice(&[0u8; 32]); // max fee
        m.extend_from_slice(&[0u8; 32]); // fee executed
        m.extend_from_slice(&[0u8; 32]); // expiration block
        m
    }

    pub fn attest(&self, message: &[u8]) -> Vec<u8> {
        let hash = keccak(&[message]);
        self.attesters
            .iter()
            .flat_map(|k| eth_sign(k, &hash))
            .collect()
    }

    pub fn receive_ix(&self, mode: u8, owner: &HomeKey, message: Vec<u8>) -> Instruction {
        let burn = c::parse_inbound(&message).unwrap();
        let identity = self.identity(mode, owner);
        let fee_recipient =
            Pubkey::try_from(&fixture("cctp_token_messenger").data[109..141]).unwrap();
        let attestation = self.attest(&message);
        let source = burn.source_domain.to_string();
        self.env.ix(
            safu_pool::accounts::CctpReceive {
                payer: self.relayer.pubkey(),
                pool: self.pool(),
                authority: self.authority(),
                inbox: self.inbox(mode),
                identity,
                account: self.account(&identity),
                held: self.held(&identity),
                usdc_mint: CIRCLE_USDC,
                token_program: TOKEN,
                system_program: SYSTEM,
                circle: safu_pool::accounts::CircleReceive {
                    message_transmitter_program: MT,
                    token_messenger_minter_program: TMM,
                    message_transmitter_authority: mt_pda(&[
                        b"message_transmitter_authority",
                        TMM.as_ref(),
                    ]),
                    message_transmitter: mt_pda(&[b"message_transmitter"]),
                    used_nonce: mt_pda(&[b"used_nonce", &burn.nonce]),
                    message_transmitter_event_authority: mt_pda(&[b"__event_authority"]),
                    token_messenger: tmm_pda(&[b"token_messenger"]),
                    remote_token_messenger: tmm_pda(&[
                        b"remote_token_messenger",
                        source.as_bytes(),
                    ]),
                    token_minter: tmm_pda(&[b"token_minter"]),
                    local_token: tmm_pda(&[b"local_token", CIRCLE_USDC.as_ref()]),
                    token_pair: tmm_pda(&[b"token_pair", source.as_bytes(), &burn.burn_token]),
                    fee_recipient_token_account: Pubkey::find_program_address(
                        &[fee_recipient.as_ref(), TOKEN.as_ref(), CIRCLE_USDC.as_ref()],
                        &ATA_PROGRAM,
                    )
                    .0,
                    custody: tmm_pda(&[b"custody", CIRCLE_USDC.as_ref()]),
                    token_messenger_event_authority: tmm_pda(&[b"__event_authority"]),
                },
            },
            safu_pool::instruction::CctpReceive {
                mode,
                home_domain: owner.domain(),
                home_owner: owner.bytes32(),
                message,
                attestation,
            },
        )
    }

    pub fn receive(&mut self, mode: u8, owner: &HomeKey, amount: u64) -> Pubkey {
        let m = self.burn(owner, mode, amount);
        let ix = self.receive_ix(mode, owner, m);
        let r = self.relayer.insecure_clone();
        self.env.ok(&[ix], &[&r]);
        self.identity(mode, owner)
    }

    pub fn call_accounts(&self, identity: &Pubkey) -> Vec<AccountMeta> {
        safu_pool::accounts::CctpCall {
            account: self.account(identity),
            identity: *identity,
            pool: self.pool(),
            instructions: IX_SYSVAR,
            program: safu_pool::ID,
        }
        .to_account_metas(None)
    }

    /// A pool call's accounts with the identity in them as the program will sign for it: not a
    /// transaction signer.
    pub fn as_identity(metas: Vec<AccountMeta>, identity: &Pubkey) -> Vec<AccountMeta> {
        metas
            .into_iter()
            .map(|mut m| {
                if m.pubkey == *identity {
                    m.is_signer = false;
                }
                m
            })
            .collect()
    }

    pub fn stake_metas(&self, identity: &Pubkey) -> Vec<AccountMeta> {
        let accounts = safu_pool::accounts::Stake {
            payer: self.relayer.pubkey(),
            staker: *identity,
            pool: self.pool(),
            vault: self.env.vault(),
            stake_record: self.env.stake_record(identity),
            staker_usdc: self.held(identity),
            backer_record: None,
            system_program: SYSTEM,
            cash: self.env.cash(),
        };
        Self::as_identity(accounts.to_account_metas(None), identity)
    }

    pub fn back_metas(&self, identity: &Pubkey) -> Vec<AccountMeta> {
        let accounts = safu_pool::accounts::Back {
            payer: self.relayer.pubkey(),
            backer: *identity,
            pool: self.pool(),
            vault: self.env.vault(),
            backer_record: self.env.backer_record(identity),
            backer_usdc: self.held(identity),
            system_program: SYSTEM,
            cash: self.env.cash(),
        };
        Self::as_identity(accounts.to_account_metas(None), identity)
    }

    pub fn withdraw_metas(&self, identity: &Pubkey) -> Vec<AccountMeta> {
        let accounts = safu_pool::accounts::Withdraw {
            staker: *identity,
            pool: self.pool(),
            vault: self.env.vault(),
            stake_record: self.env.stake_record(identity),
            beneficiary_usdc: self.held(identity),
            cash: self.env.cash(),
        };
        Self::as_identity(accounts.to_account_metas(None), identity)
    }

    pub fn deploy_ix(&self, identity: &Pubkey, mode: u8) -> Instruction {
        let mut accounts = self.call_accounts(identity);
        accounts.extend(if mode == c::MODE_STAKE {
            self.stake_metas(identity)
        } else {
            self.back_metas(identity)
        });
        Instruction {
            program_id: safu_pool::ID,
            accounts,
            data: safu_pool::instruction::CctpDeploy {}.data(),
        }
    }

    pub fn deploy(&mut self, identity: &Pubkey, mode: u8) -> Result<(), String> {
        let ix = self.deploy_ix(identity, mode);
        let r = self.relayer.insecure_clone();
        self.env.send(&[ix], &[&r])
    }

    /// The owner's signed `cctp_act` for `data` over `inner`, with the precompile instruction first
    /// for a Stellar owner. `tamper` edits the instruction after signing.
    pub fn act_tx(
        &self,
        owner: &HomeKey,
        identity: &Pubkey,
        inner: Vec<AccountMeta>,
        data: Vec<u8>,
        nonce: u64,
        expires_at: i64,
    ) -> Vec<Instruction> {
        let mut accounts = self.call_accounts(identity);
        let signed_accounts = inner.len() as u8;
        accounts.extend(inner.iter().cloned());
        let mut act = Instruction {
            program_id: safu_pool::ID,
            accounts,
            data: vec![],
        };
        let tx_view = [act.clone()];
        let signed = seen(&tx_view, &self.relayer.pubkey(), &inner);
        let hash = call_hash(&data, &signed);
        let (signature, pre) = self.sign(owner, identity, nonce, expires_at, &hash);
        act.data = safu_pool::instruction::CctpAct {
            nonce,
            expires_at,
            signature,
            signed_accounts,
            data,
        }
        .data();
        pre.into_iter().chain([act]).collect()
    }

    /// The owner's signature over an action, and the precompile instruction a Stellar one needs.
    pub fn sign(
        &self,
        owner: &HomeKey,
        identity: &Pubkey,
        nonce: u64,
        expires_at: i64,
        call_hash: &[u8; 32],
    ) -> (OwnerSignature, Option<Instruction>) {
        let preimage = c::action_preimage(
            &safu_pool::ID.to_bytes(),
            self.sign_cluster.unwrap_or(self.env.pool_state().cluster),
            &self.account(identity).to_bytes(),
            nonce,
            expires_at,
            call_hash,
        );
        let digest = solana_sha256_hasher::hash(&preimage).to_bytes();
        match owner {
            HomeKey::Eth(k) => {
                let hash = keccak(&[c::EIP191_PREFIX_32, &digest]);
                (OwnerSignature::Evm(eth_sign(k, &hash)), None)
            }
            HomeKey::Stellar(k) => {
                let text: String = digest.iter().map(|b| format!("{b:02x}")).collect();
                let message =
                    solana_sha256_hasher::hashv(&[c::SEP53_PREFIX, text.as_bytes()]).to_bytes();
                (
                    OwnerSignature::Stellar,
                    Some(ed25519_ix(k, &message, [0, 0, 0])),
                )
            }
        }
    }

    pub fn act(
        &mut self,
        owner: &HomeKey,
        identity: &Pubkey,
        inner: Vec<AccountMeta>,
        data: Vec<u8>,
    ) -> Result<(), String> {
        let nonce = self.state(identity).nonce;
        let tx = self.act_tx(owner, identity, inner, data, nonce, self.env.now + 60);
        let r = self.relayer.insecure_clone();
        self.env.send(&tx, &[&r])
    }

    pub fn send_home_tx(
        &self,
        owner: &HomeKey,
        identity: &Pubkey,
        event: &Keypair,
        nonce: u64,
    ) -> Vec<Instruction> {
        self.send_home_from_tx(owner, identity, event, nonce, self.held(identity))
    }

    /// Sends home from `from`, any USDC account the identity owns.
    pub fn send_home_from_tx(
        &self,
        owner: &HomeKey,
        identity: &Pubkey,
        event: &Keypair,
        nonce: u64,
        from: Pubkey,
    ) -> Vec<Instruction> {
        let hash = solana_sha256_hasher::hashv(&[
            <safu_pool::instruction::CctpSendHome as anchor_lang::Discriminator>::DISCRIMINATOR,
            from.as_ref(),
        ])
        .to_bytes();
        let (signature, pre) = self.sign(owner, identity, nonce, self.env.now + 60, &hash);
        let home = owner.domain().to_string();
        let ix = self.env.ix(
            safu_pool::accounts::CctpSendHome {
                payer: self.relayer.pubkey(),
                pool: self.pool(),
                account: self.account(identity),
                identity: *identity,
                from,
                usdc_mint: CIRCLE_USDC,
                message_sent_event_data: event.pubkey(),
                instructions: IX_SYSVAR,
                token_program: TOKEN,
                system_program: SYSTEM,
                circle: safu_pool::accounts::CircleBurn {
                    message_transmitter_program: MT,
                    token_messenger_minter_program: TMM,
                    sender_authority: tmm_pda(&[b"sender_authority"]),
                    denylist_account: tmm_pda(&[b"denylist_account", identity.as_ref()]),
                    message_transmitter: mt_pda(&[b"message_transmitter"]),
                    token_messenger: tmm_pda(&[b"token_messenger"]),
                    remote_token_messenger: tmm_pda(&[b"remote_token_messenger", home.as_bytes()]),
                    token_minter: tmm_pda(&[b"token_minter"]),
                    local_token: tmm_pda(&[b"local_token", CIRCLE_USDC.as_ref()]),
                    token_messenger_event_authority: tmm_pda(&[b"__event_authority"]),
                },
            },
            safu_pool::instruction::CctpSendHome {
                nonce,
                expires_at: self.env.now + 60,
                signature,
            },
        );
        pre.into_iter().chain([ix]).collect()
    }

    /// Sends home and returns Circle's outbound message (from its message account).
    pub fn send_home(&mut self, owner: &HomeKey, identity: &Pubkey) -> Result<Vec<u8>, String> {
        let event = Keypair::new();
        let nonce = self.state(identity).nonce;
        let tx = self.send_home_tx(owner, identity, &event, nonce);
        let r = self.relayer.insecure_clone();
        self.env.send(&tx, &[&r, &event])?;
        let data = self.env.svm.get_account(&event.pubkey()).unwrap().data;
        // The message starts at its version and source domain (Solana).
        let mut head = c::MESSAGE_VERSION.to_be_bytes().to_vec();
        head.extend_from_slice(&c::domain::SOLANA.to_be_bytes());
        let at = data
            .windows(8)
            .position(|w| w == head.as_slice())
            .expect("message in event");
        Ok(data[at..].to_vec())
    }
}

pub fn read_32(m: &[u8], at: usize) -> [u8; 32] {
    m[at..at + 32].try_into().unwrap()
}
