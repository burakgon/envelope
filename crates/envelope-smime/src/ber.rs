//! A small BER reader. CMS messages and PKCS #12 files are often BER rather
//! than DER: Thunderbird, Windows and `openssl -stream` write them with
//! indefinite lengths, and cut long OCTET STRINGs into pieces. Elements keep
//! their encoding, so signed attributes can be hashed as they were sent.

use std::borrow::Cow;

use const_oid::ObjectIdentifier;

use crate::{Result, invalid};

pub(crate) const INTEGER: u8 = 0x02;
pub(crate) const BIT_STRING: u8 = 0x03;
pub(crate) const OCTET_STRING: u8 = 0x04;
pub(crate) const NULL: u8 = 0x05;
pub(crate) const OID: u8 = 0x06;
pub(crate) const UTC_TIME: u8 = 0x17;
pub(crate) const GENERALIZED_TIME: u8 = 0x18;
pub(crate) const SEQUENCE: u8 = 0x30;
pub(crate) const SET: u8 = 0x31;

const CONSTRUCTED: u8 = 0x20;

/// The tag of `[n]`, primitive.
pub(crate) const fn context(n: u8) -> u8 {
    0x80 | n
}

/// The tag of `[n]`, constructed.
pub(crate) const fn context_constructed(n: u8) -> u8 {
    0xa0 | n
}

/// How deeply elements may nest (indefinite lengths are found by reading
/// what is inside them).
const MAX_DEPTH: usize = 48;

/// One element: its tag, its content and its whole encoding.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tlv<'a> {
    pub tag: u8,
    /// The content octets (up to the end-of-contents marker of an
    /// indefinite length).
    pub value: &'a [u8],
    /// The element as encoded.
    pub raw: &'a [u8],
}

/// Reads the elements one after the other.
#[derive(Debug, Clone)]
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Reader<'a> {
        Reader { data, pos: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }

    pub fn peek_tag(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    /// The next element, or `None` at the end.
    pub fn next(&mut self) -> Result<Option<Tlv<'a>>> {
        if self.is_empty() {
            return Ok(None);
        }
        let (tlv, end) = element(self.data, self.pos, 0)?;
        self.pos = end;
        Ok(Some(tlv))
    }

    /// The next element, of any tag.
    pub fn any(&mut self) -> Result<Tlv<'a>> {
        self.next()?
            .ok_or_else(|| invalid("unexpected end of data"))
    }

    /// The next element, which must have this tag.
    pub fn read(&mut self, tag: u8) -> Result<Tlv<'a>> {
        let tlv = self.any()?;
        if tlv.tag != tag {
            return Err(invalid(&format!(
                "unexpected element {:#04x} (wanted {tag:#04x})",
                tlv.tag
            )));
        }
        Ok(tlv)
    }

    /// The next element if it has this tag.
    pub fn optional(&mut self, tag: u8) -> Result<Option<Tlv<'a>>> {
        if self.peek_tag() == Some(tag) {
            Ok(Some(self.any()?))
        } else {
            Ok(None)
        }
    }
}

/// Reads the element at `pos`; returns it and where the next one starts.
fn element(data: &[u8], pos: usize, depth: usize) -> Result<(Tlv<'_>, usize)> {
    if depth > MAX_DEPTH {
        return Err(invalid("nested too deeply"));
    }
    let short = || invalid("truncated data");
    let tag = *data.get(pos).ok_or_else(short)?;
    if tag & 0x1f == 0x1f {
        return Err(invalid("unsupported tag"));
    }
    let first = *data.get(pos + 1).ok_or_else(short)?;
    let mut start = pos + 2;
    let length = match first {
        0x80 => None,
        n if n < 0x80 => Some(n as usize),
        n => {
            let count = (n & 0x7f) as usize;
            if count > 4 {
                return Err(invalid("length too large"));
            }
            let bytes = data.get(start..start + count).ok_or_else(short)?;
            start += count;
            Some(bytes.iter().fold(0usize, |acc, b| (acc << 8) | *b as usize))
        }
    };
    match length {
        Some(len) => {
            let end = start.checked_add(len).ok_or_else(short)?;
            let value = data.get(start..end).ok_or_else(short)?;
            Ok((
                Tlv {
                    tag,
                    value,
                    raw: &data[pos..end],
                },
                end,
            ))
        }
        None => {
            if tag & CONSTRUCTED == 0 {
                return Err(invalid("indefinite length of a primitive element"));
            }
            // The content runs up to the end-of-contents marker (00 00) at
            // this level.
            let mut at = start;
            loop {
                match data.get(at..at + 2) {
                    Some([0, 0]) => break,
                    Some(_) => at = element(data, at, depth + 1)?.1,
                    None => return Err(short()),
                }
            }
            Ok((
                Tlv {
                    tag,
                    value: &data[start..at],
                    raw: &data[pos..at + 2],
                },
                at + 2,
            ))
        }
    }
}

impl<'a> Tlv<'a> {
    /// Reads a whole input that must be a single element.
    pub fn parse(data: &'a [u8]) -> Result<Tlv<'a>> {
        let mut r = Reader::new(data);
        let tlv = r.any()?;
        // Base64 decoders may leave padding behind; anything else is wrong.
        if !r.data[r.pos..].iter().all(|b| *b == 0) {
            return Err(invalid("trailing data"));
        }
        Ok(tlv)
    }

    pub fn is_constructed(&self) -> bool {
        self.tag & CONSTRUCTED != 0
    }

    /// The elements inside a constructed element.
    pub fn children(&self) -> Reader<'a> {
        Reader::new(self.value)
    }

    /// The content of a string, put together when it was sent in pieces.
    pub fn octets(&self) -> Result<Cow<'a, [u8]>> {
        if !self.is_constructed() {
            return Ok(Cow::Borrowed(self.value));
        }
        let mut out = Vec::with_capacity(self.value.len());
        self.collect(&mut out, 0)?;
        Ok(Cow::Owned(out))
    }

    fn collect(&self, out: &mut Vec<u8>, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(invalid("nested too deeply"));
        }
        let mut r = self.children();
        while let Some(piece) = r.next()? {
            if piece.is_constructed() {
                piece.collect(out, depth + 1)?;
            } else {
                out.extend_from_slice(piece.value);
            }
        }
        Ok(())
    }

    pub fn oid(&self) -> Result<ObjectIdentifier> {
        if self.tag != OID {
            return Err(invalid("expected an object identifier"));
        }
        ObjectIdentifier::from_bytes(self.value).map_err(|_| invalid("bad object identifier"))
    }

    /// A small non-negative INTEGER (versions, iteration counts, lengths).
    pub fn small_uint(&self) -> Result<u64> {
        if self.tag != INTEGER || self.value.is_empty() || self.value[0] & 0x80 != 0 {
            return Err(invalid("expected a small positive integer"));
        }
        let digits = strip_zeros(self.value);
        if digits.len() > 8 {
            return Err(invalid("integer too large"));
        }
        Ok(digits.iter().fold(0u64, |acc, b| (acc << 8) | *b as u64))
    }

    /// The DER encoding of this element: definite lengths, strings in one
    /// piece. DER input comes out unchanged.
    pub fn to_der(self) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(self.raw.len());
        self.write_der(&mut out, 0)?;
        Ok(out)
    }

    fn write_der(&self, out: &mut Vec<u8>, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(invalid("nested too deeply"));
        }
        let universal = self.tag & 0xc0 == 0;
        let number = self.tag & 0x1f;
        // OCTET STRING and the character strings, sent in pieces.
        let string = matches!(number, 4 | 12 | 18..=22 | 25..=30);
        if !self.is_constructed() {
            write_tlv(out, self.tag, self.value);
        } else if universal && string {
            write_tlv(out, self.tag & !CONSTRUCTED, &self.octets()?);
        } else if universal && number == 3 {
            // A BIT STRING in pieces: each starts with its count of unused
            // bits, which only the last may have.
            let mut bits = Vec::new();
            let mut unused = 0;
            let mut r = self.children();
            while let Some(piece) = r.next()? {
                let content = piece.to_der()?;
                let inner = Tlv::parse(&content)?;
                let (count, data) = inner
                    .value
                    .split_first()
                    .ok_or_else(|| invalid("empty bit string"))?;
                unused = *count;
                bits.extend_from_slice(data);
            }
            let mut value = vec![unused];
            value.extend_from_slice(&bits);
            write_tlv(out, BIT_STRING, &value);
        } else {
            let mut inner = Vec::with_capacity(self.value.len());
            let mut r = self.children();
            while let Some(child) = r.next()? {
                child.write_der(&mut inner, depth + 1)?;
            }
            write_tlv(out, self.tag, &inner);
        }
        Ok(())
    }
}

/// An INTEGER's content without leading zero octets (serial numbers are
/// compared this way).
pub(crate) fn strip_zeros(bytes: &[u8]) -> &[u8] {
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
    &bytes[first..]
}

/// Appends a DER element.
pub(crate) fn write_tlv(out: &mut Vec<u8>, tag: u8, value: &[u8]) {
    out.push(tag);
    let len = value.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = len.to_be_bytes();
        let skip = bytes
            .iter()
            .position(|b| *b != 0)
            .unwrap_or(bytes.len() - 1);
        out.push(0x80 | (bytes.len() - skip) as u8);
        out.extend_from_slice(&bytes[skip..]);
    }
    out.extend_from_slice(value);
}

/// A DER element.
pub(crate) fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len() + 6);
    write_tlv(&mut out, tag, value);
    out
}

/// An AlgorithmIdentifier: the algorithm and its parameters (if any).
#[derive(Debug, Clone, Copy)]
pub(crate) struct AlgorithmId<'a> {
    pub oid: ObjectIdentifier,
    pub params: Option<Tlv<'a>>,
}

impl<'a> AlgorithmId<'a> {
    pub fn read(r: &mut Reader<'a>) -> Result<AlgorithmId<'a>> {
        AlgorithmId::from_tlv(r.read(SEQUENCE)?)
    }

    pub fn from_tlv(tlv: Tlv<'a>) -> Result<AlgorithmId<'a>> {
        let mut r = tlv.children();
        let oid = r.read(OID)?.oid()?;
        let params = r.next()?.filter(|p| p.tag != NULL);
        Ok(AlgorithmId { oid, params })
    }
}

/// Unix seconds of a UTCTime or GeneralizedTime.
pub(crate) fn time(tlv: &Tlv<'_>) -> Result<i64> {
    use der::Decode;
    let der = tlv.to_der()?;
    let duration = match tlv.tag {
        UTC_TIME => der::asn1::UtcTime::from_der(&der)
            .map_err(|_| invalid("bad time"))?
            .to_unix_duration(),
        GENERALIZED_TIME => der::asn1::GeneralizedTime::from_der(&der)
            .map_err(|_| invalid("bad time"))?
            .to_unix_duration(),
        _ => return Err(invalid("expected a time")),
    };
    Ok(duration.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_indefinite_lengths_and_pieces() {
        // SEQUENCE (indefinite) { OID 1.2.840.113549, [0] (indefinite) {
        // OCTET STRING in two pieces } }
        let ber = [
            0x30, 0x80, 0x06, 0x06, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0xa0, 0x80, 0x24, 0x80,
            0x04, 0x02, b'h', b'e', 0x04, 0x03, b'l', b'l', b'o', 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00,
        ];
        let tlv = Tlv::parse(&ber).unwrap();
        assert_eq!(tlv.tag, SEQUENCE);
        assert_eq!(tlv.raw.len(), ber.len());
        let mut r = tlv.children();
        assert_eq!(
            r.read(OID).unwrap().oid().unwrap().to_string(),
            "1.2.840.113549"
        );
        let explicit = r.read(context_constructed(0)).unwrap();
        let string = explicit.children().read(0x24).unwrap();
        assert_eq!(&*string.octets().unwrap(), b"hello");
        assert!(r.next().unwrap().is_none());
        // The DER form: definite lengths, the string in one piece.
        assert_eq!(
            tlv.to_der().unwrap(),
            [
                0x30, 0x11, 0x06, 0x06, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0xa0, 0x07, 0x04, 0x05,
                b'h', b'e', b'l', b'l', b'o'
            ]
        );
    }

    #[test]
    fn refuses_broken_input() {
        assert!(Tlv::parse(&[0x30, 0x05, 0x01]).is_err(), "truncated");
        assert!(
            Tlv::parse(&[0x30, 0x80, 0x02, 0x01, 0x00]).is_err(),
            "no end marker"
        );
        assert!(
            Tlv::parse(&[0x04, 0x80, 0x00, 0x00]).is_err(),
            "primitive indefinite"
        );
        let mut deep = vec![];
        for _ in 0..100 {
            deep.extend_from_slice(&[0x30, 0x80]);
        }
        assert!(Tlv::parse(&deep).is_err(), "too deep");
    }

    #[test]
    fn writes_long_lengths() {
        let value = vec![7u8; 300];
        let der = tlv(OCTET_STRING, &value);
        assert_eq!(&der[..4], &[0x04, 0x82, 0x01, 0x2c]);
        assert_eq!(Tlv::parse(&der).unwrap().value.len(), 300);
    }
}
