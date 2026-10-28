//! Owned representations of IMAP responses.
//!
//! `imap-proto` hands out responses that borrow from the read buffer; we convert
//! them into owned values right away so the buffer can be reused.

use std::ops::RangeInclusive;

use chrono::{DateTime, FixedOffset};
use imap_proto::{
    AttributeValue, BodyStructure, Capability, MailboxDatum, MessageSection, NameAttribute,
    Response, ResponseCode, SectionPath, StatusAttribute,
};

use imap_proto::rfc4315::UidSetMember;

use crate::utf7;

/// Completion status of a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    No,
    Bad,
    PreAuth,
    Bye,
}

/// A response code (`[UIDVALIDITY 123]` …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Code {
    Alert,
    Capabilities(Vec<String>),
    HighestModSeq(u64),
    PermanentFlags(Vec<String>),
    ReadOnly,
    ReadWrite,
    TryCreate,
    UidNext(u32),
    UidValidity(u32),
    Unseen(u32),
    AppendUid {
        uid_validity: u32,
        uids: Vec<u32>,
    },
    CopyUid {
        uid_validity: u32,
        source: Vec<u32>,
        dest: Vec<u32>,
    },
    UidNotSticky,
    Other,
}

/// A mailbox returned by `LIST`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    /// Decoded (UTF-8) mailbox name.
    pub name: String,
    /// Name exactly as the server sent it (modified UTF-7), used for commands.
    pub raw_name: String,
    pub delimiter: Option<String>,
    pub attributes: Vec<String>,
}

impl ListEntry {
    pub fn has_attr(&self, attr: &str) -> bool {
        self.attributes.iter().any(|a| a.eq_ignore_ascii_case(attr))
    }

    /// RFC 6154 special-use role, if any (`\Sent`, `\Trash`, …).
    pub fn special_use(&self) -> Option<&'static str> {
        const ROLES: [&str; 7] = [
            "\\All",
            "\\Archive",
            "\\Drafts",
            "\\Flagged",
            "\\Junk",
            "\\Sent",
            "\\Trash",
        ];
        ROLES.into_iter().find(|r| self.has_attr(r))
    }

    pub fn is_selectable(&self) -> bool {
        !self.has_attr("\\Noselect") && !self.has_attr("\\NonExistent")
    }
}

/// `STATUS` data for a mailbox.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailboxStatus {
    pub mailbox: String,
    pub messages: Option<u32>,
    pub recent: Option<u32>,
    pub uid_next: Option<u32>,
    pub uid_validity: Option<u32>,
    pub unseen: Option<u32>,
    pub highest_modseq: Option<u64>,
}

/// A node of a message's MIME structure as reported by `BODYSTRUCTURE`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct BodyPart {
    /// IMAP part specifier (`1`, `1.2`, …). Empty for a top-level multipart.
    pub part_id: String,
    /// Lower-case `type/subtype`.
    pub mime_type: String,
    /// Content-Type parameters with lower-cased names; values are raw (may be RFC 2047/2231 encoded).
    pub params: Vec<(String, String)>,
    pub content_id: Option<String>,
    pub description: Option<String>,
    /// Lower-case transfer encoding (`base64`, `quoted-printable`, `7bit`, …).
    pub encoding: String,
    /// Encoded size in octets.
    pub size: u32,
    /// Lower-case disposition (`inline`, `attachment`).
    pub disposition: Option<String>,
    pub disposition_params: Vec<(String, String)>,
    pub children: Vec<BodyPart>,
}

impl BodyPart {
    pub fn is_multipart(&self) -> bool {
        self.mime_type.starts_with("multipart/")
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn disposition_param(&self, name: &str) -> Option<&str> {
        self.disposition_params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Depth-first iterator over all leaf parts.
    pub fn leaves(&self) -> Vec<&BodyPart> {
        let mut out = Vec::new();
        fn walk<'a>(p: &'a BodyPart, out: &mut Vec<&'a BodyPart>) {
            if p.children.is_empty() {
                out.push(p);
            } else {
                for c in &p.children {
                    walk(c, out);
                }
            }
        }
        walk(self, &mut out);
        out
    }
}

/// Data returned for one message by `FETCH`.
#[derive(Debug, Clone, Default)]
pub struct FetchItem {
    pub seq: u32,
    pub uid: Option<u32>,
    pub flags: Option<Vec<String>>,
    pub modseq: Option<u64>,
    pub internal_date: Option<DateTime<FixedOffset>>,
    pub size: Option<u32>,
    /// `BODY[HEADER]`, `BODY[HEADER.FIELDS (…)]` or `RFC822.HEADER`.
    pub header: Option<Vec<u8>>,
    /// `BODY[]` / `RFC822`.
    pub body: Option<Vec<u8>>,
    /// Other body sections keyed by part specifier (`1.2`, `2.MIME`, `TEXT`).
    pub sections: Vec<(String, Vec<u8>)>,
    pub bodystructure: Option<BodyPart>,
    pub gm_msgid: Option<u64>,
    pub gm_thrid: Option<u64>,
    pub gm_labels: Option<Vec<String>>,
    pub email_id: Option<String>,
    pub thread_id: Option<String>,
}

impl FetchItem {
    pub fn has_flag(&self, flag: &str) -> bool {
        self.flags
            .as_ref()
            .is_some_and(|f| f.iter().any(|x| x.eq_ignore_ascii_case(flag)))
    }

    pub fn section(&self, spec: &str) -> Option<&[u8]> {
        self.sections
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(spec))
            .map(|(_, v)| v.as_slice())
    }
}

/// Untagged server data.
#[derive(Debug, Clone)]
pub enum Untagged {
    Status {
        status: Status,
        code: Option<Code>,
        text: String,
    },
    Capabilities(Vec<String>),
    Exists(u32),
    Recent(u32),
    Expunge(u32),
    Vanished {
        earlier: bool,
        uids: Vec<RangeInclusive<u32>>,
    },
    Fetch(Box<FetchItem>),
    List(ListEntry),
    Flags(Vec<String>),
    Search(Vec<u32>),
    MailboxStatus(MailboxStatus),
    Other,
}

/// A parsed server response.
#[derive(Debug, Clone)]
pub enum Resp {
    Tagged {
        tag: String,
        status: Status,
        code: Option<Code>,
        text: String,
    },
    Untagged(Untagged),
    Continue {
        text: String,
    },
}

fn status_from(s: imap_proto::Status) -> Status {
    match s {
        imap_proto::Status::Ok => Status::Ok,
        imap_proto::Status::No => Status::No,
        imap_proto::Status::Bad => Status::Bad,
        imap_proto::Status::PreAuth => Status::PreAuth,
        imap_proto::Status::Bye => Status::Bye,
    }
}

fn caps_from(caps: &[Capability<'_>]) -> Vec<String> {
    caps.iter()
        .map(|c| match c {
            Capability::Imap4rev1 => "IMAP4REV1".to_string(),
            Capability::Auth(a) => format!("AUTH={}", a.to_ascii_uppercase()),
            Capability::Atom(a) => a.to_ascii_uppercase(),
        })
        .collect()
}

fn uid_set(members: &[UidSetMember]) -> Vec<u32> {
    let mut out = Vec::new();
    for m in members {
        match m {
            UidSetMember::Uid(u) => out.push(*u),
            UidSetMember::UidRange(r) => {
                // Guard against absurd ranges from broken servers.
                if r.end().saturating_sub(*r.start()) < 1_000_000 {
                    out.extend(r.clone());
                }
            }
        }
    }
    out
}

fn code_from(code: &ResponseCode<'_>) -> Code {
    match code {
        ResponseCode::Alert => Code::Alert,
        ResponseCode::Capabilities(c) => Code::Capabilities(caps_from(c)),
        ResponseCode::HighestModSeq(m) => Code::HighestModSeq(*m),
        ResponseCode::PermanentFlags(f) => {
            Code::PermanentFlags(f.iter().map(|s| s.to_string()).collect())
        }
        ResponseCode::ReadOnly => Code::ReadOnly,
        ResponseCode::ReadWrite => Code::ReadWrite,
        ResponseCode::TryCreate => Code::TryCreate,
        ResponseCode::UidNext(n) => Code::UidNext(*n),
        ResponseCode::UidValidity(n) => Code::UidValidity(*n),
        ResponseCode::Unseen(n) => Code::Unseen(*n),
        ResponseCode::AppendUid(v, uids) => Code::AppendUid {
            uid_validity: *v,
            uids: uid_set(uids),
        },
        ResponseCode::CopyUid(v, src, dst) => Code::CopyUid {
            uid_validity: *v,
            source: uid_set(src),
            dest: uid_set(dst),
        },
        ResponseCode::UidNotSticky => Code::UidNotSticky,
        _ => Code::Other,
    }
}

fn section_name(section: &Option<SectionPath>) -> String {
    fn msg_section(s: &MessageSection) -> &'static str {
        match s {
            MessageSection::Header => "HEADER",
            MessageSection::Mime => "MIME",
            MessageSection::Text => "TEXT",
        }
    }
    match section {
        None => String::new(),
        Some(SectionPath::Full(s)) => msg_section(s).to_string(),
        Some(SectionPath::Part(parts, sub)) => {
            let mut s = parts
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(".");
            if let Some(sub) = sub {
                s.push('.');
                s.push_str(msg_section(sub));
            }
            s
        }
    }
}

fn params(p: &imap_proto::BodyParams<'_>) -> Vec<(String, String)> {
    p.as_ref()
        .map(|v| {
            v.iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn disposition(
    d: &Option<imap_proto::ContentDisposition<'_>>,
) -> (Option<String>, Vec<(String, String)>) {
    match d {
        Some(d) => (Some(d.ty.to_ascii_lowercase()), params(&d.params)),
        None => (None, Vec::new()),
    }
}

fn encoding(e: &imap_proto::ContentEncoding<'_>) -> String {
    use imap_proto::ContentEncoding as E;
    match e {
        E::SevenBit => "7bit".into(),
        E::EightBit => "8bit".into(),
        E::Binary => "binary".into(),
        E::Base64 => "base64".into(),
        E::QuotedPrintable => "quoted-printable".into(),
        E::Other(o) => o.to_ascii_lowercase(),
    }
}

fn child_id(parent: &str, index: usize) -> String {
    if parent.is_empty() {
        (index + 1).to_string()
    } else {
        format!("{parent}.{}", index + 1)
    }
}

/// Converts a `BODYSTRUCTURE` into an owned tree with IMAP part numbers.
pub fn convert_bodystructure(bs: &BodyStructure<'_>) -> BodyPart {
    match bs {
        BodyStructure::Multipart { .. } => convert_part(bs, String::new()),
        _ => convert_part(bs, "1".to_string()),
    }
}

fn convert_part(bs: &BodyStructure<'_>, id: String) -> BodyPart {
    match bs {
        BodyStructure::Multipart { common, bodies, .. } => {
            let (disp, dparams) = disposition(&common.disposition);
            BodyPart {
                mime_type: format!(
                    "{}/{}",
                    common.ty.ty.to_ascii_lowercase(),
                    common.ty.subtype.to_ascii_lowercase()
                ),
                params: params(&common.ty.params),
                disposition: disp,
                disposition_params: dparams,
                children: bodies
                    .iter()
                    .enumerate()
                    .map(|(i, b)| convert_part(b, child_id(&id, i)))
                    .collect(),
                part_id: id,
                ..Default::default()
            }
        }
        BodyStructure::Basic { common, other, .. } | BodyStructure::Text { common, other, .. } => {
            leaf(common, other, id, Vec::new())
        }
        BodyStructure::Message {
            common,
            other,
            body,
            ..
        } => {
            let inner = match body.as_ref() {
                BodyStructure::Multipart { .. } => convert_part(body, id.clone()),
                other => convert_part(other, format!("{id}.1")),
            };
            leaf(common, other, id, vec![inner])
        }
    }
}

fn leaf(
    common: &imap_proto::BodyContentCommon<'_>,
    other: &imap_proto::BodyContentSinglePart<'_>,
    id: String,
    children: Vec<BodyPart>,
) -> BodyPart {
    let (disp, dparams) = disposition(&common.disposition);
    BodyPart {
        part_id: id,
        mime_type: format!(
            "{}/{}",
            common.ty.ty.to_ascii_lowercase(),
            common.ty.subtype.to_ascii_lowercase()
        ),
        params: params(&common.ty.params),
        content_id: other.id.as_ref().map(|s| s.to_string()),
        description: other.description.as_ref().map(|s| s.to_string()),
        encoding: encoding(&other.transfer_encoding),
        size: other.octets,
        disposition: disp,
        disposition_params: dparams,
        children,
    }
}

fn fetch_from(seq: u32, attrs: &[AttributeValue<'_>]) -> FetchItem {
    let mut item = FetchItem {
        seq,
        ..Default::default()
    };
    for attr in attrs {
        match attr {
            AttributeValue::Uid(u) => item.uid = Some(*u),
            AttributeValue::Flags(f) => {
                item.flags = Some(f.iter().map(|s| s.to_string()).collect())
            }
            AttributeValue::ModSeq(m) => item.modseq = Some(*m),
            AttributeValue::InternalDate(d) => {
                item.internal_date = DateTime::parse_from_str(d.trim(), "%d-%b-%Y %H:%M:%S %z").ok()
            }
            AttributeValue::Rfc822Size(s) => item.size = Some(*s),
            AttributeValue::Rfc822(Some(data)) => item.body = Some(data.to_vec()),
            AttributeValue::Rfc822Header(Some(data)) => item.header = Some(data.to_vec()),
            AttributeValue::Rfc822Text(Some(data)) => {
                item.sections.push(("TEXT".to_string(), data.to_vec()))
            }
            AttributeValue::BodySection {
                section,
                index,
                data,
            } => {
                let data = data.as_ref().map(|d| d.to_vec()).unwrap_or_default();
                let name = section_name(section);
                match section {
                    None if index.is_none() || *index == Some(0) => item.body = Some(data),
                    Some(SectionPath::Full(MessageSection::Header)) => item.header = Some(data),
                    _ => item.sections.push((name, data)),
                }
            }
            AttributeValue::BodyStructure(bs) => {
                item.bodystructure = Some(convert_bodystructure(bs))
            }
            AttributeValue::GmailMsgId(id) => item.gm_msgid = Some(*id),
            AttributeValue::GmailThrId(id) => item.gm_thrid = Some(*id),
            AttributeValue::GmailLabels(labels) => {
                item.gm_labels = Some(labels.iter().map(|l| utf7::decode_lossy(l)).collect())
            }
            AttributeValue::EmailId(id) => item.email_id = Some(id.to_string()),
            AttributeValue::ThreadId(id) => item.thread_id = id.as_ref().map(|s| s.to_string()),
            _ => {}
        }
    }
    item
}

fn name_attr(a: &NameAttribute<'_>) -> String {
    match a {
        NameAttribute::NoInferiors => "\\Noinferiors".into(),
        NameAttribute::NoSelect => "\\Noselect".into(),
        NameAttribute::Marked => "\\Marked".into(),
        NameAttribute::Unmarked => "\\Unmarked".into(),
        NameAttribute::All => "\\All".into(),
        NameAttribute::Archive => "\\Archive".into(),
        NameAttribute::Drafts => "\\Drafts".into(),
        NameAttribute::Flagged => "\\Flagged".into(),
        NameAttribute::Junk => "\\Junk".into(),
        NameAttribute::Sent => "\\Sent".into(),
        NameAttribute::Trash => "\\Trash".into(),
        NameAttribute::Extension(e) => e.to_string(),
        _ => String::new(),
    }
}

/// Converts a borrowed `imap-proto` response into an owned [`Resp`].
pub fn convert(resp: Response<'_>) -> Resp {
    match resp {
        Response::Done {
            tag,
            status,
            outcome,
        } => Resp::Tagged {
            tag: tag.0,
            status: status_from(status),
            code: outcome.code.as_ref().map(code_from),
            text: outcome
                .information
                .map(|s| s.to_string())
                .unwrap_or_default(),
        },
        Response::Continue(outcome) => Resp::Continue {
            text: outcome
                .information
                .map(|s| s.to_string())
                .unwrap_or_default(),
        },
        Response::Data { status, outcome } => {
            let code = outcome.code.as_ref().map(code_from);
            Resp::Untagged(Untagged::Status {
                status: status_from(status),
                code,
                text: outcome
                    .information
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
            })
        }
        Response::Capabilities(caps) => Resp::Untagged(Untagged::Capabilities(caps_from(&caps))),
        Response::Expunge(n) => Resp::Untagged(Untagged::Expunge(n)),
        Response::Vanished { earlier, uids } => {
            Resp::Untagged(Untagged::Vanished { earlier, uids })
        }
        Response::Fetch(seq, attrs) => {
            Resp::Untagged(Untagged::Fetch(Box::new(fetch_from(seq, &attrs))))
        }
        Response::MailboxData(datum) => Resp::Untagged(match datum {
            MailboxDatum::Exists(n) => Untagged::Exists(n),
            MailboxDatum::Recent(n) => Untagged::Recent(n),
            MailboxDatum::Flags(f) => Untagged::Flags(f.iter().map(|s| s.to_string()).collect()),
            MailboxDatum::Search(v) | MailboxDatum::Sort(v) => Untagged::Search(v),
            MailboxDatum::List(l) => Untagged::List(ListEntry {
                name: utf7::decode_lossy(&l.name),
                raw_name: l.name.to_string(),
                delimiter: l.delimiter.map(|d| d.to_string()),
                attributes: l.name_attributes.iter().map(name_attr).collect(),
            }),
            MailboxDatum::Status { mailbox, status } => {
                let mut st = MailboxStatus {
                    mailbox: utf7::decode_lossy(&mailbox),
                    ..Default::default()
                };
                for a in status {
                    match a {
                        StatusAttribute::HighestModSeq(v) => st.highest_modseq = Some(v),
                        StatusAttribute::Messages(v) => st.messages = Some(v),
                        StatusAttribute::Recent(v) => st.recent = Some(v),
                        StatusAttribute::UidNext(v) => st.uid_next = Some(v),
                        StatusAttribute::UidValidity(v) => st.uid_validity = Some(v),
                        StatusAttribute::Unseen(v) => st.unseen = Some(v),
                        _ => {}
                    }
                }
                Untagged::MailboxStatus(st)
            }
            _ => Untagged::Other,
        }),
        _ => Resp::Untagged(Untagged::Other),
    }
}

/// Compresses a sorted list of UIDs into an IMAP sequence set (`1:5,7,9:12`).
pub fn uid_set_string(uids: &[u32]) -> String {
    let mut sorted: Vec<u32> = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = String::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        if !out.is_empty() {
            out.push(',');
        }
        if start == end {
            out.push_str(&start.to_string());
        } else {
            out.push_str(&format!("{start}:{end}"));
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &[u8]) -> Resp {
        let (_, r) = Response::parse(input).expect("parse");
        convert(r)
    }

    #[test]
    fn uid_set_compression() {
        assert_eq!(uid_set_string(&[5, 1, 2, 3, 9, 7, 8]), "1:3,5,7:9");
        assert_eq!(uid_set_string(&[42]), "42");
        assert_eq!(uid_set_string(&[]), "");
    }

    #[test]
    fn fetch_with_gmail_attributes() {
        let r = parse(
            b"* 12 FETCH (UID 345 FLAGS (\\Seen $Forwarded) X-GM-MSGID 1278455344230334865 X-GM-THRID 1278455344230334865 X-GM-LABELS (\\Inbox \"Work/Clients\") RFC822.SIZE 4321 MODSEQ (99))\r\n",
        );
        let Resp::Untagged(Untagged::Fetch(f)) = r else {
            panic!("expected fetch")
        };
        assert_eq!(f.uid, Some(345));
        assert!(f.has_flag("\\seen"));
        assert_eq!(f.gm_thrid, Some(1278455344230334865));
        assert_eq!(
            f.gm_labels.as_deref(),
            Some(&["\\Inbox".to_string(), "Work/Clients".to_string()][..])
        );
        assert_eq!(f.size, Some(4321));
        assert_eq!(f.modseq, Some(99));
    }

    #[test]
    fn fetch_header_fields_literal() {
        let r = parse(
            b"* 1 FETCH (UID 7 BODY[HEADER.FIELDS (SUBJECT)] {15}\r\nSubject: Hi\r\n\r\n)\r\n",
        );
        let Resp::Untagged(Untagged::Fetch(f)) = r else {
            panic!("expected fetch")
        };
        // HEADER.FIELDS sections are reported as a part path by imap-proto; accept either.
        let data = f
            .header
            .clone()
            .or_else(|| f.sections.first().map(|(_, d)| d.clone()))
            .expect("header data");
        assert_eq!(data, b"Subject: Hi\r\n\r\n");
    }

    #[test]
    fn bodystructure_numbering() {
        let r = parse(b"* 1 FETCH (UID 1 BODYSTRUCTURE ((\"text\" \"plain\" (\"charset\" \"utf-8\") NIL NIL \"7bit\" 10 1 NIL NIL NIL)(\"application\" \"pdf\" (\"name\" \"a.pdf\") NIL NIL \"base64\" 2000 NIL (\"attachment\" (\"filename\" \"a.pdf\")) NIL) \"mixed\" (\"boundary\" \"x\") NIL NIL))\r\n");
        let Resp::Untagged(Untagged::Fetch(f)) = r else {
            panic!("expected fetch")
        };
        let bs = f.bodystructure.expect("bodystructure");
        assert_eq!(bs.mime_type, "multipart/mixed");
        assert_eq!(bs.children[0].part_id, "1");
        assert_eq!(bs.children[0].param("charset"), Some("utf-8"));
        assert_eq!(bs.children[1].part_id, "2");
        assert_eq!(bs.children[1].disposition.as_deref(), Some("attachment"));
        assert_eq!(bs.children[1].disposition_param("filename"), Some("a.pdf"));
    }

    #[test]
    fn single_part_is_part_one() {
        let r = parse(b"* 1 FETCH (BODYSTRUCTURE (\"text\" \"html\" (\"charset\" \"utf-8\") NIL NIL \"quoted-printable\" 100 3 NIL NIL NIL))\r\n");
        let Resp::Untagged(Untagged::Fetch(f)) = r else {
            panic!("expected fetch")
        };
        let bs = f.bodystructure.expect("bodystructure");
        assert_eq!(bs.part_id, "1");
        assert_eq!(bs.encoding, "quoted-printable");
    }

    #[test]
    fn list_with_special_use() {
        let r = parse(b"* LIST (\\HasNoChildren \\Sent) \"/\" \"Sent Items\"\r\n");
        let Resp::Untagged(Untagged::List(l)) = r else {
            panic!("expected list")
        };
        assert_eq!(l.name, "Sent Items");
        assert_eq!(l.special_use(), Some("\\Sent"));
    }

    #[test]
    fn tagged_with_code() {
        let r = parse(b"A3 OK [READ-WRITE] SELECT completed\r\n");
        let Resp::Tagged {
            tag, status, code, ..
        } = r
        else {
            panic!("expected tagged")
        };
        assert_eq!(tag, "A3");
        assert_eq!(status, Status::Ok);
        assert_eq!(code, Some(Code::ReadWrite));
    }
}
