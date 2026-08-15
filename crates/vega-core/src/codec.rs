//! Canonical byte encoding for anything that gets signed.
//!
//! Signatures must cover exactly one byte string, and both signer and verifier
//! must derive it identically. `serde_json` cannot promise that — map ordering
//! and number formatting are not pinned by the format — so signed structures
//! are encoded by hand here instead. Every field is length-prefixed, so no two
//! distinct field sequences can produce the same bytes.

/// Append-only canonical writer.
#[derive(Default)]
pub struct Writer(Vec<u8>);

impl Writer {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// A single discriminant or tag byte.
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }

    /// Big-endian, fixed width — no varints, so length is never ambiguous.
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// Length-prefixed bytes. The prefix is what makes the encoding injective.
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(&(v.len() as u32).to_be_bytes());
        self.0.extend_from_slice(v);
        self
    }

    /// Fixed-width bytes, for values whose length is pinned by the type.
    pub fn fixed(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }

    pub fn str(&mut self, v: &str) -> &mut Self {
        self.bytes(v.as_bytes())
    }

    pub fn finish(&self) -> Vec<u8> {
        self.0.clone()
    }
}

/// Anything that can be signed knows how to write its own canonical form.
pub trait Canonical {
    fn write_canonical(&self, w: &mut Writer);

    fn canonical_bytes(&self) -> Vec<u8> {
        let mut w = Writer::new();
        self.write_canonical(&mut w);
        w.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_prefix_prevents_field_confusion() {
        // Without a length prefix, ("ab", "c") and ("a", "bc") would collide.
        let mut a = Writer::new();
        a.str("ab").str("c");

        let mut b = Writer::new();
        b.str("a").str("bc");

        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn encoding_is_stable_across_calls() {
        let mut a = Writer::new();
        a.u8(3).u64(7).str("x");
        let mut b = Writer::new();
        b.u8(3).u64(7).str("x");
        assert_eq!(a.finish(), b.finish());
    }
}
