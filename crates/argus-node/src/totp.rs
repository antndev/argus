use std::fmt::Write;

use data_encoding::BASE32_NOPAD;
use hkdf::hmac::{Hmac, Mac};
use qrcodegen::{QrCode, QrCodeEcc};
use sha1::Sha1;

use crate::sync::same_secret;
use crate::util::SECOND;

const STEP: i64 = 30 * SECOND;
const QUIET_ZONE: i32 = 4;

pub fn secret() -> String {
    let mut key = [0u8; 20];
    getrandom::fill(&mut key).expect("operating system random source");
    BASE32_NOPAD.encode(&key)
}

pub fn uri(secret: &str) -> String {
    format!("otpauth://totp/Argus?secret={secret}&issuer=Argus&algorithm=SHA1&digits=6&period=30")
}

fn code(key: &[u8], step: u64) -> String {
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(key).expect("hmac takes any key length");
    mac.update(&step.to_be_bytes());
    let hash = mac.finalize().into_bytes();
    let offset = usize::from(hash[19] & 0x0f);
    let value = u32::from_be_bytes([hash[offset] & 0x7f, hash[offset + 1], hash[offset + 2], hash[offset + 3]]);
    format!("{:06}", value % 1_000_000)
}

pub fn accept(secret: &str, input: &str, now: i64, last_step: i64) -> Option<i64> {
    let input: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if input.len() != 6 || !input.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let key = BASE32_NOPAD.decode(secret.as_bytes()).ok()?;
    let current = now / STEP;
    (current - 1..=current + 1).filter(|step| *step > last_step).find(|step| same_secret(&input, &code(&key, *step as u64)))
}

pub fn qr(text: &str) -> Option<String> {
    let qr = QrCode::encode_text(text, QrCodeEcc::Medium).ok()?;
    let size = qr.size() + 2 * QUIET_ZONE;
    let mut path = String::new();
    for y in 0..qr.size() {
        for x in (0..qr.size()).filter(|x| qr.get_module(*x, y)) {
            let _ = write!(path, "M{} {}h1v1h-1z", x + QUIET_ZONE, y + QUIET_ZONE);
        }
    }
    Some(format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}" shape-rendering="crispEdges" aria-hidden="true"><rect width="{size}" height="{size}" fill="#fff"/><path d="{path}" fill="#000"/></svg>"##
    ))
}

#[cfg(test)]
pub fn current_code(secret: &str, now: i64) -> String {
    code(&BASE32_NOPAD.decode(secret.as_bytes()).expect("base32"), (now / STEP) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_the_rfc_6238_vectors() {
        let key = b"12345678901234567890";
        assert_eq!(code(key, 59 / 30), "287082");
        assert_eq!(code(key, 1_111_111_109 / 30), "081804");
        assert_eq!(code(key, 1_234_567_890 / 30), "005924");
        assert_eq!(code(key, 20_000_000_000 / 30), "353130");
    }

    #[test]
    fn codes_are_accepted_once_within_one_step_of_the_clock() {
        let secret = secret();
        let key = BASE32_NOPAD.decode(secret.as_bytes()).expect("base32");
        let now = 1_700_000_000_000;
        let step = now / STEP;
        let current = code(&key, step as u64);
        assert_eq!(accept(&secret, &current, now, 0), Some(step));
        assert_eq!(accept(&secret, &format!("{} {}", &current[..3], &current[3..]), now, 0), Some(step));
        assert_eq!(accept(&secret, &current, now, step), None);
        assert_eq!(accept(&secret, &code(&key, step as u64 - 1), now, 0), Some(step - 1));
        assert_eq!(accept(&secret, &code(&key, step as u64 - 2), now, 0), None);
        assert_eq!(accept(&secret, "12345", now, 0), None);
        assert_eq!(accept("not base32!", &current, now, 0), None);
    }

    #[test]
    fn the_qr_code_is_a_self_contained_svg() {
        let svg = qr(&uri(&secret())).expect("encodes");
        assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
        assert!(!svg.contains("<script"));
    }
}
