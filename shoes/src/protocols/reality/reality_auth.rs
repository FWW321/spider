use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use aws_lc_rs::agreement;
use aws_lc_rs::hkdf::{HKDF_SHA256, Salt};

#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

/// Custom error type for REALITY cryptographic operations
#[derive(Debug)]
pub enum CryptoError {
    InvalidKeyLength,
    InvalidNonceLength,
    InvalidCiphertextLength,
    EncryptionFailed,
    DecryptionFailed,
    EcdhFailed,
    HkdfFailed,
    CertificateGenerationFailed(String),
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CryptoError::InvalidKeyLength => write!(f, "Invalid key length"),
            CryptoError::InvalidNonceLength => write!(f, "Invalid nonce length"),
            CryptoError::InvalidCiphertextLength => write!(f, "Invalid ciphertext length"),
            CryptoError::EncryptionFailed => write!(f, "Encryption failed"),
            CryptoError::DecryptionFailed => write!(f, "Decryption failed"),
            CryptoError::EcdhFailed => write!(f, "ECDH key exchange failed"),
            CryptoError::HkdfFailed => write!(f, "HKDF derivation failed"),
            CryptoError::CertificateGenerationFailed(e) => {
                write!(f, "Certificate generation failed: {}", e)
            }
        }
    }
}

impl std::error::Error for CryptoError {}

impl From<CryptoError> for std::io::Error {
    fn from(err: CryptoError) -> Self {
        std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string())
    }
}

pub fn perform_ecdh(
    private_key: &[u8; 32],
    public_key: &[u8; 32],
) -> Result<[u8; 32], CryptoError> {
    let my_private_key = agreement::PrivateKey::from_private_key(&agreement::X25519, private_key)
        .map_err(|_| CryptoError::EcdhFailed)?;

    let _my_public_key = my_private_key
        .compute_public_key()
        .map_err(|_| CryptoError::EcdhFailed)?;

    let peer_public_key =
        agreement::UnparsedPublicKey::new(&agreement::X25519, public_key.as_ref());

    let mut shared_secret = [0u8; 32];
    agreement::agree(
        &my_private_key,
        peer_public_key,
        CryptoError::EcdhFailed,
        |key_material| {
            shared_secret.copy_from_slice(key_material);
            Ok(())
        },
    )?;

    Ok(shared_secret)
}

pub fn derive_auth_key(
    shared_secret: &[u8; 32],
    salt: &[u8],
    info: &[u8],
) -> Result<[u8; 32], CryptoError> {
    debug_assert_eq!(salt.len(), 20, "salt must be exactly 20 bytes");
    let salt = Salt::new(HKDF_SHA256, salt);
    let prk = salt.extract(shared_secret);
    let info_pieces = [info];
    let okm = prk
        .expand(&info_pieces, HKDF_SHA256)
        .map_err(|_| CryptoError::HkdfFailed)?;
    let mut auth_key = [0u8; 32];
    okm.fill(&mut auth_key)
        .map_err(|_| CryptoError::HkdfFailed)?;
    Ok(auth_key)
}

pub fn encrypt_session_id(
    plaintext: &[u8; 16],
    auth_key: &[u8; 32],
    nonce: &[u8],
    aad: &[u8],
) -> Result<[u8; 32], CryptoError> {
    debug_assert_eq!(nonce.len(), 12, "nonce must be exactly 12 bytes");
    let unbound_key =
        UnboundKey::new(&AES_256_GCM, auth_key).map_err(|_| CryptoError::EncryptionFailed)?;
    let sealing_key = LessSafeKey::new(unbound_key);

    let nonce_obj =
        Nonce::try_assume_unique_for_key(nonce).map_err(|_| CryptoError::InvalidNonceLength)?;

    let aad_obj = Aad::from(aad);

    let mut in_out = plaintext.to_vec();
    sealing_key
        .seal_in_place_append_tag(nonce_obj, aad_obj, &mut in_out)
        .map_err(|_| CryptoError::EncryptionFailed)?;

    if in_out.len() != 32 {
        return Err(CryptoError::EncryptionFailed);
    }

    let mut result = [0u8; 32];
    result.copy_from_slice(&in_out);
    Ok(result)
}

pub fn decrypt_session_id(
    ciphertext_and_tag: &[u8; 32],
    auth_key: &[u8; 32],
    nonce: &[u8],
    aad: &[u8],
) -> Result<[u8; 16], CryptoError> {
    debug_assert_eq!(nonce.len(), 12, "nonce must be exactly 12 bytes");
    let unbound_key =
        UnboundKey::new(&AES_256_GCM, auth_key).map_err(|_| CryptoError::DecryptionFailed)?;
    let opening_key = LessSafeKey::new(unbound_key);

    let nonce_obj =
        Nonce::try_assume_unique_for_key(nonce).map_err(|_| CryptoError::InvalidNonceLength)?;

    let aad_obj = Aad::from(aad);

    let mut in_out = ciphertext_and_tag.to_vec();
    let plaintext = opening_key
        .open_in_place(nonce_obj, aad_obj, &mut in_out)
        .map_err(|_| CryptoError::DecryptionFailed)?;

    if plaintext.len() != 16 {
        return Err(CryptoError::DecryptionFailed);
    }

    let mut result = [0u8; 16];
    result.copy_from_slice(plaintext);
    Ok(result)
}

#[cfg(test)]
fn create_session_id(version: [u8; 3], timestamp: u32, short_id: &[u8; 8]) -> [u8; 32] {
    let mut session_id = [0u8; 32];
    session_id[0] = version[0];
    session_id[1] = version[1];
    session_id[2] = version[2];
    session_id[4..8].copy_from_slice(&timestamp.to_be_bytes());
    session_id[8..16].copy_from_slice(short_id);
    session_id
}

#[cfg(test)]
fn get_current_timestamp() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ecdh() {
        let private_key_a = [1u8; 32];
        let private_key_b = [2u8; 32];

        let priv_a =
            agreement::PrivateKey::from_private_key(&agreement::X25519, &private_key_a).unwrap();
        let pub_a = priv_a.compute_public_key().unwrap();

        let priv_b =
            agreement::PrivateKey::from_private_key(&agreement::X25519, &private_key_b).unwrap();
        let pub_b = priv_b.compute_public_key().unwrap();

        let mut shared_a_bytes = [0u8; 32];
        agreement::agree(
            &priv_a,
            &agreement::UnparsedPublicKey::new(&agreement::X25519, pub_b.as_ref()),
            (),
            |key_material| {
                shared_a_bytes.copy_from_slice(key_material);
                Ok(())
            },
        )
        .unwrap();

        let mut shared_b_bytes = [0u8; 32];
        agreement::agree(
            &priv_b,
            &agreement::UnparsedPublicKey::new(&agreement::X25519, pub_a.as_ref()),
            (),
            |key_material| {
                shared_b_bytes.copy_from_slice(key_material);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(shared_a_bytes, shared_b_bytes);
    }

    #[test]
    fn test_session_id_structure() {
        let version = [1, 8, 1];
        let timestamp = get_current_timestamp();
        let short_id = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0];
        let session_id = create_session_id(version, timestamp, &short_id);
        assert_eq!(session_id[0], 1);
        let extracted_timestamp =
            u32::from_be_bytes([session_id[4], session_id[5], session_id[6], session_id[7]]);
        assert_eq!(extracted_timestamp, timestamp);
        assert_eq!(&session_id[8..16], &short_id[..]);
    }

    #[test]
    fn test_timestamp_validation_logic() {
        let now = get_current_timestamp();
        let max_diff_secs = 60u64;
        let diff = now.abs_diff(now);
        assert!((diff as u64) <= max_diff_secs);
        let past_timestamp = now - 30;
        let diff = now.abs_diff(past_timestamp);
        assert!((diff as u64) <= max_diff_secs);
    }
}