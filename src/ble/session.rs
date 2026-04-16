//! BLE session management, pairing, and AES-128-CCM encryption.
//!
//! Implements the security model from ADR-003:
//! - Pairing secret verification (constant-time compare)
//! - HKDF-SHA256 session key derivation
//! - JWT issuance for HTTPS bridge
//! - AES-128-CCM authenticated encryption with counter-based replay protection

use std::time::Instant;

use aes::Aes128;
use ccm::Ccm;
use ccm::aead::generic_array::GenericArray;
use ccm::aead::{Aead, KeyInit, Payload};
use hkdf::Hkdf;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rand::RngCore;
use serde::Serialize;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;

/// AES-128-CCM with 4-byte tag and 13-byte nonce.
type NomoCcm = Ccm<Aes128, ccm::aead::consts::U4, ccm::aead::consts::U13>;

/// HKDF info string for session key derivation (ADR-003).
const HKDF_INFO: &[u8] = b"nomon-ble-session";

/// CCM nonce prefix: `"NM"`.
const NONCE_PREFIX: &[u8; 2] = b"NM";

/// Direction byte: client → server.
const DIR_CLIENT_TO_SERVER: u8 = 0x00;
/// Direction byte: server → client.
const DIR_SERVER_TO_CLIENT: u8 = 0x01;

/// CCM nonce length (2 prefix + 1 direction + 2 counter + 8 padding = 13).
const NONCE_LEN: usize = 13;

// ── Errors ─────────────────────────────────────────────────────────────

/// Errors during BLE pairing.
#[derive(Debug, Error)]
pub enum PairingError {
    #[error("invalid pairing secret")]
    InvalidSecret,
    #[error("key derivation failed")]
    KeyDerivation,
    #[error("JWT signing failed: {0}")]
    JwtSigning(String),
}

/// Errors during AES-CCM encryption/decryption.
#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("encryption failed")]
    EncryptionFailed,
    #[error("decryption failed (invalid ciphertext or tag)")]
    DecryptionFailed,
    #[error("replay detected: counter {received} <= last seen {last_seen}")]
    ReplayDetected { received: u16, last_seen: u16 },
    #[error("ciphertext too short: need at least 2 bytes for counter")]
    TooShort,
}

// ── JWT Claims ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct BleJwtClaims {
    sub: &'static str,
    iss: &'static str,
    exp: u64,
    iat: u64,
}

// ── BLE Session ────────────────────────────────────────────────────────

/// Active BLE session after successful pairing.
pub struct BleSession {
    /// Derived AES-128 session key (16 bytes).
    session_key: [u8; 16],
    /// Server → client counter (monotonically increasing).
    tx_counter: u16,
    /// Last seen client → server counter (for replay detection).
    rx_counter: u16,
    /// Whether we have received at least one valid message (for counter-0 replay protection).
    received_first_message: bool,
    /// JWT issued during pairing.
    pub jwt: String,
    /// When pairing was established.
    pub paired_at: Instant,
}

/// Container for optional BLE session state, shared across characteristic
/// handlers via `Arc<Mutex<SessionState>>`.
pub struct SessionState {
    inner: Option<BleSession>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionState {
    /// Create a new unpaired session state.
    pub fn new() -> Self {
        Self { inner: None }
    }

    /// Whether a BLE session is currently active.
    pub fn is_paired(&self) -> bool {
        self.inner.is_some()
    }

    /// Set the active session (replaces any existing session).
    pub fn set_session(&mut self, session: BleSession) {
        self.inner = Some(session);
    }

    /// Clear the active session (e.g. on disconnect).
    pub fn clear(&mut self) {
        self.inner = None;
    }

    /// Access the active session for encryption/decryption.
    pub fn session_mut(&mut self) -> Option<&mut BleSession> {
        self.inner.as_mut()
    }
}

// ── Pairing ────────────────────────────────────────────────────────────

/// Perform BLE pairing: verify the secret, derive a session key, and issue a JWT.
///
/// Returns `(BleSession, auth_payload)` where `auth_payload` is the bytes to
/// send on the Auth Token characteristic: `salt (16 bytes) || JWT (UTF-8)`.
///
/// # Security
/// - Constant-time comparison of pairing secret (checklist B1).
/// - HKDF-SHA256 key derivation with random salt (checklist B3).
/// - JWT with `iss: "nomon-device"` (checklist B9).
pub fn pair(
    secret: &str,
    stored_secret: &str,
    jwt_secret: &str,
) -> Result<(BleSession, Vec<u8>), PairingError> {
    // Constant-time compare (security checklist B1).
    let secret_bytes = secret.as_bytes();
    let stored_bytes = stored_secret.as_bytes();

    if secret_bytes.len() != stored_bytes.len() || !bool::from(secret_bytes.ct_eq(stored_bytes)) {
        return Err(PairingError::InvalidSecret);
    }

    // Generate 16-byte random salt.
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);

    // Derive session key via HKDF-SHA256 (security checklist B3).
    let hk = Hkdf::<Sha256>::new(Some(&salt), secret_bytes);
    let mut session_key = [0u8; 16];
    hk.expand(HKDF_INFO, &mut session_key)
        .map_err(|_| PairingError::KeyDerivation)?;

    // Issue JWT (security checklist B8, B9).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| PairingError::JwtSigning("system clock error".into()))?;
    let now_secs = now.as_secs();

    let claims = BleJwtClaims {
        sub: "device-owner@local",
        iss: "nomon-device",
        exp: now_secs + 86400, // 24 hours
        iat: now_secs,
    };
    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(jwt_secret.as_bytes()),
    )
    .map_err(|e| PairingError::JwtSigning(e.to_string()))?;

    // Build auth payload: salt || JWT bytes.
    let mut auth_payload = Vec::with_capacity(16 + token.len());
    auth_payload.extend_from_slice(&salt);
    auth_payload.extend_from_slice(token.as_bytes());

    let session = BleSession {
        session_key,
        tx_counter: 0,
        rx_counter: 0,
        received_first_message: false,
        jwt: token,
        paired_at: Instant::now(),
    };

    Ok((session, auth_payload))
}

// ── Encryption ─────────────────────────────────────────────────────────

/// Build a 13-byte AES-CCM nonce.
///
/// Format: `"NM" || direction (1B) || counter LE (2B) || 0x00 × 8`
fn build_nonce(direction: u8, counter: u16) -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    nonce[0] = NONCE_PREFIX[0];
    nonce[1] = NONCE_PREFIX[1];
    nonce[2] = direction;
    nonce[3..5].copy_from_slice(&counter.to_le_bytes());
    // Bytes 5–12 are zero-padded.
    nonce
}

/// Encrypt a plaintext payload for server → client transmission.
///
/// Returns `counter_le (2B) || ciphertext || tag (4B)`.
/// Increments `session.tx_counter` after successful encryption.
///
/// AAD (associated data) should be the 3-byte frame header
/// (`opcode || seq_nr || length`) for authenticated-but-unencrypted
/// routing fields.
pub fn encrypt(
    session: &mut BleSession,
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let counter = session.tx_counter;
    let nonce_bytes = build_nonce(DIR_SERVER_TO_CLIENT, counter);
    let nonce = GenericArray::from_slice(&nonce_bytes);
    let key = GenericArray::from_slice(&session.session_key);

    let cipher = NomoCcm::new(key);
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::EncryptionFailed)?;

    // Increment counter — error at u16::MAX to prevent nonce reuse.
    if session.tx_counter == u16::MAX {
        return Err(CryptoError::EncryptionFailed);
    }
    session.tx_counter += 1;

    // Output: counter (2B) || ciphertext+tag
    let mut output = Vec::with_capacity(2 + ciphertext.len());
    output.extend_from_slice(&counter.to_le_bytes());
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

/// Decrypt a ciphertext payload from client → server.
///
/// Input format: `counter_le (2B) || ciphertext || tag (4B)`.
/// Verifies the counter is strictly greater than the last seen value
/// (replay protection, security checklist B5).
pub fn decrypt(
    session: &mut BleSession,
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    if ciphertext.len() < 2 {
        return Err(CryptoError::TooShort);
    }

    let counter = u16::from_le_bytes([ciphertext[0], ciphertext[1]]);

    // Replay protection (security checklist B5): counter must be
    // strictly greater than the last seen value.
    if session.received_first_message && counter <= session.rx_counter {
        return Err(CryptoError::ReplayDetected {
            received: counter,
            last_seen: session.rx_counter,
        });
    }

    let nonce_bytes = build_nonce(DIR_CLIENT_TO_SERVER, counter);
    let nonce = GenericArray::from_slice(&nonce_bytes);
    let key = GenericArray::from_slice(&session.session_key);

    let cipher = NomoCcm::new(key);
    let plaintext = cipher
        .decrypt(
            nonce,
            Payload {
                msg: &ciphertext[2..],
                aad,
            },
        )
        .map_err(|_| CryptoError::DecryptionFailed)?;

    session.rx_counter = counter;
    session.received_first_message = true;
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &str = "test-pairing-secret-abc123";
    const TEST_JWT_SECRET: &str = "test-jwt-secret-xyz789";

    #[test]
    fn pair_succeeds_with_correct_secret() {
        let result = pair(TEST_SECRET, TEST_SECRET, TEST_JWT_SECRET);
        assert!(result.is_ok());
        let (session, auth_payload) = result.unwrap();
        assert!(!session.jwt.is_empty());
        assert_eq!(session.tx_counter, 0);
        assert_eq!(session.rx_counter, 0);
        // Auth payload starts with 16-byte salt.
        assert!(auth_payload.len() > 16);
    }

    #[test]
    fn pair_fails_with_wrong_secret() {
        let result = pair("wrong-secret", TEST_SECRET, TEST_JWT_SECRET);
        assert!(matches!(result, Err(PairingError::InvalidSecret)));
    }

    #[test]
    fn pair_fails_with_different_length() {
        let result = pair("short", TEST_SECRET, TEST_JWT_SECRET);
        assert!(matches!(result, Err(PairingError::InvalidSecret)));
    }

    #[test]
    fn pair_produces_unique_salts() {
        let (_, payload1) = pair(TEST_SECRET, TEST_SECRET, TEST_JWT_SECRET).unwrap();
        let (_, payload2) = pair(TEST_SECRET, TEST_SECRET, TEST_JWT_SECRET).unwrap();
        // Salts (first 16 bytes) should differ.
        assert_ne!(&payload1[..16], &payload2[..16]);
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let (mut session, _) = pair(TEST_SECRET, TEST_SECRET, TEST_JWT_SECRET).unwrap();
        // Create a second session with the same key for the "client" side.
        let mut client_session = BleSession {
            session_key: session.session_key,
            tx_counter: 0,
            rx_counter: 0,
            received_first_message: false,
            jwt: String::new(),
            paired_at: Instant::now(),
        };

        let plaintext = b"hello world";
        let aad = &[0x03, 0x01, 0x0B]; // fake header

        // Server encrypts → client decrypts.
        let encrypted = encrypt(&mut session, plaintext, aad).unwrap();
        assert_ne!(&encrypted[2..], plaintext);
        assert_eq!(session.tx_counter, 1);

        // Client sees server's counter as rx; adjust direction in nonce.
        // For this test, decrypt using the same session but flip direction manually.
        // Actually, we test the server-side decrypt (client→server direction).

        // Let's test client→server: client encrypts, server decrypts.
        let client_plaintext = b"motor speed 50";
        let client_aad = &[0x06, 0x02, 0x04];

        // Manually encrypt on client side (client→server: direction 0x00).
        let counter = client_session.tx_counter;
        let nonce_bytes = build_nonce(DIR_CLIENT_TO_SERVER, counter);
        let nonce = GenericArray::from_slice(&nonce_bytes);
        let key = GenericArray::from_slice(&client_session.session_key);
        let cipher = NomoCcm::new(key);
        let ct = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: &client_plaintext[..],
                    aad: client_aad,
                },
            )
            .unwrap();
        client_session.tx_counter += 1;

        let mut client_msg = Vec::with_capacity(2 + ct.len());
        client_msg.extend_from_slice(&counter.to_le_bytes());
        client_msg.extend_from_slice(&ct);

        // Server decrypts.
        let decrypted = decrypt(&mut session, &client_msg, client_aad).unwrap();
        assert_eq!(&decrypted, client_plaintext);
        assert_eq!(session.rx_counter, 0); // first message had counter=0
    }

    #[test]
    fn replay_detection() {
        let (mut session, _) = pair(TEST_SECRET, TEST_SECRET, TEST_JWT_SECRET).unwrap();

        // Build a fake encrypted message with counter=1.
        let counter: u16 = 1;
        let nonce_bytes = build_nonce(DIR_CLIENT_TO_SERVER, counter);
        let nonce = GenericArray::from_slice(&nonce_bytes);
        let key = GenericArray::from_slice(&session.session_key);
        let cipher = NomoCcm::new(key);
        let aad = &[0x01, 0x01, 0x00];
        let ct = cipher.encrypt(nonce, Payload { msg: b"", aad }).unwrap();

        let mut msg = Vec::new();
        msg.extend_from_slice(&counter.to_le_bytes());
        msg.extend_from_slice(&ct);

        // First decrypt succeeds.
        let result = decrypt(&mut session, &msg, aad);
        assert!(result.is_ok());
        assert_eq!(session.rx_counter, 1);

        // Replay with same counter is rejected.
        let result = decrypt(&mut session, &msg, aad);
        assert!(matches!(result, Err(CryptoError::ReplayDetected { .. })));
    }

    #[test]
    fn decrypt_too_short() {
        let (mut session, _) = pair(TEST_SECRET, TEST_SECRET, TEST_JWT_SECRET).unwrap();
        let result = decrypt(&mut session, &[0x00], &[]);
        assert!(matches!(result, Err(CryptoError::TooShort)));
    }

    #[test]
    fn tx_counter_increments() {
        let (mut session, _) = pair(TEST_SECRET, TEST_SECRET, TEST_JWT_SECRET).unwrap();
        let aad = &[0x01, 0x01, 0x00];
        for i in 0..5u16 {
            assert_eq!(session.tx_counter, i);
            let _ = encrypt(&mut session, b"test", aad).unwrap();
        }
        assert_eq!(session.tx_counter, 5);
    }

    #[test]
    fn session_state_lifecycle() {
        let mut state = SessionState::new();
        assert!(!state.is_paired());

        let (session, _) = pair(TEST_SECRET, TEST_SECRET, TEST_JWT_SECRET).unwrap();
        state.set_session(session);
        assert!(state.is_paired());

        state.clear();
        assert!(!state.is_paired());
    }

    #[test]
    fn nonce_includes_direction_byte() {
        let nonce_c2s = build_nonce(DIR_CLIENT_TO_SERVER, 42);
        let nonce_s2c = build_nonce(DIR_SERVER_TO_CLIENT, 42);
        // Same counter, different direction → different nonces (checklist B6).
        assert_ne!(nonce_c2s, nonce_s2c);
        assert_eq!(nonce_c2s[2], 0x00);
        assert_eq!(nonce_s2c[2], 0x01);
    }
}
