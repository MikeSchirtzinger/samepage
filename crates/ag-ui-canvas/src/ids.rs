//! Stable, coordination-free object identifiers.
//!
//! IDs embed the minting Doc's yrs client id, so two peers can never mint the
//! same id without a round-trip — the same guarantee yrs uses internally for
//! CRDT block ids, surfaced into the scene-graph key space.

use std::fmt;

/// Key of a scene object inside the root `"objects"` map.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(String);

impl ObjectId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for ObjectId {
    fn from(s: String) -> Self {
        ObjectId(s)
    }
}

impl From<&str> for ObjectId {
    fn from(s: &str) -> Self {
        ObjectId(s.to_string())
    }
}

/// Mint a collision-free object id: `{client_id:016x}-{seq:08x}`.
///
/// `client_id` is the minting Doc's yrs client id (random per Doc instance);
/// `seq` is a per-session monotonic counter kept by [`crate::scene::Scene`].
pub fn mint_object_id(client_id: u64, seq: u32) -> ObjectId {
    ObjectId(format!("{client_id:016x}-{seq:08x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_is_deterministic_and_distinct() {
        let a = mint_object_id(0xDEAD_BEEF, 0);
        let b = mint_object_id(0xDEAD_BEEF, 1);
        let c = mint_object_id(0xFEED_FACE, 0);
        assert_eq!(a.as_str(), "00000000deadbeef-00000000");
        assert_ne!(a, b);
        assert_ne!(a, c);
    }
}
