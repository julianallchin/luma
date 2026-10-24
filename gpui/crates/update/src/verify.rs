//! The signature check every downloaded archive passes before it is unpacked.
//!
//! HTTPS says the bytes came from GitHub; this says they came from whoever
//! holds the release key. Without it, anyone able to publish a release asset
//! (a leaked token, a compromised workflow) could ship code to every install.
//!
//! The key and the signature are each the base64 of a whole minisign file,
//! comment lines included, which is what the release workflow's signer
//! writes.

use std::io::Read;
use std::path::Path;

use base64::Engine as _;
use minisign_verify::{PublicKey, Signature};

/// The release public key (minisign key id `D0F3AAE2A309160A`). A build can
/// embed another through `LUMA_UPDATE_PUBKEY` — see `RELEASING.md`.
pub const PUBLIC_KEY: &str = match option_env!("LUMA_UPDATE_PUBKEY") {
    Some(key) => key,
    None => "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEQwRjNBQUUyQTMwOTE2MEEKUldRS0ZnbWo0cXJ6ME5zZzlVdlVuRisxRk9oSzNQSDUwTm1CWHdId09uYlF3ekNhYWpJQVdybmwK",
};

/// Check `file` against `signature`, both base64-encoded minisign files.
pub fn verify_file(file: &Path, signature: &str, public_key: &str) -> Result<(), String> {
    let key = PublicKey::decode(&decode(public_key, "public key")?)
        .map_err(|error| format!("bad update public key: {error}"))?;
    let signature = Signature::decode(&decode(signature, "signature")?)
        .map_err(|error| format!("bad update signature: {error}"))?;
    let mut verifier = key
        .verify_stream(&signature)
        .map_err(|error| format!("update signature does not match the release key: {error}"))?;
    let mut reader = std::fs::File::open(file)
        .map_err(|error| format!("could not read {}: {error}", file.display()))?;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let read = reader
            .read(&mut buf)
            .map_err(|error| format!("could not read {}: {error}", file.display()))?;
        if read == 0 {
            break;
        }
        verifier.update(&buf[..read]);
    }
    verifier
        .finalize()
        .map_err(|error| format!("update signature is not valid: {error}"))
}

fn decode(text: &str, what: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|error| format!("update {what} is not base64: {error}"))?;
    String::from_utf8(bytes).map_err(|_| format!("update {what} is not text"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    /// A throwaway key and the signature of `data` under it, encoded as the
    /// release workflow publishes them.
    pub(crate) fn sign(data: &[u8]) -> (String, String) {
        let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let signature = minisign::sign(Some(&pair.pk), &pair.sk, data, None, None).unwrap();
        let public = pair.pk.to_box().unwrap().into_string();
        (
            STANDARD.encode(public),
            STANDARD.encode(signature.into_string()),
        )
    }

    fn file(data: &[u8]) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), data).unwrap();
        file
    }

    #[test]
    fn accepts_what_the_key_signed() {
        let (key, signature) = sign(b"release");
        verify_file(file(b"release").path(), &signature, &key).unwrap();
    }

    #[test]
    fn refuses_altered_bytes() {
        let (key, signature) = sign(b"release");
        assert!(verify_file(file(b"releasf").path(), &signature, &key).is_err());
    }

    #[test]
    fn refuses_another_key() {
        let (_, signature) = sign(b"release");
        let (other, _) = sign(b"release");
        assert!(verify_file(file(b"release").path(), &signature, &other).is_err());
    }

    #[test]
    fn refuses_garbage_signature() {
        let (key, _) = sign(b"release");
        assert!(verify_file(file(b"release").path(), "not base64!", &key).is_err());
    }

    /// The embedded key must decode, or every update fails at the last step.
    #[test]
    fn embedded_key_decodes() {
        PublicKey::decode(&decode(PUBLIC_KEY, "public key").unwrap()).unwrap();
    }
}
