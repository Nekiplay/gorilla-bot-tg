//! Stream framing fixes for `grammers` clients.
//!
//! Two constructs of the intermediate transport would otherwise break the
//! proxy, and both are handled here.

/// Transport tag `0xeeeeeeee` that `grammers` prepends to the first frame.
const TAG: [u8; 4] = [0xee, 0xee, 0xee, 0xee];

/// Removes the one-off transport tag from the start of a client stream.
///
/// `grammers-mtproto`'s intermediate transport emits `ee ee ee ee` once, right
/// before the first frame, while carrying the tag inside the obfuscated2 header.
/// The proxy reads those four bytes as a frame length (0x7ffffffe), waits for a
/// packet that never arrives and drops the connection without answering, so the
/// bridge deletes them before anything is forwarded.
///
/// The removal happens at most once per connection: `stripped` is the caller's
/// flag and is set even when the prefix is not a tag, because only the very
/// first bytes of a stream can carry one.
///
/// # Examples
///
/// ```
/// use tgws_bridge::strip_transport_tag;
///
/// let mut stream = vec![0xee, 0xee, 0xee, 0xee, 0x28, 0, 0, 0];
/// let mut stripped = false;
/// strip_transport_tag(&mut stream, &mut stripped);
/// assert_eq!(stream, vec![0x28, 0, 0, 0]);
/// assert!(stripped);
///
/// // A second call is a no-op.
/// strip_transport_tag(&mut stream, &mut stripped);
/// assert_eq!(stream, vec![0x28, 0, 0, 0]);
/// ```
pub fn strip_transport_tag(stream: &mut Vec<u8>, stripped: &mut bool) {
    if *stripped || stream.len() < TAG.len() {
        return;
    }
    if stream[..TAG.len()] == TAG {
        stream.drain(..TAG.len());
    }
    *stripped = true;
}

/// Finds the offset where the first complete framed packet starts.
///
/// Upstream responses are intermediate-framed (`u32` little-endian length that
/// counts itself, followed by the body), but some routes prepend leftover bytes
/// — most commonly a 64-byte echo of the generated handshake — and others
/// prepend nothing. Instead of assuming either layout, the scan walks in 4-byte
/// steps and accepts the first plausible, fully-received frame.
///
/// Returns `None` while no complete frame is visible.
///
/// # Examples
///
/// ```
/// use tgws_bridge::first_frame_offset;
///
/// // A 104-byte frame preceded by the 64-byte echo.
/// let mut stream = vec![0u8; 64];
/// stream.extend_from_slice(&104u32.to_le_bytes());
/// stream.extend_from_slice(&[7u8; 100]);
/// assert_eq!(first_frame_offset(&stream, 8 * 1024 * 1024), Some(64));
///
/// // Nothing complete yet.
/// assert_eq!(first_frame_offset(&[1, 2], 8 * 1024 * 1024), None);
/// ```
pub fn first_frame_offset(stream: &[u8], cap: usize) -> Option<usize> {
    const SCAN_LIMIT: usize = 256;
    let mut off = 0;
    while off + 4 <= stream.len().min(SCAN_LIMIT) {
        let len = u32::from_le_bytes([
            stream[off],
            stream[off + 1],
            stream[off + 2],
            stream[off + 3],
        ]) as usize;
        if len >= 4 && len <= cap && stream.len() >= off + len {
            return Some(off);
        }
        off += 4;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_is_removed_once() {
        let mut stream = vec![0xee, 0xee, 0xee, 0xee, 1, 2, 3, 4];
        let mut stripped = false;
        strip_transport_tag(&mut stream, &mut stripped);
        assert_eq!(stream, vec![1, 2, 3, 4]);
        assert!(stripped);

        let mut stream = vec![0xee, 0xee, 0xee, 0xee, 1];
        strip_transport_tag(&mut stream, &mut stripped);
        assert_eq!(stream, vec![0xee, 0xee, 0xee, 0xee, 1]);
    }

    #[test]
    fn short_prefix_is_left_alone() {
        let mut stream = vec![0xee, 0xee];
        let mut stripped = false;
        strip_transport_tag(&mut stream, &mut stripped);
        assert_eq!(stream, vec![0xee, 0xee]);
        assert!(!stripped);

        // Once the stream is long enough the tag is still recognised.
        stream.push(0xee);
        stream.push(0xee);
        strip_transport_tag(&mut stream, &mut stripped);
        assert!(stream.is_empty());
        assert!(stripped);
    }

    #[test]
    fn non_tag_prefix_is_skipped_not_removed() {
        let mut stream = vec![0x28, 0, 0, 0, 0, 0, 0, 0];
        let mut stripped = false;
        strip_transport_tag(&mut stream, &mut stripped);
        assert_eq!(stream, vec![0x28, 0, 0, 0, 0, 0, 0, 0]);
        assert!(stripped);
    }

    #[test]
    fn first_frame_without_echo() {
        let mut stream = 20u32.to_le_bytes().to_vec();
        stream.extend_from_slice(&[0xab; 16]);
        assert_eq!(first_frame_offset(&stream, 4096), Some(0));
    }

    #[test]
    fn first_frame_after_echo() {
        let mut stream = vec![0xff; 64];
        stream.extend_from_slice(&20u32.to_le_bytes());
        stream.extend_from_slice(&[0xab; 16]);
        assert_eq!(first_frame_offset(&stream, 4096), Some(64));
    }

    #[test]
    fn partial_frame_is_not_reported() {
        let mut stream = 20u32.to_le_bytes().to_vec();
        stream.extend_from_slice(&[0xab; 8]);
        assert_eq!(first_frame_offset(&stream, 4096), None);
    }

    #[test]
    fn dumb_lengths_are_rejected() {
        // 0xeeeeeeee & ... is far above any sane cap.
        let mut stream = vec![0xee; 4];
        stream.extend_from_slice(&[0u8; 4096]);
        assert_eq!(first_frame_offset(&stream, 1024), None);
        // Too small to contain its own length.
        let mut stream = 2u32.to_le_bytes().to_vec();
        stream.extend_from_slice(&[0u8; 4096]);
        assert_eq!(first_frame_offset(&stream, 8 * 1024 * 1024), None);
    }
}