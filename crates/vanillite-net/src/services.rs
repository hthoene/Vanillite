//! Mojang's services public keys (`YggdrasilServicesKeyInfo`): the keys that sign players'
//! chat profile keys, fetched once at startup like Vanilla.

use std::time::Duration;

/// Where Vanilla's authlib fetches the services key set.
pub const PUBLIC_KEYS_URL: &str = "https://api.minecraftservices.com/publickeys";

/// Fetches the profile key signers (`playerCertificateKeys`) as X.509 DER.
pub fn fetch_profile_keys(timeout: Duration) -> Result<Vec<Vec<u8>>, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .new_agent();
    let body = agent
        .get(PUBLIC_KEYS_URL)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    parse_profile_keys(&body)
}

/// Parses the key set JSON.
pub fn parse_profile_keys(body: &str) -> Result<Vec<Vec<u8>>, String> {
    let json: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let keys = json
        .get("playerCertificateKeys")
        .and_then(serde_json::Value::as_array)
        .ok_or("missing playerCertificateKeys")?;
    Ok(keys
        .iter()
        .filter_map(|k| k.get("publicKey").and_then(serde_json::Value::as_str))
        .filter_map(base64_decode)
        .collect())
}

/// Standard Base64 with optional padding; `None` on invalid input.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(text.len());
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            break;
        }
        let v = ALPHABET.iter().position(|&a| a == c)?;
        acc = acc.wrapping_shl(6) | u32::try_from(v).ok()?;
        bits = bits.wrapping_add(6);
        if bits >= 8 {
            bits = bits.wrapping_sub(8);
            out.push(acc.wrapping_shr(bits).to_le_bytes().first().copied()?);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64() {
        assert_eq!(base64_decode("aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(base64_decode("aGVsbG8hIQ"), Some(b"hello!!".to_vec()));
        assert_eq!(base64_decode("a$"), None);
    }

    #[test]
    fn parses_key_set() {
        let keys = parse_profile_keys(
            r#"{"profilePropertyKeys":[{"publicKey":"AAAA"}],"playerCertificateKeys":[{"publicKey":"aGVsbG8="},{"publicKey":"AQID"}]}"#,
        );
        assert_eq!(keys, Ok(vec![b"hello".to_vec(), vec![1, 2, 3]]));
    }
}
