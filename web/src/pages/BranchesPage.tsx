import { useState, useTransition } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { api, type BranchRow } from "../api";
import { useRepo } from "./RepoLayout";
import { useData } from "../data";
import { Box } from "../components/Layout";
import { Avatar } from "../components/CommitRow";
import { CopyButton } from "../components/CopyButton";
import { relTime } from "../format";

type View = "overview" | "active" | "stale" | "all";
const VIEWS: [View, string][] = [
  ["overview", "Overview"],
  ["active", "Active"],
  ["stale", "Stale"],
  ["all", "All"],
];
const TITLES: Record<View, string> = {
  overview: "Active branches",
  active: "Active branches",
  stale: "Stale branches",
  all: "All branches",
};

/** GitHub's `/branches`: Overview (default + newest active), Active, Stale, All, with search
 * and paging in the URL (`?q=&page=`), so every state is a shareable link. Read-only. */
export function BranchesPage() {
  const { full } = useRepo();
  const param = useParams().view;
  const view: View = VIEWS.some(([v]) => v === param) ? (param as View) : "overview";
  const [params, setParams] = useSearchParams();
  const q = params.get("q") ?? "";
  const page = Math.max(1, Number(params.get("page")) || 1);
  const [text, setText] = useState(q);
  // URL updates run in a transition: the current list stays up while the next one loads.
  const [, startTransition] = useTransition();
  const go = (next: { q?: string; page?: number }) =>
    startTransition(() => {
      const p = new URLSearchParams();
      const nq = next.q ?? q;
      if (nq) p.set("q", nq);
      if ((next.page ?? 1) > 1) p.set("page", String(next.page));
      setParams(p);
    });
  const base = `/${full}/branches`;
  return (
    <>
      <h2 className="page-title">Branches</h2>
      <div className="branches-bar">
        <nav className="subtabs" aria-label="Branch views">
          {VIEWS.map(([v, label]) => (
            <Link
              key={v}
              to={`${base}${v === "overview" ? "" : `/${v}`}${q ? `?q=${encodeURIComponent(q)}` : ""}`}
              className={v === view ? "subtab active" : "subtab"}
            >
              {label}
            </Link>
          ))}
        </nav>
        <input
          type="search"
          className="branch-search"
          placeholder="Search branches…"
          aria-label="Search branches"
          value={text}
          onChange={(e) => {
            setText(e.target.value);
            go({ q: e.target.value, page: 1 });
          }}
        />
      </div>
      <Branches full={full} view={view} q={q} page={page} onPage={(p) => go({ page: p })} />
    </>
  );
}

function Branches({ full, view, q, page, onPage }: { full: string; view: View; q: string; page: number; onPage: (p: number) => void }) {
  const data = useData(`branches:${full}:${view}:${q}:${page}`, () => api.branchPage(full, { view, q: q || undefined, page }));
  // From Overview a search lists every match (the server searches like All).
  const listing = view !== "overview" || q !== "";
  return (
    <>
      {data.default && !q && (
        <Box title="Default">
          <BranchTable full={full} rows={[data.default]} isDefault />
        </Box>
      )}
      <Box title={q ? "Search results" : TITLES[view]}>
        {data.branches.length > 0 ? (
          <BranchTable full={full} rows={data.branches} />
        ) : (
          <div className="pad muted small">{q ? `No branches match “${q}”.` : `There aren't any ${view === "all" || view === "overview" ? "other" : view} branches.`}</div>
        )}
        {!listing && data.more && (
          <div className="branch-more">
            <Link to={`/${full}/branches/active`}>View more active branches →</Link>
          </div>
        )}
      </Box>
      {listing && (page > 1 || data.more) && <Pager page={page} more={data.more} onPage={onPage} />}
    </>
  );
}

function BranchTable({ full, rows, isDefault = false }: { full: string; rows: BranchRow[]; isDefault?: boolean }) {
  const max = Math.max(1, ...rows.map((b) => Math.max(b.ahead ?? 0, b.behind ?? 0)));
  return (
    <div className="branch-table" role="table">
      <div className="branch-row branch-cols" role="row">
        <span role="columnheader">Branch</span>
        <span role="columnheader">Updated</span>
        <span role="columnheader" className="ab-head">
          Behind | Ahead
        </span>
      </div>
      {rows.map((b) => (
        <div key={b.name} className="branch-row" role="row">
          <span className="branch-name" role="cell">
            <Link to={`/${full}/tree/${b.name}`} className="ellipsis">
              {b.name}
            </Link>
            <CopyButton text={b.name} className="branch-copy" />
          </span>
          <span className="branch-updated muted small" role="cell" title={b.updated ?? undefined}>
            {b.author && <Avatar name={b.author} />}
            {b.author && <span className="ellipsis">{b.author}</span>}
            {b.updated && <span>{relTime(b.updated)}</span>}
          </span>
          <span role="cell">
            {isDefault ? <span className="pill">default</span> : <AheadBehind ahead={b.ahead} behind={b.behind} max={max} />}
          </span>
        </div>
      ))}
    </div>
  );
}

/** GitHub's behind|ahead gauge: counts over two bars growing out from a center line. */
function AheadBehind({ ahead, behind, max }: { ahead: number | null; behind: number | null; max: number }) {
  if (ahead === null || behind === null) return null;
  const width = (n: number) => `${n === 0 ? 0 : Math.max(6, (n / max) * 100)}%`;
  return (
    <span className="ab" title={`${behind} commit${behind === 1 ? "" : "s"} behind, ${ahead} ahead of the default branch`}>
      <span className="ab-side ab-behind">
        <span className="tabular">{behind}</span>
        <i style={{ width: width(behind) }} />
      </span>
      <span className="ab-side ab-ahead">
        <span className="tabular">{ahead}</span>
        <i style={{ width: width(ahead) }} />
      </span>
    </span>
  );
}

function Pager({ page, more, onPage }: { page: number; more: boolean; onPage: (p: number) => void }) {
  return (
    <div className="pager">
      <button type="button" className="btn" disabled={page <= 1} onClick={() => onPage(page - 1)}>
        Previous
      </button>
      <span className="muted small tabular">Page {page}</span>
      <button type="button" className="btn" disabled={!more} onClick={() => onPage(page + 1)}>
        Next
      </button>
    </div>
  );
}
