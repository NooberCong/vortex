//! Clear-key AES-128 (04 §6).
//!
//! HLS `METHOD=AES-128` encrypts each segment as one complete AES-128-CBC object with
//! PKCS#7 padding, keyed by a 16-byte blob fetched over HTTP with the same session
//! credentials as the manifest. The IV comes from `#EXT-X-KEY:IV`, and when that is absent
//! it is the segment's media sequence number as a 128-bit big-endian integer. Getting that
//! default wrong does not error — it decrypts to noise — which is why it is spelled out
//! here rather than inferred at the call site.
//!
//! This is not DRM. There is no key exchange, no device binding, and no protection measure
//! being circumvented: the browser fetches the same key from the same URL.

use aes::cipher::block_padding::NoPadding;
use aes::cipher::{BlockModeDecrypt, KeyIvInit};

type Decryptor = cbc::Decryptor<aes::Aes128>;

/// The IV to use when `#EXT-X-KEY` does not carry one.
pub fn iv_from_sequence(sequence: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    iv
}

/// Decrypts one segment in place.
///
/// A segment whose length is not a multiple of the block size cannot be AES-128-CBC at all,
/// so that is refused rather than truncated. A *padding* that does not validate is treated
/// differently: some encoders pad only the final segment, and discarding a whole segment of
/// video over a trailing byte would be a worse answer than handing the muxer the plaintext.
pub fn decrypt(key: &[u8; 16], iv: &[u8; 16], data: Vec<u8>) -> Result<Vec<u8>, String> {
    if data.is_empty() {
        return Ok(data);
    }
    if data.len() % 16 != 0 {
        return Err(format!(
            "an encrypted segment was {} bytes, which is not a whole number of AES blocks",
            data.len()
        ));
    }

    // Decrypt first, unpad second. `decrypt_padded::<Pkcs7>` rewrites the buffer *before*
    // it validates the trailer, so a failed padding check leaves plaintext behind that a
    // second pass would happily decrypt again into noise.
    let mut buffer = data;
    Decryptor::new(key.into(), iv.into())
        .decrypt_padded::<NoPadding>(&mut buffer)
        .map_err(|e| format!("a segment would not decrypt: {e}"))?;
    if let Some(padding) = pkcs7_length(&buffer) {
        buffer.truncate(buffer.len() - padding);
    }
    Ok(buffer)
}

/// The number of trailing bytes that are valid PKCS#7 padding, if any.
fn pkcs7_length(buffer: &[u8]) -> Option<usize> {
    let padding = *buffer.last()? as usize;
    let valid = (1..=16).contains(&padding)
        && buffer.len() >= padding
        && buffer[buffer.len() - padding..]
            .iter()
            .all(|byte| *byte as usize == padding);
    valid.then_some(padding)
}

/// Parses a key blob. HLS keys are exactly 16 bytes; anything else is a key-server error
/// page, and treating one as a key produces a file that looks downloaded and plays as
/// static.
pub fn key_from_bytes(bytes: &[u8]) -> Result<[u8; 16], String> {
    if bytes.len() != 16 {
        return Err(format!(
            "the key server returned {} bytes where a key is 16",
            bytes.len()
        ));
    }
    let mut key = [0u8; 16];
    key.copy_from_slice(bytes);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::{block_padding::Pkcs7 as Pad, BlockModeEncrypt};

    type Encryptor = cbc::Encryptor<aes::Aes128>;

    fn encrypt(key: &[u8; 16], iv: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
        let mut buffer = vec![0u8; plaintext.len() + 16];
        let out = Encryptor::new(key.into(), iv.into())
            .encrypt_padded_b2b::<Pad>(plaintext, &mut buffer)
            .unwrap()
            .len();
        buffer.truncate(out);
        buffer
    }

    #[test]
    fn a_segment_survives_the_round_trip() {
        let key = [7u8; 16];
        let iv = iv_from_sequence(42);
        let plaintext = b"\x47\x40\x00\x10 a transport stream packet, roughly".repeat(9);
        let ciphertext = encrypt(&key, &iv, &plaintext);
        assert_eq!(decrypt(&key, &iv, ciphertext).unwrap(), plaintext);
    }

    #[test]
    fn the_default_iv_is_the_media_sequence_number() {
        assert_eq!(
            iv_from_sequence(1),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
        assert_eq!(iv_from_sequence(0), [0u8; 16]);
    }

    #[test]
    fn unpadded_ciphertext_keeps_its_bytes_rather_than_being_thrown_away() {
        let key = [3u8; 16];
        let iv = [0u8; 16];
        // 32 bytes of ciphertext produced without PKCS#7: the last block's "padding" is
        // whatever the plaintext happened to be.
        let plaintext = [0xABu8; 32];
        let mut buffer = plaintext.to_vec();
        cbc::Encryptor::<aes::Aes128>::new(&key.into(), &iv.into())
            .encrypt_padded::<NoPadding>(&mut buffer, 32)
            .unwrap();
        let out = decrypt(&key, &iv, buffer).unwrap();
        assert_eq!(out, plaintext);
    }

    #[test]
    fn a_ragged_segment_is_refused_rather_than_guessed_at() {
        assert!(decrypt(&[0u8; 16], &[0u8; 16], vec![1, 2, 3]).is_err());
    }

    #[test]
    fn an_error_page_is_not_a_key() {
        assert!(key_from_bytes(b"<html>403 Forbidden</html>").is_err());
        assert!(key_from_bytes(&[0u8; 16]).is_ok());
    }
}
