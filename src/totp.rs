//! Two-step sign-in: the six-digit codes of an authenticator app (TOTP, RFC 6238: HMAC-SHA1 of
//! the 30-second step, as every such app makes them), from a secret shared once as base32 text.

/// SHA-1 (FIPS 180-4), for HMAC only: what authenticator apps use.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476, 0xc3d2_e1f0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, t);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

fn hmac_sha1(key: &[u8], msg: &[u8]) -> [u8; 20] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..20].copy_from_slice(&sha1(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).chain(msg.iter().copied()).collect();
    let outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).chain(sha1(&inner)).collect();
    sha1(&outer)
}

/// The code for the 30-second `step` (RFC 4226's truncation, six digits).
fn code_at(secret: &[u8], step: u64) -> u32 {
    let h = hmac_sha1(secret, &step.to_be_bytes());
    let o = (h[19] & 0x0f) as usize;
    (u32::from_be_bytes([h[o], h[o + 1], h[o + 2], h[o + 3]]) & 0x7fff_ffff) % 1_000_000
}

fn step_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) / 30
}

/// Whether `code` is the app's code now, or of the step before or after (clocks drift).
pub fn check(secret_b32: &str, code: &str) -> bool {
    let Some(secret) = base32_decode(secret_b32) else { return false };
    let code = code.trim().replace(' ', "");
    let Ok(n) = code.parse::<u32>() else { return false };
    code.len() == 6 && [-1i64, 0, 1].iter().any(|d| code_at(&secret, (step_now() as i64 + d) as u64) == n)
}

/// The code an app shows now, for tests.
#[cfg(test)]
pub fn now(secret_b32: &str) -> String {
    format!("{:06}", code_at(&base32_decode(secret_b32).unwrap_or_default(), step_now()))
}

/// A new secret: 20 random bytes as base32 text, as authenticator apps take it.
pub fn new_secret() -> String {
    let mut b = [0u8; 20];
    let _ = getrandom::getrandom(&mut b);
    base32_encode(&b)
}

/// The secret in groups of four, to type into an app.
pub fn grouped(secret: &str) -> String {
    secret.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

/// The link authenticator apps read (from a QR code, or pasted): the account named by the ID.
pub fn uri(secret: &str, id: &str) -> String {
    format!("otpauth://totp/SSH%20Remote:{id}?secret={secret}&issuer=SSH%20Remote")
}

const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

fn base32_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let (mut acc, mut bits) = (0u32, 0);
    for &b in data {
        acc = (acc << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(B32[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(B32[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.chars().filter(|c| !c.is_whitespace() && *c != '=') {
        let v = B32.iter().position(|&x| x as char == c.to_ascii_uppercase())? as u32;
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_and_hmac_match_their_standards() {
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(&sha1(&[b'a'; 1000])), "291e9a6c66994949b57ba5e650361e98fc36b1ba");
        // RFC 2202, case 2.
        assert_eq!(hex(&hmac_sha1(b"Jefe", b"what do ya want for nothing?")), "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79");
    }

    /// RFC 6238's SHA-1 vectors (eight digits there: the last six are ours).
    #[test]
    fn codes_match_rfc_6238() {
        let secret = b"12345678901234567890";
        for (t, want) in [(59u64, 94_287_082u32), (1_111_111_109, 7_081_804), (1_234_567_890, 89_005_924), (2_000_000_000, 69_279_037)] {
            assert_eq!(code_at(secret, t / 30), want % 1_000_000, "{t}");
        }
    }

    #[test]
    fn secrets_round_trip_and_codes_are_checked() {
        let s = new_secret();
        assert_eq!(s.len(), 32);
        assert_eq!(base32_decode(&s).unwrap().len(), 20);
        assert_eq!(base32_encode(b"12345678901234567890"), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        let now = format!("{:06}", code_at(&base32_decode(&s).unwrap(), step_now()));
        assert!(check(&s, &now));
        assert!(check(&grouped(&s).to_lowercase(), &now));
        assert!(!check(&s, "12345") && !check(&s, "abcdef"));
    }
}
