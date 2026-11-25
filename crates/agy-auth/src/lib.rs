use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

pub const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const GENERATIVE_LANGUAGE_SCOPE: &str =
    "https://www.googleapis.com/auth/generative-language.retriever https://www.googleapis.com/auth/cloud-platform";

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("Missing OAuth Client ID: enter your Google Cloud Installed/Android OAuth Client ID in Config first")]
    MissingClientId,
    #[error("OAuth state mismatch: callback state did not match pending PKCE session")]
    StateMismatch,
    #[error("Invalid redirect URL or missing authorization code")]
    MissingAuthCode,
    #[error("Token seal/unseal failure (Android Keystore key mismatch or corrupted blob)")]
    CryptoFailure,
    #[error("Not authenticated: sign in via Google OAuth PKCE or configure an authorized API key")]
    NotAuthenticated,
    #[error("Authentication rejected by Google ({status}): {detail}")]
    RemoteAuthRejected { status: u16, detail: String },
    #[error("Network error during authentication: {0}")]
    Network(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Invalid URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("Serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

// Prevents tokens from ever leaking via Debug/Display formatting or panic messages.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretToken(String);

impl SecretToken {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED_TOKEN]")
    }
}

impl fmt::Display for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED_TOKEN]")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkceSession {
    pub client_id: String,
    pub code_verifier: SecretToken,
    pub code_challenge: String,
    pub state: String,
    pub redirect_uri: String,
    pub authorization_url: String,
    pub created_at_ms: u64,
}

impl PkceSession {
    pub fn generate(client_id: &str, redirect_uri: &str, scopes: &str) -> Result<Self, AuthError> {
        let trimmed_id = client_id.trim();
        if trimmed_id.is_empty() {
            return Err(AuthError::MissingClientId);
        }

        let mut verifier_bytes = [0u8; 32];
        let mut state_bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut verifier_bytes);
        rand::thread_rng().fill_bytes(&mut state_bytes);

        let code_verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
        let challenge_hash = Sha256::digest(code_verifier.as_bytes());
        let code_challenge = URL_SAFE_NO_PAD.encode(challenge_hash);
        let state = hex::encode(state_bytes);

        let mut url = Url::parse(GOOGLE_AUTH_URL)?;
        url.query_pairs_mut()
            .append_pair("client_id", trimmed_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", scopes)
            .append_pair("code_challenge", &code_challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .append_pair("state", &state);

        Ok(Self {
            client_id: trimmed_id.to_string(),
            code_verifier: SecretToken::new(code_verifier),
            code_challenge,
            state,
            redirect_uri: redirect_uri.to_string(),
            authorization_url: url.to_string(),
            created_at_ms: now_ms(),
        })
    }

    pub fn verify_and_extract_code(&self, callback_url_or_code: &str) -> Result<String, AuthError> {
        let input = callback_url_or_code.trim();
        if input.starts_with("http://") || input.starts_with("https://") || input.contains("://") {
            let parsed = Url::parse(input)?;
            let mut extracted_code = None;
            let mut extracted_state = None;
            for (k, v) in parsed.query_pairs() {
                if k == "code" {
                    extracted_code = Some(v.into_owned());
                } else if k == "state" {
                    extracted_state = Some(v.into_owned());
                }
            }
            if let Some(st) = extracted_state {
                if st != self.state {
                    return Err(AuthError::StateMismatch);
                }
            } else {
                return Err(AuthError::StateMismatch);
            }
            extracted_code.filter(|c| !c.is_empty()).ok_or(AuthError::MissingAuthCode)
        } else if !input.is_empty() && !input.contains(' ') {
            Ok(input.to_string())
        } else {
            Err(AuthError::MissingAuthCode)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredCredential {
    OAuthTokens {
        client_id: String,
        access_token: SecretToken,
        refresh_token: Option<SecretToken>,
        expires_at_ms: Option<u64>,
        token_type: String,
    },
    GeminiApiKey {
        api_key: SecretToken,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthStatusSummary {
    pub authenticated: bool,
    pub method: String,
    pub expires_at_ms: Option<u64>,
    pub has_refresh_token: bool,
    pub keystore_protected: bool,
}

#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    token_type: Option<String>,
}

pub async fn exchange_pkce_code(
    http: &reqwest::Client,
    token_endpoint: &str,
    session: &PkceSession,
    authorization_code: &str,
) -> Result<StoredCredential, AuthError> {
    let endpoint = if token_endpoint.is_empty() {
        GOOGLE_TOKEN_URL
    } else {
        token_endpoint
    };
    let form = [
        ("client_id", session.client_id.as_str()),
        ("code", authorization_code),
        ("code_verifier", session.code_verifier.expose_secret()),
        ("grant_type", "authorization_code"),
        ("redirect_uri", session.redirect_uri.as_str()),
    ];

    let resp = http.post(endpoint).form(&form).send().await?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let body = scrub_sensitive_text(&resp.text().await.unwrap_or_default());
        return Err(AuthError::RemoteAuthRejected {
            status,
            detail: body.chars().take(240).collect(),
        });
    }

    let parsed: OAuthTokenResponse = resp.json().await?;
    let expires_at_ms = parsed.expires_in.map(|secs| now_ms() + secs * 1000);
    Ok(StoredCredential::OAuthTokens {
        client_id: session.client_id.clone(),
        access_token: SecretToken::new(parsed.access_token),
        refresh_token: parsed.refresh_token.map(SecretToken::new),
        expires_at_ms,
        token_type: parsed.token_type.unwrap_or_else(|| "Bearer".to_string()),
    })
}

pub async fn refresh_oauth_credential(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    refresh_token: &SecretToken,
) -> Result<StoredCredential, AuthError> {
    let endpoint = if token_endpoint.is_empty() {
        GOOGLE_TOKEN_URL
    } else {
        token_endpoint
    };
    let form = [
        ("client_id", client_id),
        ("refresh_token", refresh_token.expose_secret()),
        ("grant_type", "refresh_token"),
    ];

    let resp = http.post(endpoint).form(&form).send().await?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let body = scrub_sensitive_text(&resp.text().await.unwrap_or_default());
        return Err(AuthError::RemoteAuthRejected {
            status,
            detail: body.chars().take(240).collect(),
        });
    }

    let parsed: OAuthTokenResponse = resp.json().await?;
    let expires_at_ms = parsed.expires_in.map(|secs| now_ms() + secs * 1000);
    Ok(StoredCredential::OAuthTokens {
        client_id: client_id.to_string(),
        access_token: SecretToken::new(parsed.access_token),
        refresh_token: parsed
            .refresh_token
            .map(SecretToken::new)
            .or_else(|| Some(refresh_token.clone())),
        expires_at_ms,
        token_type: parsed.token_type.unwrap_or_else(|| "Bearer".to_string()),
    })
}

// Binds an ephemeral localhost port (RFC 8252 loopback redirect) for system browser OAuth callbacks.
pub async fn bind_loopback_oauth_listener() -> Result<(u16, TcpListener), AuthError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    Ok((port, listener))
}

pub async fn await_loopback_redirect_url(listener: TcpListener) -> Result<String, AuthError> {
    let (mut stream, _) = listener.accept().await?;
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let request_line = req.lines().next().unwrap_or("");
    let path_and_query = request_line
        .split_whitespace()
        .nth(1)
        .ok_or(AuthError::MissingAuthCode)?;

    let html = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>AGY Mobile Sign-In</title><style>body{background:#121417;color:#EDEAE2;font-family:sans-serif;padding:24px}h1{font-size:1.25rem;color:#F2A93B}</style></head><body><h1>Sign-in received</h1><p>Return to AGY Mobile to continue your terminal session.</p></body></html>";
    let _ = stream.write_all(html.as_bytes()).await;
    let _ = stream.flush().await;

    Ok(format!("http://127.0.0.1{path_and_query}"))
}

#[derive(Serialize, Deserialize)]
struct SealedEnvelope {
    version: u8,
    nonce_hex: String,
    ciphertext_hex: String,
}

// Seals credentials with AES-256-GCM using a 32-byte key derived or unwrapped via Android Keystore.
pub struct EncryptedTokenStore;

impl EncryptedTokenStore {
    pub fn save_sealed(
        path: &Path,
        key_bytes: &[u8; 32],
        credential: &StoredCredential,
    ) -> Result<(), AuthError> {
        let cipher = Aes256Gcm::new_from_slice(key_bytes).map_err(|_| AuthError::CryptoFailure)?;
        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let plaintext = serde_json::to_vec(credential)?;
        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_ref())
            .map_err(|_| AuthError::CryptoFailure)?;

        let envelope = SealedEnvelope {
            version: 1,
            nonce_hex: hex::encode(nonce_bytes),
            ciphertext_hex: hex::encode(ciphertext),
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("enc.tmp");
        fs::write(&tmp, serde_json::to_vec(&envelope)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn load_sealed(
        path: &Path,
        key_bytes: &[u8; 32],
    ) -> Result<Option<StoredCredential>, AuthError> {
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read(path)?;
        let envelope: SealedEnvelope = serde_json::from_slice(&raw)?;
        let nonce_vec = hex::decode(envelope.nonce_hex).map_err(|_| AuthError::CryptoFailure)?;
        if nonce_vec.len() != 12 {
            return Err(AuthError::CryptoFailure);
        }
        let ciphertext =
            hex::decode(envelope.ciphertext_hex).map_err(|_| AuthError::CryptoFailure)?;
        let cipher = Aes256Gcm::new_from_slice(key_bytes).map_err(|_| AuthError::CryptoFailure)?;
        let plaintext = cipher
            .decrypt(Nonce::from_slice(&nonce_vec), ciphertext.as_ref())
            .map_err(|_| AuthError::CryptoFailure)?;
        let cred: StoredCredential = serde_json::from_slice(&plaintext)?;
        Ok(Some(cred))
    }

    pub fn clear(path: &Path) -> Result<(), AuthError> {
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

// Strips OAuth bearer tokens, Google API keys, and GitHub PATs before logging or UI display.
pub fn scrub_sensitive_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for token in input.split_inclusive(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '&') {
        let trimmed = token.trim_end_matches(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '&');
        let suffix = &token[trimmed.len()..];
        let lower = trimmed.to_ascii_lowercase();
        let should_redact = trimmed.starts_with("ya29.")
            || trimmed.starts_with("AIza")
            || trimmed.starts_with("ghp_")
            || trimmed.starts_with("github_pat_")
            || trimmed.starts_with("1//")
            || lower.starts_with("access_token=")
            || lower.starts_with("refresh_token=")
            || lower.starts_with("key=aiza");
        if should_redact {
            out.push_str("[REDACTED]");
            out.push_str(suffix);
        } else {
            out.push_str(token);
        }
    }
    out
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_token_never_prints_raw_value() {
        let secret = SecretToken::new("ya29.super_secret_token_value");
        assert_eq!(format!("{secret}"), "[REDACTED_TOKEN]");
        assert_eq!(format!("{secret:?}"), "[REDACTED_TOKEN]");
    }

    #[test]
    fn pkce_challenge_and_state_validation_work() {
        let pkce = PkceSession::generate(
            "12345-example.apps.googleusercontent.com",
            "http://127.0.0.1:43123/callback",
            GENERATIVE_LANGUAGE_SCOPE,
        )
        .unwrap();
        assert!(pkce.authorization_url.contains("code_challenge_method=S256"));

        let bad_cb = "http://127.0.0.1:43123/callback?code=abc123&state=wrong";
        assert!(matches!(
            pkce.verify_and_extract_code(bad_cb),
            Err(AuthError::StateMismatch)
        ));

        let good_cb = format!(
            "http://127.0.0.1:43123/callback?code=auth_code_99&state={}",
            pkce.state
        );
        assert_eq!(pkce.verify_and_extract_code(&good_cb).unwrap(), "auth_code_99");
    }

    #[test]
    fn aes_gcm_envelope_seals_and_rejects_wrong_key() {
        let path = std::env::temp_dir().join(format!("agy-auth-seal-{}.enc", now_ms()));
        let key_a = [0x42u8; 32];
        let key_b = [0x24u8; 32];
        let cred = StoredCredential::GeminiApiKey {
            api_key: SecretToken::new("AIzaSyTestKey000000000000"),
        };

        EncryptedTokenStore::save_sealed(&path, &key_a, &cred).unwrap();
        let raw_disk = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
        assert!(!raw_disk.contains("AIzaSyTestKey"));

        assert!(EncryptedTokenStore::load_sealed(&path, &key_b).is_err());
        let loaded = EncryptedTokenStore::load_sealed(&path, &key_a)
            .unwrap()
            .unwrap();
        match loaded {
            StoredCredential::GeminiApiKey { api_key } => {
                assert_eq!(api_key.expose_secret(), "AIzaSyTestKey000000000000");
            }
            _ => panic!("unexpected variant"),
        }
        let _ = EncryptedTokenStore::clear(&path);
    }

    #[test]
    fn scrubber_removes_sensitive_tokens() {
        let msg = "Bearer ya29.a0AfH6SMBxyz and AIzaSySecretKey123 plus ghp_TestToken999";
        let scrubbed = scrub_sensitive_text(msg);
        assert!(!scrubbed.contains("ya29."));
        assert!(!scrubbed.contains("AIzaSy"));
        assert!(!scrubbed.contains("ghp_"));
        assert!(scrubbed.contains("[REDACTED]"));
    }
}
