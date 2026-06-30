//! Personal access tokens used for `docker login` and API automation.
//!
//! A PAT is a high-entropy random secret, so we store only its SHA-256 hash
//! (not Argon2 — fast lookup is fine for non-guessable secrets). The plaintext
//! is shown to the user exactly once.

use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Human-facing prefix so tokens are recognizable in logs and the UI.
pub const PAT_PREFIX: &str = "rsk_";

/// A freshly minted token: the plaintext to show once, plus what we persist.
pub struct NewPat {
    /// Full secret string (`rsk_<base64url>`), shown to the user once.
    pub plaintext: String,
    /// Short identifying prefix stored for display.
    pub display_prefix: String,
    /// SHA-256 hex of the full plaintext, stored for verification.
    pub hash: String,
}

/// Generate a new PAT with 32 bytes of entropy.
pub fn generate() -> NewPat {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let plaintext = format!("{PAT_PREFIX}{body}");
    let display_prefix = plaintext.chars().take(PAT_PREFIX.len() + 6).collect();
    NewPat {
        hash: hash(&plaintext),
        display_prefix,
        plaintext,
    }
}

/// Compute the stored hash for a plaintext token.
pub fn hash(plaintext: &str) -> String {
    let mut h = Sha256::new();
    h.update(plaintext.as_bytes());
    hex::encode(h.finalize())
}
