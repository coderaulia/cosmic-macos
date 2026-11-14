// SPDX-License-Identifier: GPL-3.0-only

//! OAuth2 PKCE loopback browser sign-in flow (`127.0.0.1:<ephemeral_port>`)
//! for Gmail and Outlook/Microsoft 365, token refresh, and IMAP `XOAUTH2`
//! SASL authenticator.

use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use serde::{Deserialize, Serialize};
use url::Url;

const LOOPBACK_TIMEOUT: Duration = Duration::from_secs(180);

/// Supported OAuth2 providers for browser sign-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthProvider {
    Gmail,
    Outlook,
}

impl OAuthProvider {
    pub fn display_name(self) -> &'static str {
        match self {
            OAuthProvider::Gmail => "Google",
            OAuthProvider::Outlook => "Microsoft",
        }
    }

    pub fn auth_endpoint(self) -> &'static str {
        match self {
            OAuthProvider::Gmail => "https://accounts.google.com/o/oauth2/v2/auth",
            OAuthProvider::Outlook => {
                "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
            }
        }
    }

    pub fn token_endpoint(self) -> &'static str {
        match self {
            OAuthProvider::Gmail => "https://oauth2.googleapis.com/token",
            OAuthProvider::Outlook => "https://login.microsoftonline.com/common/oauth2/v2.0/token",
        }
    }

    pub fn scopes(self) -> &'static str {
        match self {
            OAuthProvider::Gmail => "https://mail.google.com/ openid email",
            OAuthProvider::Outlook => {
                "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access openid email"
            }
        }
    }

    pub fn imap_host(self) -> &'static str {
        match self {
            OAuthProvider::Gmail => "imap.gmail.com",
            OAuthProvider::Outlook => "outlook.office365.com",
        }
    }

    pub fn imap_port(self) -> u16 {
        993
    }

    pub fn smtp_host(self) -> &'static str {
        match self {
            OAuthProvider::Gmail => "smtp.gmail.com",
            OAuthProvider::Outlook => "smtp.office365.com",
        }
    }

    pub fn smtp_port(self) -> u16 {
        match self {
            OAuthProvider::Gmail => 465,
            OAuthProvider::Outlook => 587,
        }
    }

    pub fn env_client_id_var(self) -> &'static str {
        match self {
            OAuthProvider::Gmail => "MAC_MAIL_GMAIL_CLIENT_ID",
            OAuthProvider::Outlook => "MAC_MAIL_OUTLOOK_CLIENT_ID",
        }
    }

    pub fn alt_env_client_id_var(self) -> &'static str {
        match self {
            OAuthProvider::Gmail => "GMAIL_OAUTH_CLIENT_ID",
            OAuthProvider::Outlook => "OUTLOOK_OAUTH_CLIENT_ID",
        }
    }

    pub fn env_client_secret_var(self) -> &'static str {
        match self {
            OAuthProvider::Gmail => "MAC_MAIL_GMAIL_CLIENT_SECRET",
            OAuthProvider::Outlook => "MAC_MAIL_OUTLOOK_CLIENT_SECRET",
        }
    }
}

/// Persisted OAuth2 token bundle.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
}

impl OAuthTokens {
    /// Returns `true` if the token has expired or will expire within 60 seconds.
    pub fn is_expired_soon(&self, now_unix: i64) -> bool {
        self.expires_at
            .is_some_and(|exp| now_unix.saturating_add(60) >= exp)
    }
}

/// Result of completing a browser PKCE sign-in flow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthSignInResult {
    pub provider: OAuthProvider,
    pub client_id: String,
    pub email: Option<String>,
    pub tokens: OAuthTokens,
}

/// PKCE verifier + S256 challenge pair (RFC 7636).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PkceChallenge {
    pub verifier: String,
    pub challenge: String,
    pub method: &'static str,
}

/// Resolve the OAuth2 Client ID from the explicit UI input or environment variables.
pub fn resolve_client_id(provider: OAuthProvider, explicit: Option<&str>) -> Result<String, String> {
    if let Some(id) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(id.to_owned());
    }
    for var in [
        provider.env_client_id_var(),
        provider.alt_env_client_id_var(),
    ] {
        if let Ok(val) = std::env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_owned());
            }
        }
    }
    Err(format!(
        "Enter a {} OAuth2 Client ID or set {}",
        provider.display_name(),
        provider.env_client_id_var()
    ))
}

/// Resolve an optional OAuth2 Client Secret from environment variables (for desktop clients that require one).
pub fn resolve_client_secret(provider: OAuthProvider) -> Option<String> {
    std::env::var(provider.env_client_secret_var())
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Generate a fresh PKCE `code_verifier` and S256 `code_challenge`.
pub fn generate_pkce() -> PkceChallenge {
    let bytes = random_bytes(32);
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let challenge = pkce_s256_challenge(&verifier);
    PkceChallenge {
        verifier,
        challenge,
        method: "S256",
    }
}

/// Compute `BASE64URL-ENCODE(SHA256(ASCII(verifier)))` per RFC 7636.
pub fn pkce_s256_challenge(verifier: &str) -> String {
    let digest = sha256(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Generate a random CSRF `state` token for OAuth2 authorization requests.
pub fn generate_state() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes(16))
}

/// Build the provider's OAuth2 authorization URL with PKCE parameters.
pub fn build_authorization_url(
    provider: OAuthProvider,
    client_id: &str,
    redirect_uri: &str,
    pkce: &PkceChallenge,
    state: &str,
    login_hint: Option<&str>,
) -> Result<String, String> {
    let mut params = vec![
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", provider.scopes()),
        ("code_challenge", pkce.challenge.as_str()),
        ("code_challenge_method", pkce.method),
        ("state", state),
    ];
    if provider == OAuthProvider::Gmail {
        params.push(("access_type", "offline"));
        params.push(("prompt", "consent"));
    }
    if let Some(hint) = login_hint.map(str::trim).filter(|s| !s.is_empty()) {
        params.push(("login_hint", hint));
    }
    Url::parse_with_params(provider.auth_endpoint(), params)
        .map(|u| u.into())
        .map_err(|err| format!("Invalid OAuth authorization URL: {err}"))
}

/// Parsed outcome of a single HTTP request on the loopback listener.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallbackOutcome {
    Code(String),
    Ignore,
}

/// Parse an HTTP request line received on the loopback listener and validate `state`.
pub fn parse_callback_request_line(
    request_line: &str,
    expected_state: &str,
) -> Result<CallbackOutcome, String> {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    if method != "GET" || target.is_empty() {
        return Ok(CallbackOutcome::Ignore);
    }

    let url = Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|err| format!("Malformed OAuth callback URL: {err}"))?;
    if url.path() != "/callback" && url.path() != "/" {
        return Ok(CallbackOutcome::Ignore);
    }

    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut error_description = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            "error_description" => error_description = Some(v.into_owned()),
            _ => {}
        }
    }

    if let Some(err) = error {
        let detail = error_description
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| err.clone());
        return Err(format!("OAuth sign-in rejected: {detail}"));
    }

    let Some(received_state) = state else {
        return Err("OAuth callback missing state parameter".to_owned());
    };
    if received_state != expected_state {
        return Err("OAuth callback state mismatch".to_owned());
    }

    code.filter(|c| !c.is_empty())
        .map(CallbackOutcome::Code)
        .ok_or_else(|| "OAuth callback missing authorization code".to_owned())
}

/// Run the full interactive PKCE loopback browser sign-in flow for `provider`.
///
/// Binds `127.0.0.1:0` (ephemeral port), launches the system browser to the
/// authorization URL, captures the loopback callback, and exchanges the code
/// for access + refresh tokens.
pub fn run_pkce_browser_flow(
    provider: OAuthProvider,
    explicit_client_id: Option<&str>,
    login_hint: Option<&str>,
) -> Result<OAuthSignInResult, String> {
    let client_id = resolve_client_id(provider, explicit_client_id)?;
    let client_secret = resolve_client_secret(provider);
    let pkce = generate_pkce();
    let state = generate_state();

    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|err| format!("Could not bind local OAuth callback port: {err}"))?;
    let port = listener
        .local_addr()
        .map_err(|err| format!("Could not read local callback port: {err}"))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let auth_url = build_authorization_url(
        provider,
        &client_id,
        &redirect_uri,
        &pkce,
        &state,
        login_hint,
    )?;

    open::that(&auth_url).map_err(|err| format!("Could not open browser for sign-in: {err}"))?;

    let code = wait_for_loopback_code(&listener, &state, LOOPBACK_TIMEOUT)?;
    let (tokens, token_email) = exchange_code_for_tokens(
        provider,
        &client_id,
        client_secret.as_deref(),
        &code,
        &redirect_uri,
        &pkce.verifier,
    )?;

    let email = token_email.or_else(|| {
        login_hint
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    });

    Ok(OAuthSignInResult {
        provider,
        client_id,
        email,
        tokens,
    })
}

fn wait_for_loopback_code(
    listener: &TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<String, String> {
    listener
        .set_nonblocking(true)
        .map_err(|err| format!("Could not configure OAuth listener: {err}"))?;
    let deadline = Instant::now() + timeout;

    loop {
        if Instant::now() >= deadline {
            return Err("Timed out waiting for browser sign-in to complete".to_owned());
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let req_line = read_http_request_line(&mut stream);
                match req_line {
                    Ok(line) => match parse_callback_request_line(&line, expected_state) {
                        Ok(CallbackOutcome::Code(code)) => {
                            write_http_html(
                                &mut stream,
                                "200 OK",
                                "Sign-in complete",
                                "You have signed in to Mail. You can close this browser tab.",
                            );
                            return Ok(code);
                        }
                        Ok(CallbackOutcome::Ignore) => {
                            write_http_html(&mut stream, "404 Not Found", "Not found", "");
                        }
                        Err(err) => {
                            write_http_html(&mut stream, "400 Bad Request", "Sign-in failed", &err);
                            return Err(err);
                        }
                    },
                    Err(_) => {
                        write_http_html(
                            &mut stream,
                            "400 Bad Request",
                            "Bad request",
                            "Could not read OAuth callback request.",
                        );
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => {
                return Err(format!("OAuth callback listener failed: {err}"));
            }
        }
    }
}

fn read_http_request_line(stream: &mut TcpStream) -> Result<String, String> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|err| format!("Failed reading OAuth callback: {err}"))?;
    Ok(line.trim().to_owned())
}

fn write_http_html(stream: &mut TcpStream, status: &str, title: &str, body: &str) {
    let safe_title = escape_html(title);
    let safe_body = escape_html(body);
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{safe_title}</title></head>\
         <body style=\"font-family:system-ui,sans-serif;max-width:520px;margin:4rem auto;text-align:center\">\
         <h2>{safe_title}</h2><p>{safe_body}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[derive(Deserialize)]
struct TokenEndpointResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

/// Exchange an authorization code for OAuth2 tokens at `provider.token_endpoint()`.
pub fn exchange_code_for_tokens(
    provider: OAuthProvider,
    client_id: &str,
    client_secret: Option<&str>,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<(OAuthTokens, Option<String>), String> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
    ];
    if let Some(secret) = client_secret.filter(|s| !s.is_empty()) {
        form.push(("client_secret", secret));
    }

    let payload = post_token_form(provider.token_endpoint(), &form)?;
    let email = payload
        .id_token
        .as_deref()
        .and_then(extract_email_from_id_token);
    let tokens = tokens_from_response(payload, None);
    Ok((tokens, email))
}

/// Refresh an expired or expiring OAuth2 access token using its `refresh_token`.
pub fn refresh_access_token(
    provider: OAuthProvider,
    client_id: &str,
    client_secret: Option<&str>,
    refresh_token: &str,
) -> Result<OAuthTokens, String> {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", refresh_token),
    ];
    if let Some(secret) = client_secret.filter(|s| !s.is_empty()) {
        form.push(("client_secret", secret));
    }

    let payload = post_token_form(provider.token_endpoint(), &form)?;
    Ok(tokens_from_response(payload, Some(refresh_token)))
}

fn post_token_form(
    endpoint: &str,
    form: &[(&str, &str)],
) -> Result<TokenEndpointResponse, String> {
    let mut response = ureq::post(endpoint)
        .send_form(form.iter().copied())
        .map_err(|err| format!("OAuth token request failed: {err}"))?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|err| format!("Failed reading OAuth token response: {err}"))?;
    serde_json::from_str::<TokenEndpointResponse>(&body)
        .map_err(|err| format!("Invalid OAuth token response: {err}"))
}

fn tokens_from_response(
    resp: TokenEndpointResponse,
    fallback_refresh: Option<&str>,
) -> OAuthTokens {
    let now = current_unix_secs();
    let expires_at = resp.expires_in.map(|secs| now.saturating_add(secs));
    OAuthTokens {
        access_token: resp.access_token,
        refresh_token: resp
            .refresh_token
            .or_else(|| fallback_refresh.map(str::to_owned)),
        expires_at,
        token_type: resp.token_type,
    }
}

/// Extract the user's email address from an OpenID Connect `id_token` JWT payload.
pub fn extract_email_from_id_token(id_token: &str) -> Option<String> {
    let payload_b64 = id_token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .or_else(|_| URL_SAFE.decode(payload_b64))
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    for key in ["email", "preferred_username", "upn"] {
        if let Some(val) = json.get(key).and_then(|v| v.as_str()) {
            let trimmed = val.trim();
            if trimmed.contains('@') {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

/// Format the raw SASL `XOAUTH2` initial client response before base64 encoding.
pub fn format_xoauth2_sasl(user: &str, access_token: &str) -> String {
    format!("user={user}\x01auth=Bearer {access_token}\x01\x01")
}

/// `imap::Authenticator` implementation for `AUTHENTICATE XOAUTH2`.
#[derive(Clone, Debug)]
pub struct XOAuth2Authenticator {
    pub user: String,
    pub access_token: String,
}

impl imap::Authenticator for XOAuth2Authenticator {
    type Response = String;

    fn process(&self, _challenge: &[u8]) -> Self::Response {
        format_xoauth2_sasl(&self.user, &self.access_token)
    }
}

pub fn current_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn random_bytes(len: usize) -> Vec<u8> {
    use std::io::Read;
    let mut buf = vec![0u8; len];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom")
        && f.read_exact(&mut buf).is_ok()
    {
        return buf;
    }
    // Fallback mixing nanosecond timestamp and process ID if /dev/urandom is unavailable.
    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15)
        ^ u64::from(std::process::id()).wrapping_mul(0x517c_c1b7_2722_0a95);
    for b in &mut buf {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        *b = (seed & 0xff) as u8;
    }
    buf
}

/// Pure-Rust SHA-256 (FIPS 180-4) for PKCE S256 challenge derivation.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1,
        0x923f_82a4, 0xab1c_5ed5, 0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3,
        0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174, 0xe49b_69c1, 0xefbe_4786,
        0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
        0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7, 0xc6e0_0bf3, 0xd5a7_9147,
        0x06ca_6351, 0x1429_2967, 0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13,
        0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85, 0xa2bf_e8a1, 0xa81a_664b,
        0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
        0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a,
        0x5b9c_ca4f, 0x682e_6ff3, 0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208,
        0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2,
    ];

    let mut state: [u32; 8] = [
        0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a, 0x510e_527f, 0x9b05_688c,
        0x1f83_d9ab, 0x5be0_cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(data.len() + 72);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut out = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_s256_matches_rfc7636_appendix_b_vector() {
        // RFC 7636 Appendix B test vector:
        // verifier: dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk
        // expected challenge: E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_s256_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn builds_gmail_and_outlook_authorization_urls_with_pkce() {
        let pkce = PkceChallenge {
            verifier: "ver123".to_owned(),
            challenge: "chal456".to_owned(),
            method: "S256",
        };
        let gmail = build_authorization_url(
            OAuthProvider::Gmail,
            "gmail-client-id",
            "http://127.0.0.1:54321/callback",
            &pkce,
            "state-abc",
            Some("alice@gmail.com"),
        )
        .unwrap();
        let parsed = Url::parse(&gmail).unwrap();
        assert_eq!(parsed.host_str(), Some("accounts.google.com"));
        let params: std::collections::HashMap<_, _> = parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(params.get("code_challenge").map(String::as_str), Some("chal456"));
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(params.get("state").map(String::as_str), Some("state-abc"));
        assert_eq!(params.get("access_type").map(String::as_str), Some("offline"));
        assert_eq!(
            params.get("login_hint").map(String::as_str),
            Some("alice@gmail.com")
        );

        let outlook = build_authorization_url(
            OAuthProvider::Outlook,
            "ms-client-id",
            "http://127.0.0.1:54321/callback",
            &pkce,
            "state-xyz",
            None,
        )
        .unwrap();
        let parsed_ms = Url::parse(&outlook).unwrap();
        assert_eq!(parsed_ms.host_str(), Some("login.microsoftonline.com"));
        let ms_params: std::collections::HashMap<_, _> = parsed_ms.query_pairs().collect();
        assert!(
            ms_params
                .get("scope")
                .is_some_and(|s| s.contains("IMAP.AccessAsUser.All"))
        );
    }

    #[test]
    fn loopback_callback_parser_validates_state_and_errors() {
        assert_eq!(
            parse_callback_request_line(
                "GET /callback?code=auth-code-123&state=s1 HTTP/1.1",
                "s1"
            )
            .unwrap(),
            CallbackOutcome::Code("auth-code-123".to_owned())
        );
        assert_eq!(
            parse_callback_request_line("GET /favicon.ico HTTP/1.1", "s1").unwrap(),
            CallbackOutcome::Ignore
        );
        assert!(
            parse_callback_request_line("GET /callback?code=c&state=wrong HTTP/1.1", "s1")
                .is_err()
        );
        assert!(
            parse_callback_request_line(
                "GET /callback?error=access_denied&error_description=Cancelled HTTP/1.1",
                "s1"
            )
            .is_err()
        );
    }

    #[test]
    fn extracts_email_from_jwt_id_token() {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#);
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"123","email":"alice@gmail.com"}"#);
        let token = format!("{header}.{payload}.sig");
        assert_eq!(
            extract_email_from_id_token(&token).as_deref(),
            Some("alice@gmail.com")
        );
    }

    #[test]
    fn xoauth2_sasl_payload_matches_spec() {
        use imap::Authenticator;
        let auth = XOAuth2Authenticator {
            user: "alice@gmail.com".to_owned(),
            access_token: "ya29.token".to_owned(),
        };
        assert_eq!(
            auth.process(b""),
            "user=alice@gmail.com\x01auth=Bearer ya29.token\x01\x01"
        );
    }
}
