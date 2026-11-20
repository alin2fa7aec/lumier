use std::fmt;
use std::str::FromStr;

/// Opaque, stable handle for one image.
///
/// Derived from the image's root-relative path, so it stays the same across
/// restarts and rescans as long as the file keeps its place. The API addresses
/// images only by this id and never by path, which keeps filesystem paths out
/// of URLs entirely.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageId([u8; Self::LEN]);

impl ImageId {
    const LEN: usize = 16;

    pub fn for_rel_path(rel_path: &str) -> Self {
        let digest = blake3::hash(rel_path.as_bytes());
        let mut bytes = [0u8; Self::LEN];
        bytes.copy_from_slice(&digest.as_bytes()[..Self::LEN]);
        Self(bytes)
    }
}

impl fmt::Display for ImageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ImageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ImageId({self})")
    }
}

#[derive(Debug, thiserror::Error)]
#[error("image id must be {} hex characters", ImageId::LEN * 2)]
pub struct ParseImageIdError;

impl FromStr for ImageId {
    type Err = ParseImageIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Index the raw bytes rather than slicing the &str: a 32-byte string is
        // not necessarily 32 characters, and slicing off a char boundary panics.
        let hex = s.as_bytes();
        if hex.len() != Self::LEN * 2 {
            return Err(ParseImageIdError);
        }
        let mut bytes = [0u8; Self::LEN];
        for (i, byte) in bytes.iter_mut().enumerate() {
            let hi = (hex[2 * i] as char).to_digit(16).ok_or(ParseImageIdError)?;
            let lo = (hex[2 * i + 1] as char)
                .to_digit(16)
                .ok_or(ParseImageIdError)?;
            *byte = (hi << 4 | lo) as u8;
        }
        Ok(Self(bytes))
    }
}

impl serde::Serialize for ImageId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_for_the_same_path() {
        assert_eq!(
            ImageId::for_rel_path("photos/a.jpg"),
            ImageId::for_rel_path("photos/a.jpg")
        );
        assert_ne!(
            ImageId::for_rel_path("photos/a.jpg"),
            ImageId::for_rel_path("photos/b.jpg")
        );
    }

    #[test]
    fn display_and_parse_round_trip() {
        let id = ImageId::for_rel_path("photos/a.jpg");
        let text = id.to_string();
        assert_eq!(text.len(), 32);
        assert_eq!(text.parse::<ImageId>().unwrap(), id);
    }

    #[test]
    fn rejects_malformed_ids() {
        assert!("".parse::<ImageId>().is_err());
        assert!("abc".parse::<ImageId>().is_err());
        assert!("z".repeat(32).parse::<ImageId>().is_err());
        // Exactly 32 bytes but only 12 characters, so it clears the length
        // check and then exercises the byte indexing: must not panic.
        let multibyte = "あいうえおかきくけこab";
        assert_eq!(multibyte.len(), 32);
        assert!(multibyte.parse::<ImageId>().is_err());
    }
}
