// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Signal-style Double Ratchet with X3DH session setup for offline-first mesh E2E.
//!
//! - **v0 (legacy):** static ephemeral ECDH per message ([`super::encrypt_for`]).
//! - **v1 (ratchet):** X3DH + symmetric/DH ratcheting; wire prefix [`RATCHET_WIRE_MAGIC`].

use super::EncryptionKeypair;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::{HashMap, VecDeque};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Matches [`crate::data::Envelope::e2e_version`] and `WireMessage::EncryptedDirect::e2e_version`.
pub const E2E_VERSION_LEGACY: u8 = 0;
pub const E2E_VERSION_RATCHET: u8 = 1;

/// Max skipped message keys retained per session (offline / out-of-order delivery).
pub const MAX_SKIP_KEYS: usize = 512;

pub const RATCHET_WIRE_MAGIC: &[u8] = b"MYRT2\0";

const ROOT_INFO: &[u8] = b"mycena-ratchet/root/v1";
const CHAIN_INFO: &[u8] = b"mycena-ratchet/chain/v1";
const X3DH_INFO: &[u8] = b"mycena-ratchet/x3dh/v1";
const PREKEY_SIG_INFO: &[u8] = b"mycena-ratchet/spk-sig/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RatchetFrameKind {
    Init = 1,
    Message = 2,
}

/// Public prekey bundle (X3DH) advertised via PeerInfo / contact hello.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrekeyBundle {
    pub registration_id: u32,
    pub identity_key: [u8; 32],
    pub signed_prekey_id: u32,
    pub signed_prekey: [u8; 32],
    /// 32-byte MAC over `signed_prekey` keyed by identity secret.
    pub signed_prekey_sig: [u8; 32],
    pub one_time_prekey_id: Option<u32>,
    pub one_time_prekey: Option<[u8; 32]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct RatchetCiphertext {
    pub dh: Option<[u8; 32]>,
    pub pn: u32,
    pub n: u32,
    pub nonce: [u8; 12],
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatchetInitPayload {
    pub registration_id: u32,
    pub identity_key: [u8; 32],
    pub ephemeral_key: [u8; 32],
    pub signed_prekey_id: u32,
    pub one_time_prekey_id: Option<u32>,
    pub first_message: RatchetCiphertext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RatchetFrame {
    Init(RatchetInitPayload),
    Message(RatchetCiphertext),
}

#[derive(Debug, Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct SessionSecrets {
    root_key: [u8; 32],
    send_chain_key: Option<[u8; 32]>,
    recv_chain_key: Option<[u8; 32]>,
    send_message_number: u32,
    recv_message_number: u32,
    previous_send_chain_length: u32,
    #[serde(with = "optional_secret")]
    dh_self_secret: Option<[u8; 32]>,
    dh_remote_public: Option<[u8; 32]>,
}

mod optional_secret {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<[u8; 32]>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(b) => b.as_slice().serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<[u8; 32]>, D::Error> {
        let opt: Option<Vec<u8>> = Option::deserialize(d)?;
        Ok(match opt {
            Some(v) if v.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&v);
                Some(arr)
            }
            Some(_) => None,
            None => None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkippedKeyEntry {
    message_key: [u8; 32],
}

/// Bounded store for out-of-order message keys (offline-first).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkippedMessageKeys {
    max: usize,
    order: VecDeque<(Vec<u8>, u32)>,
    keys: HashMap<(Vec<u8>, u32), SkippedKeyEntry>,
}

impl SkippedMessageKeys {
    pub fn new(max: usize) -> Self {
        Self {
            max,
            order: VecDeque::new(),
            keys: HashMap::new(),
        }
    }

    fn insert(&mut self, ratchet_key: [u8; 32], n: u32, mk: [u8; 32]) -> anyhow::Result<()> {
        let key = (ratchet_key.to_vec(), n);
        if self.keys.contains_key(&key) {
            return Ok(());
        }
        while self.keys.len() >= self.max {
            if let Some(old) = self.order.pop_front() {
                self.keys.remove(&old);
            } else {
                break;
            }
        }
        if self.keys.len() >= self.max {
            anyhow::bail!("skipped message key cache full");
        }
        self.order.push_back(key.clone());
        self.keys.insert(key, SkippedKeyEntry { message_key: mk });
        Ok(())
    }

    fn take(&mut self, ratchet_key: &[u8; 32], n: u32) -> Option<[u8; 32]> {
        let key = (ratchet_key.to_vec(), n);
        self.keys.remove(&key).map(|e| {
            self.order.retain(|k| k != &key);
            e.message_key
        })
    }
}

/// Double-ratchet session state for one peer (persisted between restarts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatchetSession {
    peer_id: String,
    secrets: SessionSecrets,
    skipped: SkippedMessageKeys,
    is_initiator: bool,
    registration_id: u32,
    remote_identity: [u8; 32],
}

/// Local signed + one-time prekeys for X3DH responders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalPrekeyStore {
    pub registration_id: u32,
    pub signed_prekey_id: u32,
    pub signed_prekey_secret: [u8; 32],
    pub signed_prekey_public: [u8; 32],
    pub signed_prekey_sig: [u8; 32],
    one_time: HashMap<u32, [u8; 32]>,
    next_one_time_id: u32,
}

impl LocalPrekeyStore {
    pub fn generate(identity: &EncryptionKeypair, one_time_count: u32) -> Self {
        let registration_id = rand::random::<u32>();
        let signed_prekey_secret = StaticSecret::random();
        let signed_prekey_public = *PublicKey::from(&signed_prekey_secret).as_bytes();
        let signed_prekey_sig =
            sign_prekey_public(identity.public.as_bytes(), &signed_prekey_public);
        let mut one_time = HashMap::new();
        let mut next_one_time_id = 1u32;
        for _ in 0..one_time_count {
            let sk = StaticSecret::random();
            one_time.insert(next_one_time_id, sk.to_bytes());
            next_one_time_id += 1;
        }
        Self {
            registration_id,
            signed_prekey_id: 1,
            signed_prekey_secret: signed_prekey_secret.to_bytes(),
            signed_prekey_public,
            signed_prekey_sig,
            one_time,
            next_one_time_id,
        }
    }

    pub fn public_bundle(&self, identity: &EncryptionKeypair) -> PrekeyBundle {
        let (opk_id, opk_pub) = self
            .one_time
            .iter()
            .next()
            .map(|(id, sk)| {
                let pk = PublicKey::from(&StaticSecret::from(*sk));
                (Some(*id), Some(*pk.as_bytes()))
            })
            .unwrap_or((None, None));
        PrekeyBundle {
            registration_id: self.registration_id,
            identity_key: *identity.public.as_bytes(),
            signed_prekey_id: self.signed_prekey_id,
            signed_prekey: self.signed_prekey_public,
            signed_prekey_sig: self.signed_prekey_sig,
            one_time_prekey_id: opk_id,
            one_time_prekey: opk_pub,
        }
    }

    pub fn take_one_time_secret(&mut self, id: u32) -> Option<[u8; 32]> {
        self.one_time.remove(&id)
    }
}

pub fn sign_prekey_public(identity_public: &[u8; 32], signed_prekey_public: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PREKEY_SIG_INFO);
    hasher.update(identity_public);
    hasher.update(signed_prekey_public);
    *hasher.finalize().as_bytes()
}

pub fn sign_prekey(identity: &EncryptionKeypair, signed_prekey_public: &[u8; 32]) -> [u8; 32] {
    sign_prekey_public(identity.public.as_bytes(), signed_prekey_public)
}

pub fn verify_prekey_bundle(bundle: &PrekeyBundle) -> bool {
    sign_prekey_public(&bundle.identity_key, &bundle.signed_prekey) == bundle.signed_prekey_sig
}

pub fn is_ratchet_wire(payload: &[u8]) -> bool {
    payload.starts_with(RATCHET_WIRE_MAGIC)
}

pub fn is_ratchet_init_frame(payload: &[u8]) -> bool {
    decode_frame(payload)
        .ok()
        .is_some_and(|f| matches!(f, RatchetFrame::Init(_)))
}

pub fn parse_hex_32(hex_str: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = hex::decode(hex_str.trim())?;
    if bytes.len() != 32 {
        anyhow::bail!("expected 32-byte hex key, got {} bytes", bytes.len());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Build a prekey bundle from optional PeerInfo ratchet fields.
pub fn prekey_bundle_from_peer_info(
    registration_id: u32,
    identity_key: [u8; 32],
    signed_prekey_id: u32,
    signed_prekey_hex: &str,
    signed_prekey_sig_hex: &str,
    one_time_prekey_id: Option<u32>,
    one_time_prekey_hex: Option<&str>,
) -> anyhow::Result<PrekeyBundle> {
    let signed_prekey = parse_hex_32(signed_prekey_hex)?;
    let signed_prekey_sig = parse_hex_32(signed_prekey_sig_hex)?;
    let one_time_prekey = match (one_time_prekey_id, one_time_prekey_hex) {
        (Some(_), Some(h)) => Some(parse_hex_32(h)?),
        _ => None,
    };
    Ok(PrekeyBundle {
        registration_id,
        identity_key,
        signed_prekey_id,
        signed_prekey,
        signed_prekey_sig,
        one_time_prekey_id,
        one_time_prekey,
    })
}

pub fn wire_e2e_version(payload: &[u8], explicit: u8) -> u8 {
    if is_ratchet_wire(payload) {
        E2E_VERSION_RATCHET
    } else {
        explicit
    }
}

fn encode_frame(frame: &RatchetFrame) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(RATCHET_WIRE_MAGIC.len() + 64);
    out.extend_from_slice(RATCHET_WIRE_MAGIC);
    out.extend_from_slice(&bincode::serialize(frame)?);
    Ok(out)
}

fn decode_frame(bytes: &[u8]) -> anyhow::Result<RatchetFrame> {
    if !bytes.starts_with(RATCHET_WIRE_MAGIC) {
        anyhow::bail!("not a ratchet frame");
    }
    Ok(bincode::deserialize(&bytes[RATCHET_WIRE_MAGIC.len()..])?)
}

fn kdf_root(root_key: &[u8; 32], dh: &[u8]) -> ([u8; 32], [u8; 32]) {
    let mut ikm = Vec::with_capacity(32 + dh.len());
    ikm.extend_from_slice(root_key);
    ikm.extend_from_slice(dh);
    let okm = hkdf_64(&ikm, ROOT_INFO);
    split_hkdf_halves(&okm)
}

fn kdf_chain(chain_key: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let okm = hkdf_64(chain_key, CHAIN_INFO);
    split_hkdf_halves(&okm)
}

#[allow(clippy::expect_used)] // INVARIANT: hkdf_64 always yields 64 bytes; halves are [0..32] and [32..64].
fn split_hkdf_halves(okm: &[u8; 64]) -> ([u8; 32], [u8; 32]) {
    (
        // INVARIANT: hkdf_64 output is [u8; 64]; [0..32] always converts to [u8; 32].
        okm[0..32].try_into().expect("32"),
        // INVARIANT: hkdf_64 output is [u8; 64]; [32..64] always converts to [u8; 32].
        okm[32..64].try_into().expect("32"),
    )
}

#[allow(clippy::expect_used)] // INVARIANT: HKDF expand length is fixed at 64 bytes.
fn hkdf_64(ikm: &[u8], info: &[u8]) -> [u8; 64] {
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut okm = [0u8; 64];
    // INVARIANT: HKDF expand length is fixed at 64 bytes for chain/root KDF.
    hk.expand(info, &mut okm).expect("HKDF expand 64 bytes");
    okm
}

fn x3dh_shared_secret(
    identity: &EncryptionKeypair,
    ek_secret: &StaticSecret,
    bundle: &PrekeyBundle,
) -> anyhow::Result<[u8; 32]> {
    let spk = PublicKey::from(bundle.signed_prekey);
    let ik_b = PublicKey::from(bundle.identity_key);
    let mut parts = Vec::new();
    parts.extend_from_slice(identity.diffie_hellman(&spk).as_bytes());
    parts.extend_from_slice(ek_secret.diffie_hellman(&ik_b).as_bytes());
    parts.extend_from_slice(ek_secret.diffie_hellman(&spk).as_bytes());
    if let (Some(opk_id), Some(opk)) = (bundle.one_time_prekey_id, bundle.one_time_prekey) {
        let _ = opk_id;
        parts.extend_from_slice(ek_secret.diffie_hellman(&PublicKey::from(opk)).as_bytes());
    }
    let hk = Hkdf::<Sha256>::new(None, &parts);
    let mut out = [0u8; 32];
    hk.expand(X3DH_INFO, &mut out)
        .map_err(|_| anyhow::anyhow!("x3dh hkdf failed"))?;
    Ok(out)
}

fn header_bytes(ct: &RatchetCiphertext) -> Vec<u8> {
    let mut h = Vec::new();
    if let Some(dh) = ct.dh {
        h.extend_from_slice(&dh);
    }
    h.extend_from_slice(&ct.pn.to_le_bytes());
    h.extend_from_slice(&ct.n.to_le_bytes());
    h
}

fn encrypt_with_mk(
    mk: &[u8; 32],
    ad: &[u8],
    plaintext: &[u8],
) -> anyhow::Result<RatchetCiphertext> {
    let key = Key::from_slice(mk);
    let cipher = ChaCha20Poly1305::new(key);
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let body = cipher
        .encrypt(
            &nonce,
            chacha20poly1305::aead::Payload {
                msg: plaintext,
                aad: ad,
            },
        )
        .map_err(|e| anyhow::anyhow!("ratchet encrypt: {e}"))?;
    Ok(RatchetCiphertext {
        dh: None,
        pn: 0,
        n: 0,
        nonce: nonce.into(),
        body,
    })
}

fn decrypt_with_mk(mk: &[u8; 32], ad: &[u8], ct: &RatchetCiphertext) -> anyhow::Result<Vec<u8>> {
    let key = Key::from_slice(mk);
    let cipher = ChaCha20Poly1305::new(key);
    cipher
        .decrypt(
            Nonce::from_slice(&ct.nonce),
            chacha20poly1305::aead::Payload {
                msg: &ct.body,
                aad: ad,
            },
        )
        .map_err(|_| anyhow::anyhow!("ratchet decrypt failed"))
}

impl RatchetSession {
    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    pub fn with_peer_id(mut self, peer_id: String) -> Self {
        self.peer_id = peer_id;
        self
    }

    pub fn remote_identity(&self) -> &[u8; 32] {
        &self.remote_identity
    }

    /// Initiator: X3DH + first ratchet message containing `first_plaintext`.
    pub fn init_outbound(
        peer_id: String,
        identity: &EncryptionKeypair,
        bundle: &PrekeyBundle,
        first_plaintext: &[u8],
    ) -> anyhow::Result<(Self, Vec<u8>)> {
        if !verify_prekey_bundle(bundle) {
            anyhow::bail!("invalid prekey bundle signature");
        }
        let ek_secret = StaticSecret::random();
        let shared = x3dh_shared_secret(identity, &ek_secret, bundle)?;
        let dh_send = StaticSecret::random();
        let dh_send_pub = *PublicKey::from(&dh_send).as_bytes();
        let (root_key, send_chain_key) = kdf_root(&shared, &dh_send_pub);
        let mut session = Self {
            peer_id,
            secrets: SessionSecrets {
                root_key,
                send_chain_key: Some(send_chain_key),
                recv_chain_key: None,
                send_message_number: 0,
                recv_message_number: 0,
                previous_send_chain_length: 0,
                dh_self_secret: Some(dh_send.to_bytes()),
                dh_remote_public: None,
            },
            skipped: SkippedMessageKeys::new(MAX_SKIP_KEYS),
            is_initiator: true,
            registration_id: bundle.registration_id,
            remote_identity: bundle.identity_key,
        };
        let (mut first_message, _) = session.encrypt(first_plaintext)?;
        let inner = decode_frame(&first_message)?
            .into_message()
            .ok_or_else(|| anyhow::anyhow!("init encrypt did not produce message frame"))?;
        let init = RatchetInitPayload {
            registration_id: bundle.registration_id,
            identity_key: *identity.public.as_bytes(),
            ephemeral_key: *PublicKey::from(&ek_secret).as_bytes(),
            signed_prekey_id: bundle.signed_prekey_id,
            one_time_prekey_id: bundle.one_time_prekey_id,
            first_message: inner,
        };
        first_message = encode_frame(&RatchetFrame::Init(init))?;
        Ok((session, first_message))
    }

    /// Responder: consume X3DH init and establish session.
    pub fn init_inbound(
        peer_id: String,
        identity: &EncryptionKeypair,
        prekeys: &mut LocalPrekeyStore,
        frame_bytes: &[u8],
    ) -> anyhow::Result<(Self, Vec<u8>)> {
        let frame = decode_frame(frame_bytes)?;
        let init = match frame {
            RatchetFrame::Init(i) => i,
            _ => anyhow::bail!("expected ratchet init frame"),
        };
        let ephemeral = PublicKey::from(init.ephemeral_key);
        let ik_remote = PublicKey::from(init.identity_key);
        let spk_secret = StaticSecret::from(prekeys.signed_prekey_secret);
        let mut parts = Vec::new();
        parts.extend_from_slice(spk_secret.diffie_hellman(&ik_remote).as_bytes());
        parts.extend_from_slice(identity.diffie_hellman(&ephemeral).as_bytes());
        parts.extend_from_slice(spk_secret.diffie_hellman(&ephemeral).as_bytes());
        if let Some(opk_id) = init.one_time_prekey_id {
            let opk_sec = prekeys
                .take_one_time_secret(opk_id)
                .ok_or_else(|| anyhow::anyhow!("unknown one-time prekey"))?;
            parts.extend_from_slice(
                StaticSecret::from(opk_sec)
                    .diffie_hellman(&ephemeral)
                    .as_bytes(),
            );
        }
        let hk = Hkdf::<Sha256>::new(None, &parts);
        let mut shared = [0u8; 32];
        hk.expand(X3DH_INFO, &mut shared)
            .map_err(|_| anyhow::anyhow!("x3dh hkdf failed"))?;
        let dh_remote = init
            .first_message
            .dh
            .ok_or_else(|| anyhow::anyhow!("init message missing dh ratchet key"))?;
        let (root_key, recv_chain_key) = kdf_root(&shared, &dh_remote);
        let mut session = Self {
            peer_id,
            secrets: SessionSecrets {
                root_key,
                send_chain_key: None,
                recv_chain_key: Some(recv_chain_key),
                send_message_number: 0,
                recv_message_number: 0,
                previous_send_chain_length: 0,
                dh_self_secret: None,
                dh_remote_public: Some(dh_remote),
            },
            skipped: SkippedMessageKeys::new(MAX_SKIP_KEYS),
            is_initiator: false,
            registration_id: init.registration_id,
            remote_identity: init.identity_key,
        };
        let _plain = session.decrypt_inbound(&init.first_message)?;
        Ok((session, _plain))
    }

    pub fn encrypt(&mut self, plaintext: &[u8]) -> anyhow::Result<(Vec<u8>, RatchetCiphertext)> {
        if self.secrets.send_chain_key.is_none() {
            self.bootstrap_send_chain()?;
        }
        let ck = self
            .secrets
            .send_chain_key
            .ok_or_else(|| anyhow::anyhow!("no send chain"))?;
        let (new_ck, mk) = kdf_chain(&ck);
        self.secrets.send_chain_key = Some(new_ck);
        let n = self.secrets.send_message_number;
        self.secrets.send_message_number += 1;
        let dh = self
            .secrets
            .dh_self_secret
            .map(|s| *PublicKey::from(&StaticSecret::from(s)).as_bytes());
        let mut ct = encrypt_with_mk(&mk, &[], plaintext)?;
        ct.dh = dh;
        ct.pn = self.secrets.previous_send_chain_length;
        ct.n = n;
        let ad = header_bytes(&ct);
        // Re-encrypt with proper AD binding header fields.
        let key = Key::from_slice(&mk);
        let cipher = ChaCha20Poly1305::new(key);
        ct.body = cipher
            .encrypt(
                Nonce::from_slice(&ct.nonce),
                chacha20poly1305::aead::Payload {
                    msg: plaintext,
                    aad: &ad,
                },
            )
            .map_err(|e| anyhow::anyhow!("ratchet encrypt: {e}"))?;
        let wire = encode_frame(&RatchetFrame::Message(ct.clone()))?;
        Ok((wire, ct))
    }

    pub fn decrypt(&mut self, frame_bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        let frame = decode_frame(frame_bytes)?;
        match frame {
            RatchetFrame::Init(init) => {
                let _ = init;
                anyhow::bail!("unexpected init on established session")
            }
            RatchetFrame::Message(ct) => self.decrypt_inbound(&ct),
        }
    }

    fn decrypt_inbound(&mut self, ct: &RatchetCiphertext) -> anyhow::Result<Vec<u8>> {
        if let Some(dh) = ct.dh {
            if self.secrets.dh_remote_public != Some(dh) {
                self.dh_ratchet_step(&dh)?;
            }
        }
        let ratchet_key = self
            .secrets
            .dh_remote_public
            .ok_or_else(|| anyhow::anyhow!("no remote ratchet key"))?;
        if ct.n == self.secrets.recv_message_number {
            let mk = self.next_recv_mk()?;
            let ad = header_bytes(ct);
            return decrypt_with_mk(&mk, &ad, ct);
        }
        if ct.n > self.secrets.recv_message_number {
            self.store_skipped_keys_until(ct)?;
            if ct.n == self.secrets.recv_message_number {
                let mk = self.next_recv_mk()?;
                let ad = header_bytes(ct);
                return decrypt_with_mk(&mk, &ad, ct);
            }
        }
        if let Some(mk) = self.skipped.take(&ratchet_key, ct.n) {
            let ad = header_bytes(ct);
            return decrypt_with_mk(&mk, &ad, ct);
        }
        anyhow::bail!("message key not found (n={})", ct.n)
    }

    fn bootstrap_send_chain(&mut self) -> anyhow::Result<()> {
        let dh_remote = self
            .secrets
            .dh_remote_public
            .ok_or_else(|| anyhow::anyhow!("no remote ratchet key for send bootstrap"))?;
        let dh_send = StaticSecret::random();
        let ss = dh_send.diffie_hellman(&PublicKey::from(dh_remote));
        let (rk, ck) = kdf_root(&self.secrets.root_key, ss.as_bytes());
        self.secrets.root_key = rk;
        self.secrets.send_chain_key = Some(ck);
        self.secrets.dh_self_secret = Some(dh_send.to_bytes());
        self.secrets.send_message_number = 0;
        Ok(())
    }

    fn dh_ratchet_step(&mut self, dh_remote: &[u8; 32]) -> anyhow::Result<()> {
        self.secrets.previous_send_chain_length = self.secrets.send_message_number;
        self.secrets.send_message_number = 0;
        self.secrets.recv_message_number = 0;
        let dh_recv = PublicKey::from(*dh_remote);
        if let Some(dh_self) = self.secrets.dh_self_secret.take() {
            let ss = StaticSecret::from(dh_self).diffie_hellman(&dh_recv);
            let (rk, ck) = kdf_root(&self.secrets.root_key, ss.as_bytes());
            self.secrets.root_key = rk;
            self.secrets.recv_chain_key = Some(ck);
        }
        let dh_send = StaticSecret::random();
        let ss = dh_send.diffie_hellman(&dh_recv);
        let (rk, ck) = kdf_root(&self.secrets.root_key, ss.as_bytes());
        self.secrets.root_key = rk;
        self.secrets.send_chain_key = Some(ck);
        self.secrets.dh_self_secret = Some(dh_send.to_bytes());
        self.secrets.dh_remote_public = Some(*dh_remote);
        Ok(())
    }

    fn next_recv_mk(&mut self) -> anyhow::Result<[u8; 32]> {
        let ck = self
            .secrets
            .recv_chain_key
            .ok_or_else(|| anyhow::anyhow!("no recv chain"))?;
        let (new_ck, mk) = kdf_chain(&ck);
        self.secrets.recv_chain_key = Some(new_ck);
        self.secrets.recv_message_number += 1;
        Ok(mk)
    }

    fn store_skipped_keys_until(&mut self, target: &RatchetCiphertext) -> anyhow::Result<()> {
        let ratchet_key = self
            .secrets
            .dh_remote_public
            .ok_or_else(|| anyhow::anyhow!("no remote ratchet key"))?;
        while self.secrets.recv_message_number < target.n {
            let mk = self.next_recv_mk()?;
            self.skipped
                .insert(ratchet_key, self.secrets.recv_message_number - 1, mk)?;
            if self.secrets.recv_message_number >= target.n {
                break;
            }
            if self.skipped.order.len() >= MAX_SKIP_KEYS {
                anyhow::bail!("too many skipped message keys");
            }
        }
        Ok(())
    }
}

impl RatchetFrame {
    fn into_message(self) -> Option<RatchetCiphertext> {
        match self {
            Self::Message(m) => Some(m),
            _ => None,
        }
    }
}

/// Encrypt using ratchet when session exists; caller handles session persistence.
pub fn ratchet_encrypt(session: &mut RatchetSession, plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
    session.encrypt(plaintext).map(|(w, _)| w)
}

/// Decrypt ratchet wire bytes; returns plaintext or error (no panic).
pub fn ratchet_decrypt(session: &mut RatchetSession, wire: &[u8]) -> anyhow::Result<Vec<u8>> {
    session.decrypt(wire)
}

/// Legacy-compatible decrypt: tries ratchet when magic present, else static v0.
pub fn decrypt_direct_e2e(
    payload: &[u8],
    e2e_version: u8,
    ratchet_session: Option<&mut RatchetSession>,
    legacy_keypair: &EncryptionKeypair,
) -> anyhow::Result<Vec<u8>> {
    if e2e_version == E2E_VERSION_RATCHET || is_ratchet_wire(payload) {
        let session = ratchet_session
            .ok_or_else(|| anyhow::anyhow!("ratchet session required for v1 payload"))?;
        return ratchet_decrypt(session, payload);
    }
    super::decrypt_with(payload, legacy_keypair)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn alice_bundle(bob: &EncryptionKeypair, bob_prekeys: &LocalPrekeyStore) -> PrekeyBundle {
        bob_prekeys.public_bundle(bob)
    }

    #[test]
    fn x3dh_and_ratchet_roundtrip() {
        let alice = EncryptionKeypair::generate();
        let bob = EncryptionKeypair::generate();
        let mut bob_prekeys = LocalPrekeyStore::generate(&bob, 5);
        let bundle = alice_bundle(&bob, &bob_prekeys);

        let (mut alice_sess, init_wire) =
            RatchetSession::init_outbound("bob".into(), &alice, &bundle, b"hello 1")
                .expect("init out");

        let (mut bob_sess, plain1) =
            RatchetSession::init_inbound("alice".into(), &bob, &mut bob_prekeys, &init_wire)
                .expect("init in");
        assert_eq!(plain1, b"hello 1");

        let (wire2, _) = bob_sess.encrypt(b"reply 2").expect("enc2");
        let plain2 = alice_sess.decrypt(&wire2).expect("dec2");
        assert_eq!(plain2, b"reply 2");
    }

    #[test]
    fn out_of_order_within_skip_window() {
        let alice = EncryptionKeypair::generate();
        let bob = EncryptionKeypair::generate();
        let mut bob_prekeys = LocalPrekeyStore::generate(&bob, 5);
        let bundle = bob_prekeys.public_bundle(&bob);
        let (mut alice_sess, init_wire) =
            RatchetSession::init_outbound("bob".into(), &alice, &bundle, b"bootstrap")
                .expect("init");
        let (mut bob_sess, _) =
            RatchetSession::init_inbound("alice".into(), &bob, &mut bob_prekeys, &init_wire)
                .expect("in");

        let (w1, _) = alice_sess.encrypt(b"one").expect("w1");
        let (w2, _) = alice_sess.encrypt(b"two").expect("w2");
        let (w3, _) = alice_sess.encrypt(b"three").expect("w3");

        assert_eq!(bob_sess.decrypt(&w3).expect("d3"), b"three");
        assert_eq!(bob_sess.decrypt(&w1).expect("d1"), b"one");
        assert_eq!(bob_sess.decrypt(&w2).expect("d2"), b"two");
    }

    #[test]
    fn malformed_frame_returns_err() {
        let alice = EncryptionKeypair::generate();
        let mut bob_prekeys = LocalPrekeyStore::generate(&alice, 1);
        let bundle = bob_prekeys.public_bundle(&alice);
        let (_, init) = RatchetSession::init_outbound("b".into(), &alice, &bundle, b"x").unwrap();
        assert!(
            RatchetSession::init_inbound("a".into(), &alice, &mut bob_prekeys, b"bad").is_err()
        );
        assert!(decode_frame(&init).is_ok());
        assert!(decode_frame(b"MYRT2\0garbage").is_err());
    }

    #[test]
    fn legacy_fallback_unchanged() {
        let bob = EncryptionKeypair::generate();
        let ct = super::super::encrypt_for(b"legacy", &bob.public).unwrap();
        let plain = decrypt_direct_e2e(&ct, E2E_VERSION_LEGACY, None, &bob).unwrap();
        assert_eq!(plain, b"legacy");
    }
}
