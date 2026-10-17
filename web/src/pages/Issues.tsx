import { Suspense, useEffect, useMemo, useState, useTransition, type CSSProperties, type ReactNode } from "react";
import { Link, Navigate, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { api, atLeast, client, type Issue, type IssueDetail, type LineComment, type MergeMethod, type PullDetail, type ReviewState, type TimelineItem } from "../api";
import { useRepo } from "./RepoLayout";
import { invalidate, reportError, useData } from "../data";
import { Box } from "../components/Layout";
import { Skeleton } from "../components/Loading";
import { Avatar, CommitRow } from "../components/CommitRow";
import { Markdown } from "../components/Markdown";
import { Pager } from "./BranchesPage";
import { Diff, type LineNote } from "./CommitPage";
import { relTime } from "../format";

// Octicons (MIT): issue-opened, issue-closed, circle-slash (not planned), comment,
// git-pull-request, git-merge.
const ICON = {
  merged:
    "M5.45 5.154A4.25 4.25 0 0 0 9.25 7.5h1.378a2.251 2.251 0 1 1 0 1.5H9.25A5.734 5.734 0 0 1 5 7.123v3.505a2.25 2.25 0 1 1-1.5 0V5.372a2.25 2.25 0 1 1 1.95-.218ZM4.25 13.5a.75.75 0 1 0 0-1.5.75.75 0 0 0 0 1.5Zm8.5-4.5a.75.75 0 1 0 0-1.5.75.75 0 0 0 0 1.5ZM5 3.25a.75.75 0 1 0 0 .005V3.25Z",
  pull: "M1.5 3.25a2.25 2.25 0 1 1 3 2.122v5.256a2.251 2.251 0 1 1-1.5 0V5.372A2.25 2.25 0 0 1 1.5 3.25Zm5.677-.177L9.573.677A.25.25 0 0 1 10 .854V2.5h1A2.5 2.5 0 0 1 13.5 5v5.628a2.251 2.251 0 1 1-1.5 0V5a1 1 0 0 0-1-1h-1v1.646a.25.25 0 0 1-.427.177L7.177 3.427a.25.25 0 0 1 0-.354ZM3.75 2.5a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Zm0 9.5a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Zm8.25.75a.75.75 0 1 0 1.5 0 .75.75 0 0 0-1.5 0Z",
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

type StateKey = "open" | "completed" | "not_planned" | "merged" | "draft";
const stateOf = (i: Issue): StateKey =>
  i.merged ? "merged" : i.state === "open" ? (i.draft ? "draft" : "open") : i.state_reason === "not_planned" ? "not_planned" : "completed";
const STATE_LABEL: Record<StateKey, string> = { open: "Open", completed: "Closed", not_planned: "Closed as not planned", merged: "Merged", draft: "Draft" };
/** The icon of an item's state: pull requests use the PR / merge glyphs. */
const stateIcon = (i: Issue) => {
  if (i.kind === "pull") return i.merged ? ICON.merged : ICON.pull;
  const s = stateOf(i);
  return ICON[s === "draft" ? "open" : s];
};

function StateIcon({ issue }: { issue: Issue }) {
  return <Icon d={stateIcon(issue)} className={`issue-state ${stateOf(issue)}`} />;
}

/** URL segment of one item: GitHub's `/issues/{n}` and `/pull/{n}`. */
const itemPath = (full: string, i: { kind: Issue["kind"]; number: number }) => `/${full}/${i.kind === "pull" ? "pull" : "issues"}/${i.number}`;

/** Label name → `#rrggbb` from the repository's catalog. */
function useLabelColors(full: string): Map<string, string> {
  const labels = useData(`labels:${full}`, () => api.labelList(full), 30_000);
  return useMemo(() => new Map(labels.map((l) => [l.name, l.color])), [labels]);
}
/** A chip's color dot (CSS `--label`); hue is the label's meaning, the chip stays greyscale. */
const chipStyle = (color?: string) => (color ? ({ "--label": color } as CSSProperties) : undefined);

/** Who this browser is (`/api/v1/me`); anonymous readers get no write controls. */
function useMe() {
  return useData("me", () => api.me(), 60_000);
}

/** D51: the caller's role on this repository (from the summary); it decides the controls. */
function useRole(full: string) {
  return useData(`summary:${full}`, () => api.summary(full)).role;
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

/** GitHub's `/issues` and `/pulls`: search box, Open/Closed counts, sort, rows; query and page
 * in the URL. */
export function IssuesPage({ pull = false }: { pull?: boolean }) {
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
        <Link to={`/${full}/labels`} className="btn">
          Labels
        </Link>
        <Link to={pull ? `/${full}/compare` : `/${full}/issues/new`} className="btn primary">
          {pull ? "New pull request" : "New issue"}
        </Link>
      </div>
      <IssueList full={full} q={q} page={page} go={go} pull={pull} />
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

function IssueList({ full, q, page, go, pull }: { full: string; q: string; page: number; go: (n: { q?: string; page?: number }) => void; pull: boolean }) {
  const colors = useLabelColors(full);
  const data = useData(`${pull ? "pulls" : "issues"}:${full}:${q}:${page}`, () => (pull ? api.pullPage(full, q, page) : api.issuePage(full, q, page)));
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
              <Icon d={pull ? ICON.pull : ICON.open} /> {data.open_count} Open
            </button>
            <button type="button" className={state === "closed" ? "subtab active" : "subtab"} onClick={() => go({ q: setQ(q, STATE_RE, "is:closed"), page: 1 })}>
              <Icon d={ICON.completed} /> {data.closed_count} Closed
            </button>
            <span className="spacer" />
            <FilterMenu label="Author" qualifier="author" values={data.authors} q={q} go={go} />
            <FilterMenu label="Label" qualifier="label" values={data.labels} q={q} go={go} />
            <FilterMenu label="Assignee" qualifier="assignee" values={data.assignees} q={q} go={go} />
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
          data.items.map((i) => <IssueRow key={i.number} full={full} issue={i} onFilter={filter} colors={colors} />)
        ) : (
          <div className="pad muted small">No results matched your search.</div>
        )}
      </Box>
      {(page > 1 || page * data.per_page < total) && <Pager page={page} more={page * data.per_page < total} onPage={(p) => go({ page: p })} />}
    </>
  );
}

/** GitHub's Author / Label / Assignee menus: picking a value puts `qualifier:value` in the
 * query (labels add up, the others replace); the title entry clears it. */
function FilterMenu({
  label,
  qualifier,
  values,
  q,
  go,
}: {
  label: string;
  qualifier: string;
  values: string[];
  q: string;
  go: (n: { q?: string; page?: number }) => void;
}) {
  const re = new RegExp(`^${qualifier}:`);
  const current = qualifier === "label" ? "" : (words(q).find((t) => re.test(t))?.slice(qualifier.length + 1).replace(/"/g, "") ?? "");
  if (values.length === 0) return null;
  return (
    <select
      aria-label={label}
      value={current}
      onChange={(e) => {
        const v = e.target.value;
        const token = `${qualifier}:${quote(v)}`;
        const next = !v ? words(q).filter((t) => !re.test(t)).join(" ") : qualifier === "label" ? addQ(q, token) : setQ(q, re, token);
        go({ q: next, page: 1 });
      }}
    >
      <option value="">{label}</option>
      {values.map((v) => (
        <option key={v} value={v}>
          {v}
        </option>
      ))}
    </select>
  );
}

function IssueRow({ full, issue: i, onFilter, colors }: { full: string; issue: Issue; onFilter: (token: string) => void; colors: Map<string, string> }) {
  return (
    <div className="issue-row">
      <StateIcon issue={i} />
      <div className="issue-main">
        <div className="issue-line">
          <Link to={itemPath(full, i)} className="issue-title">
            {i.title}
          </Link>
          {i.labels.map((l) => (
            <button key={l} type="button" className="label-chip" style={chipStyle(colors.get(l))} onClick={() => onFilter(`label:${quote(l)}`)}>
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
          {i.review_decision && (
            <button type="button" className={`btn link review-chip ${i.review_decision === "approved" ? "review-approved" : "review-changes"}`} onClick={() => onFilter(`review:${i.review_decision}`)}>
              · {i.review_decision === "approved" ? "Approved" : "Changes requested"}
            </button>
          )}
        </div>
      </div>
      <div className="issue-side muted small">
        {i.assignees.map((a) => (
          <Avatar key={a} name={a} />
        ))}
        {i.comments > 0 && (
          <Link to={itemPath(full, i)} className="issue-comments" aria-label={`${i.comments} comments`}>
            <Icon d={ICON.comment} /> {i.comments}
          </Link>
        )}
      </div>
    </div>
  );
}

// ---- one issue ----

/** GitHub's `/issues/{n}` and `/pull/{n}[/commits|/files]`: title, state, timeline, composer,
 * assignees/labels sidebar; a pull request adds its branches and the Commits/Files tabs. */
export function IssuePage({ pull = false }: { pull?: boolean }) {
  const { full } = useRepo();
  const { number, tab } = useParams();
  const n = Number(number);
  const key = `${pull ? "pull" : "issue"}:${full}:${n}`;
  const issue: IssueDetail & Partial<PullDetail> = useData(key, () => (pull ? api.pull(full, n) : api.issue(full, n)));
  const me = useMe();
  const role = useRole(full);
  const colors = useLabelColors(full);
  const signedIn = !me.anonymous;
  const mine = (author: string) => signedIn && me.login === author;
  // Editing others' text needs write; closing and reopening them, triage (D51).
  const canEdit = (author: string) => atLeast(role, "write") || mine(author);
  const canClose = (author: string) => atLeast(role, "triage") || mine(author);
  const canTriage = atLeast(role, "triage");
  const issues = pull ? api.pulls(full) : api.issues(full);
  /** Run a write; refresh this item and the lists. False (+ the error tray) when refused. */
  const act = async (what: string, write: () => Promise<unknown>) => {
    try {
      await write();
      invalidate(key);
      invalidate(`${pull ? "pulls" : "issues"}:${full}`);
      return true;
    } catch (e) {
      reportError(e, what);
      return false;
    }
  };
  // `#n` links go to /issues/{n}; a pull request lives at /pull/{n}.
  if (!pull && issue.kind === "pull") return <Navigate to={`/${full}/pull/${n}`} replace />;
  const participants = [...new Set([issue.author, ...issue.timeline.map((t) => t.actor)])];
  return (
    <>
      <IssueHeader issue={issue} canEdit={canEdit(issue.author)} onTitle={(title) => act("rename", () => issues.update(n, { title }))} />
      {pull && <PullTabs full={full} pr={issue as PullDetail} tab={tab} />}
      {pull && tab ? (
        <Suspense fallback={<Skeleton title={false} rows={6} />}>
          <PullChanges
            full={full}
            pr={issue as PullDetail}
            tab={tab}
            me={signedIn ? me.login : null}
            onReview={(review) => act("review", () => api.pulls(full).review(n, review))}
          />
        </Suspense>
      ) : (
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
              full={full}
              pull={pull}
              head={issue.head_sha}
              canEdit={canEdit(t.actor)}
              onSave={(body) => act("edit comment", () => issues.editComment(n, t.id, body))}
              onDelete={() => act("delete comment", () => issues.deleteComment(n, t.id))}
            />
          ))}
          {pull && (
            <MergeBox
              full={full}
              pr={issue as PullDetail}
              canMerge={atLeast(role, "write")}
              onMerge={(m) => act("merge", () => api.pulls(full).merge(n, m))}
              onDeleteBranch={() => act("delete branch", () => api.pulls(full).deleteBranch(n))}
              canReady={canEdit(issue.author)}
              onDraft={(draft) => act(draft ? "convert to draft" : "ready for review", () => issues.update(n, { draft }))}
            />
          )}
          {signedIn ? (
            <Composer
              issue={issue}
              me={me.login}
              canClose={canClose(issue.author)}
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
            editable={canTriage}
            onAdd={(v) => act("assign", () => issues.update(n, { add_assignees: [v] }))}
            suggest={async (q) => (await api.users(q)).map((u) => u.login)}
            onRemove={(v) => act("unassign", () => issues.update(n, { remove_assignees: [v] }))}
          />
          <SideList
            title="Labels"
            values={issue.labels}
            empty="None yet"
            chips
            editable={canTriage}
            onAdd={(v) => act("label", () => issues.update(n, { add_labels: [v] }))}
            onRemove={(v) => act("unlabel", () => issues.update(n, { remove_labels: [v] }))}
            colors={colors}
          />
          {pull && issue.state === "open" && !issue.draft && canEdit(issue.author) && (
            <div className="side-section small">
              <span className="muted">Still in progress? </span>
              <button type="button" className="btn link small" onClick={() => void act("convert to draft", () => issues.update(n, { draft: true }))}>
                Convert to draft
              </button>
            </div>
          )}
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
      )}
    </>
  );
}

/** Conversation / Commits / Files changed, with GitHub's counts. The compare behind the counts is
 * fetched quietly (no count when it fails, e.g. a remote-served repository). */
function PullTabs({ full, pr, tab }: { full: string; pr: PullDetail; tab?: string }) {
  const [counts, setCounts] = useState<{ commits: number; files: number } | null>(null);
  useEffect(() => {
    let live = true;
    client
      .repo(full)
      .compare(pr.base_sha, pr.head_sha)
      .then(
        (c) => live && setCounts({ commits: c.commits.length, files: c.stats.length }),
        () => {},
      );
    return () => {
      live = false;
    };
  }, [full, pr.base_sha, pr.head_sha]);
  const talk = pr.timeline.filter((t) => (t.type === "comment" && !t.deleted) || t.type === "review").length;
  const base = `/${full}/pull/${pr.number}`;
  const tabs: [string | undefined, string, string, number | undefined][] = [
    [undefined, base, "Conversation", talk],
    ["commits", `${base}/commits`, "Commits", counts?.commits],
    ["files", `${base}/files`, "Files changed", counts?.files],
  ];
  return (
    <nav className="subtabs pull-tabs" aria-label="Pull request views">
      {tabs.map(([t, to, label, count]) => (
        <Link key={label} to={to} className={t === tab ? "subtab active" : "subtab"}>
          {label}
          {count !== undefined && <span className="tab-count">{count}</span>}
        </Link>
      ))}
    </nav>
  );
}

type ReviewInput = { state: ReviewState; body?: string; comments?: LineComment[] };
type Spot = { path: string; side: "LEFT" | "RIGHT"; line: number };
const spotKey = (s: Spot) => `${s.path}:${s.side}:${s.line}`;

/** Commits / Files changed: the three-dot compare of the tips the PR shows (immutable by sha).
 * Files changed is also where a review is written: "+" on a line opens a comment form; comments
 * either go out alone or collect into a pending review (kept in the page until submitted). */
function PullChanges({
  full,
  pr,
  tab,
  me,
  onReview,
}: {
  full: string;
  pr: PullDetail;
  tab: string;
  me: string | null;
  onReview: (r: ReviewInput) => Promise<boolean>;
}) {
  const spec = `${pr.base_sha}...${pr.head_sha}`;
  const cmp = useData(`compare:${full}:${spec}`, () => api.compare(full, pr.base_sha, pr.head_sha), Infinity);
  const [pending, setPending] = useState<LineComment[]>([]);
  const [open, setOpen] = useState<Spot | null>(null);
  if (tab === "commits") {
    return (
      <Box title={`${cmp.commits.length} commit${cmp.commits.length === 1 ? "" : "s"}`}>
        {cmp.commits.map((c) => (
          <CommitRow key={c.sha} repo={full} commit={c} />
        ))}
      </Box>
    );
  }
  const reviewing = me !== null && pr.state === "open";
  // Threads at each line: earlier reviews of this head, then this page's pending comments.
  const threads = new Map<string, { spot: Spot; items: { who: string; body: string; pending: boolean }[] }>();
  const at = (spot: Spot) => threads.get(spotKey(spot)) ?? threads.set(spotKey(spot), { spot, items: [] }).get(spotKey(spot))!;
  for (const t of pr.timeline) {
    if (t.type !== "review" || t.commit_oid !== pr.head_sha) continue;
    for (const c of t.comments) at(c).items.push({ who: t.actor, body: c.body, pending: false });
  }
  for (const c of pending) at(c).items.push({ who: me ?? "", body: c.body, pending: true });
  if (open) at(open);
  const notes: LineNote[] = [...threads.values()].map(({ spot, items }) => ({
    path: spot.path,
    side: spot.side,
    line: spot.line,
    node: (
      <div className="review-thread">
        {items.map((c, k) => (
          <div key={k} className="review-thread-item small">
            <strong>{c.who}</strong> {c.pending && <span className="pill">Pending</span>}
            <Markdown source={c.body} />
          </div>
        ))}
        {open && spotKey(open) === spotKey(spot) && (
          <LineForm
            pendingReview={pending.length > 0}
            onCancel={() => setOpen(null)}
            onSingle={async (body) => {
              if (await onReview({ state: "commented", comments: [{ ...spot, body }] })) setOpen(null);
            }}
            onAdd={(body) => {
              setPending((p) => [...p, { ...spot, body }]);
              setOpen(null);
            }}
          />
        )}
      </div>
    ),
  }));
  return (
    <>
      {reviewing && (
        <ReviewPanel
          pending={pending.length}
          own={pr.author === me}
          onSubmit={async (state, body) => {
            const ok = await onReview({ state, body, comments: pending });
            if (ok) setPending([]);
            return ok;
          }}
        />
      )}
      <Diff patch={cmp.patch} stats={cmp.stats} cacheKey={spec} notes={notes} onLine={reviewing ? (path, side, line) => setOpen({ path, side, line }) : undefined} />
    </>
  );
}

function LineForm({
  pendingReview,
  onCancel,
  onSingle,
  onAdd,
}: {
  pendingReview: boolean;
  onCancel: () => void;
  onSingle: (body: string) => Promise<void>;
  onAdd: (body: string) => void;
}) {
  const [text, setText] = useState("");
  const body = text.trim();
  return (
    <div className="line-form">
      <Editor value={text} onChange={setText} placeholder="Leave a comment" />
      <div className="composer-actions">
        <button type="button" className="btn" onClick={onCancel}>
          Cancel
        </button>
        {!pendingReview && (
          <button type="button" className="btn" disabled={!body} onClick={() => void onSingle(body)}>
            Add single comment
          </button>
        )}
        <button type="button" className="btn primary" disabled={!body} onClick={() => onAdd(body)}>
          {pendingReview ? "Add review comment" : "Start a review"}
        </button>
      </div>
    </div>
  );
}

/** GitHub's "Review changes": a summary and Comment / Approve / Request changes. */
function ReviewPanel({ pending, own, onSubmit }: { pending: number; own: boolean; onSubmit: (state: ReviewState, body: string) => Promise<boolean> }) {
  const [openPanel, setOpenPanel] = useState(false);
  const [text, setText] = useState("");
  const [state, setState] = useState<ReviewState>("commented");
  const [busy, setBusy] = useState(false);
  const choices: [ReviewState, string, string][] = [
    ["commented", "Comment", "Submit general feedback without explicit approval."],
    ["approved", "Approve", "Submit feedback and approve merging these changes."],
    ["changes_requested", "Request changes", "Submit feedback that must be addressed before merging."],
  ];
  return (
    <div className="review-panel">
      <button type="button" className="btn primary" onClick={() => setOpenPanel((o) => !o)} aria-expanded={openPanel}>
        Review changes{pending > 0 && <span className="pill">{pending}</span>}
      </button>
      {openPanel && (
        <div className="box review-pop">
          <Editor value={text} onChange={setText} placeholder="Leave a comment" />
          <div className="review-choices">
            {choices.map(([v, label, help]) => (
              <label key={v} className={own && v !== "commented" ? "muted" : ""}>
                <input type="radio" name="review" value={v} checked={state === v} disabled={own && v !== "commented"} onChange={() => setState(v)} />
                <span>
                  <strong>{label}</strong>
                  <span className="muted small"> {own && v !== "commented" ? "Pull request authors can't approve their own pull request." : help}</span>
                </span>
              </label>
            ))}
          </div>
          <div className="composer-actions">
            <button
              type="button"
              className="btn primary"
              disabled={busy || (state !== "approved" && !text.trim() && pending === 0)}
              onClick={async () => {
                setBusy(true);
                if (await onSubmit(state, text.trim())) {
                  setText("");
                  setOpenPanel(false);
                }
                setBusy(false);
              }}
            >
              Submit review
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

const MERGE_METHODS: [MergeMethod, string][] = [
  ["merge", "Create a merge commit"],
  ["squash", "Squash and merge"],
  ["rebase", "Rebase and merge"],
];

/** GitHub's merge box: review status, whether it merges cleanly, the merge button with its
 * method and a confirm step for the message; afterwards, "Delete branch". */
function MergeBox({
  full,
  pr,
  canMerge,
  onMerge,
  onDeleteBranch,
  canReady,
  onDraft,
}: {
  full: string;
  pr: PullDetail;
  canMerge: boolean;
  onMerge: (m: { method: MergeMethod; sha: string; message?: string }) => Promise<boolean>;
  onDeleteBranch: () => Promise<boolean>;
  /** The author or a collaborator: may mark a draft ready for review. */
  canReady: boolean;
  onDraft: (draft: boolean) => Promise<boolean>;
}) {
  if (pr.state !== "open") {
    return (
      <div className={`box review-status ${pr.merged ? "merged" : ""}`}>
        <Icon d={pr.merged ? ICON.merged : ICON.pull} />
        <div className="grow">
          <strong>{pr.merged ? "Pull request successfully merged and closed" : "Closed with unmerged commits"}</strong>
          <div className="muted small">
            {pr.merged && pr.merge_commit && (
              <>
                <Link to={`/${full}/commit/${pr.merge_commit}`} className="sha">
                  {pr.merge_commit.slice(0, 7)}
                </Link>{" "}
                is on <code className="branch-tag">{pr.base_ref}</code>.{" "}
              </>
            )}
            The <code className="branch-tag">{pr.head_ref}</code> branch can be deleted.
          </div>
        </div>
        {canMerge && (
          <button type="button" className="btn" onClick={() => void onDeleteBranch()}>
            Delete branch
          </button>
        )}
      </div>
    );
  }
  if (pr.draft) {
    return (
      <div className="box review-status">
        <Icon d={ICON.pull} />
        <div className="grow">
          <strong>This pull request is still a work in progress</strong>
          <div className="muted small">Draft pull requests can't be merged.</div>
        </div>
        {canReady && (
          <button type="button" className="btn" onClick={() => void onDraft(false)}>
            Ready for review
          </button>
        )}
      </div>
    );
  }
  return (
    <>
      <ReviewStatus full={full} n={pr.number} decision={pr.review_decision} />
      <Suspense
        fallback={
          <div className="box review-status">
            <span className="muted small">Checking for the ability to merge automatically…</span>
          </div>
        }
      >
        <MergeSection full={full} pr={pr} canMerge={canMerge} onMerge={onMerge} />
      </Suspense>
    </>
  );
}

function MergeSection({
  full,
  pr,
  canMerge,
  onMerge,
}: {
  full: string;
  pr: PullDetail;
  canMerge: boolean;
  onMerge: (m: { method: MergeMethod; sha: string; message?: string }) => Promise<boolean>;
}) {
  // Keyed by both tips: a push to either branch asks again.
  const m = useData(`mergeable:${full}:${pr.number}:${pr.base_sha}:${pr.head_sha}`, () => api.pulls(full).mergeability(pr.number), 10_000);
  const [method, setMethod] = useState<MergeMethod>("merge");
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const defaultMessage = method === "squash" ? `${pr.title} (#${pr.number})\n\n${pr.body}`.trim() : `Merge pull request #${pr.number} from ${pr.head_ref}\n\n${pr.title}`;
  const [cls, title, text] = m.up_to_date
    ? ["", "This branch is up to date", `${pr.base_ref} already contains every commit of ${pr.head_ref}.`]
    : m.mergeable
      ? ["approved", "This branch has no conflicts with the base branch", "Merging can be performed automatically."]
      : ["changes", "This branch has conflicts that must be resolved", m.conflicts.join(", ")];
  const confirm = async () => {
    setBusy(true);
    if (await onMerge({ method, sha: m.head_sha, message: method === "rebase" ? undefined : (message ?? defaultMessage) })) setMessage(null);
    setBusy(false);
  };
  return (
    <div className={`box review-status ${cls}`}>
      <Icon d={m.mergeable ? ICON.completed : ICON.not_planned} />
      <div className="grow">
        <strong>{title}</strong>
        <div className="muted small">{text}</div>
        {canMerge && m.mergeable && message === null && (
          <div className="merge-actions">
            <select aria-label="Merge method" value={method} onChange={(e) => setMethod(e.target.value as MergeMethod)}>
              {MERGE_METHODS.filter(([v]) => v !== "rebase" || m.rebaseable).map(([v, label]) => (
                <option key={v} value={v}>
                  {label}
                </option>
              ))}
            </select>
            <button type="button" className="btn primary" onClick={() => (method === "rebase" ? void confirm() : setMessage(defaultMessage))} disabled={busy}>
              {MERGE_METHODS.find(([v]) => v === method)?.[1]}
            </button>
          </div>
        )}
        {message !== null && (
          <div className="merge-confirm">
            <textarea value={message} onChange={(e) => setMessage(e.target.value)} rows={4} aria-label="Commit message" />
            <div className="composer-actions">
              <button type="button" className="btn" onClick={() => setMessage(null)}>
                Cancel
              </button>
              <button type="button" className="btn primary" disabled={busy || !message.trim()} onClick={() => void confirm()}>
                Confirm {method === "squash" ? "squash and merge" : "merge"}
              </button>
            </div>
          </div>
        )}
        {!canMerge && m.mergeable && <div className="muted small">Only collaborators with write access can merge.</div>}
      </div>
    </div>
  );
}

/** The review part of GitHub's merge box. */
function ReviewStatus({ full, n, decision }: { full: string; n: number; decision?: "approved" | "changes_requested" }) {
  const [cls, title, text] =
    decision === "approved"
      ? ["approved", "Changes approved", "At least one reviewer approved these changes."]
      : decision === "changes_requested"
        ? ["changes", "Changes requested", "A reviewer requested changes; their next review can clear it."]
        : ["", "Review requested", "No one has approved or requested changes yet."];
  return (
    <div className={`box review-status ${cls}`}>
      <Icon d={decision === "approved" ? ICON.completed : ICON.comment} />
      <div>
        <strong>{title}</strong>
        <div className="muted small">
          {text} <Link to={`/${full}/pull/${n}/files`}>Review the changes →</Link>
        </div>
      </div>
    </div>
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
          <Icon d={stateIcon(issue)} /> {issue.kind === "pull" && (s === "completed" || s === "not_planned") ? "Closed" : STATE_LABEL[s]}
        </span>
        {issue.kind === "pull" ? (
          <span>
            <strong>{issue.merged ? (issue.timeline.find((t) => t.type === "merged")?.actor ?? issue.author) : issue.author}</strong>{" "}
            {issue.merged ? "merged" : "wants to merge"} into <code className="branch-tag">{issue.base_ref}</code> from <code className="branch-tag">{issue.head_ref}</code> ·{" "}
            {issue.merged ? `merged ${relTime(issue.closed_at ?? issue.updated_at)}` : `opened ${relTime(issue.created_at)}`}
          </span>
        ) : (
          <span>
            <strong>{issue.author}</strong> opened this issue {relTime(issue.created_at)} · {issue.comments} comment{issue.comments === 1 ? "" : "s"}
          </span>
        )}
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
  full,
  pull,
  head,
  canEdit,
  onSave,
  onDelete,
}: {
  item: TimelineItem;
  full: string;
  pull: boolean;
  /** The PR's shown head: line comments on another commit are outdated. */
  head?: string;
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
      return pull
        ? event(ICON.completed, <>closed this</>)
        : event(ICON[t.reason === "not_planned" ? "not_planned" : "completed"], <>closed this as {t.reason === "not_planned" ? "not planned" : "completed"}</>);
    case "reopened":
      return event(ICON.open, <>reopened this</>);
    case "renamed":
      return event(ICON.comment, <>changed the title <s className="muted">{t.from}</s> {t.to}</>);
    case "labeled":
      return event(ICON.comment, <>{t.added.length > 0 && <>added {names(t.added)} </>}{t.removed.length > 0 && <>removed {names(t.removed)} </>}</>);
    case "assigned":
      return event(ICON.comment, <>{t.added.length > 0 && <>assigned {t.added.join(", ")} </>}{t.removed.length > 0 && <>unassigned {t.removed.join(", ")} </>}</>);
    case "draft":
      return event(ICON.pull, <>{t.draft ? "marked this pull request as draft" : "marked this pull request as ready for review"}</>);
    case "referenced":
      return event(
        ICON.comment,
        <>
          mentioned this in <Link to={`/${full}/issues/${t.source}`}>#{t.source}</Link>
        </>,
      );
    case "merged":
      return event(
        ICON.merged,
        <>
          {t.method === "squash" ? "squashed and merged" : t.method === "rebase" ? "rebased and merged" : "merged"} commit <span className="sha">{t.commit.slice(0, 7)}</span>
        </>,
      );
    case "review": {
      const verb = t.state === "approved" ? "approved these changes" : t.state === "changes_requested" ? "requested changes" : "reviewed";
      const cls = t.state === "approved" ? "review-approved" : t.state === "changes_requested" ? "review-changes" : "";
      return (
        <div className="review-entry">
          <div className={`timeline-event small ${cls}`}>
            <Icon d={t.state === "approved" ? ICON.completed : ICON.comment} />
            <strong>{t.actor}</strong> {verb}{" "}
            <span className="muted" title={t.at}>
              {relTime(t.at)}
            </span>
          </div>
          {(t.body || t.comments.length > 0) && (
            <div className="comment-box review-box">
              {t.body && (
                <div className="comment-body">
                  <Markdown source={t.body} />
                </div>
              )}
              {t.comments.map((c, k) => (
                <div key={k} className="review-line">
                  <div className="small">
                    <code>
                      {c.path}:{c.line}
                    </code>{" "}
                    {head && t.commit_oid !== head && <span className="pill">Outdated</span>}
                  </div>
                  <Markdown source={c.body} />
                </div>
              ))}
            </div>
          )}
        </div>
      );
    }
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
  me,
  canClose,
  onComment,
  onClose,
  onReopen,
}: {
  issue: IssueDetail;
  me: string;
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
  const noun = issue.kind === "pull" ? "pull request" : "issue";
  return (
    <div className="comment composer">
      <Avatar name={me} />
      <div className="comment-box">
        <Editor value={text} onChange={setText} placeholder="Leave a comment" />
        <div className="composer-actions">
          {canClose &&
            !issue.merged &&
            (issue.state === "open" ? (
              <>
                <button type="button" className="btn" disabled={busy} onClick={() => run(() => onClose("completed", body))}>
                  <Icon d={ICON.completed} /> {body ? "Close with comment" : `Close ${noun}`}
                </button>
                {issue.kind === "issue" && (
                  <button type="button" className="btn" disabled={busy} onClick={() => run(() => onClose("not_planned", body))}>
                    <Icon d={ICON.not_planned} /> Close as not planned
                  </button>
                )}
              </>
            ) : (
              <button type="button" className="btn" disabled={busy} onClick={() => run(() => onReopen(body))}>
                <Icon d={ICON.open} /> {body ? "Reopen with comment" : `Reopen ${noun}`}
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
  colors,
  suggest,
}: {
  title: string;
  values: string[];
  empty: string;
  chips?: boolean;
  editable: boolean;
  onAdd: (v: string) => Promise<boolean>;
  onRemove: (v: string) => Promise<boolean>;
  /** Chip colors and the input's suggestions (the label catalog). */
  colors?: Map<string, string>;
  /** Suggestions for what is typed (D51: assignees pick from the user directory). */
  suggest?: (q: string) => Promise<string[]>;
}) {
  const [text, setText] = useState("");
  const [found, setFound] = useState<string[]>([]);
  const listId = `side-${title}`;
  const options = colors ? [...colors.keys()] : found;
  return (
    <div className="side-section">
      <div className="side-title">{title}</div>
      {values.length === 0 && <div className="muted small">{empty}</div>}
      <div className={chips ? "row wrap gap" : "side-values"}>
        {values.map((v) => (
          <span key={v} className={chips ? "label-chip static" : "side-value small"} style={chipStyle(colors?.get(v))}>
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
          <input
            className="side-input"
            value={text}
            list={colors || suggest ? listId : undefined}
            onChange={(e) => {
              setText(e.target.value);
              suggest?.(e.target.value.trim()).then(setFound, () => setFound([]));
            }}
            placeholder={`Add ${title.toLowerCase().replace(/s$/, "")}…`}
            aria-label={`Add ${title}`}
          />
          {(colors || suggest) && (
            <datalist id={listId}>
              {options
                .filter((k) => !values.includes(k))
                .map((k) => (
                  <option key={k} value={k} />
                ))}
            </datalist>
          )}
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
  if (me.anonymous) return <div className="pad muted">Sign in to open an issue.</div>;
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
        <Avatar name={me.login} />
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

// ---- compare / new pull request ----

/** GitHub's `/compare/{base}...{head}`: pick two branches, see what merging brings, open a PR. */
export function ComparePage() {
  const { full, refs } = useRepo();
  const spec = useParams()["*"] ?? "";
  const navigate = useNavigate();
  const [b, h] = spec.includes("...") ? spec.split("...", 2) : ["", spec];
  const base = b || refs.head?.name || "main";
  const head = h ?? "";
  const branches = useData(`branch-pick:${full}`, () => api.refList(full, "branches", { n: 1000 }));
  const pick = (nb: string, nh: string) => navigate(`/${full}/compare/${nb}...${nh}`);
  const options = branches.refs.map((r) => (
    <option key={r.name} value={r.name}>
      {r.name}
    </option>
  ));
  return (
    <>
      <h2 className="page-title">Compare changes</h2>
      <p className="muted small">Choose two branches to see what's changed or to start a new pull request.</p>
      <div className="box compare-bar">
        <label>
          base{" "}
          <select value={base} onChange={(e) => pick(e.target.value, head)}>
            {options}
          </select>
        </label>
        <span className="muted">←</span>
        <label>
          compare{" "}
          <select value={head} onChange={(e) => pick(base, e.target.value)}>
            <option value="">choose a branch…</option>
            {options}
          </select>
        </label>
      </div>
      {head && head !== base && (
        <Suspense fallback={<Skeleton title={false} rows={6} />}>
          <CompareBody key={`${base}...${head}`} full={full} base={base} head={head} />
        </Suspense>
      )}
    </>
  );
}

function CompareBody({ full, base, head }: { full: string; base: string; head: string }) {
  const cmp = useData(`compare:${full}:${base}...${head}`, () => api.compare(full, base, head));
  const me = useMe();
  const navigate = useNavigate();
  const one = cmp.commits.length === 1 ? cmp.commits[0] : undefined;
  const [form, setForm] = useState(false);
  const [title, setTitle] = useState(one?.subject ?? head);
  const [body, setBody] = useState(one?.body ?? "");
  const [busy, setBusy] = useState(false);
  if (cmp.ahead === 0) {
    return (
      <Box>
        <div className="pad muted">
          There isn't anything to compare: <strong>{base}</strong> is up to date with all commits from <strong>{head}</strong>.
        </div>
      </Box>
    );
  }
  const create = async (draft: boolean) => {
    setBusy(true);
    try {
      const pr = await api.pulls(full).create({ title: title.trim(), body, base, head, draft });
      invalidate(`pulls:${full}`);
      navigate(`/${full}/pull/${pr.number}`);
    } catch (e) {
      reportError(e, "open pull request");
      setBusy(false);
    }
  };
  return (
    <>
      {form ? (
        <div className="comment">
          <Avatar name={me.login} />
          <div className="comment-box">
            <div className="comment-body">
              <input className="issue-title-input" value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Title" aria-label="Title" autoFocus />
              <Editor value={body} onChange={setBody} placeholder="Add a description" />
              <div className="composer-actions">
                <button type="button" className="btn" onClick={() => setForm(false)}>
                  Cancel
                </button>
                <button type="button" className="btn" disabled={busy || !title.trim()} onClick={() => void create(true)}>
                  Create draft pull request
                </button>
                <button type="button" className="btn primary" disabled={busy || !title.trim()} onClick={() => void create(false)}>
                  Create pull request
                </button>
              </div>
            </div>
          </div>
        </div>
      ) : (
        <div className="box compare-cta">
          <span className="small">
            <strong>{cmp.ahead}</strong> commit{cmp.ahead === 1 ? "" : "s"} · <strong>{cmp.stats.length}</strong> file{cmp.stats.length === 1 ? "" : "s"} changed
            {cmp.behind > 0 && <span className="muted"> · {cmp.behind} behind {base}</span>}
          </span>
          <span className="spacer" />
          {!me.anonymous ? (
            <button type="button" className="btn primary" onClick={() => setForm(true)}>
              Create pull request
            </button>
          ) : (
            <span className="muted small">Sign in to open a pull request.</span>
          )}
        </div>
      )}
      <Box title={`${cmp.commits.length} commit${cmp.commits.length === 1 ? "" : "s"}`}>
        {cmp.commits.map((c) => (
          <CommitRow key={c.sha} repo={full} commit={c} />
        ))}
      </Box>
      <Diff patch={cmp.patch} stats={cmp.stats} cacheKey={`${cmp.base}...${cmp.head}`} />
    </>
  );
}

// ---- labels ----

type LabelDraft = { name: string; color: string; description: string; isNew: boolean };
const randomColor = () => `#${Math.floor(Math.random() * 0xffffff).toString(16).padStart(6, "0")}`;

/** GitHub's `/labels`: the catalog with colors, descriptions and open counts; writers edit it.
 * Deleting a label also takes it off every issue and pull request. */
export function LabelsPage() {
  const { full } = useRepo();
  const labels = useData(`labels:${full}`, () => api.labelList(full), 30_000);
  const canTriage = atLeast(useRole(full), "triage");
  const [draft, setDraft] = useState<LabelDraft | null>(null);
  const refresh = () => {
    invalidate(`labels:${full}`);
    invalidate(`issues:${full}`);
    invalidate(`pulls:${full}`);
  };
  const save = async (d: LabelDraft) => {
    try {
      await api.labels(full).put(d.name.trim(), { color: d.color, description: d.description });
      refresh();
      setDraft(null);
    } catch (e) {
      reportError(e, "save label");
    }
  };
  const remove = async (name: string) => {
    if (!window.confirm(`Delete the label "${name}"? It is removed from every issue and pull request.`)) return;
    try {
      await api.labels(full).delete(name);
      refresh();
    } catch (e) {
      reportError(e, "delete label");
    }
  };
  return (
    <>
      <div className="issues-bar">
        <h2 className="page-title">Labels</h2>
        <span className="spacer" />
        {canTriage && (
          <button type="button" className="btn primary" onClick={() => setDraft({ name: "", color: randomColor(), description: "", isNew: true })}>
            New label
          </button>
        )}
      </div>
      {draft?.isNew && <LabelForm draft={draft} onChange={setDraft} onSave={save} onCancel={() => setDraft(null)} />}
      <Box title={`${labels.length} label${labels.length === 1 ? "" : "s"}`}>
        {labels.length === 0 && <div className="pad muted small">No labels yet.</div>}
        {labels.map((l) =>
          draft && !draft.isNew && draft.name === l.name ? (
            <LabelForm key={l.name} draft={draft} onChange={setDraft} onSave={save} onCancel={() => setDraft(null)} />
          ) : (
            <div key={l.name} className="label-row">
              <Link to={`/${full}/issues?q=${encodeURIComponent(`is:open label:${quote(l.name)}`)}`} className="label-chip static" style={chipStyle(l.color)}>
                {l.name}
              </Link>
              <span className="muted small grow">{l.description}</span>
              <span className="muted small">{l.open} open</span>
              {canTriage && (
                <span className="row gap">
                  <button type="button" className="btn link small" onClick={() => setDraft({ name: l.name, color: l.color, description: l.description, isNew: false })}>
                    Edit
                  </button>
                  <button type="button" className="btn link small danger-text" onClick={() => void remove(l.name)}>
                    Delete
                  </button>
                </span>
              )}
            </div>
          ),
        )}
      </Box>
    </>
  );
}

function LabelForm({ draft, onChange, onSave, onCancel }: { draft: LabelDraft; onChange: (d: LabelDraft) => void; onSave: (d: LabelDraft) => Promise<void>; onCancel: () => void }) {
  return (
    <form
      className="label-row label-form"
      onSubmit={(e) => {
        e.preventDefault();
        void onSave(draft);
      }}
    >
      <span className="label-chip static" style={chipStyle(draft.color)}>
        {draft.name || "preview"}
      </span>
      <input aria-label="Label name" placeholder="Label name" value={draft.name} disabled={!draft.isNew} onChange={(e) => onChange({ ...draft, name: e.target.value })} />
      <input aria-label="Description" placeholder="Description (optional)" className="grow" value={draft.description} onChange={(e) => onChange({ ...draft, description: e.target.value })} />
      <input aria-label="Color" type="color" value={draft.color} onChange={(e) => onChange({ ...draft, color: e.target.value })} />
      <button type="button" className="btn" onClick={onCancel}>
        Cancel
      </button>
      <button type="submit" className="btn primary" disabled={!draft.name.trim()}>
        {draft.isNew ? "Create label" : "Save changes"}
      </button>
    </form>
  );
}
