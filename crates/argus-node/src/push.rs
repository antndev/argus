use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;

use crate::config::{Device, Push};
use crate::notify::HTTP;

const RECORD_SIZE: u32 = 4096;
const TTL_SECONDS: &str = "86400";

pub enum Outcome {
    Sent,
    Gone,
    Failed(String),
}

pub fn decode(text: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(text.trim().trim_end_matches('=')).ok()
}

fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn random_secret() -> SecretKey {
    loop {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("operating system random source");
        if let Ok(secret) = SecretKey::from_slice(&bytes) {
            return secret;
        }
    }
}

pub fn keypair() -> (String, String) {
    let secret = random_secret();
    (encode(secret.public_key().to_encoded_point(false).as_bytes()), encode(&secret.to_bytes()))
}

pub fn valid_public_key(text: &str) -> bool {
    decode(text).is_some_and(|bytes| PublicKey::from_sec1_bytes(&bytes).is_ok())
}

pub fn valid_private_key(text: &str) -> bool {
    decode(text).is_some_and(|bytes| SecretKey::from_slice(&bytes).is_ok())
}

fn derive(salt: &[u8], ikm: &[u8], info: &[u8], length: usize) -> Vec<u8> {
    let mut out = vec![0u8; length];
    Hkdf::<Sha256>::new(Some(salt), ikm).expand(info, &mut out).expect("supported length");
    out
}

pub fn encrypt(plaintext: &[u8], receiver: &[u8], auth: &[u8], sender: &SecretKey, salt: &[u8; 16]) -> Option<Vec<u8>> {
    let receiver_key = PublicKey::from_sec1_bytes(receiver).ok()?;
    let shared = p256::ecdh::diffie_hellman(sender.to_nonzero_scalar(), receiver_key.as_affine());
    let sender_public = sender.public_key().to_encoded_point(false);
    let key_info = [b"WebPush: info\0".as_slice(), receiver, sender_public.as_bytes()].concat();
    let ikm = derive(auth, shared.raw_secret_bytes(), &key_info, 32);
    let cek = derive(salt, &ikm, b"Content-Encoding: aes128gcm\0", 16);
    let nonce = derive(salt, &ikm, b"Content-Encoding: nonce\0", 12);
    let record = [plaintext, &[2]].concat();
    let ciphertext = Aes128Gcm::new_from_slice(&cek).ok()?.encrypt(Nonce::from_slice(&nonce), record.as_slice()).ok()?;
    Some([salt.as_slice(), &RECORD_SIZE.to_be_bytes(), &[65], sender_public.as_bytes(), &ciphertext].concat())
}

fn origin(endpoint: &str) -> Option<&str> {
    let start = endpoint.find("://")? + 3;
    Some(endpoint[start..].find('/').map_or(endpoint, |end| &endpoint[..start + end]))
}

pub fn vapid(endpoint: &str, subject: &str, private_key: &str, now_s: i64) -> Option<String> {
    let key = SigningKey::from_slice(&decode(private_key)?).ok()?;
    let header = encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = serde_json::json!({ "aud": origin(endpoint)?, "exp": now_s + 12 * 3600, "sub": subject });
    let input = format!("{header}.{}", encode(claims.to_string().as_bytes()));
    let signature: Signature = key.sign(input.as_bytes());
    let public = encode(key.verifying_key().to_encoded_point(false).as_bytes());
    Some(format!("vapid t={input}.{}, k={public}", encode(&signature.to_bytes())))
}

pub fn send(push: &Push, device: &Device, payload: &[u8], topic: &str, urgent: bool, now_s: i64) -> Outcome {
    let (Some(receiver), Some(auth)) = (decode(&device.p256dh), decode(&device.auth)) else {
        return Outcome::Gone;
    };
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).expect("operating system random source");
    let Some(body) = encrypt(payload, &receiver, &auth, &random_secret(), &salt) else { return Outcome::Gone };
    let Some(authorization) = vapid(&device.endpoint, &push.subject, &push.private_key, now_s) else {
        return Outcome::Failed("the push key is invalid".to_string());
    };
    let sent = HTTP
        .post(&device.endpoint)
        .header("TTL", TTL_SECONDS)
        .header("Urgency", if urgent { "high" } else { "normal" })
        .header("Topic", topic)
        .header("Content-Type", "application/octet-stream")
        .header("Content-Encoding", "aes128gcm")
        .header("Authorization", &authorization)
        .send(&body[..]);
    match sent {
        Ok(_) => Outcome::Sent,
        Err(ureq::Error::StatusCode(404 | 410)) => Outcome::Gone,
        Err(error) => Outcome::Failed(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier;

    fn bytes(text: &str) -> Vec<u8> {
        decode(text).expect("base64url")
    }

    #[test]
    fn encryption_matches_rfc_8291_example() {
        let sender = SecretKey::from_slice(&bytes("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")).expect("key");
        let salt: [u8; 16] = bytes("DGv6ra1nlYgDCS1FRnbzlw").try_into().expect("salt");
        let body = encrypt(
            b"When I grow up, I want to be a watermelon",
            &bytes("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4"),
            &bytes("BTBZMqHH6r4Tts7J_aSIgg"),
            &sender,
            &salt,
        )
        .expect("encrypts");
        let expected = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";
        assert_eq!(encode(&body), expected);
    }

    #[test]
    fn vapid_token_is_signed_for_the_push_service_origin() {
        let (public, private) = keypair();
        assert!(valid_public_key(&public) && valid_private_key(&private));
        let header = vapid("https://web.push.apple.com/QGuQyavXutnMH", "mailto:admin@example.com", &private, 1_000).expect("token");
        let (token, key) = header.strip_prefix("vapid t=").and_then(|rest| rest.split_once(", k=")).expect("format");
        assert_eq!(key, public);
        let (signed, signature) = token.rsplit_once('.').expect("signature");
        let claims: serde_json::Value = serde_json::from_slice(&bytes(signed.split('.').nth(1).expect("claims"))).expect("json");
        assert_eq!(claims["aud"], "https://web.push.apple.com");
        assert_eq!(claims["exp"], 1_000 + 12 * 3600);
        let verifier = VerifyingKey::from_sec1_bytes(&bytes(&public)).expect("public key");
        let signature = Signature::from_slice(&bytes(signature)).expect("signature bytes");
        assert!(verifier.verify(signed.as_bytes(), &signature).is_ok());
    }
}
