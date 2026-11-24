//! Bedrock login: the connection request of the `login` packet, the Xbox Live identity, the
//! client data and the key exchange of the encryption handshake.
//!
//! Current clients send a multiplayer token issued by Minecraft's authorization service
//! (RS256, keys published as OpenID JWKS) holding the XUID, gamertag and the client's public
//! key (`cpk`). Older clients send a JWT chain rooted in Mojang's P-384 key. The client data
//! JWT is signed with the client key. Nothing in the request is trusted before its signature
//! chain is verified (SEC-02).

use p384::elliptic_curve::Generate;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use p384::ecdsa::signature::{Signer, Verifier};
use p384::pkcs8::{DecodePublicKey, EncodePublicKey};
use rsa::BoxedUint;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::io::Reader;

/// Mojang's root key of the legacy login chain (base64 DER).
const MOJANG_KEY: &str = "MHYwEAYHKoZIzj0CAQYFK4EEACIDYgAECRXueJeTDqNRRgJi/vlRufByu/2G0i2Ebt6YMar5QX/R0DIIyrJMcUpruK4QveTfJSTp3Shlq4Gk34cD/4GUWwkv0DVuzeuB+tXija7HBxii03NHDbPAD0AKnLr2wdAp";
/// Issuer of multiplayer tokens.
pub const ISSUER: &str = "https://authorization.franchise.minecraft-services.net";
/// Audience of multiplayer tokens.
const AUDIENCE: &str = "api://auth-minecraft-services/multiplayer";
/// Accepted clock skew.
const LEEWAY: u64 = 60;
/// Largest JWT accepted (a client data JWT carries the skin).
const MAX_JWT: usize = 4 << 20;

/// Why a login is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginError(pub String);

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// `v[key]` without indexing: a missing key or a non-object gives `Null`.
pub fn field<'a>(v: &'a Value, key: &str) -> &'a Value {
    static NULL: Value = Value::Null;
    v.get(key).unwrap_or(&NULL)
}

fn err<T>(msg: impl Into<String>) -> Result<T, LoginError> {
    Err(LoginError(msg.into()))
}

/// The verified identity of a Bedrock player.
#[derive(Debug, Clone)]
pub struct Identity {
    /// Xbox user id (empty if not authenticated, offline mode only).
    pub xuid: String,
    /// Gamertag.
    pub gamertag: String,
    /// Whether Xbox Live vouched for the identity.
    pub authenticated: bool,
    /// The client's key for the client data and the key exchange.
    pub client_key: p384::PublicKey,
}

/// Signing keys of the authorization service (RSA, by key id).
#[derive(Debug, Default)]
pub struct TokenKeys {
    keys: RwLock<HashMap<String, rsa::PublicKey>>,
    fetched: RwLock<Option<Instant>>,
}

impl TokenKeys {
    /// Keys from a JWKS document.
    pub fn set_from_jwks(&self, jwks: &Value) -> usize {
        let mut map = HashMap::new();
        for key in field(jwks, "keys").as_array().into_iter().flatten() {
            if field(key, "kty").as_str() != Some("RSA") {
                continue;
            }
            let (Some(kid), Some(n), Some(e)) = (
                field(key, "kid").as_str(),
                field(key, "n").as_str(),
                field(key, "e").as_str(),
            ) else {
                continue;
            };
            let (Ok(n), Ok(e)) = (URL_SAFE_NO_PAD.decode(n), URL_SAFE_NO_PAD.decode(e)) else {
                continue;
            };
            if let Ok(k) = rsa::PublicKey::new(
                BoxedUint::from_be_slice_vartime(&n),
                BoxedUint::from_be_slice_vartime(&e),
            ) {
                map.insert(kid.to_owned(), k);
            }
        }
        let n = map.len();
        if let Ok(mut g) = self.keys.write() {
            *g = map;
        }
        if let Ok(mut g) = self.fetched.write() {
            *g = Some(Instant::now());
        }
        n
    }

    fn get(&self, kid: &str) -> Option<rsa::PublicKey> {
        self.keys.read().ok()?.get(kid).cloned()
    }

    /// Whether the keys should be fetched again (never fetched, or older than `max_age`).
    pub fn stale(&self, max_age: Duration) -> bool {
        self.fetched
            .read()
            .ok()
            .and_then(|g| *g)
            .is_none_or(|t| t.elapsed() >= max_age)
    }

    /// Fetches the keys from the authorization service (blocking).
    pub fn fetch(&self, timeout: Duration) -> Result<usize, String> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .build()
            .new_agent();
        let get = |url: &str| -> Result<Value, String> {
            let body = agent
                .get(url)
                .header("Accept", "application/json")
                .call()
                .map_err(|e| e.to_string())?
                .body_mut()
                .with_config()
                .limit(1 << 20)
                .read_to_string()
                .map_err(|e| e.to_string())?;
            serde_json::from_str(&body).map_err(|e| e.to_string())
        };
        let config = get(&format!("{ISSUER}/.well-known/openid-configuration"))?;
        let uri = field(&config, "jwks_uri")
            .as_str()
            .filter(|u| u.starts_with(ISSUER))
            .ok_or("no jwks_uri")?
            .to_owned();
        Ok(self.set_from_jwks(&get(&uri)?))
    }
}

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A decoded, not yet verified JWT.
struct Jwt<'a> {
    signing_input: &'a str,
    header: Value,
    claims: Value,
    signature: Vec<u8>,
}

fn decode_part(part: &str) -> Result<Value, LoginError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part.trim_end_matches('='))
        .or_else(|_| err("bad JWT base64"))?;
    serde_json::from_slice(&bytes).or_else(|_| err("bad JWT JSON"))
}

fn parse_jwt(token: &str) -> Result<Jwt<'_>, LoginError> {
    if token.len() > MAX_JWT {
        return err("JWT too long");
    }
    let mut parts = token.split('.');
    let (Some(h), Some(c), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return err("malformed JWT");
    };
    let signing_input = token
        .get(..h.len().saturating_add(c.len()).saturating_add(1))
        .unwrap_or_default();
    Ok(Jwt {
        signing_input,
        header: decode_part(h)?,
        claims: decode_part(c)?,
        signature: URL_SAFE_NO_PAD
            .decode(s.trim_end_matches('='))
            .or_else(|_| err("bad JWT signature"))?,
    })
}

impl Jwt<'_> {
    fn verify_es384(&self, key: &p384::PublicKey) -> Result<(), LoginError> {
        if field(&self.header, "alg").as_str() != Some("ES384") {
            return err("unexpected JWT algorithm");
        }
        let sig = p384::ecdsa::Signature::from_slice(&self.signature)
            .or_else(|_| err("bad ES384 signature"))?;
        p384::ecdsa::VerifyingKey::from(key)
            .verify(self.signing_input.as_bytes(), &sig)
            .or_else(|_| err("JWT signature does not verify"))
    }

    fn verify_rs256(&self, keys: &TokenKeys) -> Result<(), LoginError> {
        if field(&self.header, "alg").as_str() != Some("RS256") {
            return err("unexpected token algorithm");
        }
        let kid = field(&self.header, "kid").as_str().unwrap_or_default();
        let Some(key) = keys.get(kid) else {
            return err("token signed with an unknown key");
        };
        let sig = rsa::pkcs1v15::Signature::try_from(self.signature.as_slice())
            .or_else(|_| err("bad RS256 signature"))?;
        rsa::pkcs1v15::VerifyingKey::<Sha256>::new(key)
            .verify(self.signing_input.as_bytes(), &sig)
            .or_else(|_| err("token signature does not verify"))
    }

    fn check_time(&self, now: u64) -> Result<(), LoginError> {
        if let Some(exp) = field(&self.claims, "exp").as_u64()
            && now > exp.saturating_add(LEEWAY)
        {
            return err("token expired");
        }
        if let Some(nbf) = field(&self.claims, "nbf").as_u64()
            && now.saturating_add(LEEWAY) < nbf
        {
            return err("token not valid yet");
        }
        Ok(())
    }
}

/// Parses a base64 DER public key.
pub fn parse_key(b64: &str) -> Result<p384::PublicKey, LoginError> {
    let der = STANDARD.decode(b64).or_else(|_| err("bad key base64"))?;
    p384::PublicKey::from_public_key_der(&der).or_else(|_| err("bad public key"))
}

/// Base64 DER of a public key.
pub fn encode_key(key: &p384::PublicKey) -> String {
    key.to_public_key_der()
        .map(|d| STANDARD.encode(d.as_bytes()))
        .unwrap_or_default()
}

/// The connection request of a `login` packet, verified.
#[derive(Debug)]
pub struct Request {
    /// The player.
    pub identity: Identity,
    /// The client data claims (skin, device, language); signed by the client key.
    pub client_data: Value,
}

/// Verifies a connection request. `online` requires an Xbox Live identity.
pub fn verify(
    request: &[u8],
    keys: &TokenKeys,
    online: bool,
    now: u64,
) -> Result<Request, LoginError> {
    let mut r = Reader::new(request);
    let chain_len = usize::try_from(r.u32().or_else(|_| err("short request"))?).unwrap_or(0);
    let chain = r.take(chain_len).or_else(|_| err("short request"))?;
    let raw_len = usize::try_from(r.u32().or_else(|_| err("short request"))?).unwrap_or(0);
    let raw = r.take(raw_len).or_else(|_| err("short request"))?;
    if r.remaining() != 0 {
        return err("trailing bytes in the login request");
    }
    let outer: Value = serde_json::from_slice(chain).or_else(|_| err("bad request JSON"))?;
    if field(&outer, "AuthenticationType").as_u64() == Some(1) {
        return err("guest logins are not supported");
    }
    let certificate: Value = match field(&outer, "Certificate").as_str() {
        Some(c) if !c.is_empty() => serde_json::from_str(c).or_else(|_| err("bad certificate"))?,
        _ => outer.clone(),
    };
    let token = field(&outer, "Token").as_str().filter(|t| !t.is_empty());
    let chain: Vec<&str> = field(&certificate, "chain")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|s| !s.is_empty())
        .collect();

    let identity = match token {
        Some(token) => verify_token(token, keys, online, now)?,
        None => verify_chain(&chain, now)?,
    };
    if online && !identity.authenticated {
        return err("not signed in to Xbox Live");
    }
    let raw = std::str::from_utf8(raw).or_else(|_| err("bad client data"))?;
    let data = parse_jwt(raw)?;
    data.verify_es384(&identity.client_key)?;
    Ok(Request {
        identity,
        client_data: data.claims,
    })
}

fn verify_token(
    token: &str,
    keys: &TokenKeys,
    online: bool,
    now: u64,
) -> Result<Identity, LoginError> {
    let jwt = parse_jwt(token)?;
    let authenticated = if online {
        jwt.verify_rs256(keys)?;
        let iss = field(&jwt.claims, "iss").as_str().unwrap_or_default();
        if iss.trim_end_matches('/') != ISSUER {
            return err("token from an unknown issuer");
        }
        let aud = &field(&jwt.claims, "aud");
        let audience_ok = aud.as_str() == Some(AUDIENCE)
            || aud
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(AUDIENCE)));
        if !audience_ok {
            return err("token for another audience");
        }
        true
    } else {
        // Offline mode (tests and trusted networks): identity as the client claims it.
        false
    };
    jwt.check_time(now)?;
    let claims = &jwt.claims;
    let xuid = field(claims, "xid").as_str().unwrap_or_default().to_owned();
    let gamertag = field(claims, "xname")
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let client_key = parse_key(field(claims, "cpk").as_str().unwrap_or_default())?;
    if online && (xuid.is_empty() || gamertag.is_empty()) {
        return err("token without an Xbox identity");
    }
    Ok(Identity {
        xuid,
        gamertag,
        authenticated,
        client_key,
    })
}

fn verify_chain(chain: &[&str], now: u64) -> Result<Identity, LoginError> {
    let mojang = parse_key(MOJANG_KEY)?;
    let Some(first) = chain.first() else {
        return err("empty login chain");
    };
    let first = parse_jwt(first)?;
    let mut key = parse_key(field(&first.header, "x5u").as_str().unwrap_or_default())?;
    let mut authenticated = false;
    let mut extra = Value::Null;
    if chain.len() > 3 {
        return err("login chain too long");
    }
    for (i, token) in chain.iter().enumerate() {
        let jwt = parse_jwt(token)?;
        jwt.verify_es384(&key)?;
        jwt.check_time(now)?;
        if i == 1 && key == mojang {
            authenticated = true;
        }
        if let Some(next) = field(&jwt.claims, "identityPublicKey").as_str() {
            key = parse_key(next)?;
        }
        if field(&jwt.claims, "extraData").is_object() {
            extra = field(&jwt.claims, "extraData").clone();
        }
    }
    let authenticated = authenticated && chain.len() == 3;
    Ok(Identity {
        xuid: field(&extra, "XUID")
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        gamertag: field(&extra, "displayName")
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        authenticated,
        client_key: key,
    })
}

/// The Java profile of a Bedrock player: UUID `00000000-0000-0000-<XUID as 64 bits>` like
/// Floodgate, name = prefix + gamertag (spaces as `_`, at most 16 characters).
pub fn java_profile(identity: &Identity, prefix: &str) -> (u128, String) {
    let mut name: String = prefix
        .chars()
        .chain(
            identity
                .gamertag
                .chars()
                .map(|c| if c == ' ' { '_' } else { c }),
        )
        .filter(|c| ('!'..='~').contains(c))
        .take(16)
        .collect();
    if name.is_empty() {
        name.push_str("BedrockPlayer");
    }
    let uuid = match identity.xuid.parse::<u64>() {
        Ok(xuid) if identity.authenticated => u128::from(xuid),
        _ => vanillite_protocol::types::Uuid::offline(&name).0,
    };
    (uuid, name)
}

/// The server half of the key exchange.
#[derive(Debug)]
pub struct Handshake {
    /// The `server_to_client_handshake` JWT.
    pub jwt: String,
    /// The AES key both sides derive.
    pub key: [u8; 32],
}

/// Generates a server key and salt, derives the shared AES key and signs the handshake JWT.
pub fn handshake(client: &p384::PublicKey) -> Result<Handshake, LoginError> {
    let secret = <p384::SecretKey as Generate>::generate();
    let public = secret.public_key();
    let salt: [u8; 16] = vanillite_net::rand_bytes();
    let shared = p384::ecdh::diffie_hellman(secret.to_nonzero_scalar(), client.as_affine());
    let mut h = Sha256::new();
    h.update(salt);
    h.update(shared.raw_secret_bytes());
    let key: [u8; 32] = h.finalize().into();

    let header = serde_json::json!({ "alg": "ES384", "x5u": encode_key(&public) });
    let claims = serde_json::json!({ "salt": STANDARD.encode(salt) });
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let signer = p384::ecdsa::SigningKey::from(&secret);
    let sig: p384::ecdsa::Signature = signer.sign(input.as_bytes());
    Ok(Handshake {
        jwt: format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes())),
        key,
    })
}

/// Client side of the key exchange (test client): the AES key from the handshake JWT.
pub fn client_key_from_handshake(
    jwt: &str,
    secret: &p384::SecretKey,
) -> Result<[u8; 32], LoginError> {
    let jwt = parse_jwt(jwt)?;
    let server = parse_key(field(&jwt.header, "x5u").as_str().unwrap_or_default())?;
    jwt.verify_es384(&server)?;
    let salt = STANDARD
        .decode(field(&jwt.claims, "salt").as_str().unwrap_or_default())
        .or_else(|_| err("bad salt"))?;
    let shared = p384::ecdh::diffie_hellman(secret.to_nonzero_scalar(), server.as_affine());
    let mut h = Sha256::new();
    h.update(&salt);
    h.update(shared.raw_secret_bytes());
    Ok(h.finalize().into())
}

/// Signs `claims` as an ES384 JWT with `x5u` = the key's public half (test client).
pub fn sign_jwt(secret: &p384::SecretKey, claims: &Value) -> String {
    let header = serde_json::json!({ "alg": "ES384", "x5u": encode_key(&secret.public_key()) });
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let sig: p384::ecdsa::Signature = p384::ecdsa::SigningKey::from(secret).sign(input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
}

/// An offline connection request like a client in offline mode builds it (test client):
/// a self-signed multiplayer token and client data.
pub fn offline_request(secret: &p384::SecretKey, xuid: &str, gamertag: &str) -> Vec<u8> {
    let now = now_secs();
    let token = sign_jwt(
        secret,
        &serde_json::json!({
            "xid": xuid, "xname": gamertag, "cpk": encode_key(&secret.public_key()),
            "aud": AUDIENCE, "exp": now.saturating_add(3600), "nbf": now.saturating_sub(3600),
        }),
    );
    let outer = serde_json::json!({
        "AuthenticationType": 2,
        "Certificate": serde_json::json!({ "chain": [] }).to_string(),
        "Token": token,
    })
    .to_string();
    let data = sign_jwt(
        secret,
        &serde_json::json!({ "DeviceOS": 7, "LanguageCode": "en_US", "SkinId": "test" }),
    );
    let mut out = Vec::new();
    for part in [outer.as_bytes(), data.as_bytes()] {
        out.extend_from_slice(&u32::try_from(part.len()).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(part);
    }
    out
}

/// Keeps the token keys fresh in the background (every 30 minutes; retries after failures).
pub fn spawn_key_refresh(keys: Arc<TokenKeys>, runtime: &tokio::runtime::Handle) {
    runtime.spawn(async move {
        loop {
            if keys.stale(Duration::from_secs(1800)) {
                let k = Arc::clone(&keys);
                match tokio::task::spawn_blocking(move || k.fetch(Duration::from_secs(10))).await {
                    Ok(Ok(n)) => log::debug!("Fetched {n} Bedrock token key(s)"),
                    Ok(Err(e)) => log::warn!("Cannot fetch the Bedrock token keys: {e}"),
                    Err(_) => {}
                }
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn offline_request_verifies_offline_only() {
        let secret = <p384::SecretKey as Generate>::generate();
        let req = offline_request(&secret, "2535400000000001", "Some Tag");
        let keys = TokenKeys::default();
        let ok = verify(&req, &keys, false, now_secs()).unwrap();
        assert_eq!(ok.identity.gamertag, "Some Tag");
        assert!(!ok.identity.authenticated);
        assert_eq!(field(&ok.client_data, "LanguageCode"), "en_US");
        assert!(verify(&req, &keys, true, now_secs()).is_err());
        // Offline identities get offline UUIDs, never the XUID mapping.
        let (uuid, name) = java_profile(&ok.identity, ".");
        assert_eq!(name, ".Some_Tag");
        assert_eq!(
            uuid,
            vanillite_protocol::types::Uuid::offline(".Some_Tag").0
        );
    }

    #[test]
    fn rejects_tampered_client_data() {
        let secret = <p384::SecretKey as Generate>::generate();
        let other = <p384::SecretKey as Generate>::generate();
        let mut req = offline_request(&secret, "1", "A");
        // Replace the client data with one signed by another key.
        let data = sign_jwt(&other, &serde_json::json!({}));
        let chain_len = usize::try_from(u32::from_le_bytes(req[..4].try_into().unwrap())).unwrap();
        req.truncate(4 + chain_len);
        req.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
        req.extend_from_slice(data.as_bytes());
        assert!(verify(&req, &TokenKeys::default(), false, now_secs()).is_err());
    }

    #[test]
    fn key_exchange_agrees() {
        let client = <p384::SecretKey as Generate>::generate();
        let hs = handshake(&client.public_key()).unwrap();
        assert_eq!(client_key_from_handshake(&hs.jwt, &client).unwrap(), hs.key);
    }

    #[test]
    fn xbox_identities_map_like_floodgate() {
        let secret = <p384::SecretKey as Generate>::generate();
        let id = Identity {
            xuid: "2535428119340356".into(),
            gamertag: "A Very Long Gamertag".into(),
            authenticated: true,
            client_key: secret.public_key(),
        };
        let (uuid, name) = java_profile(&id, ".");
        assert_eq!(uuid, 2_535_428_119_340_356);
        assert_eq!(name, ".A_Very_Long_Gam");
    }
}
