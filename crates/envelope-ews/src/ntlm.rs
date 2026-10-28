//! NTLMv2 client messages ([MS-NLMP]): the NEGOTIATE message, parsing of
//! the server's CHALLENGE and the AUTHENTICATE message with the NTLMv2
//! response, the message integrity code and TLS channel bindings
//! (Extended Protection for Authentication, RFC 5929
//! `tls-server-end-point`).
//!
//! Only what HTTP authentication needs is implemented: no session
//! security (signing / sealing), hence no key exchange, and no NTLMv1.
//!
//! [MS-NLMP]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-nlmp/

use hmac::{Hmac, Mac};
use md4::Md4;
use md5::{Digest as _, Md5};
use sha2::{Sha224, Sha256, Sha384, Sha512};

type HmacMd5 = Hmac<Md5>;

const SIGNATURE: &[u8; 8] = b"NTLMSSP\0";

const NEGOTIATE_UNICODE: u32 = 0x0000_0001;
const REQUEST_TARGET: u32 = 0x0000_0004;
const NEGOTIATE_NTLM: u32 = 0x0000_0200;
const NEGOTIATE_ALWAYS_SIGN: u32 = 0x0000_8000;
const TARGET_TYPE_DOMAIN: u32 = 0x0001_0000;
const NEGOTIATE_EXTENDED_SESSIONSECURITY: u32 = 0x0008_0000;
const NEGOTIATE_TARGET_INFO: u32 = 0x0080_0000;
const NEGOTIATE_VERSION: u32 = 0x0200_0000;
const NEGOTIATE_128: u32 = 0x2000_0000;
const NEGOTIATE_56: u32 = 0x8000_0000;

/// Flags sent in the NEGOTIATE message.
const CLIENT_FLAGS: u32 = NEGOTIATE_UNICODE
    | REQUEST_TARGET
    | NEGOTIATE_NTLM
    | NEGOTIATE_ALWAYS_SIGN
    | NEGOTIATE_EXTENDED_SESSIONSECURITY
    | NEGOTIATE_TARGET_INFO
    | NEGOTIATE_VERSION
    | NEGOTIATE_128
    | NEGOTIATE_56;

const AV_EOL: u16 = 0;
const AV_FLAGS: u16 = 6;
const AV_TIMESTAMP: u16 = 7;
const AV_TARGET_NAME: u16 = 9;
const AV_CHANNEL_BINDINGS: u16 = 10;

/// `MsvAvFlags` bit: the AUTHENTICATE message carries a MIC.
const AV_FLAG_MIC_PRESENT: u32 = 0x0000_0002;

/// VERSION structure (informational only): 10.0, build 19041, NTLM revision 15.
const CLIENT_VERSION: [u8; 8] = [10, 0, 0x61, 0x4A, 0, 0, 0, 15];

/// Size of the fixed part of the AUTHENTICATE message (with VERSION and MIC).
const AUTHENTICATE_HEADER_LEN: usize = 88;
/// Offset of the MIC inside the AUTHENTICATE message.
const MIC_OFFSET: usize = 72;

/// Seconds between 1601-01-01 (FILETIME epoch) and 1970-01-01.
const FILETIME_UNIX_OFFSET_SECS: u64 = 11_644_473_600;

/// A malformed or unusable server message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NtlmError(pub(crate) String);

impl std::fmt::Display for NtlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The NEGOTIATE message (type 1).
pub(crate) fn negotiate_message() -> Vec<u8> {
    let header_len: u32 = 40;
    let mut message = Vec::with_capacity(40);
    message.extend_from_slice(SIGNATURE);
    message.extend_from_slice(&1u32.to_le_bytes());
    message.extend_from_slice(&CLIENT_FLAGS.to_le_bytes());
    // Domain and workstation are not supplied.
    push_fields(&mut message, 0, header_len);
    push_fields(&mut message, 0, header_len);
    message.extend_from_slice(&CLIENT_VERSION);
    message
}

/// A parsed CHALLENGE message (type 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Challenge {
    pub(crate) flags: u32,
    pub(crate) server_challenge: [u8; 8],
    /// The target name (domain or server), UTF-16LE.
    pub(crate) target_name: Vec<u8>,
    /// The raw `TargetInfo` AV pairs.
    pub(crate) target_info: Vec<u8>,
    /// The message as received (input of the MIC).
    pub(crate) raw: Vec<u8>,
}

impl Challenge {
    /// The target name as text, when it names a domain.
    pub(crate) fn domain_name(&self) -> Option<String> {
        if self.flags & TARGET_TYPE_DOMAIN == 0 || self.target_name.is_empty() {
            return None;
        }
        Some(from_utf16le(&self.target_name))
    }
}

/// Parses a CHALLENGE message.
pub(crate) fn parse_challenge(bytes: &[u8]) -> Result<Challenge, NtlmError> {
    if bytes.len() < 32 || &bytes[..8] != SIGNATURE {
        return Err(NtlmError("not an NTLM message".into()));
    }
    if read_u32(bytes, 8)? != 2 {
        return Err(NtlmError("expected an NTLM CHALLENGE message".into()));
    }
    let target_name = read_field(bytes, 12)?.to_vec();
    let flags = read_u32(bytes, 20)?;
    let mut server_challenge = [0u8; 8];
    server_challenge.copy_from_slice(&bytes[24..32]);
    let target_info = if bytes.len() >= 48 && flags & NEGOTIATE_TARGET_INFO != 0 {
        read_field(bytes, 40)?.to_vec()
    } else {
        Vec::new()
    };
    if flags & NEGOTIATE_UNICODE == 0 {
        return Err(NtlmError(
            "the server does not support Unicode NTLM messages".into(),
        ));
    }
    // Validate the AV pairs up front so that a broken server fails here.
    parse_av_pairs(&target_info)?;
    Ok(Challenge {
        flags,
        server_challenge,
        target_name,
        target_info,
        raw: bytes.to_vec(),
    })
}

/// Account and channel data for the AUTHENTICATE message.
#[derive(Debug, Clone)]
pub(crate) struct AuthenticateInput<'a> {
    pub(crate) user: &'a str,
    /// Domain as configured; empty for UPN logons.
    pub(crate) domain: &'a str,
    pub(crate) password: &'a str,
    pub(crate) workstation: &'a str,
    /// The NEGOTIATE message that was sent (input of the MIC).
    pub(crate) negotiate: &'a [u8],
    pub(crate) client_challenge: [u8; 8],
    /// Current time as FILETIME, used when the server sent no timestamp.
    pub(crate) now: u64,
    /// MD5 of the GSS channel bindings, for TLS connections.
    pub(crate) channel_bindings: Option<[u8; 16]>,
    /// Service principal name of the server (`HTTP/mail.example.com`).
    pub(crate) target_spn: Option<&'a str>,
}

/// Builds the AUTHENTICATE message (type 3) answering `challenge`.
pub(crate) fn authenticate_message(
    challenge: &Challenge,
    input: &AuthenticateInput<'_>,
) -> Result<Vec<u8>, NtlmError> {
    let pairs = parse_av_pairs(&challenge.target_info)?;
    let server_timestamp = pairs
        .iter()
        .find(|(id, value)| *id == AV_TIMESTAMP && value.len() == 8)
        .map(|(_, value)| {
            let mut buf = [0u8; 8];
            buf.copy_from_slice(value);
            u64::from_le_bytes(buf)
        });
    // With a server timestamp the MIC is sent and the LM response omitted.
    let use_mic = server_timestamp.is_some();
    let timestamp = server_timestamp.unwrap_or(input.now);

    // A bare user name without domain logs on to the server's domain.
    let domain = if input.domain.is_empty() && !input.user.contains('@') {
        challenge.domain_name().unwrap_or_default()
    } else {
        input.domain.to_owned()
    };

    let target_info = client_target_info(&pairs, use_mic, input);
    let response_key = ntowfv2(input.password, input.user, &domain);
    let (nt_response, session_base_key) = ntlmv2_response(
        &response_key,
        &challenge.server_challenge,
        &input.client_challenge,
        timestamp,
        &target_info,
    );
    let lm_response = if use_mic {
        vec![0u8; 24]
    } else {
        lmv2_response(
            &response_key,
            &challenge.server_challenge,
            &input.client_challenge,
        )
    };

    let domain_bytes = to_utf16le(&domain);
    let user_bytes = to_utf16le(input.user);
    let workstation_bytes = to_utf16le(input.workstation);
    let flags = challenge.flags & (CLIENT_FLAGS | NEGOTIATE_TARGET_INFO);

    let domain_offset = AUTHENTICATE_HEADER_LEN;
    let user_offset = domain_offset + domain_bytes.len();
    let workstation_offset = user_offset + user_bytes.len();
    let lm_offset = workstation_offset + workstation_bytes.len();
    let nt_offset = lm_offset + lm_response.len();
    let key_offset = nt_offset + nt_response.len();

    let mut message = Vec::with_capacity(key_offset);
    message.extend_from_slice(SIGNATURE);
    message.extend_from_slice(&3u32.to_le_bytes());
    push_field(&mut message, lm_response.len(), lm_offset)?;
    push_field(&mut message, nt_response.len(), nt_offset)?;
    push_field(&mut message, domain_bytes.len(), domain_offset)?;
    push_field(&mut message, user_bytes.len(), user_offset)?;
    push_field(&mut message, workstation_bytes.len(), workstation_offset)?;
    // No key exchange: the encrypted random session key is empty.
    push_field(&mut message, 0, key_offset)?;
    message.extend_from_slice(&flags.to_le_bytes());
    message.extend_from_slice(&CLIENT_VERSION);
    message.extend_from_slice(&[0u8; 16]); // MIC, filled in below
    debug_assert_eq!(message.len(), AUTHENTICATE_HEADER_LEN);
    message.extend_from_slice(&domain_bytes);
    message.extend_from_slice(&user_bytes);
    message.extend_from_slice(&workstation_bytes);
    message.extend_from_slice(&lm_response);
    message.extend_from_slice(&nt_response);

    if use_mic {
        // No key exchange: the exported session key is the session base key.
        let mic = hmac_md5(
            &session_base_key,
            &[input.negotiate, &challenge.raw, &message],
        );
        message[MIC_OFFSET..MIC_OFFSET + 16].copy_from_slice(&mic);
    }
    Ok(message)
}

/// The target info the client sends back: the server's pairs plus
/// `MsvAvFlags` (MIC present), channel bindings and the target SPN.
fn client_target_info(
    pairs: &[(u16, &[u8])],
    use_mic: bool,
    input: &AuthenticateInput<'_>,
) -> Vec<u8> {
    let mut info = Vec::new();
    let mut av_flags: u32 = 0;
    for (id, value) in pairs {
        match *id {
            AV_FLAGS if value.len() == 4 => {
                let mut buf = [0u8; 4];
                buf.copy_from_slice(value);
                av_flags = u32::from_le_bytes(buf);
            }
            AV_FLAGS | AV_CHANNEL_BINDINGS | AV_TARGET_NAME => {}
            _ => push_av(&mut info, *id, value),
        }
    }
    if use_mic {
        av_flags |= AV_FLAG_MIC_PRESENT;
    }
    if av_flags != 0 {
        push_av(&mut info, AV_FLAGS, &av_flags.to_le_bytes());
    }
    push_av(
        &mut info,
        AV_CHANNEL_BINDINGS,
        &input.channel_bindings.unwrap_or([0u8; 16]),
    );
    if let Some(spn) = input.target_spn {
        push_av(&mut info, AV_TARGET_NAME, &to_utf16le(spn));
    }
    push_av(&mut info, AV_EOL, &[]);
    info
}

/// `NTOWFv2`: HMAC-MD5 keyed with the NT hash over the upper-cased user
/// name and the domain.
pub(crate) fn ntowfv2(password: &str, user: &str, domain: &str) -> [u8; 16] {
    let nt_hash: [u8; 16] = Md4::digest(to_utf16le(password)).into();
    let identity = to_utf16le(&format!("{}{}", windows_upper(user), domain));
    hmac_md5(&nt_hash, &[&identity])
}

/// The NTLMv2 response (`NTProofStr || temp`) and the session base key.
pub(crate) fn ntlmv2_response(
    response_key: &[u8; 16],
    server_challenge: &[u8; 8],
    client_challenge: &[u8; 8],
    timestamp: u64,
    target_info: &[u8],
) -> (Vec<u8>, [u8; 16]) {
    let mut temp = Vec::with_capacity(32 + target_info.len());
    temp.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0, 0]);
    temp.extend_from_slice(&timestamp.to_le_bytes());
    temp.extend_from_slice(client_challenge);
    temp.extend_from_slice(&[0u8; 4]);
    temp.extend_from_slice(target_info);
    temp.extend_from_slice(&[0u8; 4]);
    let nt_proof = hmac_md5(response_key, &[server_challenge, &temp]);
    let session_base_key = hmac_md5(response_key, &[&nt_proof]);
    let mut response = nt_proof.to_vec();
    response.extend_from_slice(&temp);
    (response, session_base_key)
}

/// The LMv2 response.
pub(crate) fn lmv2_response(
    response_key: &[u8; 16],
    server_challenge: &[u8; 8],
    client_challenge: &[u8; 8],
) -> Vec<u8> {
    let mut response = hmac_md5(response_key, &[server_challenge, client_challenge]).to_vec();
    response.extend_from_slice(client_challenge);
    response
}

/// `tls-server-end-point` channel bindings (RFC 5929) of a TLS server
/// certificate, hashed the way NTLM carries them in `MsvAvChannelBindings`:
/// MD5 over the GSS channel bindings structure with empty addresses.
pub(crate) fn tls_channel_bindings(certificate_der: &[u8]) -> [u8; 16] {
    let mut application_data = b"tls-server-end-point:".to_vec();
    application_data.extend_from_slice(&certificate_hash(certificate_der));
    let length = u32::try_from(application_data.len()).unwrap_or(u32::MAX);
    let mut bindings = Vec::with_capacity(20 + application_data.len());
    // initiator address type and length, acceptor address type and length
    bindings.extend_from_slice(&[0u8; 16]);
    bindings.extend_from_slice(&length.to_le_bytes());
    bindings.extend_from_slice(&application_data);
    Md5::digest(&bindings).into()
}

/// Hash of the certificate for `tls-server-end-point`: the hash function of
/// the certificate's signature algorithm, except that MD5 and SHA-1 (and
/// anything unrecognized) are replaced by SHA-256.
fn certificate_hash(der: &[u8]) -> Vec<u8> {
    match signature_hash(der) {
        SignatureHash::Sha224 => Sha224::digest(der).to_vec(),
        SignatureHash::Sha384 => Sha384::digest(der).to_vec(),
        SignatureHash::Sha512 => Sha512::digest(der).to_vec(),
        SignatureHash::Sha256 => Sha256::digest(der).to_vec(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignatureHash {
    Sha224,
    Sha256,
    Sha384,
    Sha512,
}

/// The hash function named by the certificate's `signatureAlgorithm`.
fn signature_hash(der: &[u8]) -> SignatureHash {
    const RSA_PREFIX: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01];
    const ECDSA_PREFIX: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03];
    let Some(oid) = signature_algorithm_oid(der) else {
        return SignatureHash::Sha256;
    };
    if let Some(rest) = oid.strip_prefix(RSA_PREFIX) {
        return match rest {
            [0x0C] => SignatureHash::Sha384,
            [0x0D] => SignatureHash::Sha512,
            [0x0E] => SignatureHash::Sha224,
            _ => SignatureHash::Sha256,
        };
    }
    if let Some(rest) = oid.strip_prefix(ECDSA_PREFIX) {
        return match rest {
            [0x01] => SignatureHash::Sha224,
            [0x03] => SignatureHash::Sha384,
            [0x04] => SignatureHash::Sha512,
            _ => SignatureHash::Sha256,
        };
    }
    SignatureHash::Sha256
}

/// The OID bytes of `Certificate.signatureAlgorithm.algorithm`.
fn signature_algorithm_oid(der: &[u8]) -> Option<&[u8]> {
    // Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signatureValue }
    let (tag, certificate, _) = der_element(der)?;
    if tag != 0x30 {
        return None;
    }
    let (_, _, after_tbs) = der_element(certificate)?;
    let (tag, algorithm, _) = der_element(after_tbs)?;
    if tag != 0x30 {
        return None;
    }
    let (tag, oid, _) = der_element(algorithm)?;
    (tag == 0x06).then_some(oid)
}

/// Splits one DER element off `input`: (tag, contents, rest).
fn der_element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *input.first()?;
    let first = *input.get(1)? as usize;
    let (length, header) = if first < 0x80 {
        (first, 2)
    } else {
        let count = first & 0x7F;
        if count == 0 || count > 4 {
            return None;
        }
        let mut length = 0usize;
        for i in 0..count {
            length = (length << 8) | *input.get(2 + i)? as usize;
        }
        (length, 2 + count)
    };
    let end = header.checked_add(length)?;
    let contents = input.get(header..end)?;
    Some((tag, contents, &input[end..]))
}

/// Current time as a FILETIME (100 ns ticks since 1601-01-01 UTC).
pub(crate) fn filetime_now() -> u64 {
    let since_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (since_unix.as_secs() + FILETIME_UNIX_OFFSET_SECS) * 10_000_000
        + u64::from(since_unix.subsec_nanos() / 100)
}

/// Eight random bytes for the client challenge.
pub(crate) fn random_client_challenge() -> Result<[u8; 8], NtlmError> {
    let mut challenge = [0u8; 8];
    getrandom::fill(&mut challenge)
        .map_err(|e| NtlmError(format!("no randomness available: {e}")))?;
    Ok(challenge)
}

fn hmac_md5(key: &[u8], parts: &[&[u8]]) -> [u8; 16] {
    // HMAC accepts keys of any length; the error case cannot happen.
    let mut mac = match HmacMd5::new_from_slice(key) {
        Ok(mac) => mac,
        Err(_) => return [0u8; 16],
    };
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

fn push_field(message: &mut Vec<u8>, len: usize, offset: usize) -> Result<(), NtlmError> {
    let len = u16::try_from(len).map_err(|_| NtlmError("NTLM field too large".into()))?;
    let offset = u32::try_from(offset).map_err(|_| NtlmError("NTLM message too large".into()))?;
    push_fields(message, len, offset);
    Ok(())
}

fn push_fields(message: &mut Vec<u8>, len: u16, offset: u32) {
    message.extend_from_slice(&len.to_le_bytes());
    message.extend_from_slice(&len.to_le_bytes());
    message.extend_from_slice(&offset.to_le_bytes());
}

fn push_av(info: &mut Vec<u8>, id: u16, value: &[u8]) {
    let len = u16::try_from(value.len()).unwrap_or(u16::MAX);
    info.extend_from_slice(&id.to_le_bytes());
    info.extend_from_slice(&len.to_le_bytes());
    info.extend_from_slice(&value[..usize::from(len)]);
}

fn read_u16(bytes: &[u8], at: usize) -> Result<u16, NtlmError> {
    bytes
        .get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| NtlmError("truncated NTLM message".into()))
}

fn read_u32(bytes: &[u8], at: usize) -> Result<u32, NtlmError> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| NtlmError("truncated NTLM message".into()))
}

/// Reads a (length, max length, offset) field descriptor and returns the
/// bytes it points to.
fn read_field(bytes: &[u8], at: usize) -> Result<&[u8], NtlmError> {
    let len = usize::from(read_u16(bytes, at)?);
    let offset = read_u32(bytes, at + 4)? as usize;
    if len == 0 {
        return Ok(&[]);
    }
    bytes
        .get(offset..offset.saturating_add(len))
        .ok_or_else(|| NtlmError("NTLM field points outside the message".into()))
}

/// Splits target info into (id, value) pairs, stopping at `MsvAvEOL`.
fn parse_av_pairs(info: &[u8]) -> Result<Vec<(u16, &[u8])>, NtlmError> {
    let mut pairs = Vec::new();
    let mut at = 0usize;
    while at < info.len() {
        let id = read_u16(info, at)?;
        let len = usize::from(read_u16(info, at + 2)?);
        if id == AV_EOL {
            break;
        }
        let value = info
            .get(at + 4..at + 4 + len)
            .ok_or_else(|| NtlmError("truncated NTLM target info".into()))?;
        pairs.push((id, value));
        at += 4 + len;
    }
    Ok(pairs)
}

fn to_utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn from_utf16le(bytes: &[u8]) -> String {
    let (pairs, _) = bytes.as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
    String::from_utf16_lossy(&units)
}

/// Upper-cases like Windows does for NTOWFv2: per character, keeping
/// characters whose upper case is not a single character (`ß`).
fn windows_upper(text: &str) -> String {
    text.chars()
        .map(|c| {
            let mut upper = c.to_uppercase();
            match (upper.next(), upper.next()) {
                (Some(u), None) => u,
                _ => c,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn unhex(text: &str) -> Vec<u8> {
        let text: String = text.split_whitespace().collect();
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The CHALLENGE_MESSAGE of MS-NLMP 4.2.4.3.
    const SPEC_CHALLENGE: &str = concat!(
        "4e544c4d53535000 02000000 0c000c00 38000000 33828ae2 0123456789abcdef",
        "0000000000000000 24002400 44000000 060070170000000f",
        "530065007200760065007200",
        "02000c0044006f006d00610069006e00 01000c00530065007200760065007200 00000000"
    );

    fn spec_challenge_bytes() -> Vec<u8> {
        unhex(SPEC_CHALLENGE)
    }

    #[test]
    fn ms_nlmp_ntowfv2_vector() {
        // MS-NLMP 4.2.4.1.1
        assert_eq!(
            hex(&ntowfv2("Password", "User", "Domain")),
            "0c868a403bfd7a93a3001ef22ef02e3f"
        );
        // The user name is upper-cased, the domain is not.
        assert_eq!(
            ntowfv2("Password", "user", "Domain"),
            ntowfv2("Password", "User", "Domain")
        );
        assert_ne!(
            ntowfv2("Password", "User", "DOMAIN"),
            ntowfv2("Password", "User", "Domain")
        );
    }

    #[test]
    fn ms_nlmp_ntlmv2_and_lmv2_vectors() {
        let challenge = parse_challenge(&spec_challenge_bytes()).expect("challenge");
        assert_eq!(hex(&challenge.server_challenge), "0123456789abcdef");
        assert_eq!(challenge.flags, 0xe28a_8233);
        assert_eq!(from_utf16le(&challenge.target_name), "Server");
        assert_eq!(challenge.target_info.len(), 0x24);
        let key = ntowfv2("Password", "User", "Domain");
        let client_challenge = [0xaa; 8];
        // MS-NLMP 4.2.4.2.1
        assert_eq!(
            hex(&lmv2_response(
                &key,
                &challenge.server_challenge,
                &client_challenge
            )),
            "86c35097ac9cec102554764a57cccc19aaaaaaaaaaaaaaaa"
        );
        // MS-NLMP 4.2.4.2.2 (time zero, the server's target info verbatim)
        let (response, session_base_key) = ntlmv2_response(
            &key,
            &challenge.server_challenge,
            &client_challenge,
            0,
            &challenge.target_info,
        );
        assert_eq!(hex(&response[..16]), "68cd0ab851e51c96aabc927bebef6a1c");
        // MS-NLMP 4.2.4.1.2
        assert_eq!(hex(&session_base_key), "8de40ccadbc14a82f15cb0ad0de95ca3");
        assert_eq!(&response[16..24], &[1, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&response[32..40], &[0xaa; 8]);
    }

    #[test]
    fn negotiate_message_layout() {
        let message = negotiate_message();
        assert_eq!(message.len(), 40);
        assert_eq!(&message[..8], b"NTLMSSP\0");
        assert_eq!(read_u32(&message, 8), Ok(1));
        let flags = read_u32(&message, 12).unwrap();
        assert_ne!(flags & NEGOTIATE_UNICODE, 0);
        assert_ne!(flags & NEGOTIATE_EXTENDED_SESSIONSECURITY, 0);
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(&message[..8]),
            "TlRMTVNTUAA="
        );
    }

    #[test]
    fn rejects_malformed_challenges() {
        assert!(parse_challenge(b"garbage").is_err());
        let mut wrong_type = spec_challenge_bytes();
        wrong_type[8] = 1;
        assert!(parse_challenge(&wrong_type).is_err());
        let mut outside = spec_challenge_bytes();
        outside[44] = 0xff; // target info offset beyond the end
        assert!(parse_challenge(&outside).is_err());
        let mut oem = spec_challenge_bytes();
        oem[20] &= !0x01;
        assert!(parse_challenge(&oem).is_err());
    }

    /// Reads the payload a header field of an AUTHENTICATE message points to.
    fn field(message: &[u8], at: usize) -> Vec<u8> {
        read_field(message, at).unwrap().to_vec()
    }

    fn challenge_with_timestamp() -> Challenge {
        let mut info = Vec::new();
        push_av(&mut info, 2, &to_utf16le("CORP"));
        push_av(&mut info, 1, &to_utf16le("EXCH01"));
        push_av(&mut info, 7, &0x01d9_0000_0000_0000u64.to_le_bytes());
        push_av(&mut info, AV_EOL, &[]);
        let target_name = to_utf16le("CORP");
        let mut message = Vec::new();
        message.extend_from_slice(SIGNATURE);
        message.extend_from_slice(&2u32.to_le_bytes());
        push_fields(&mut message, target_name.len() as u16, 56);
        let flags = CLIENT_FLAGS | TARGET_TYPE_DOMAIN;
        message.extend_from_slice(&flags.to_le_bytes());
        message.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        message.extend_from_slice(&[0u8; 8]);
        push_fields(
            &mut message,
            info.len() as u16,
            56 + target_name.len() as u32,
        );
        message.extend_from_slice(&CLIENT_VERSION);
        message.extend_from_slice(&target_name);
        message.extend_from_slice(&info);
        parse_challenge(&message).expect("challenge")
    }

    #[test]
    fn authenticate_message_with_mic_and_channel_bindings() {
        let challenge = challenge_with_timestamp();
        assert_eq!(challenge.domain_name().as_deref(), Some("CORP"));
        let negotiate = negotiate_message();
        let bindings = [7u8; 16];
        let input = AuthenticateInput {
            user: "jane",
            domain: "",
            password: "Secret!",
            workstation: "LAPTOP",
            negotiate: &negotiate,
            client_challenge: [0x11; 8],
            now: 1,
            channel_bindings: Some(bindings),
            target_spn: Some("HTTP/mail.corp.example"),
        };
        let message = authenticate_message(&challenge, &input).expect("authenticate");
        assert_eq!(&message[..8], SIGNATURE);
        assert_eq!(read_u32(&message, 8), Ok(3));
        // LM response is all zeros when the server sent a timestamp.
        assert_eq!(field(&message, 12), vec![0u8; 24]);
        // The bare user name logs on to the server's domain.
        assert_eq!(from_utf16le(&field(&message, 28)), "CORP");
        assert_eq!(from_utf16le(&field(&message, 36)), "jane");
        assert_eq!(from_utf16le(&field(&message, 44)), "LAPTOP");

        // The NT response embeds the server timestamp and the amended target info.
        let nt = field(&message, 20);
        assert_eq!(&nt[24..32], &0x01d9_0000_0000_0000u64.to_le_bytes());
        assert_eq!(&nt[32..40], &[0x11; 8]);
        let info = &nt[44..nt.len() - 4];
        let pairs = parse_av_pairs(info).unwrap();
        let get = |id: u16| {
            pairs
                .iter()
                .find(|(i, _)| *i == id)
                .map(|(_, v)| v.to_vec())
        };
        assert_eq!(
            get(AV_FLAGS),
            Some(AV_FLAG_MIC_PRESENT.to_le_bytes().to_vec())
        );
        assert_eq!(get(AV_CHANNEL_BINDINGS), Some(bindings.to_vec()));
        assert_eq!(
            get(AV_TARGET_NAME),
            Some(to_utf16le("HTTP/mail.corp.example"))
        );
        assert_eq!(get(1), Some(to_utf16le("EXCH01")));

        // The NT proof verifies with the expected key and the MIC covers all
        // three messages.
        let key = ntowfv2("Secret!", "jane", "CORP");
        let (expected, session_base_key) = ntlmv2_response(
            &key,
            &challenge.server_challenge,
            &[0x11; 8],
            0x01d9_0000_0000_0000,
            info,
        );
        assert_eq!(nt, expected);
        let mut zeroed = message.clone();
        zeroed[MIC_OFFSET..MIC_OFFSET + 16].fill(0);
        let mic = hmac_md5(&session_base_key, &[&negotiate, &challenge.raw, &zeroed]);
        assert_eq!(&message[MIC_OFFSET..MIC_OFFSET + 16], &mic);
    }

    #[test]
    fn authenticate_message_without_server_timestamp_sends_lmv2() {
        let challenge = parse_challenge(&spec_challenge_bytes()).expect("challenge");
        let negotiate = negotiate_message();
        let input = AuthenticateInput {
            user: "User",
            domain: "Domain",
            password: "Password",
            workstation: "COMPUTER",
            negotiate: &negotiate,
            client_challenge: [0xaa; 8],
            now: 0,
            channel_bindings: None,
            target_spn: None,
        };
        let message = authenticate_message(&challenge, &input).expect("authenticate");
        assert_eq!(
            hex(&field(&message, 12)),
            "86c35097ac9cec102554764a57cccc19aaaaaaaaaaaaaaaa"
        );
        assert_eq!(&message[MIC_OFFSET..MIC_OFFSET + 16], &[0u8; 16]);
        assert_eq!(from_utf16le(&field(&message, 28)), "Domain");
        let nt = field(&message, 20);
        let info = &nt[44..nt.len() - 4];
        let pairs = parse_av_pairs(info).unwrap();
        // Zero channel bindings are sent when there are none.
        assert!(
            pairs
                .iter()
                .any(|(id, v)| *id == AV_CHANNEL_BINDINGS && *v == [0u8; 16])
        );
        assert!(!pairs.iter().any(|(id, _)| *id == AV_FLAGS));
    }

    /// A self-signed P-256 certificate signed with ECDSA/SHA-384.
    const CERT_ECDSA_SHA384: &str = "MIIBizCCATGgAwIBAgIUXL576lc4V/yH7c6gsWljKudWdMUwCgYIKoZIzj0EAwMwGzEZMBcGA1UEAwwQbWFpbC5leGFtcGxlLmNvbTAeFw0yNjEwMjgxOTA2MTVaFw0zNjEwMjUxOTA2MTVaMBsxGTAXBgNVBAMMEG1haWwuZXhhbXBsZS5jb20wWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAQWRSuRCfIKD2bKgHNkKEFuK9vu2JX4ldJl1qbknhAQQO70+U/jiNpHek+Npsc8252U8wtys99iakt0vzCPGloFo1MwUTAdBgNVHQ4EFgQUk5RyqrGwgnM7Hatji6K5QcQQ7NcwHwYDVR0jBBgwFoAUk5RyqrGwgnM7Hatji6K5QcQQ7NcwDwYDVR0TAQH/BAUwAwEB/zAKBggqhkjOPQQDAwNIADBFAiEAhUuvWNKRimrY3bQZeDWE0GozVrrPKvBoJYqfsf3JgjYCIAY385s4d6e0ghf4fJm6zSI6BmKLxzhk6+gc8JFi5lq2";

    #[test]
    fn channel_bindings_follow_the_signature_hash() {
        let der = base64::engine::general_purpose::STANDARD
            .decode(CERT_ECDSA_SHA384)
            .expect("certificate");
        assert_eq!(signature_hash(&der), SignatureHash::Sha384);
        let mut expected_input = vec![0u8; 16];
        let mut app = b"tls-server-end-point:".to_vec();
        app.extend_from_slice(&Sha384::digest(&der));
        expected_input.extend_from_slice(&(app.len() as u32).to_le_bytes());
        expected_input.extend_from_slice(&app);
        let expected: [u8; 16] = Md5::digest(&expected_input).into();
        assert_eq!(tls_channel_bindings(&der), expected);
        // Computed independently (Python hashlib) for this certificate.
        assert_eq!(hex(&expected), "1f60973c2208871cd4559607d56d2209");
        // Garbage falls back to SHA-256 rather than failing.
        assert_eq!(
            signature_hash(b"\x30\x03\x02\x01\x00"),
            SignatureHash::Sha256
        );
        assert_eq!(signature_hash(b""), SignatureHash::Sha256);
    }

    #[test]
    fn der_lengths() {
        assert_eq!(
            der_element(&[0x04, 0x01, 0xAB, 0xCD]),
            Some((0x04, &[0xAB][..], &[0xCD][..]))
        );
        let mut long = vec![0x04, 0x81, 0x80];
        long.extend(std::iter::repeat_n(0u8, 0x80));
        assert_eq!(der_element(&long).map(|(_, c, _)| c.len()), Some(0x80));
        assert_eq!(der_element(&[0x04, 0x85, 1, 1, 1, 1, 1]), None);
        assert_eq!(der_element(&[0x04, 0x05, 1]), None);
    }

    #[test]
    fn helpers() {
        assert_eq!(windows_upper("jane.doe"), "JANE.DOE");
        assert_eq!(windows_upper("straße"), "STRAßE");
        assert_eq!(from_utf16le(&to_utf16le("Ünïcode")), "Ünïcode");
        assert!(filetime_now() > 133_000_000_000_000_000);
        let a = random_client_challenge().unwrap();
        let b = random_client_challenge().unwrap();
        assert_ne!(a, b);
    }
}
