//! Mail rules: the user's filters, applied to mail as it arrives.
//!
//! Rules are edited in the interface and stored with the other settings
//! (`ui.rules`). The sync workers hand every newly arrived inbox message to
//! [`apply`], which runs the enabled rules in order and performs their actions
//! through the same operations as the interface, so they sync to the server
//! like any user action.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::model::{Address, FlagChange, MoveTarget};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailRule {
    #[serde(default)]
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub account_id: Option<i64>,
    #[serde(rename = "match", default)]
    pub match_mode: Match,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub stop: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Match {
    #[default]
    All,
    Any,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    pub field: Field,
    pub op: Op,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Field {
    From,
    To,
    Subject,
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Op {
    Contains,
    NotContains,
    Is,
    EndsWith,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Action {
    Move { folder_id: i64 },
    Label { folder_id: i64 },
    Archive,
    Trash,
    Spam,
    Read,
    Star,
    Mute,
}

/// A newly arrived message, as far as rules can see it.
#[derive(Debug, Clone)]
pub struct Incoming {
    pub account_id: i64,
    pub thread_id: i64,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
}

fn address_text(a: &Address) -> String {
    match &a.name {
        Some(name) if !name.is_empty() => format!("{} <{}>", name, a.email).to_lowercase(),
        _ => a.email.to_lowercase(),
    }
}

/// Whether one text satisfies a positive test (`contains`, `is`, `endsWith`).
fn test(op: Op, haystack: &str, email: Option<&str>, needle: &str) -> bool {
    match op {
        Op::Contains | Op::NotContains => haystack.contains(needle),
        Op::Is => haystack == needle || email.is_some_and(|e| e == needle),
        Op::EndsWith => email.unwrap_or(haystack).ends_with(needle) || haystack.ends_with(needle),
    }
}

impl Condition {
    pub fn matches(&self, m: &Incoming) -> bool {
        let needle = self.value.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        let address = |a: &Address| {
            test(
                self.op,
                &address_text(a),
                Some(&a.email.to_lowercase()),
                &needle,
            )
        };
        let from = || m.from.as_ref().is_some_and(address);
        let to = || m.to.iter().chain(&m.cc).any(address);
        let subject = || test(self.op, &m.subject.to_lowercase(), None, &needle);
        let hit = match self.field {
            Field::From => from(),
            Field::To => to(),
            Field::Subject => subject(),
            Field::Any => from() || to() || subject(),
        };
        if self.op == Op::NotContains {
            !hit
        } else {
            hit
        }
    }
}

impl MailRule {
    pub fn matches(&self, m: &Incoming) -> bool {
        if !self.enabled
            || self.conditions.is_empty()
            || self.account_id.is_some_and(|a| a != m.account_id)
        {
            return false;
        }
        match self.match_mode {
            Match::All => self.conditions.iter().all(|c| c.matches(m)),
            Match::Any => self.conditions.iter().any(|c| c.matches(m)),
        }
    }
}

/// The enabled rules, in order.
pub fn load(engine: &Engine) -> Vec<MailRule> {
    engine
        .get_setting("rules")
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value::<Vec<MailRule>>(v).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.enabled)
        .collect()
}

/// Which actions to run on which threads.
pub fn plan(rules: &[MailRule], incoming: &[Incoming]) -> BTreeMap<ActionKey, Vec<i64>> {
    let mut out: BTreeMap<ActionKey, Vec<i64>> = BTreeMap::new();
    for m in incoming {
        for rule in rules {
            if !rule.matches(m) {
                continue;
            }
            for action in &rule.actions {
                let ids = out.entry(ActionKey::from(action)).or_default();
                if !ids.contains(&m.thread_id) {
                    ids.push(m.thread_id);
                }
            }
            if rule.stop {
                break;
            }
        }
    }
    out
}

/// An action, ordered so that flag changes run before moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionKey {
    Read,
    Star,
    Label(i64),
    Mute,
    Move(i64),
    Archive,
    Spam,
    Trash,
}

impl From<&Action> for ActionKey {
    fn from(a: &Action) -> Self {
        match a {
            Action::Move { folder_id } => ActionKey::Move(*folder_id),
            Action::Label { folder_id } => ActionKey::Label(*folder_id),
            Action::Archive => ActionKey::Archive,
            Action::Trash => ActionKey::Trash,
            Action::Spam => ActionKey::Spam,
            Action::Read => ActionKey::Read,
            Action::Star => ActionKey::Star,
            Action::Mute => ActionKey::Mute,
        }
    }
}

/// Runs the rules on newly arrived messages and returns the threads that
/// shouldn't produce a notification (muted, read, or moved out of the inbox).
pub fn apply(engine: &Engine, incoming: &[Incoming]) -> HashSet<i64> {
    let mut quiet = HashSet::new();
    if incoming.is_empty() {
        return quiet;
    }
    // Replies in muted conversations skip the inbox.
    let threads: Vec<i64> = incoming.iter().map(|m| m.thread_id).collect();
    let muted = engine
        .db
        .read(|c| crate::db::mail::muted_among(c, &threads))
        .unwrap_or_default();
    if !muted.is_empty() {
        if let Err(e) = engine.archive_from_inbox(&muted) {
            tracing::warn!("muted conversations not archived: {e}");
        }
        quiet.extend(muted);
    }
    let rules = load(engine);
    if rules.is_empty() {
        return quiet;
    }
    for (action, ids) in plan(&rules, incoming) {
        let result = match action {
            ActionKey::Read => engine.set_flags(
                &ids,
                &[],
                &FlagChange {
                    seen: Some(true),
                    flagged: None,
                },
            ),
            ActionKey::Star => engine.set_flags(
                &ids,
                &[],
                &FlagChange {
                    seen: None,
                    flagged: Some(true),
                },
            ),
            ActionKey::Label(folder) => engine.set_label(&ids, folder, true),
            ActionKey::Mute => Ok(()),
            ActionKey::Move(folder) => engine
                .move_threads(&ids, None, &MoveTarget::Folder { id: folder })
                .map(|_| ()),
            ActionKey::Archive => engine.archive_from_inbox(&ids).map(|_| ()),
            ActionKey::Spam => engine
                .move_threads(&ids, None, &MoveTarget::Junk)
                .map(|_| ()),
            ActionKey::Trash => engine
                .move_threads(&ids, None, &MoveTarget::Trash)
                .map(|_| ()),
        };
        if let Err(e) = result {
            tracing::warn!("rule action {action:?} failed: {e}");
        }
        if !matches!(action, ActionKey::Star | ActionKey::Label(_)) {
            quiet.extend(ids);
        }
    }
    quiet
}

// ---------------------------------------------------------------------------
// Rules on mail that is already here

/// The conversations a rule matches in mail already here, newest first.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleMatches {
    /// How many conversations match in all.
    pub total: i64,
    /// The newest of them, up to the limit asked for.
    pub thread_ids: Vec<i64>,
}

/// What running a rule on mail already here changed, to undo it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RuleUndo {
    /// Conversations moved (archived, filed, to Spam or Trash).
    pub moves: Vec<crate::UndoToken>,
    /// Messages marked read that were unread.
    pub read: Vec<i64>,
    /// Messages starred that were not.
    pub starred: Vec<i64>,
    /// Labels added: (label folder, conversations that did not have it).
    pub labeled: Vec<(i64, Vec<i64>)>,
}

/// Whether running the rule changes anything in mail already here ("don't
/// notify" only matters as mail arrives).
pub fn acts_on_existing(rule: &MailRule) -> bool {
    rule.actions.iter().any(|a| *a != Action::Mute)
}

impl Engine {
    /// The conversations `rule` matches among mail already here: messages in
    /// any folder but Sent, Drafts, Spam and Trash, not written by the user.
    /// Lists at most `limit` of them, the newest first.
    pub fn rule_matches(&self, rule: &MailRule, limit: i64) -> crate::Result<RuleMatches> {
        let mut rule = rule.clone();
        rule.enabled = true;
        let needs_recipients = rule
            .conditions
            .iter()
            .any(|c| matches!(c.field, Field::To | Field::Any));
        self.db.read(|c| {
            let mine: HashSet<String> = crate::crypto::my_emails(c)?;
            let mut st = c.prepare(
                "SELECT m.account_id, m.thread_id, m.from_name, m.from_email, m.to_json, m.cc_json, m.subject
                 FROM messages m
                 WHERE (?1 IS NULL OR m.account_id = ?1) AND (m.flags & 8) = 0
                   AND EXISTS (SELECT 1 FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
                               WHERE mf.message_id = m.id
                                 AND (f.role IS NULL OR f.role NOT IN ('sent', 'drafts', 'trash', 'junk')))
                 ORDER BY m.date DESC",
            )?;
            let rows = st.query_map([rule.account_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })?;
            let mut seen: HashSet<i64> = HashSet::new();
            let mut out = RuleMatches::default();
            for row in rows {
                let (account_id, thread_id, from_name, from_email, to, cc, subject) = row?;
                if seen.contains(&thread_id) {
                    continue;
                }
                let from = from_email.map(|e| Address::new(from_name, e));
                if from
                    .as_ref()
                    .is_some_and(|f| mine.contains(&f.email.to_lowercase()))
                {
                    continue;
                }
                let parse = |json: &str| {
                    if needs_recipients {
                        serde_json::from_str::<Vec<Address>>(json).unwrap_or_default()
                    } else {
                        Vec::new()
                    }
                };
                let message = Incoming {
                    account_id,
                    thread_id,
                    from,
                    to: parse(&to),
                    cc: parse(&cc),
                    subject,
                };
                if rule.matches(&message) {
                    seen.insert(thread_id);
                    out.total += 1;
                    if (out.thread_ids.len() as i64) < limit {
                        out.thread_ids.push(thread_id);
                    }
                }
            }
            Ok(out)
        })
    }

    /// Runs a rule's actions on conversations already here, as if their mail
    /// had just arrived, and returns what undoes it.
    pub fn apply_rule(&self, rule: &MailRule, thread_ids: &[i64]) -> crate::Result<RuleUndo> {
        let mut undo = RuleUndo::default();
        if thread_ids.is_empty() {
            return Ok(undo);
        }
        let keys: BTreeSet<ActionKey> = rule.actions.iter().map(ActionKey::from).collect();
        let summaries = self
            .db
            .read(|c| crate::db::mail::thread_summaries(c, thread_ids))?;
        // Folders and labels belong to one account: only its conversations go there.
        let of_account = |folder: i64| -> crate::Result<Vec<i64>> {
            let account = self
                .db
                .read(|c| crate::db::mail::get_folder(c, folder))?
                .account_id;
            Ok(summaries
                .iter()
                .filter(|t| t.account_id == account)
                .map(|t| t.id)
                .collect())
        };
        for key in keys {
            match key {
                ActionKey::Read => {
                    let before = self.message_ids_with(thread_ids, "(flags & 1) = 0")?;
                    self.set_flags(
                        thread_ids,
                        &[],
                        &FlagChange {
                            seen: Some(true),
                            flagged: None,
                        },
                    )?;
                    let after = self.message_ids_with(thread_ids, "(flags & 1) = 0")?;
                    undo.read = before.difference(&after).copied().collect();
                }
                ActionKey::Star => {
                    let before = self.message_ids_with(thread_ids, "(flags & 2) != 0")?;
                    self.set_flags(
                        thread_ids,
                        &[],
                        &FlagChange {
                            seen: None,
                            flagged: Some(true),
                        },
                    )?;
                    let after = self.message_ids_with(thread_ids, "(flags & 2) != 0")?;
                    undo.starred = after.difference(&before).copied().collect();
                }
                ActionKey::Label(folder) => {
                    let ids = of_account(folder)?;
                    let missing: Vec<i64> = self.db.read(|c| {
                        let mut st = c.prepare_cached(
                            "SELECT 1 FROM thread_folders WHERE folder_id = ? AND thread_id = ?",
                        )?;
                        let mut out = Vec::new();
                        for t in &ids {
                            if !st.exists(rusqlite::params![folder, t])? {
                                out.push(*t);
                            }
                        }
                        Ok(out)
                    })?;
                    if !missing.is_empty() {
                        self.set_label(&missing, folder, true)?;
                        undo.labeled.push((folder, missing));
                    }
                }
                ActionKey::Mute => {}
                ActionKey::Move(folder) => {
                    let ids = of_account(folder)?;
                    if !ids.is_empty() {
                        undo.moves.push(self.move_threads(
                            &ids,
                            None,
                            &MoveTarget::Folder { id: folder },
                        )?);
                    }
                }
                ActionKey::Archive => undo.moves.extend(self.archive_from_inbox(thread_ids)?),
                ActionKey::Spam => {
                    undo.moves
                        .push(self.move_threads(thread_ids, None, &MoveTarget::Junk)?);
                }
                ActionKey::Trash => {
                    undo.moves
                        .push(self.move_threads(thread_ids, None, &MoveTarget::Trash)?);
                }
            }
        }
        Ok(undo)
    }

    /// Reverts what [`Engine::apply_rule`] did.
    pub fn undo_rule(&self, undo: &RuleUndo) -> crate::Result<()> {
        for token in undo.moves.iter().rev() {
            self.undo(token)?;
        }
        for (folder, threads) in &undo.labeled {
            self.set_label(threads, *folder, false)?;
        }
        if !undo.read.is_empty() {
            self.set_flags(
                &[],
                &undo.read,
                &FlagChange {
                    seen: Some(false),
                    flagged: None,
                },
            )?;
        }
        if !undo.starred.is_empty() {
            self.set_flags(
                &[],
                &undo.starred,
                &FlagChange {
                    seen: None,
                    flagged: Some(false),
                },
            )?;
        }
        Ok(())
    }

    /// Messages (not drafts) of conversations that meet a flags condition.
    fn message_ids_with(
        &self,
        thread_ids: &[i64],
        condition: &str,
    ) -> crate::Result<BTreeSet<i64>> {
        self.db.read(|c| {
            let mut st = c.prepare(&format!(
                "SELECT id FROM messages WHERE thread_id = ? AND (flags & 8) = 0 AND {condition}"
            ))?;
            let mut out = BTreeSet::new();
            for t in thread_ids {
                for id in st.query_map([t], |r| r.get::<_, i64>(0))? {
                    out.insert(id?);
                }
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(from: &str, subject: &str) -> Incoming {
        Incoming {
            account_id: 1,
            thread_id: 7,
            from: Some(Address::new(Some("Billing Team".into()), from)),
            to: vec![Address::new(None, "me@example.com")],
            cc: vec![],
            subject: subject.into(),
        }
    }

    fn rules(json: &str) -> Vec<MailRule> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn parses_the_interface_format() {
        let r = rules(
            r#"[{"id":"a","name":"Receipts","enabled":true,"accountId":null,"match":"all",
                 "conditions":[{"field":"from","op":"endsWith","value":"@shop.example"}],
                 "actions":[{"kind":"label","folderId":12},{"kind":"archive"}],"stop":true}]"#,
        );
        assert_eq!(
            r[0].actions,
            vec![Action::Label { folder_id: 12 }, Action::Archive]
        );
        assert!(r[0].stop);
        assert!(r[0].matches(&msg("orders@shop.example", "Your order")));
        assert!(!r[0].matches(&msg("orders@shop.example.net", "Your order")));
    }

    #[test]
    fn conditions() {
        let c = |field, op, value: &str| Condition {
            field,
            op,
            value: value.into(),
        };
        let m = msg("invoices@acme.io", "Invoice #42 for October");
        assert!(c(Field::From, Op::Contains, "billing team").matches(&m));
        assert!(c(Field::From, Op::Is, "INVOICES@acme.io").matches(&m));
        assert!(c(Field::To, Op::Is, "me@example.com").matches(&m));
        assert!(c(Field::Subject, Op::Contains, "invoice").matches(&m));
        assert!(c(Field::Subject, Op::NotContains, "receipt").matches(&m));
        assert!(!c(Field::Any, Op::NotContains, "acme").matches(&m));
        assert!(c(Field::Any, Op::Contains, "october").matches(&m));
        assert!(c(Field::From, Op::EndsWith, "acme.io").matches(&m));
    }

    #[test]
    fn match_modes_accounts_and_stop() {
        let r = rules(
            r#"[{"name":"A","match":"any","accountId":2,
                 "conditions":[{"field":"subject","op":"contains","value":"x"}],"actions":[{"kind":"read"}]},
                {"name":"B","match":"any",
                 "conditions":[{"field":"subject","op":"contains","value":"nope"},{"field":"from","op":"contains","value":"acme"}],
                 "actions":[{"kind":"star"}],"stop":true},
                {"name":"C","conditions":[{"field":"from","op":"contains","value":"acme"}],"actions":[{"kind":"trash"}]}]"#,
        );
        let m = msg("a@acme.io", "x marks the spot");
        // A is for another account, B matches and stops, C never runs.
        let planned = plan(&r, &[m]);
        assert_eq!(
            planned.keys().copied().collect::<Vec<_>>(),
            vec![ActionKey::Star]
        );
    }

    #[test]
    fn disabled_and_empty_rules_never_match() {
        let r = rules(
            r#"[{"name":"off","enabled":false,"conditions":[{"field":"any","op":"contains","value":""}],"actions":[{"kind":"read"}]}]"#,
        );
        assert!(!r[0].matches(&msg("a@b.c", "hi")));
        let r = rules(r#"[{"name":"none","conditions":[],"actions":[{"kind":"read"}]}]"#);
        assert!(!r[0].matches(&msg("a@b.c", "hi")));
    }
}
