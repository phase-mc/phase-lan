//! Phase Pro rights: a signed token, its verification and storage.
//!
//! **LAN is entirely free** (owner's decision of 2026-11-18): seats in the room (up to 50 with
//! Phase Companion), a custom header title, nickname colors, prefixes, silhouette highlight and
//! the footer in `Tab` are for everyone. The room, styling and guest do not ask this module.
//! Phase Pro will later be about cosmetics and Phase's own servers; the module stays for them:
//! token format, signature, binding to a device and the test issuer.
//!
//! **Where Pro comes from.** Rights are a token signed by the Phase server (Ed25519):
//!
//! ```text
//! phase1.<hex(JSON with rights)>.<hex(signature)>
//! { "dev": "<device ID>", "plan": "pro", "style": { "color": "ice_sapphire" }, "exp": 1794000000 }
//! ```
//!
//! The token is bound to a device ID (`dev`): it cannot be handed to a friend. The host checks
//! the signature with the public key from [`trusted_issuers`], the binding to the guest's device and the expiry.
//! The check can be bypassed in your own copy of the program (we do not build copy protection),
//! but another host will believe only a genuine signature.
//!
//! **There is no Phase server yet** (stage 9.4–9.5), so in a release build the list of trusted
//! keys is empty. In a debug build the developer key is also trusted
//! ([`dev_issuer`]): it signs the token in [`set_test_plan`] in tests.

use std::{
    fs,
    io::ErrorKind,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use phase_lang::tr;
use phase_tunnel::{EndpointId, SecretKey, fsutil, identity};
use serde::{Deserialize, Serialize};

/// File with the rights token in the profile folder.
pub const ENTITLEMENT_FILE: &str = "entitlement.txt";

/// How long the token stays valid after `exp` on its own device: the program refreshes
/// it at launch, and a week without network must not take away a paid Pro.
pub const OFFLINE_GRACE_SECS: u64 = 7 * 24 * 60 * 60;
/// Lifetime of a test token ([`set_test_plan`]).
const TEST_TOKEN_SECS: u64 = 30 * 24 * 60 * 60;

const TOKEN_PREFIX: &str = "phase1";
/// What exactly is signed: a rights signature cannot be passed off as a signature of anything else.
const SIGN_CONTEXT: &[u8] = b"phase-entitlement-v1\n";
/// Maximum token length (as in the guest's greeting).
const TOKEN_MAX_LEN: usize = phase_tunnel::wire::ENTITLEMENT_MAX_LEN;

/// Plan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Plan {
    #[default]
    Free,
    Pro,
}

impl Plan {
    pub fn is_pro(self) -> bool {
        self == Self::Pro
    }
}

/// Personal style signed together with the rights: a color from the Phase palette and a prefix.
/// A foundation for Pro cosmetics; the LAN room does not apply it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleClaim {
    /// Identifier of a Phase palette color (`ice_sapphire`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Prefix without brackets, up to 8 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// Contents of the rights token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    /// ID of the device the rights were issued to.
    pub dev: String,
    pub plan: Plan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<StyleClaim>,
    /// Expiry time, Unix seconds.
    pub exp: u64,
}

/// Why the token was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    /// Not a Phase token or it is damaged.
    Malformed,
    /// The signature is not from a trusted key.
    BadSignature,
    /// The token was issued to another device.
    WrongDevice,
    /// The validity period has ended.
    Expired,
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Malformed => tr!("токен прав повреждён", "the license is damaged"),
            Self::BadSignature => tr!(
                "подпись токена прав не от сервера Phase",
                "the license is not signed by Phase"
            ),
            Self::WrongDevice => tr!(
                "токен прав выдан другому устройству",
                "the license was issued to another device"
            ),
            Self::Expired => tr!("срок токена прав вышел", "the license has expired"),
        })
    }
}

impl std::error::Error for TokenError {}

/// Unix seconds now.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

fn signed_message(payload: &[u8]) -> Vec<u8> {
    [SIGN_CONTEXT, payload].concat()
}

/// Signs rights with the issuer key. In the program only tests use this;
/// real tokens are signed by the server.
pub fn sign(issuer: &SecretKey, claims: &Claims) -> String {
    let payload = serde_json::to_vec(claims).expect("rights serialize to JSON");
    let signature = issuer.sign(&signed_message(&payload));
    format!(
        "{TOKEN_PREFIX}.{}.{}",
        to_hex(&payload),
        to_hex(&signature.to_bytes())
    )
}

/// Checks a token: the signature of one of `issuers`, the binding to device `device` and the expiry
/// (`now_unix` is the verifier's time). Returns the rights.
pub fn verify(
    token: &str,
    issuers: &[EndpointId],
    device: &EndpointId,
    now_unix: u64,
) -> Result<Claims, TokenError> {
    if token.len() > TOKEN_MAX_LEN {
        return Err(TokenError::Malformed);
    }
    let mut parts = token.trim().split('.');
    let (Some(TOKEN_PREFIX), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenError::Malformed);
    };
    let payload = from_hex(payload).ok_or(TokenError::Malformed)?;
    let signature: [u8; 64] = from_hex(signature)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(TokenError::Malformed)?;
    let signature = phase_tunnel::Signature::from_bytes(&signature);
    let message = signed_message(&payload);
    // The signature first: the contents of an unsigned token cannot be trusted even in an error
    // message.
    if !issuers
        .iter()
        .any(|issuer| issuer.verify(&message, &signature).is_ok())
    {
        return Err(TokenError::BadSignature);
    }
    let claims: Claims = serde_json::from_slice(&payload).map_err(|_| TokenError::Malformed)?;
    if claims.dev != device.to_string() {
        return Err(TokenError::WrongDevice);
    }
    if claims.exp <= now_unix {
        return Err(TokenError::Expired);
    }
    Ok(claims)
}

/// The developer key: test tokens are signed with it. The key is public (it is in the
/// source), so **only a debug build** trusts it; see [`trusted_issuers`].
pub fn dev_issuer() -> SecretKey {
    SecretKey::from_bytes(b"phase dev issuer key: debug only")
}

/// Whose signatures this build trusts. A release build trusts only the Phase server (its key does not
/// exist yet: the server will appear at stage 9.4, until then Pro does not exist in a release build).
/// A debug build also trusts the developer key.
pub fn trusted_issuers() -> Vec<EndpointId> {
    let mut issuers = Vec::new();
    if cfg!(debug_assertions) {
        issuers.push(dev_issuer().public());
    }
    issuers
}

/// Rights of this device.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Rights {
    pub plan: Plan,
    pub pro: bool,
    /// Personal style from the token (color and prefix).
    pub style: Option<StyleClaim>,
    /// Until when Pro is valid, Unix seconds.
    pub expires_unix: Option<u64>,
    /// Pro is signed by the developer key ([`set_test_plan`]), not by the server.
    pub test: bool,
}

impl Rights {
    pub fn free() -> Self {
        Self::of(Plan::Free)
    }

    fn of(plan: Plan) -> Self {
        Self {
            plan,
            pro: plan.is_pro(),
            style: None,
            expires_unix: None,
            test: false,
        }
    }
}

/// The rights token from the profile folder (`None` means it is missing or unreadable).
pub fn load_token(profile_dir: &Path) -> Option<String> {
    let file = profile_dir.join(ENTITLEMENT_FILE);
    match fs::read_to_string(&file) {
        Ok(text) => Some(text.trim().to_string()).filter(|t| !t.is_empty()),
        Err(err) if err.kind() == ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!("не удалось прочитать {}: {err}", file.display());
            None
        }
    }
}

/// Saves the rights token to the profile folder.
pub fn store_token(profile_dir: &Path, token: &str) -> Result<()> {
    let file = profile_dir.join(ENTITLEMENT_FILE);
    fsutil::write_atomic(&file, format!("{token}\n").as_bytes()).with_context(|| {
        tr!(
            format!("не удалось сохранить права в {}", file.display()),
            format!("could not save the license to {}", file.display())
        )
    })
}

/// Removes the rights token: the profile is Free again.
pub fn clear_token(profile_dir: &Path) -> Result<()> {
    let file = profile_dir.join(ENTITLEMENT_FILE);
    match fs::remove_file(&file) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| {
            tr!(
                format!("не удалось удалить {}", file.display()),
                format!("could not delete {}", file.display())
            )
        }),
    }
}

/// Rights of the profile in `profile_dir`: the token from the file, verified by the `issuers` keys
/// for this profile's device key. If there is no token or key, or verification failed, Free.
/// The call blocks (the device key is decrypted via DPAPI).
pub fn local_rights(profile_dir: &Path, issuers: &[EndpointId]) -> Rights {
    let Some(token) = load_token(profile_dir) else {
        return Rights::free();
    };
    let device = match identity::load_secret_key(profile_dir) {
        Ok(Some(key)) => key.public(),
        _ => return Rights::free(),
    };
    // On its own device the token lives another week after expiry (see OFFLINE_GRACE_SECS).
    let now = now_unix().saturating_sub(OFFLINE_GRACE_SECS);
    match verify(&token, issuers, &device, now) {
        Ok(claims) => rights_from(&token, claims),
        Err(err) => {
            tracing::info!("права профиля не приняты ({err}) — работаю как Phase Free");
            Rights::free()
        }
    }
}

fn rights_from(token: &str, claims: Claims) -> Rights {
    let dev = dev_issuer().public();
    Rights {
        style: claims.style.clone(),
        expires_unix: Some(claims.exp),
        test: verify_issuer_is(token, &dev),
        ..Rights::of(claims.plan)
    }
}

/// The token is signed by exactly this key (the contents are not checked).
fn verify_issuer_is(token: &str, issuer: &EndpointId) -> bool {
    let mut parts = token.trim().split('.');
    let (Some(_), Some(payload), Some(signature)) = (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Some(payload), Some(signature)) = (from_hex(payload), from_hex(signature)) else {
        return false;
    };
    let Ok(signature) = <[u8; 64]>::try_from(signature) else {
        return false;
    };
    issuer
        .verify(
            &signed_message(&payload),
            &phase_tunnel::Signature::from_bytes(&signature),
        )
        .is_ok()
}

/// Test Pro: signs a Pro token for this profile's device with the developer key
/// (with style `style`) and saves it; `pro == false` removes the token.
/// Returns the rights as a build with trusted keys `issuers` sees them: a release
/// build does not believe the test token and stays Free.
pub fn set_test_plan(
    profile_dir: &Path,
    pro: bool,
    style: Option<StyleClaim>,
    issuers: &[EndpointId],
) -> Result<Rights> {
    if pro {
        let device = identity::load_or_create_secret_key(profile_dir)?.public();
        let claims = Claims {
            dev: device.to_string(),
            plan: Plan::Pro,
            style,
            exp: now_unix() + TEST_TOKEN_SECS,
        };
        store_token(profile_dir, &sign(&dev_issuer(), &claims))?;
    } else {
        clear_token(profile_dir)?;
    }
    Ok(local_rights(profile_dir, issuers))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(device: &EndpointId, exp: u64) -> Claims {
        Claims {
            dev: device.to_string(),
            plan: Plan::Pro,
            style: Some(StyleClaim {
                color: Some("ice_sapphire".into()),
                prefix: Some("VIP".into()),
            }),
            exp,
        }
    }

    #[test]
    fn signed_token_is_accepted_only_as_issued() {
        let issuer = SecretKey::generate();
        let device = SecretKey::generate().public();
        let token = sign(&issuer, &claims(&device, 2_000));
        let issuers = [issuer.public()];

        let got = verify(&token, &issuers, &device, 1_000).unwrap();
        assert_eq!(got, claims(&device, 2_000));

        // Another device: a friend's token does not fit.
        let other = SecretKey::generate().public();
        assert_eq!(
            verify(&token, &issuers, &other, 1_000),
            Err(TokenError::WrongDevice)
        );
        // The validity period ended.
        assert_eq!(
            verify(&token, &issuers, &device, 2_000),
            Err(TokenError::Expired)
        );
        // Signed by someone other than the server.
        let stranger = [SecretKey::generate().public()];
        assert_eq!(
            verify(&token, &stranger, &device, 1_000),
            Err(TokenError::BadSignature)
        );
        assert_eq!(
            verify(&token, &[], &device, 1_000),
            Err(TokenError::BadSignature)
        );
    }

    #[test]
    fn forged_and_broken_tokens_are_rejected() {
        let issuer = SecretKey::generate();
        let device = SecretKey::generate().public();
        let issuers = [issuer.public()];
        let token = sign(&issuer, &claims(&device, 2_000));
        let parts: Vec<&str> = token.split('.').collect();

        // The contents were swapped (own ID instead of another's, a longer expiry): the signature does not match.
        let mut forged = claims(&device, 9_000_000);
        forged.style = None;
        let forged_payload = to_hex(&serde_json::to_vec(&forged).unwrap());
        let forged_token = format!("{}.{forged_payload}.{}", parts[0], parts[2]);
        assert_eq!(
            verify(&forged_token, &issuers, &device, 1_000),
            Err(TokenError::BadSignature)
        );

        // A self-signed token: the guest signed the rights with their own device key.
        let guest_key = SecretKey::generate();
        let self_signed = sign(&guest_key, &claims(&guest_key.public(), 2_000));
        assert_eq!(
            verify(&self_signed, &issuers, &guest_key.public(), 1_000),
            Err(TokenError::BadSignature)
        );

        for broken in [
            "",
            "phase1",
            "phase1.zz.zz",
            "phase2.00.00",
            &format!("{token}.extra"),
            &format!("{}.{}.{}", parts[0], parts[1], &parts[2][2..]),
            &"x".repeat(TOKEN_MAX_LEN + 1),
        ] {
            assert_eq!(
                verify(broken, &issuers, &device, 1_000),
                Err(TokenError::Malformed),
                "{broken:.40}"
            );
        }
    }

    #[test]
    fn test_plan_switch_is_trusted_only_with_the_dev_key() {
        let dir = std::env::temp_dir().join(format!("phase-entitlement-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let dev = [dev_issuer().public()];

        assert_eq!(local_rights(&dir, &dev), Rights::free(), "no token yet");

        let style = StyleClaim {
            color: Some("amber_gold".into()),
            prefix: None,
        };
        let rights = set_test_plan(&dir, true, Some(style.clone()), &dev).unwrap();
        assert!(rights.pro && rights.test);
        assert_eq!(rights.style, Some(style));
        assert!(!Rights::free().pro);
        assert_eq!(local_rights(&dir, &dev), rights, "rights survive a restart");

        // A release build does not trust the developer key: the same file is Free.
        assert_eq!(local_rights(&dir, &[]), Rights::free());
        let server = [SecretKey::generate().public()];
        assert_eq!(local_rights(&dir, &server), Rights::free());

        // The token is bound to the device key: with another key in the profile it is not valid.
        let token = load_token(&dir).unwrap();
        let other = dir.join("other");
        fs::create_dir_all(&other).unwrap();
        identity::load_or_create_secret_key(&other).unwrap();
        store_token(&other, &token).unwrap();
        assert_eq!(local_rights(&other, &dev), Rights::free());

        assert_eq!(
            set_test_plan(&dir, false, None, &dev).unwrap(),
            Rights::free()
        );
        assert_eq!(load_token(&dir), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
