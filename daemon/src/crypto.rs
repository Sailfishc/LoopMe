//! End-to-end encryption between the daemon and paired devices.
//!
//! The scheme is deliberately small so that it can be mirrored exactly with
//! Apple CryptoKit on iOS and macOS:
//!
//! * Identity keys are X25519 (`Curve25519.KeyAgreement` in CryptoKit).
//! * A session key pair is derived with HKDF-SHA256 from the static-static
//!   shared secret, salted with `client_nonce || server_nonce`, using the info
//!   strings `loopme v1 c2s` and `loopme v1 s2c`.
//! * Every frame is ChaCha20-Poly1305 sealed. The wire form is CryptoKit's
//!   `SealedBox.combined`: `nonce(12) || ciphertext || tag(16)`. The nonce is
//!   4 zero bytes followed by a little-endian 64-bit counter; receivers reject
//!   counters that do not strictly increase.

use anyhow::{anyhow, bail, Result};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use rand::RngCore;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

pub const INFO_C2S: &[u8] = b"loopme v1 c2s";
pub const INFO_S2C: &[u8] = b"loopme v1 s2c";

pub struct KeyPair {
    secret: StaticSecret,
    public: PublicKey,
}

impl KeyPair {
    pub fn generate() -> Self {
        let secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let public = PublicKey::from(&secret);
        Self { secret, public }
    }

    pub fn from_secret_bytes(bytes: [u8; 32]) -> Self {
        let secret = StaticSecret::from(bytes);
        let public = PublicKey::from(&secret);
        Self { secret, public }
    }

    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret.to_bytes()
    }

    pub fn public_bytes(&self) -> [u8; 32] {
        *self.public.as_bytes()
    }

    pub fn shared_secret(&self, peer_public: &[u8; 32]) -> [u8; 32] {
        let peer = PublicKey::from(*peer_public);
        *self.secret.diffie_hellman(&peer).as_bytes()
    }
}

pub struct SessionKeys {
    pub c2s: [u8; 32],
    pub s2c: [u8; 32],
}

pub fn derive_session_keys(shared: &[u8; 32], client_nonce: &[u8], server_nonce: &[u8]) -> SessionKeys {
    let mut salt = Vec::with_capacity(client_nonce.len() + server_nonce.len());
    salt.extend_from_slice(client_nonce);
    salt.extend_from_slice(server_nonce);
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut c2s = [0u8; 32];
    let mut s2c = [0u8; 32];
    hk.expand(INFO_C2S, &mut c2s).expect("32 bytes is a valid HKDF length");
    hk.expand(INFO_S2C, &mut s2c).expect("32 bytes is a valid HKDF length");
    SessionKeys { c2s, s2c }
}

/// Short authentication string shown on both screens during pairing.
pub fn pairing_sas(mac_public: &[u8; 32], device_public: &[u8; 32]) -> String {
    let mut h = Sha256::new();
    h.update(b"loopme v1 sas");
    h.update(mac_public);
    h.update(device_public);
    let d = h.finalize();
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 10_000;
    format!("{n:04}")
}

pub struct FrameSealer {
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl FrameSealer {
    pub fn new(key: &[u8; 32]) -> Self {
        Self { cipher: ChaCha20Poly1305::new(Key::from_slice(key)), counter: 0 }
    }

    pub fn seal(&mut self, plaintext: &[u8]) -> Vec<u8> {
        let nonce = nonce_for(self.counter);
        self.counter += 1;
        let ct = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext)
            .expect("chacha20poly1305 encryption cannot fail for in-memory buffers");
        let mut out = Vec::with_capacity(12 + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }
}

pub struct FrameOpener {
    cipher: ChaCha20Poly1305,
    next_counter: u64,
}

impl FrameOpener {
    pub fn new(key: &[u8; 32]) -> Self {
        Self { cipher: ChaCha20Poly1305::new(Key::from_slice(key)), next_counter: 0 }
    }

    pub fn open(&mut self, frame: &[u8]) -> Result<Vec<u8>> {
        if frame.len() < 12 + 16 {
            bail!("frame too short");
        }
        let (nonce, ct) = frame.split_at(12);
        if nonce[..4] != [0, 0, 0, 0] {
            bail!("bad nonce prefix");
        }
        let mut c = [0u8; 8];
        c.copy_from_slice(&nonce[4..12]);
        let counter = u64::from_le_bytes(c);
        if counter < self.next_counter {
            bail!("replayed or reordered frame");
        }
        let pt = self
            .cipher
            .decrypt(Nonce::from_slice(nonce), ct)
            .map_err(|_| anyhow!("frame authentication failed"))?;
        self.next_counter = counter + 1;
        Ok(pt)
    }
}

fn nonce_for(counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_le_bytes());
    n
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b
}

pub fn b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn from_b64(s: &str) -> Result<Vec<u8>> {
    STANDARD
        .decode(s)
        .or_else(|_| URL_SAFE_NO_PAD.decode(s))
        .map_err(|e| anyhow!("invalid base64: {e}"))
}

pub fn key32(s: &str) -> Result<[u8; 32]> {
    let v = from_b64(s)?;
    v.try_into().map_err(|_| anyhow!("expected 32 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_roundtrip_and_replay_protection() {
        let mac = KeyPair::generate();
        let phone = KeyPair::generate();
        let s1 = mac.shared_secret(&phone.public_bytes());
        let s2 = phone.shared_secret(&mac.public_bytes());
        assert_eq!(s1, s2);
        let cn = random_bytes::<16>();
        let sn = random_bytes::<16>();
        let a = derive_session_keys(&s1, &cn, &sn);
        let b = derive_session_keys(&s2, &cn, &sn);
        assert_eq!(a.c2s, b.c2s);
        assert_ne!(a.c2s, a.s2c);

        let mut sealer = FrameSealer::new(&a.c2s);
        let mut opener = FrameOpener::new(&b.c2s);
        let f1 = sealer.seal(b"hello");
        let f2 = sealer.seal(b"world");
        assert_eq!(opener.open(&f1).unwrap(), b"hello");
        assert_eq!(opener.open(&f2).unwrap(), b"world");
        assert!(opener.open(&f1).is_err(), "replay must fail");

        let mut tampered = sealer.seal(b"x");
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(opener.open(&tampered).is_err());
    }

    /// Shared with `apple/LoopMeKit/Tests/LoopMeKitTests/CryptoTests.swift`
    /// (generated with Python `cryptography`, which follows the same RFCs as CryptoKit).
    #[test]
    fn cross_platform_vector() {
        let mac = KeyPair::from_secret_bytes(core::array::from_fn(|i| i as u8 + 1));
        let dev = KeyPair::from_secret_bytes(core::array::from_fn(|i| i as u8 + 101));
        assert_eq!(b64(&mac.public_bytes()), "B6N8vBQgk8i3VdwbEOhstCY3StFqqFPtC9/AsrhtHHw=");
        assert_eq!(b64(&dev.public_bytes()), "VxR2nRFr92Q2rnS8eT0sMK0ZA8WaxSc4BcfiaYtBDDY=");
        let shared = mac.shared_secret(&dev.public_bytes());
        assert_eq!(b64(&shared), "yepqP3mgALYLB21K/JkLJy8/C1qqPwuHE8IJJz42OGM=");
        let k = derive_session_keys(&shared, &[7u8; 16], &[9u8; 16]);
        assert_eq!(b64(&k.c2s), "qi6TOdRLZWDhxkfbYMzVK32BvUi4lsSL0x8x40Xe8HY=");
        assert_eq!(b64(&k.s2c), "qxUFz+Y9wMnGIEunFVSKdDPBV0cgQM/XCpNUFCjr5ew=");
        assert_eq!(b64(&FrameSealer::new(&k.c2s).seal(b"hello")), "AAAAAAAAAAAAAAAA83T5AE3+p7Bg+CcHg/9Egoq2yH6q");
        assert_eq!(pairing_sas(&mac.public_bytes(), &dev.public_bytes()), "5832");
    }

    #[test]
    fn sas_is_four_digits_and_order_sensitive() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let s = pairing_sas(&a, &b);
        assert_eq!(s.len(), 4);
        assert!(s.chars().all(|c| c.is_ascii_digit()));
        assert_ne!(pairing_sas(&a, &b), pairing_sas(&b, &a));
    }
}
