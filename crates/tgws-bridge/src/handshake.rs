//! Obfuscated2 client handshake and session ciphers.
//!
//! The layout follows the MTProto "obfuscated2" scheme: 64 random bytes whose
//! middle carries the prekey and IV, optionally patched (through the keystream)
//! with the transport tag and the DC index. The sender cipher is the same key
//! stream advanced past the 64-byte header; the receiver cipher uses the
//! reversed prekey and IV without hashing.

use aes::Aes256;
use ctr::cipher::{KeyIvInit, StreamCipher};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Length of the obfuscated2 header in bytes.
pub const HANDSHAKE_LEN: usize = 64;
/// Transport tag announcing the intermediate framing (`0xeeeeeeee`).
pub const PROTO_TAG_INTERMEDIATE: [u8; 4] = [0xee, 0xee, 0xee, 0xee];
/// Transport tag announcing the abridged framing (`0xefefefef`).
pub const PROTO_TAG_ABRIDGED: [u8; 4] = [0xef, 0xef, 0xef, 0xef];

const SKIP_LEN: usize = 8;
const PREKEY_LEN: usize = 32;
const IV_LEN: usize = 16;
const PROTO_TAG_POS: usize = 56;
const DC_IDX_POS: usize = 60;
const RESERVED_FIRST_BYTES: &[u8] = &[0xef];
const RESERVED_STARTS: &[[u8; 4]] = &[
    *b"HEAD",
    *b"POST",
    *b"GET ",
    [0xee, 0xee, 0xee, 0xee],
    [0xdd, 0xdd, 0xdd, 0xdd],
    [0x16, 0x03, 0x01, 0x02], // TLS ClientHello
];

/// AES-256 in big-endian counter mode, as used by obfuscated2.
pub type AesCtr256 = ctr::Ctr128BE<Aes256>;

fn make_cipher(key: &[u8], iv: &[u8]) -> AesCtr256 {
    AesCtr256::new_from_slices(key, iv).expect("32-byte key and 16-byte IV")
}

/// Builds a 64-byte obfuscated2 header for `dc_idx` plus the two session
/// ciphers: `(header, sender, receiver)`.
///
/// The header has to be sent to the proxy as-is; `sender` then encrypts every
/// byte the client sends and `receiver` decrypts every byte the proxy returns.
/// A negative `dc_idx` addresses the media ring of that DC.
///
/// # Examples
///
/// ```
/// use tgws_bridge::generate_client_handshake;
///
/// let (header, _sender, _receiver) =
///     generate_client_handshake(&[0x11; 16], 2);
/// assert_eq!(header.len(), 64);
/// ```
pub fn generate_client_handshake(
    secret: &[u8; 16],
    dc_idx: i16,
) -> ([u8; HANDSHAKE_LEN], AesCtr256, AesCtr256) {
    let dc_bytes = dc_idx.to_le_bytes();

    loop {
        let mut raw = [0u8; HANDSHAKE_LEN];
        rand::rng().fill_bytes(&mut raw);

        // Browsers and DPI treat some prefixes specially; a valid handshake
        // must not look like HTTP, TLS or another obfuscated2 mode.
        if RESERVED_FIRST_BYTES.contains(&raw[0]) {
            continue;
        }
        if RESERVED_STARTS.iter().any(|start| raw[..4] == *start) {
            continue;
        }
        if raw[4..8] == [0; 4] {
            continue;
        }

        // The key is derived from the raw bytes `[8..56]` and the secret.
        let mut key = Sha256::new();
        key.update(&raw[SKIP_LEN..SKIP_LEN + PREKEY_LEN]);
        key.update(secret);
        let key: [u8; 32] = key.finalize().into();
        let iv = &raw[SKIP_LEN + PREKEY_LEN..SKIP_LEN + PREKEY_LEN + IV_LEN];

        // Keystream over the header: encrypting zeros yields it directly.
        let mut keystream = [0u8; HANDSHAKE_LEN];
        make_cipher(&key, iv).apply_keystream(&mut keystream);

        let mut handshake = raw;
        for i in 0..4 {
            handshake[PROTO_TAG_POS + i] = PROTO_TAG_INTERMEDIATE[i] ^ keystream[PROTO_TAG_POS + i];
        }
        for i in 0..2 {
            handshake[DC_IDX_POS + i] = dc_bytes[i] ^ keystream[DC_IDX_POS + i];
        }
        // handshake[62..64] stays random; the proxy does not read it.

        // Sender cipher: same key and IV, advanced past the header.
        let mut enc_key = Sha256::new();
        enc_key.update(&handshake[SKIP_LEN..SKIP_LEN + PREKEY_LEN]);
        enc_key.update(secret);
        let enc_key: [u8; 32] = enc_key.finalize().into();
        let mut enc = make_cipher(
            &enc_key,
            &handshake[SKIP_LEN + PREKEY_LEN..SKIP_LEN + PREKEY_LEN + IV_LEN],
        );
        let mut dummy = [0u8; HANDSHAKE_LEN];
        enc.apply_keystream(&mut dummy);

        // Receiver cipher: reversed prekey and IV, no hashing, no advance.
        let mut reversed = [0u8; PREKEY_LEN + IV_LEN];
        reversed.copy_from_slice(&handshake[SKIP_LEN..SKIP_LEN + PREKEY_LEN + IV_LEN]);
        reversed.reverse();
        let mut dec_key = Sha256::new();
        dec_key.update(&reversed[..PREKEY_LEN]);
        dec_key.update(secret);
        let dec_key: [u8; 32] = dec_key.finalize().into();
        let dec = make_cipher(&dec_key, &reversed[PREKEY_LEN..]);

        return (handshake, enc, dec);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_avoids_reserved_prefixes() {
        for dc in [1i16, 2, -2, 4, -4, 5] {
            for _ in 0..64 {
                let (header, _, _) = generate_client_handshake(&[0x42; 16], dc);
                assert_ne!(header[0], 0xef);
                assert!(!RESERVED_STARTS.iter().any(|start| header[..4] == *start));
                assert_ne!(header[4..8], [0; 4]);
            }
        }
    }

    #[test]
    fn plaintext_tail_round_trips_through_both_ciphers() {
        let secret = [0x11u8; 16];
        let (header, mut sender, _receiver) = generate_client_handshake(&secret, 2);

        // The proxy decrypts the header with the secret: rebuild its view.
        let mut key = Sha256::new();
        key.update(&header[8..40]);
        key.update(&secret);
        let key: [u8; 32] = key.finalize().into();
        let mut server_view = header;
        let mut cipher = make_cipher(&key, &header[40..56]);
        let mut keystream = [0u8; HANDSHAKE_LEN];
        cipher.apply_keystream(&mut keystream);
        for (i, byte) in server_view.iter_mut().enumerate() {
            *byte ^= keystream[i];
        }

        assert_eq!(server_view[56..60], PROTO_TAG_INTERMEDIATE);
        assert_eq!(i16::from_le_bytes([server_view[60], server_view[61]]), 2);

        // Whatever the sender encrypts must stay in sync with its own state.
        let mut a = b"first frame".to_vec();
        let mut b = a.clone();
        sender.apply_keystream(&mut a);
        assert_ne!(a, b);
        b.clear();
        assert!(b.is_empty());
    }
}