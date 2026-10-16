import { useState, useTransition, type ReactNode } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { api, type Issue, type IssueDetail, type TimelineItem } from "../api";
import { useRepo } from "./RepoLayout";
import { invalidate, reportError, useData } from "../data";
import { Box } from "../components/Layout";
import { Avatar } from "../components/CommitRow";
import { Markdown } from "../components/Markdown";
import { Pager } from "./BranchesPage";
import { relTime } from "../format";

// Octicons (MIT): issue-opened, issue-closed, circle-slash (not planned), comment.
const ICON = {
  open: "M8 9.5a1.5 1.5 0 1 0 0-3 1.5 1.5 0 0 0 0 3ZM8 0a8 8 0 1 1 0 16A8 8 0 0 1 8 0ZM1.5 8a6.5 6.5 0 1 0 13 0 6.5 6.5 0 0 0-13 0Z",
  completed:
    "M11.28 6.78a.75.75 0 0 0-1.06-1.06L7.25 8.69 5.78 7.22a.75.75 0 0 0-1.06 1.06l2 2a.75.75 0 0 0 1.06 0l3.5-3.5ZM16 8A8 8 0 1 1 0 8a8 8 0 0 1 16 0Zm-1.5 0a6.5 6.5 0 1 0-13 0 6.5 6.5 0 0 0 13 0Z",
  not_planned:
    "M8 0a8 8 0 1 1 0 16A8 8 0 0 1 8 0ZM1.5 8a6.5 6.5 0 0 0 10.535 5.096l-9.131-9.13A6.472 6.472 0 0 0 1.5 8Zm6.5-6.5c-1.52 0-2.917.521-4.025 1.396l9.129 9.129A6.5 6.5 0 0 0 8 1.5Z",
  comment:
    "M1 2.75C1 1.784 1.784 1 2.75 1h10.5c.966 0 1.75.784 1.75 1.75v7.5A1.75 1.75 0 0 1 13.25 12H9.06l-2.573 2.573A1.458 1.458 0 0 1 4 13.543V12H2.75A1.75 1.75 0 0 1 1 10.25Zm1.75-.25a.25.25 0 0 0-.25.25v7.5c0 .138.112.25.25.25h2a.75.75 0 0 1 .75.75v2.19l2.72-2.72a.749.749 0 0 1 .53-.22h4.5a.25.25 0 0 0 .25-.25v-7.5a.25.25 0 0 0-.25-.25Z",
};

function Icon({ d, className = "" }: { d: string; className?: string }) {
  return (
    <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden className={className}>
      <path fill="currentColor" d={d} />
    </svg>
  );
}

type StateKey = "open" | "completed" | "not_planned";
const stateOf = (i: Issue): StateKey => (i.state === "open" ? "open" : i.state_reason === "not_planned" ? "not_planned" : "completed");
const STATE_LABEL: Record<StateKey, string> = { open: "Open", completed: "Closed", not_planned: "Closed as not planned" };

function StateIcon({ issue }: { issue: Issue }) {
  const s = stateOf(issue);
  return <Icon d={ICON[s]} className={`issue-state ${s}`} />;
}

/** Signed-in principal (`/api/v1/me`); anonymous readers get no write controls. */
function useMe() {
  return useData("me", () => api.me(), 60_000);
}

// ---- query helpers: the URL's `q` is GitHub's search string ----

const words = (q: string) => q.split(/\s+/).filter(Boolean);
/** Replace every token matching `re` with `token` (put first). */
const setQ = (q: string, re: RegExp, token: string) => [token, ...words(q).filter((t) => !re.test(t))].join(" ");
const addQ = (q: string, token: string) => (words(q).includes(token) ? q : `${q} ${token}`.trim());
const STATE_RE = /^is:(open|closed)$/;
const SORT_RE = /^sort:/;
const quote = (v: string) => (/\s/.test(v) ? `"${v}"` : v);
const SORTS: [string, string][] = [
  ["created-desc", "Newest"],
  ["created-asc", "Oldest"],
  ["updated-desc", "Recently updated"],
  ["comments-desc", "Most commented"],
];

/** GitHub's `/issues`: search box, Open/Closed counts, sort, rows; query and page in the URL. */
export function IssuesPage() {
  const { full } = useRepo();
  const [params, setParams] = useSearchParams();
  const q = params.get("q") ?? "is:open";
  const page = Math.max(1, Number(params.get("page")) || 1);
  const [, startTransition] = useTransition();
  const go = (next: { q?: string; page?: number }) =>
    startTransition(() => {
      const p = new URLSearchParams();
      const nq = next.q ?? q;
      if (nq !== "is:open") p.set("q", nq);
      if ((next.page ?? 1) > 1) p.set("page", String(next.page));
      setParams(p);
    });
  return (
    <>
      <div className="issues-bar">
        <SearchBox key={q} q={q} onSearch={(nq) => go({ q: nq, page: 1 })} />
        <Link to={`/${full}/issues/new`} className="btn primary">
          New issue
        </Link>
      </div>
      <IssueList full={full} q={q} page={page} go={go} />
    </>
  );
}

function SearchBox({ q, onSearch }: { q: string; onSearch: (q: string) => void }) {
  const [text, setText] = useState(q);
  return (
    <form
      className="issues-search"
      onSubmit={(e) => {
        e.preventDefault();
        onSearch(text.trim());
      }}
    >
      <input type="search" value={text} onChange={(e) => setText(e.target.value)} aria-label="Search issues" placeholder="is:open label:bug" />
    </form>
  );
}

function IssueList({ full, q, page, go }: { full: string; q: string; page: number; go: (n: { q?: string; page?: number }) => void }) {
  const data = useData(`issues:${full}:${q}:${page}`, () => api.issuePage(full, q, page));
  const state = words(q).find((t) => STATE_RE.test(t))?.slice(3);
  const total = state === "open" ? data.open_count : state === "closed" ? data.closed_count : data.open_count + data.closed_count;
  const sort = words(q).find((t) => SORT_RE.test(t))?.slice(5) ?? "created-desc";
  const filter = (token: string) => go({ q: addQ(q, token), page: 1 });
  return (
    <>
      <Box
        title={
          <div className="issues-head">
            <button type="button" className={state === "open" ? "subtab active" : "subtab"} onClick={() => go({ q: setQ(q, STATE_RE, "is:open"), page: 1 })}>
              <Icon d={ICON.open} /> {data.open_count} Open
            </button>
            <button type="button" className={state === "closed" ? "subtab active" : "subtab"} onClick={() => go({ q: setQ(q, STATE_RE, "is:closed"), page: 1 })}>
              <Icon d={ICON.completed} /> {data.closed_count} Closed
            </button>
            <span className="spacer" />
            <select aria-label="Sort" value={sort} onChange={(e) => go({ q: setQ(q, SORT_RE, `sort:${e.target.value}`), page: 1 })}>
              {SORTS.map(([v, label]) => (
                <option key={v} value={v}>
                  {label}
                </option>
              ))}
            </select>
          </div>
        }
      >
        {data.items.length > 0 ? (
          data.items.map((i) => <IssueRow key={i.number} full={full} issue={i} onFilter={filter} />)
        ) : (
          <div className="pad muted small">No results matched your search.</div>
        )}
      </Box>
      {(page > 1 || page * data.per_page < total) && <Pager page={page} more={page * data.per_page < total} onPage={(p) => go({ page: p })} />}
    </>
  );
}

function IssueRow({ full, issue: i, onFilter }: { full: string; issue: Issue; onFilter: (token: string) => void }) {
  return (
    <div className="issue-row">
      <StateIcon issue={i} />
      <div className="issue-main">
        <div className="issue-line">
          <Link to={`/${full}/issues/${i.number}`} className="issue-title">
            {i.title}
          </Link>
          {i.labels.map((l) => (
            <button key={l} type="button" className="label-chip" onClick={() => onFilter(`label:${quote(l)}`)}>
              {l}
            </button>
          ))}
        </div>
        <div className="muted small">
          #{i.number}{" "}
          {i.state === "open" ? (
            <>
              opened {relTime(i.created_at)} by{" "}
              <button type="button" className="btn link" onClick={() => onFilter(`author:${i.author}`)}>
                {i.author}
              </button>
            </>
          ) : (
            <>
              by{" "}
              <button type="button" className="btn link" onClick={() => onFilter(`author:${i.author}`)}>
                {i.author}
              </button>{" "}
              was closed {relTime(i.closed_at ?? i.updated_at)}
            </>
          )}
        </div>
      </div>
      <div className="issue-side muted small">
        {i.assignees.map((a) => (
          <Avatar key={a} name={a} />
        ))}
        {i.comments > 0 && (
          <Link to={`/${full}/issues/${i.number}`} className="issue-comments" aria-label={`${i.comments} comments`}>
            <Icon d={ICON.comment} /> {i.comments}
          </Link>
        )}
      </div>
    </div>
  );
}

// ---- one issue ----

/** GitHub's `/issues/{n}`: title, state, timeline, composer, assignees/labels sidebar. */
export function IssuePage() {
  const { full } = useRepo();
  const n = Number(useParams().number);
  const issue = useData(`issue:${full}:${n}`, () => api.issue(full, n));
  const me = useMe();
  const signedIn = !me.anonymous || me.write;
  const canEdit = (author: string) => me.write || (signedIn && me.principal === author);
  const issues = api.issues(full);
  /** Run a write; refresh this issue and the lists. False (+ the error tray) when refused. */
  const act = async (what: string, write: () => Promise<unknown>) => {
    try {
      await write();
      invalidate(`issue:${full}:${n}`);
      invalidate(`issues:${full}`);
      return true;
    } catch (e) {
      reportError(e, what);
      return false;
    }
  };
  const participants = [...new Set([issue.author, ...issue.timeline.map((t) => t.actor)])];
  return (
    <>
      <IssueHeader issue={issue} canEdit={canEdit(issue.author)} onTitle={(title) => act("rename issue", () => issues.update(n, { title }))} />
      <div className="issue-layout">
        <div className="issue-timeline">
          <CommentBox
            author={issue.author}
            at={issue.created_at}
            body={issue.body}
            edited={issue.edited}
            canEdit={canEdit(issue.author)}
            onSave={(body) => act("edit issue", () => issues.update(n, { body }))}
          />
          {issue.timeline.map((t) => (
            <TimelineEntry
              key={t.id}
              item={t}
              canEdit={canEdit(t.actor)}
              onSave={(body) => act("edit comment", () => issues.editComment(n, t.id, body))}
              onDelete={() => act("delete comment", () => issues.deleteComment(n, t.id))}
            />
          ))}
          {signedIn ? (
            <Composer
              issue={issue}
              canClose={canEdit(issue.author)}
              onComment={(body) => act("comment", () => issues.comment(n, body))}
              onClose={(reason, body) => act("close issue", () => (body ? issues.comment(n, body, reason) : issues.update(n, { state: "closed", state_reason: reason })))}
              onReopen={(body) =>
                act("reopen issue", async () => {
                  await issues.update(n, { state: "open" });
                  if (body) await issues.comment(n, body);
                })
              }
            />
          ) : (
            <div className="pad muted small">Sign in to comment.</div>
          )}
        </div>
        <aside className="issue-sidebar">
          <SideList
            title="Assignees"
            values={issue.assignees}
            empty="No one assigned"
            editable={me.write}
            onAdd={(v) => act("assign", () => issues.update(n, { add_assignees: [v] }))}
            onRemove={(v) => act("unassign", () => issues.update(n, { remove_assignees: [v] }))}
          />
          <SideList
            title="Labels"
            values={issue.labels}
            empty="None yet"
            chips
            editable={me.write}
            onAdd={(v) => act("label", () => issues.update(n, { add_labels: [v] }))}
            onRemove={(v) => act("unlabel", () => issues.update(n, { remove_labels: [v] }))}
          />
          <div className="side-section">
            <div className="side-title">{participants.length === 1 ? "1 participant" : `${participants.length} participants`}</div>
            <div className="row wrap">
              {participants.map((p) => (
                <Avatar key={p} name={p} />
              ))}
            </div>
          </div>
        </aside>
      </div>
    </>
  );
}

function IssueHeader({ issue, canEdit, onTitle }: { issue: IssueDetail; canEdit: boolean; onTitle: (t: string) => Promise<boolean> }) {
  const [editing, setEditing] = useState<string | null>(null);
  const s = stateOf(issue);
  return (
    <div className="issue-header">
      {editing === null ? (
        <h2 className="issue-h">
          <span>{issue.title}</span> <span className="muted">#{issue.number}</span>
          {canEdit && (
            <button type="button" className="btn small" onClick={() => setEditing(issue.title)}>
              Edit
            </button>
          )}
        </h2>
      ) : (
        <form
          className="issue-h-edit"
          onSubmit={async (e) => {
            e.preventDefault();
            if (await onTitle(editing.trim())) setEditing(null);
          }}
        >
          <input value={editing} onChange={(e) => setEditing(e.target.value)} aria-label="Title" autoFocus />
          <button type="submit" className="btn primary" disabled={!editing.trim()}>
            Save
          </button>
          <button type="button" className="btn" onClick={() => setEditing(null)}>
            Cancel
          </button>
        </form>
      )}
      <div className="row wrap gap small muted">
        <span className={`state-badge ${s}`}>
          <Icon d={ICON[s]} /> {STATE_LABEL[s]}
        </span>
        <span>
          <strong>{issue.author}</strong> opened this issue {relTime(issue.created_at)} · {issue.comments} comment{issue.comments === 1 ? "" : "s"}
        </span>
      </div>
    </div>
  );
}

function CommentBox({
  author,
  at,
  body,
  edited,
  canEdit,
  onSave,
  onDelete,
}: {
  author: string;
  at: string;
  body: string;
  edited: boolean;
  canEdit: boolean;
  onSave: (body: string) => Promise<boolean>;
  onDelete?: () => Promise<boolean>;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  return (
    <div className="comment">
      <Avatar name={author} />
      <div className="comment-box">
        <div className="comment-head small">
          <strong>{author}</strong>
          <span className="muted" title={at}>
            commented {relTime(at)}
          </span>
          {edited && <span className="muted">· edited</span>}
          <span className="spacer" />
          {canEdit && draft === null && (
            <>
              <button type="button" className="btn link small" onClick={() => setDraft(body)}>
                Edit
              </button>
              {onDelete && (
                <button type="button" className="btn link small danger-text" onClick={() => window.confirm("Delete this comment?") && void onDelete()}>
                  Delete
                </button>
              )}
            </>
          )}
        </div>
        {draft === null ? (
          <div className="comment-body">{body.trim() ? <Markdown source={body} /> : <span className="muted small">No description provided.</span>}</div>
        ) : (
          <div className="comment-body">
            <Editor value={draft} onChange={setDraft} />
            <div className="composer-actions">
              <button type="button" className="btn" onClick={() => setDraft(null)}>
                Cancel
              </button>
              <button
                type="button"
                className="btn primary"
                onClick={async () => {
                  if (await onSave(draft)) setDraft(null);
                }}
              >
                Update comment
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

const names = (v: string[]) =>
  v.map((x, k) => (
    <span key={x}>
      {k > 0 && ", "}
      <span className="label-chip static">{x}</span>
    </span>
  ));

function TimelineEntry({
  item: t,
  canEdit,
  onSave,
  onDelete,
}: {
  item: TimelineItem;
  canEdit: boolean;
  onSave: (body: string) => Promise<boolean>;
  onDelete: () => Promise<boolean>;
}) {
  const event = (icon: string, what: ReactNode) => (
    <div className="timeline-event small">
      <Icon d={icon} className="muted" />
      <strong>{t.actor}</strong> {what}{" "}
      <span className="muted" title={t.at}>
        {relTime(t.at)}
      </span>
    </div>
  );
  switch (t.type) {
    case "comment":
      return t.deleted ? null : <CommentBox author={t.actor} at={t.at} body={t.body} edited={t.edited} canEdit={canEdit} onSave={onSave} onDelete={onDelete} />;
    case "closed":
      return event(ICON[t.reason === "not_planned" ? "not_planned" : "completed"], <>closed this as {t.reason === "not_planned" ? "not planned" : "completed"}</>);
    case "reopened":
      return event(ICON.open, <>reopened this</>);
    case "renamed":
      return event(ICON.comment, <>changed the title <s className="muted">{t.from}</s> {t.to}</>);
    case "labeled":
      return event(ICON.comment, <>{t.added.length > 0 && <>added {names(t.added)} </>}{t.removed.length > 0 && <>removed {names(t.removed)} </>}</>);
    case "assigned":
      return event(ICON.comment, <>{t.added.length > 0 && <>assigned {t.added.join(", ")} </>}{t.removed.length > 0 && <>unassigned {t.removed.join(", ")} </>}</>);
  }
}

/** Markdown textarea with GitHub's Write / Preview tabs. */
function Editor({ value, onChange, placeholder }: { value: string; onChange: (v: string) => void; placeholder?: string }) {
  const [preview, setPreview] = useState(false);
  return (
    <div className="editor">
      <div className="editor-tabs">
        <button type="button" className={preview ? "subtab" : "subtab active"} onClick={() => setPreview(false)}>
          Write
        </button>
        <button type="button" className={preview ? "subtab active" : "subtab"} onClick={() => setPreview(true)}>
          Preview
        </button>
      </div>
      {preview ? (
        <div className="editor-preview">{value.trim() ? <Markdown source={value} /> : <span className="muted small">Nothing to preview</span>}</div>
      ) : (
        <textarea value={value} onChange={(e) => onChange(e.target.value)} placeholder={placeholder ?? "Markdown is supported"} rows={6} />
      )}
    </div>
  );
}

function Composer({
  issue,
  canClose,
  onComment,
  onClose,
  onReopen,
}: {
  issue: IssueDetail;
  canClose: boolean;
  onComment: (body: string) => Promise<boolean>;
  onClose: (reason: "completed" | "not_planned", body: string) => Promise<boolean>;
  onReopen: (body: string) => Promise<boolean>;
}) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const run = async (write: () => Promise<boolean>) => {
    setBusy(true);
    if (await write()) setText("");
    setBusy(false);
  };
  const body = text.trim();
  return (
    <div className="comment composer">
      <Avatar name="+" />
      <div className="comment-box">
        <Editor value={text} onChange={setText} placeholder="Leave a comment" />
        <div className="composer-actions">
          {canClose &&
            (issue.state === "open" ? (
              <>
                <button type="button" className="btn" disabled={busy} onClick={() => run(() => onClose("completed", body))}>
                  <Icon d={ICON.completed} /> {body ? "Close with comment" : "Close issue"}
                </button>
                <button type="button" className="btn" disabled={busy} onClick={() => run(() => onClose("not_planned", body))}>
                  <Icon d={ICON.not_planned} /> Close as not planned
                </button>
              </>
            ) : (
              <button type="button" className="btn" disabled={busy} onClick={() => run(() => onReopen(body))}>
                <Icon d={ICON.open} /> {body ? "Reopen with comment" : "Reopen issue"}
              </button>
            ))}
          <button type="button" className="btn primary" disabled={busy || !body} onClick={() => run(() => onComment(body))}>
            Comment
          </button>
        </div>
      </div>
    </div>
  );
}

function SideList({
  title,
  values,
  empty,
  chips = false,
  editable,
  onAdd,
  onRemove,
}: {
  title: string;
  values: string[];
  empty: string;
  chips?: boolean;
  editable: boolean;
  onAdd: (v: string) => Promise<boolean>;
  onRemove: (v: string) => Promise<boolean>;
}) {
  const [text, setText] = useState("");
  return (
    <div className="side-section">
      <div className="side-title">{title}</div>
      {values.length === 0 && <div className="muted small">{empty}</div>}
      <div className={chips ? "row wrap gap" : "side-values"}>
        {values.map((v) => (
          <span key={v} className={chips ? "label-chip static" : "side-value small"}>
            {!chips && <Avatar name={v} />}
            {v}
            {editable && (
              <button type="button" className="btn link small" aria-label={`Remove ${v}`} onClick={() => void onRemove(v)}>
                ×
              </button>
            )}
          </span>
        ))}
      </div>
      {editable && (
        <form
          onSubmit={async (e) => {
            e.preventDefault();
            if (text.trim() && (await onAdd(text.trim()))) setText("");
          }}
        >
          <input className="side-input" value={text} onChange={(e) => setText(e.target.value)} placeholder={`Add ${title.toLowerCase().replace(/s$/, "")}…`} aria-label={`Add ${title}`} />
        </form>
      )}
    </div>
  );
}

// ---- new issue ----

export function NewIssuePage() {
  const { full } = useRepo();
  const navigate = useNavigate();
  const me = useMe();
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const [busy, setBusy] = useState(false);
  if (me.anonymous && !me.write) return <div className="pad muted">Sign in to open an issue.</div>;
  return (
    <form
      className="new-issue"
      onSubmit={async (e) => {
        e.preventDefault();
        setBusy(true);
        try {
          const created = await api.issues(full).create({ title: title.trim(), body });
          invalidate(`issues:${full}`);
          navigate(`/${full}/issues/${created.number}`);
        } catch (err) {
          reportError(err, "open issue");
          setBusy(false);
        }
      }}
    >
      <h2 className="page-title">New issue</h2>
      <div className="comment">
        <Avatar name={me.principal} />
        <div className="comment-box">
          <div className="comment-body">
            <input className="issue-title-input" value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Title" aria-label="Title" autoFocus />
            <Editor value={body} onChange={setBody} placeholder="Add a description" />
            <div className="composer-actions">
              <Link to={`/${full}/issues`} className="btn">
                Cancel
              </Link>
              <button type="submit" className="btn primary" disabled={busy || !title.trim()}>
                Create
              </button>
            </div>
          </div>
        </div>
      </div>
    </form>
  );
}
