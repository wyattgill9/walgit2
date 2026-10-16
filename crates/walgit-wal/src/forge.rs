//! D50: issues and pull requests are a fold of WAL forge events (`docs/FORGE.md`).
//!
//! `ForgeState::apply_record` is the only interpretation of events. The state is loaded on the
//! first forge request (checkpoint `ForgeHistory` + log tail), then advanced by every sync and
//! every local publish; refs-only requests never load it.

use std::collections::{BTreeMap, BTreeSet};

use prost::Message;
use serde::Serialize;
use walgit_proto::v1::{
    ForgeEvent, ForgeHistory, ForgeKind, ForgeRecord, ForgeSetChange, LogEntry, forge_edit,
    forge_event,
};
use walgit_store::ObjectStoreExt;

use crate::error::WalError;
use crate::handle::RepoHandle;

/// Who asks for a forge change. `write` = the principal may push (triage, merge, edit others').
#[derive(Debug, Clone)]
pub struct Actor {
    pub name: String,
    pub write: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub number: u64,
    /// "issue" | "pull"
    pub kind: &'static str,
    pub title: String,
    pub body: String,
    /// The body was changed after opening.
    pub edited: bool,
    pub author: String,
    /// "open" | "closed"
    pub state: &'static str,
    /// Closing reason: `completed` | `not_planned`; empty while open.
    pub state_reason: String,
    pub labels: BTreeSet<String>,
    pub assignees: BTreeSet<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub comments: u64,
    /// Pull requests (empty on issues): `head_ref` merges into `base_ref`. The oids are the
    /// tips when opened, replaced by the tips when closed.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub base_ref: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub head_ref: String,
    #[serde(skip)]
    pub base_oid: String,
    #[serde(skip)]
    pub head_oid: String,
    /// Pull requests: `approved` | `changes_requested` from each reviewer's latest deciding
    /// review (any change request wins); empty when nobody decided.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub review_decision: String,
    /// Pull requests: merged (then also `closed`), and the commit the base moved to.
    pub merged: bool,
    /// Pull requests: a draft is not mergeable until it is marked ready for review.
    pub draft: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub merge_commit: String,
    /// Reviewer → their latest deciding review state.
    #[serde(skip)]
    pub reviews: BTreeMap<String, String>,
    #[serde(skip)]
    pub timeline: Vec<TimelineItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LineComment {
    pub path: String,
    /// `RIGHT` (the head) | `LEFT` (the merge base).
    pub side: String,
    pub line: u32,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimelineItem {
    pub id: u64,
    pub actor: String,
    pub at: String,
    #[serde(flatten)]
    pub event: Timeline,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Timeline {
    Comment {
        body: String,
        edited: bool,
        deleted: bool,
    },
    Closed {
        reason: String,
    },
    Reopened,
    Renamed {
        from: String,
        to: String,
    },
    Labeled {
        added: Vec<String>,
        removed: Vec<String>,
    },
    Assigned {
        added: Vec<String>,
        removed: Vec<String>,
    },
    Merged {
        method: String,
        commit: String,
    },
    /// Marked as draft (`true`) or ready for review.
    Draft {
        draft: bool,
    },
    /// `#n` in another item's text (once per source item): "mentioned this in #source".
    Referenced {
        source: u64,
    },
    Review {
        state: String,
        body: String,
        commit_oid: String,
        comments: Vec<LineComment>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct LabelDef {
    pub name: String,
    /// `#rrggbb`
    pub color: String,
    pub description: String,
}

/// The folded forge state at WAL `seq`.
#[derive(Debug, Clone, Default)]
pub struct ForgeState {
    pub seq: u64,
    pub items: BTreeMap<u64, Item>,
    /// The repository's label catalog: name → definition.
    pub labels: BTreeMap<String, LabelDef>,
    // ponytail: every record is kept and rewritten whole at each checkpoint; shard when
    // docs/FORGE.md measurements show forge.pb growing too large.
    records: Vec<ForgeRecord>,
}

/// One page of a search, with GitHub's open/closed counts for the same query.
#[derive(Debug, Clone, Serialize)]
pub struct SearchPage {
    pub items: Vec<Item>,
    pub open_count: usize,
    pub closed_count: usize,
    pub page: usize,
    pub per_page: usize,
    /// Every author, label and assignee among items of the searched kind (the filter menus).
    pub authors: BTreeSet<String>,
    pub labels: BTreeSet<String>,
    pub assignees: BTreeSet<String>,
}

pub const PER_PAGE: usize = 25;

fn rfc3339(at: Option<&prost_types::Timestamp>) -> String {
    let t = at.map_or(std::time::UNIX_EPOCH, walgit_proto::time::to_system);
    chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Applies a set change; returns what actually changed.
fn change_set(set: &mut BTreeSet<String>, c: &ForgeSetChange) -> (Vec<String>, Vec<String>) {
    let added = c
        .add
        .iter()
        .filter(|v| set.insert((*v).clone()))
        .cloned()
        .collect();
    let removed = c
        .remove
        .iter()
        .filter(|v| set.remove(*v))
        .cloned()
        .collect();
    (added, removed)
}

impl ForgeState {
    pub fn from_history(h: ForgeHistory) -> Self {
        let mut s = ForgeState::default();
        for r in h.records {
            s.apply_record(r);
        }
        s.seq = h.seq;
        s
    }

    /// The records up to `seq` (a checkpoint's `ForgeHistory`).
    pub fn history_upto(&self, seq: u64) -> ForgeHistory {
        ForgeHistory {
            seq,
            records: self
                .records
                .iter()
                .filter(|r| r.seq <= seq)
                .cloned()
                .collect(),
        }
    }

    /// Advances over contiguous entries after `self.seq`; stops at a gap (the next
    /// `forge_ensure` catches up from the log).
    /// `entries` are every committed entry in (`after`, last]. Committed seqs can skip (an
    /// orphaned log slot is never reused), so completeness is the caller's range, not `seq + 1`.
    /// A state behind `after` missed entries and stays where it is (`forge_ensure` catches up).
    pub(crate) fn fold_entries<'a>(
        &mut self,
        after: u64,
        entries: impl IntoIterator<Item = &'a LogEntry>,
    ) {
        if self.seq < after {
            return;
        }
        for e in entries {
            if e.seq <= self.seq {
                continue;
            }
            if !e.forge.is_empty() {
                self.apply_record(ForgeRecord {
                    seq: e.seq,
                    at: e.created_at,
                    events: e.forge.clone(),
                });
            }
            self.seq = e.seq;
        }
    }

    fn apply_record(&mut self, r: ForgeRecord) {
        let at = rfc3339(r.at.as_ref());
        for (i, ev) in r.events.iter().enumerate() {
            let id = r.seq << 10 | i as u64;
            self.apply(id, &at, ev);
            self.cross_reference(id, &at, ev);
        }
        self.records.push(r);
    }

    /// Every `#n` an event's text names gets one "mentioned this in #source" entry (derived, so
    /// a replay or a checkpoint reproduces it without an event of its own).
    fn cross_reference(&mut self, id: u64, at: &str, ev: &ForgeEvent) {
        use forge_event::Body;
        let text = match &ev.body {
            Some(Body::Opened(o)) => format!("{} {}", o.title, o.body),
            Some(Body::Comment(c)) => c.body.clone(),
            Some(Body::Edit(e)) => match &e.change {
                Some(forge_edit::Change::Title(t) | forge_edit::Change::Body(t)) => t.clone(),
                _ => return,
            },
            Some(Body::Review(r)) => r
                .comments
                .iter()
                .fold(r.body.clone(), |s, c| s + " " + &c.body),
            _ => return,
        };
        let source = ev.number;
        for target in mentions(&text) {
            let Some(item) = self.items.get_mut(&target).filter(|_| target != source) else {
                continue;
            };
            let seen = item
                .timeline
                .iter()
                .any(|t| matches!(t.event, Timeline::Referenced { source: s } if s == source));
            if !seen {
                item.timeline.push(TimelineItem {
                    id,
                    actor: ev.actor.clone(),
                    at: at.to_owned(),
                    event: Timeline::Referenced { source },
                });
            }
        }
    }

    fn apply(&mut self, id: u64, at: &str, ev: &ForgeEvent) {
        use forge_event::Body;
        let Some(body) = &ev.body else { return };
        if let Body::Label(l) = body {
            if l.deleted {
                self.labels.remove(&l.name);
                for item in self.items.values_mut() {
                    item.labels.remove(&l.name);
                }
            } else {
                self.labels.insert(
                    l.name.clone(),
                    LabelDef {
                        name: l.name.clone(),
                        color: l.color.clone(),
                        description: l.description.clone(),
                    },
                );
            }
            return;
        }
        if let Body::Opened(o) = body {
            self.items.insert(
                ev.number,
                Item {
                    number: ev.number,
                    kind: if o.kind() == ForgeKind::Pull {
                        "pull"
                    } else {
                        "issue"
                    },
                    title: o.title.clone(),
                    body: o.body.clone(),
                    edited: false,
                    author: ev.actor.clone(),
                    state: "open",
                    state_reason: String::new(),
                    labels: o.labels.iter().cloned().collect(),
                    assignees: o.assignees.iter().cloned().collect(),
                    created_at: at.to_owned(),
                    updated_at: at.to_owned(),
                    closed_at: None,
                    comments: 0,
                    base_ref: o.base_ref.clone(),
                    head_ref: o.head_ref.clone(),
                    base_oid: o.base_oid.clone(),
                    head_oid: o.head_oid.clone(),
                    review_decision: String::new(),
                    merged: false,
                    draft: o.draft,
                    merge_commit: String::new(),
                    reviews: BTreeMap::new(),
                    timeline: Vec::new(),
                },
            );
            return;
        }
        let Some(item) = self.items.get_mut(&ev.number) else {
            return;
        };
        at.clone_into(&mut item.updated_at);
        let event = match body {
            Body::Opened(_) | Body::Label(_) => return,
            Body::Draft(d) => {
                item.draft = d.draft;
                Timeline::Draft { draft: d.draft }
            }
            Body::Comment(c) => {
                item.comments += 1;
                Timeline::Comment {
                    body: c.body.clone(),
                    edited: false,
                    deleted: false,
                }
            }
            Body::Edit(e) if e.comment_id == 0 => match &e.change {
                Some(forge_edit::Change::Title(t)) => {
                    let from = std::mem::replace(&mut item.title, t.clone());
                    Timeline::Renamed {
                        from,
                        to: t.clone(),
                    }
                }
                Some(forge_edit::Change::Body(b)) => {
                    item.body.clone_from(b);
                    item.edited = true;
                    return;
                }
                _ => return,
            },
            Body::Edit(e) => {
                let comment = item.timeline.iter_mut().find(|t| t.id == e.comment_id);
                if let Some(TimelineItem {
                    event:
                        Timeline::Comment {
                            body,
                            edited,
                            deleted,
                        },
                    ..
                }) = comment
                {
                    match &e.change {
                        Some(forge_edit::Change::Body(b)) => {
                            body.clone_from(b);
                            *edited = true;
                        }
                        Some(forge_edit::Change::Deleted(true)) if !*deleted => {
                            body.clear();
                            *deleted = true;
                            item.comments = item.comments.saturating_sub(1);
                        }
                        _ => {}
                    }
                }
                return;
            }
            Body::State(s) if s.closed => {
                item.state = "closed";
                item.state_reason.clone_from(&s.reason);
                item.closed_at = Some(at.to_owned());
                if !s.head_oid.is_empty() {
                    item.base_oid.clone_from(&s.base_oid);
                    item.head_oid.clone_from(&s.head_oid);
                }
                Timeline::Closed {
                    reason: s.reason.clone(),
                }
            }
            Body::State(_) => {
                item.state = "open";
                item.state_reason.clear();
                item.closed_at = None;
                Timeline::Reopened
            }
            Body::Labels(c) => {
                let (added, removed) = change_set(&mut item.labels, c);
                Timeline::Labeled { added, removed }
            }
            Body::Assignees(c) => {
                let (added, removed) = change_set(&mut item.assignees, c);
                Timeline::Assigned { added, removed }
            }
            Body::Merged(m) => {
                item.state = "closed";
                item.merged = true;
                item.merge_commit.clone_from(&m.merge_commit_oid);
                item.closed_at = Some(at.to_owned());
                item.base_oid.clone_from(&m.base_oid);
                item.head_oid.clone_from(&m.head_oid);
                Timeline::Merged {
                    method: m.method.clone(),
                    commit: m.merge_commit_oid.clone(),
                }
            }
            Body::Review(r) => {
                if r.state != "commented" {
                    item.reviews.insert(ev.actor.clone(), r.state.clone());
                    let states = || item.reviews.values();
                    item.review_decision = if states().any(|s| s == "changes_requested") {
                        "changes_requested".into()
                    } else if states().any(|s| s == "approved") {
                        "approved".into()
                    } else {
                        String::new()
                    };
                }
                Timeline::Review {
                    state: r.state.clone(),
                    body: r.body.clone(),
                    commit_oid: r.commit_oid.clone(),
                    comments: r
                        .comments
                        .iter()
                        .map(|c| LineComment {
                            path: c.path.clone(),
                            side: c.side.clone(),
                            line: c.line,
                            body: c.body.clone(),
                        })
                        .collect(),
                }
            }
        };
        item.timeline.push(TimelineItem {
            id,
            actor: ev.actor.clone(),
            at: at.to_owned(),
            event,
        });
    }

    /// Preconditions of one request against this state, checked by the publisher on every CAS
    /// attempt. Events carry `actor` already.
    pub(crate) fn verify(&self, actor: &Actor, events: &[ForgeEvent]) -> Result<(), String> {
        use forge_event::Body;
        if events.is_empty() {
            return Err("no forge events".into());
        }
        for ev in events {
            let Some(body) = &ev.body else {
                return Err("empty forge event".into());
            };
            if let Body::Label(l) = body {
                if !actor.write {
                    return Err("only collaborators can manage labels".into());
                }
                if l.name.trim().is_empty() || l.name.len() > 50 {
                    return Err("a label name is 1 to 50 characters".into());
                }
                let hex = l.color.strip_prefix('#').unwrap_or("");
                if !l.deleted && (hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit())) {
                    return Err(format!("label color {:?} is not #rrggbb", l.color));
                }
                if l.deleted && !self.labels.contains_key(&l.name) {
                    return Err(format!("no label {:?}", l.name));
                }
                continue;
            }
            if let Body::Opened(o) = body {
                if o.kind() == ForgeKind::Unspecified {
                    return Err("an item must be an issue or a pull request".into());
                }
                if o.title.trim().is_empty() {
                    return Err("title can't be blank".into());
                }
                if !actor.write && (!o.labels.is_empty() || !o.assignees.is_empty()) {
                    return Err("only collaborators can set labels and assignees".into());
                }
                if o.kind() == ForgeKind::Pull {
                    if o.base_ref.is_empty() || o.head_ref.is_empty() || o.base_ref == o.head_ref {
                        return Err("a pull request merges one branch into another".into());
                    }
                    let open = self.items.values().find(|i| {
                        i.state == "open" && i.base_ref == o.base_ref && i.head_ref == o.head_ref
                    });
                    if let Some(i) = open {
                        return Err(format!(
                            "a pull request already exists for {}: #{}",
                            o.head_ref, i.number
                        ));
                    }
                }
                continue;
            }
            let item = self
                .items
                .get(&ev.number)
                .ok_or_else(|| format!("#{} does not exist", ev.number))?;
            let own_or_write = actor.write || item.author == actor.name;
            match body {
                Body::Opened(_) | Body::Label(_) => {}
                Body::Comment(c) => {
                    if c.body.trim().is_empty() {
                        return Err("comment can't be blank".into());
                    }
                }
                Body::Edit(e) if e.comment_id == 0 => {
                    if !own_or_write {
                        return Err("only the author or a collaborator can edit this".into());
                    }
                    match &e.change {
                        Some(forge_edit::Change::Title(t)) if t.trim().is_empty() => {
                            return Err("title can't be blank".into());
                        }
                        Some(forge_edit::Change::Title(_) | forge_edit::Change::Body(_)) => {}
                        _ => return Err("an item edit sets the title or the body".into()),
                    }
                }
                Body::Edit(e) => {
                    let author = item.timeline.iter().find_map(|t| match &t.event {
                        Timeline::Comment { deleted: false, .. } if t.id == e.comment_id => {
                            Some(t.actor.as_str())
                        }
                        _ => None,
                    });
                    let author =
                        author.ok_or_else(|| format!("comment {} does not exist", e.comment_id))?;
                    if !actor.write && author != actor.name {
                        return Err(
                            "only the author or a collaborator can edit this comment".into()
                        );
                    }
                }
                Body::State(s) => {
                    if !s.closed && item.merged {
                        return Err(format!("#{} was merged; it can't be reopened", item.number));
                    }
                    if !own_or_write {
                        return Err(
                            "only the author or a collaborator can close or reopen this".into()
                        );
                    }
                    if s.closed == (item.state == "closed") {
                        return Err(format!("#{} is already {}", item.number, item.state));
                    }
                    if s.closed && !matches!(s.reason.as_str(), "completed" | "not_planned") {
                        return Err("close reason is \"completed\" or \"not_planned\"".into());
                    }
                }
                Body::Labels(_) | Body::Assignees(_) => {
                    if !actor.write {
                        return Err("only collaborators can change labels and assignees".into());
                    }
                }
                Body::Review(r) => verify_review(item, actor, r)?,
                Body::Draft(d) => {
                    if item.kind != "pull" || item.state != "open" {
                        return Err(format!("#{} is not an open pull request", item.number));
                    }
                    if !own_or_write {
                        return Err("only the author or a collaborator can change this".into());
                    }
                    if d.draft == item.draft {
                        return Err(format!(
                            "#{} is already {}",
                            item.number,
                            if d.draft {
                                "a draft"
                            } else {
                                "ready for review"
                            }
                        ));
                    }
                }
                Body::Merged(m) => {
                    if item.kind != "pull" || item.state != "open" {
                        return Err(format!("#{} is not an open pull request", item.number));
                    }
                    if item.draft {
                        return Err(
                            "a draft pull request can't be merged; mark it ready for review".into(),
                        );
                    }
                    if !actor.write {
                        return Err("only collaborators can merge".into());
                    }
                    if !matches!(m.method.as_str(), "merge" | "squash" | "rebase") {
                        return Err(format!("{:?} is not merge, squash or rebase", m.method));
                    }
                }
            }
        }
        Ok(())
    }

    /// GitHub-style search: `is:open|closed|merged|unmerged|draft|issue|pr`, `author:`, `assignee:`, `label:` (repeat =
    /// all of them), `review:approved|changes_requested|none`, `sort:created-desc|created-asc|updated-desc|comments-desc`, free words matched
    /// against title and body. `page` is 1-based.
    pub fn search(&self, query: &str, page: usize) -> SearchPage {
        let mut state = None;
        let mut kind = None;
        let mut author = None;
        let mut assignee = None;
        let mut labels = Vec::new();
        let mut sort = "created-desc";
        let mut review = None;
        let mut merged = None;
        let mut draft = None;
        let mut words = Vec::new();
        let toks = tokens(query);
        for tok in &toks {
            match tok.split_once(':') {
                Some(("is", "open" | "closed")) => state = tok.strip_prefix("is:"),
                Some(("is", "merged")) => merged = Some(true),
                Some(("is", "draft")) => draft = Some(true),
                Some(("is", "unmerged")) => merged = Some(false),
                Some(("is", "issue")) => kind = Some("issue"),
                Some(("is", "pr")) => kind = Some("pull"),
                Some(("author", v)) => author = Some(v),
                Some(("assignee", v)) => assignee = Some(v),
                Some(("label", v)) => labels.push(v),
                Some(("sort", v)) => sort = v,
                Some(("review", v)) => review = Some(if v == "none" { "" } else { v }),
                _ => words.push(tok.to_lowercase()),
            }
        }
        let matching: Vec<&Item> = self
            .items
            .values()
            .filter(|i| kind.is_none_or(|k| i.kind == k))
            .filter(|i| author.is_none_or(|a| i.author == a))
            .filter(|i| assignee.is_none_or(|a| i.assignees.contains(a)))
            .filter(|i| labels.iter().all(|l| i.labels.contains(*l)))
            .filter(|i| review.is_none_or(|r| i.review_decision == r))
            .filter(|i| merged.is_none_or(|m| i.merged == m))
            .filter(|i| draft.is_none_or(|d| i.draft == d))
            .filter(|i| {
                words.iter().all(|w| {
                    i.title.to_lowercase().contains(w.as_str())
                        || i.body.to_lowercase().contains(w.as_str())
                })
            })
            .collect();
        let open_count = matching.iter().filter(|i| i.state == "open").count();
        let closed_count = matching.len() - open_count;
        let mut rows: Vec<&Item> = matching
            .into_iter()
            .filter(|i| state.is_none_or(|s| i.state == s))
            .collect();
        match sort {
            "created-asc" => rows.sort_by_key(|i| i.number),
            "updated-desc" => rows.sort_by(|a, b| {
                b.updated_at
                    .cmp(&a.updated_at)
                    .then(b.number.cmp(&a.number))
            }),
            "comments-desc" => {
                rows.sort_by(|a, b| b.comments.cmp(&a.comments).then(b.number.cmp(&a.number)));
            }
            _ => rows.sort_by_key(|i| std::cmp::Reverse(i.number)),
        }
        let page = page.max(1);
        let of_kind = || {
            self.items
                .values()
                .filter(|i| kind.is_none_or(|k| i.kind == k))
        };
        SearchPage {
            items: rows
                .into_iter()
                .skip((page - 1) * PER_PAGE)
                .take(PER_PAGE)
                .cloned()
                .collect(),
            open_count,
            closed_count,
            page,
            per_page: PER_PAGE,
            authors: of_kind().map(|i| i.author.clone()).collect(),
            labels: of_kind()
                .flat_map(|i| i.labels.iter().cloned())
                .chain(self.labels.keys().cloned())
                .collect(),
            assignees: of_kind()
                .flat_map(|i| i.assignees.iter().cloned())
                .collect(),
        }
    }
}

/// GitHub's review rules: open pull requests only; an author may comment on their own but not
/// approve it or request changes; a comment or change request says something.
fn verify_review(
    item: &Item,
    actor: &Actor,
    r: &walgit_proto::v1::ForgeReview,
) -> Result<(), String> {
    if item.kind != "pull" {
        return Err("only pull requests are reviewed".into());
    }
    if item.state != "open" {
        return Err(format!("#{} is closed", item.number));
    }
    match r.state.as_str() {
        "approved" | "changes_requested" if item.author == actor.name => {
            return Err("you can't approve or request changes on your own pull request".into());
        }
        "changes_requested" | "commented" if r.body.trim().is_empty() && r.comments.is_empty() => {
            return Err("a review with no approval needs a comment".into());
        }
        "approved" | "changes_requested" | "commented" => {}
        other => {
            return Err(format!(
                "review state {other:?} is not approved, changes_requested or commented"
            ));
        }
    }
    for c in &r.comments {
        if c.path.is_empty() || c.line == 0 || !matches!(c.side.as_str(), "LEFT" | "RIGHT") {
            return Err("a line comment needs a path, a line and a side (LEFT or RIGHT)".into());
        }
        if c.body.trim().is_empty() {
            return Err("comment can't be blank".into());
        }
    }
    Ok(())
}

/// `#n` references in text: `#` after a non-word character (or at the start) and before
/// digits. `page#12` and `&#12;` are not references.
fn mentions(text: &str) -> Vec<u64> {
    let mut out = Vec::new();
    let mut prev = ' ';
    for (i, c) in text.char_indices() {
        if c == '#' && !(prev.is_alphanumeric() || matches!(prev, '&' | '/' | '#')) {
            let digits: String = text
                .get(i + 1..)
                .unwrap_or_default()
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(n) = digits.parse() {
                out.push(n);
            }
        }
        prev = c;
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Whitespace-separated tokens; double quotes group (`label:"good first issue"`).
fn tokens(q: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in q.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl RepoHandle {
    /// Brings the forge state to the handle's manifest head. Call after a refs-level sync.
    pub async fn forge_ensure(&self) -> Result<(), WalError> {
        let manifest = self.manifest();
        let head = manifest.head_seq;
        let have = self.forge.lock().as_ref().map(|s| s.seq);
        if have.is_some_and(|s| s >= head) {
            return Ok(());
        }
        // Resume from what we have while the log still covers it; else from the checkpoint.
        let mut fresh = None;
        let from = match have {
            Some(seq) if seq + 1 >= manifest.min_seq.max(1) => seq,
            _ => {
                let cp = manifest.checkpoint.as_ref();
                let state = match cp.map(|c| c.forge_key.as_str()).filter(|k| !k.is_empty()) {
                    Some(key) => {
                        let (_, bytes) = self.store.get_bytes(key).await?.ok_or_else(|| {
                            WalError::Corrupt(format!("forge history missing: {key}"))
                        })?;
                        let h = ForgeHistory::decode(bytes.as_ref())
                            .map_err(|e| WalError::Corrupt(format!("forge history decode: {e}")))?;
                        ForgeState::from_history(h)
                    }
                    None => ForgeState {
                        seq: cp.map_or(0, |c| c.seq),
                        ..ForgeState::default()
                    },
                };
                let seq = state.seq;
                fresh = Some(state);
                seq
            }
        };
        let entries =
            crate::log_reader::read_log_manifest(self, &manifest, from + 1, Some(head)).await?;
        let mut guard = self.forge.lock();
        if let Some(state) = fresh
            && guard.as_ref().is_none_or(|g| g.seq < state.seq)
        {
            *guard = Some(state);
        }
        if let Some(state) = guard.as_mut() {
            state.fold_entries(from, &entries);
        }
        Ok(())
    }

    /// Reads the forge state (empty if never loaded). Pair with `sync_refs` + `forge_ensure`.
    pub fn forge_read<R>(&self, f: impl FnOnce(&ForgeState) -> R) -> R {
        match self.forge.lock().as_ref() {
            Some(s) => f(s),
            None => f(&ForgeState::default()),
        }
    }

    /// Folds entries the sync or the publisher just applied (no-op until loaded).
    pub(crate) fn forge_fold(&self, after: u64, entries: &[&LogEntry]) {
        if let Some(state) = self.forge.lock().as_mut() {
            state.fold_entries(after, entries.iter().copied());
        }
    }

    /// Publishes forge events as one FORGE entry. Returns the entry's seq; an `opened` item's
    /// number is `forge_read(|s| s.opened_at(seq))`.
    pub async fn publish_forge(
        &self,
        actor: Actor,
        events: Vec<ForgeEvent>,
    ) -> Result<u64, WalError> {
        let result = self
            .publish_forge_push(
                actor,
                events,
                None,
                walgit_proto::v1::RefTransaction::default(),
            )
            .await?;
        if let Some(msg) = result.forge_error {
            return Err(WalError::Invalid(msg));
        }
        Ok(result.seq)
    }

    /// Forge events committed with a ref transaction (and its pack) as one PUSH entry: both
    /// land or neither does. Refused refs are in `per_ref`, refused events in `forge_error`.
    pub async fn publish_forge_push(
        &self,
        actor: Actor,
        mut events: Vec<ForgeEvent>,
        pack: Option<walgit_git::IngestedPack>,
        txn: walgit_proto::v1::RefTransaction,
    ) -> Result<crate::PublishResult, WalError> {
        for ev in &mut events {
            ev.actor.clone_from(&actor.name);
        }
        self.enqueue_forge(actor, events, pack, txn).await
    }
}

impl ForgeState {
    /// The item opened by the entry at `seq`.
    pub fn opened_at(&self, seq: u64) -> Option<u64> {
        self.records
            .iter()
            .rev()
            .find(|r| r.seq == seq)?
            .events
            .iter()
            .find(|e| matches!(e.body, Some(forge_event::Body::Opened(_))))
            .map(|e| e.number)
    }

    /// Fills `opened` numbers from `next`; returns the next free number.
    pub(crate) fn number(events: &mut [ForgeEvent], mut next: u64) -> u64 {
        for ev in events {
            if matches!(ev.body, Some(forge_event::Body::Opened(_))) {
                ev.number = next;
                next += 1;
            }
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use walgit_proto::v1::{ForgeComment, ForgeEdit, ForgeOpened, ForgeStateChange};

    fn ev(number: u64, actor: &str, body: forge_event::Body) -> ForgeEvent {
        ForgeEvent {
            number,
            actor: actor.into(),
            body: Some(body),
        }
    }

    fn entry(seq: u64, forge: Vec<ForgeEvent>) -> LogEntry {
        LogEntry {
            seq,
            forge,
            ..Default::default()
        }
    }

    fn opened(title: &str) -> forge_event::Body {
        forge_event::Body::Opened(ForgeOpened {
            kind: ForgeKind::Issue as i32,
            title: title.into(),
            ..Default::default()
        })
    }

    #[test]
    fn fold_verify_search_and_history_round_trip() {
        let alice = Actor {
            name: "alice".into(),
            write: false,
        };
        let bob = Actor {
            name: "bob".into(),
            write: false,
        };
        let mut s = ForgeState::default();
        let mut first = vec![
            ev(0, "alice", opened("Crash on push")),
            ev(0, "alice", opened("Docs typo")),
        ];
        assert!(s.verify(&alice, &first).is_ok());
        assert_eq!(ForgeState::number(&mut first, 1), 3);
        let comment = ev(
            1,
            "bob",
            forge_event::Body::Comment(ForgeComment {
                body: "same here".into(),
            }),
        );
        s.fold_entries(
            0,
            &[
                entry(1, first),
                entry(2, Vec::new()),
                entry(3, vec![comment]),
            ],
        );
        assert_eq!((s.seq, s.items.len()), (3, 2));
        assert_eq!(s.opened_at(1), Some(1));

        // bob may not close alice's issue; alice may; closing twice is refused.
        let close = |n| {
            ev(
                n,
                "x",
                forge_event::Body::State(ForgeStateChange {
                    closed: true,
                    reason: "completed".into(),
                    ..Default::default()
                }),
            )
        };
        assert!(s.verify(&bob, &[close(1)]).is_err());
        assert!(s.verify(&alice, &[close(1)]).is_ok());
        s.fold_entries(3, &[entry(4, vec![close(1)])]);
        assert!(s.verify(&alice, &[close(1)]).is_err());

        // bob edits his own comment but not alice's title.
        let cid = 3 << 10;
        let edit = forge_event::Body::Edit(ForgeEdit {
            comment_id: cid,
            change: Some(forge_edit::Change::Body("+1".into())),
        });
        assert!(s.verify(&bob, &[ev(1, "bob", edit.clone())]).is_ok());
        let retitle = forge_event::Body::Edit(ForgeEdit {
            comment_id: 0,
            change: Some(forge_edit::Change::Title("t".into())),
        });
        assert!(s.verify(&bob, &[ev(1, "bob", retitle)]).is_err());

        // Entries after a later bound than the state's are refused: it missed some.
        s.fold_entries(5, &[entry(6, vec![ev(1, "bob", edit.clone())])]);
        assert_eq!(s.seq, 4);
        // Committed seqs skip an orphaned slot (5): a complete range folds across the hole.
        s.fold_entries(4, &[entry(6, vec![ev(1, "bob", edit)])]);
        assert_eq!(s.seq, 6);
        assert!(matches!(
            &s.items[&1].timeline[0].event,
            Timeline::Comment { body, edited: true, .. } if body == "+1"
        ));

        let open = s.search("is:open", 1);
        assert_eq!(
            (open.items.len(), open.open_count, open.closed_count),
            (1, 1, 1)
        );
        assert_eq!(s.search("crash", 1).items[0].number, 1);
        assert_eq!(s.search("is:closed author:alice", 1).items[0].number, 1);
        assert_eq!(s.search("sort:created-asc", 1).items[0].number, 1);

        // The checkpoint history folds back to the same state.
        let again = ForgeState::from_history(s.history_upto(s.seq));
        assert_eq!(again.seq, s.seq);
        assert_eq!(
            serde_json::to_string(&again.items.values().collect::<Vec<_>>()).ok(),
            serde_json::to_string(&s.items.values().collect::<Vec<_>>()).ok()
        );
        assert_eq!(again.items[&1].timeline.len(), s.items[&1].timeline.len());
    }

    /// Each reviewer's latest deciding review counts; a change request wins; a later comment
    /// review leaves a decision alone; reviews need an open pull request.
    #[test]
    fn review_decision() {
        use walgit_proto::v1::ForgeReview;
        let review = |who: &str, state: &str| {
            ev(
                1,
                who,
                forge_event::Body::Review(ForgeReview {
                    state: state.into(),
                    body: "b".into(),
                    ..Default::default()
                }),
            )
        };
        let pr = forge_event::Body::Opened(ForgeOpened {
            kind: ForgeKind::Pull as i32,
            title: "t".into(),
            base_ref: "main".into(),
            head_ref: "topic".into(),
            ..Default::default()
        });
        let mut s = ForgeState::default();
        s.fold_entries(0, &[entry(1, vec![ev(1, "alice", pr)])]);
        let bob = Actor {
            name: "bob".into(),
            write: false,
        };
        assert!(s.verify(&bob, &[review("bob", "approved")]).is_ok());
        assert!(s.verify(&bob, &[review("bob", "merged")]).is_err());
        let decision = |s: &ForgeState| s.items[&1].review_decision.clone();
        s.fold_entries(1, &[entry(2, vec![review("bob", "approved")])]);
        assert_eq!(decision(&s), "approved");
        s.fold_entries(2, &[entry(3, vec![review("carol", "changes_requested")])]);
        assert_eq!(decision(&s), "changes_requested");
        s.fold_entries(3, &[entry(4, vec![review("carol", "commented")])]);
        assert_eq!(decision(&s), "changes_requested");
        s.fold_entries(4, &[entry(5, vec![review("carol", "approved")])]);
        assert_eq!(decision(&s), "approved");
        assert_eq!(s.search("is:pr review:approved", 1).items.len(), 1);
        assert_eq!(s.search("is:pr review:none", 1).items.len(), 0);
    }

    /// Scale: fold, query, checkpoint size and cold rebuild at 1k/10k/100k issues with five
    /// comments each (docs/FORGE.md "Measurements"). `cargo test -p walgit-wal --release --lib
    /// forge::tests::bench_scale -- --ignored --nocapture`
    #[test]
    #[ignore = "benchmark"]
    fn bench_scale() {
        use std::time::Instant;
        for n in [1_000u64, 10_000, 100_000] {
            let mut entries = Vec::new();
            let mut seq = 0;
            let mut next = || {
                seq += 1;
                seq
            };
            for k in 1..=n {
                let mut open = vec![ev(
                    0,
                    &format!("user{}", k % 50),
                    opened(&format!("Crash {k} on push to main")),
                )];
                ForgeState::number(&mut open, k);
                entries.push(entry(next(), open));
                for c in 0..5 {
                    let body = forge_event::Body::Comment(ForgeComment {
                        body: format!(
                            "comment {c} on {k}: steps to reproduce, logs, and a stack trace"
                        ),
                    });
                    entries.push(entry(next(), vec![ev(k, "reviewer", body)]));
                }
                if k % 3 == 0 {
                    let labels = forge_event::Body::Labels(walgit_proto::v1::ForgeSetChange {
                        add: vec!["bug".into()],
                        remove: Vec::new(),
                    });
                    entries.push(entry(next(), vec![ev(k, "maint", labels)]));
                }
            }
            let log_bytes: usize = entries.iter().map(prost::Message::encoded_len).sum();
            let t = Instant::now();
            let mut s = ForgeState::default();
            s.fold_entries(0, &entries);
            let fold = t.elapsed();
            let mut q = Vec::new();
            for query in [
                "is:open",
                "is:open label:bug",
                "crash 4242",
                "sort:updated-desc author:user7",
            ] {
                let t = Instant::now();
                let page = s.search(query, 1);
                q.push(format!(
                    "{query:?} {:.1?} ({} open)",
                    t.elapsed(),
                    page.open_count
                ));
            }
            let t = Instant::now();
            let bytes = s.history_upto(s.seq).encode_to_vec();
            let encode = t.elapsed();
            let t = Instant::now();
            let again = ForgeState::from_history(
                ForgeHistory::decode(bytes.as_slice()).unwrap_or_default(),
            );
            let rebuild = t.elapsed();
            assert_eq!(again.items.len() as u64, n);
            println!(
                "{n} issues, {} events: log {} KiB ({} B/event), fold {fold:.1?}, forge.pb {} KiB encode {encode:.1?} cold rebuild {rebuild:.1?}\n  {}",
                entries.len(),
                log_bytes / 1024,
                log_bytes / entries.len(),
                bytes.len() / 1024,
                q.join("\n  ")
            );
        }
    }

    /// `#n` in a comment adds one "mentioned in" entry to #n (not to itself, not twice), and
    /// the facets list what the filter menus offer.
    #[test]
    fn cross_references_and_facets() {
        assert_eq!(mentions("see #2, #10 and page#3 or &#4; #2 again"), [2, 10]);
        let mut s = ForgeState::default();
        let mut open = vec![
            ev(0, "alice", opened("first")),
            ev(0, "bob", opened("second")),
        ];
        ForgeState::number(&mut open, 1);
        s.fold_entries(0, &[entry(1, open)]);
        let say = |n, text: &str| {
            ev(
                n,
                "carol",
                forge_event::Body::Comment(ForgeComment { body: text.into() }),
            )
        };
        s.fold_entries(
            1,
            &[
                entry(2, vec![say(1, "dup of #2, and #1 itself, and #99")]),
                entry(3, vec![say(1, "#2 again")]),
            ],
        );
        let refs: Vec<_> = s.items[&2].timeline.iter().map(|t| &t.event).collect();
        assert!(
            matches!(refs.as_slice(), [Timeline::Referenced { source: 1 }]),
            "{refs:?}"
        );
        assert!(
            s.items[&1]
                .timeline
                .iter()
                .all(|t| !matches!(t.event, Timeline::Referenced { .. }))
        );
        let page = s.search("is:issue", 1);
        assert_eq!(page.authors.iter().collect::<Vec<_>>(), ["alice", "bob"]);
    }

    #[test]
    fn tokens_group_quotes() {
        assert_eq!(
            tokens(r#"is:open label:"good first issue"  x"#),
            ["is:open", "label:good first issue", "x"]
        );
    }
}
