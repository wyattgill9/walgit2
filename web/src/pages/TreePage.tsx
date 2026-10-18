import { Link, useParams } from "react-router-dom";
import { api } from "../api";
import { useResolved } from "../use-resolved";
import { useRepo } from "./RepoLayout";
import { useData } from "../data";
import { Box } from "../components/Layout";
import { relTime } from "../format";
import { RefBar } from "../components/RefBar";
import { Markdown } from "../components/Markdown";
import { Avatar } from "../components/CommitRow";
import { useRecipes } from "../components/CloneSetup";

export function TreePage() {
  const { full, refs } = useRepo();
  const rest = useParams()["*"] ?? "";
  if (!refs.head) return <EmptyRepo full={full} />;
  return <TreeView full={full} rest={rest} />;
}

/** Plain git, nothing installed: git prompts on the 401 and keeps the token in its own
 * credential store. Commands are the server's recipes (`setup.rs`), safe in any shell. */
function EmptyRepo({ full }: { full: string }) {
  const r = useRecipes();
  const me = useData("me", () => api.me(), 60_000);
  let step = 0;
  return (
    <Box title="Push your code">
      <div className="pad">
        <p>This repository is empty. In your project folder (with at least one commit):</p>
        {r.trust && (
          <>
            <p className="muted small">{++step}. Once per computer: trust this host's self-signed certificate.</p>
            <pre className="code-block">{r.trust}</pre>
          </>
        )}
        {r.token_url && (
          <p className="muted small">
            {++step}. <a href={r.token_url}>Create an access token</a> and copy it.
          </p>
        )}
        <p className="muted small">{++step}. Push:</p>
        <pre className="code-block">
          git remote add origin {r.base_url}/{full}.git{"\n"}git push -u origin HEAD
        </pre>
        {r.token_url && (
          <p className="muted small">
            When git asks: <strong>Username</strong> {me.anonymous ? "your username" : me.login}, <strong>Password</strong> the token.
          </p>
        )}
      </div>
    </Box>
  );
}

function TreeView({ full, rest }: { full: string; rest: string }) {
  const { r, data: t } = useResolved(full, rest, (res) => api.tree(full, res.sha, res.path));
  const base = `/${full}`;
  const up = t.path.split("/").slice(0, -1).join("/");
  return (
    <>
      <RefBar refname={r.ref} refKind={r.kind} path={r.path} page="tree" />
      <Box
        className="tree"
        title={
          t.commit && (
            <div className="tree-commit">
              <Avatar name={t.commit.author} />
              <strong>{t.commit.author}</strong>
              <Link to={`${base}/commit/${t.commit.sha}`} className="commit-subject ellipsis">
                {t.commit.subject}
              </Link>
              <span className="spacer" />
              <Link to={`${base}/commit/${t.commit.sha}`} className="sha">
                {t.commit.sha.slice(0, 7)}
              </Link>
              <span className="muted small">{relTime(t.commit.commit_date)}</span>
            </div>
          )
        }
      >
        <table className="files">
          <tbody>
            {t.path && (
              <tr>
                <td className="icon" />
                <td colSpan={2}>
                  <Link to={`${base}/tree/${t.ref}${up ? "/" + up : ""}`}>..</Link>
                </td>
              </tr>
            )}
            {t.entries.map((e) => (
              <tr key={e.name}>
                <td className="icon">{e.type === "tree" ? <DirIcon /> : e.type === "commit" ? "⧉" : <FileIcon />}</td>
                <td>
                  {e.type === "commit" ? (
                    <span title={`submodule @ ${e.sha}`}>{e.name}</span>
                  ) : (
                    <Link to={`${base}/${e.type === "tree" ? "tree" : "blob"}/${t.ref}/${t.path ? t.path + "/" : ""}${e.name}`}>
                      {e.name}
                    </Link>
                  )}
                </td>
                <td className="muted small right" title={e.updated}>
                  {e.updated ? relTime(e.updated) : ""}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </Box>
      {t.readme && (
        <Box title={<span className="strong">{t.readme.name}</span>} className="readme">
          <div className="pad">
            <Markdown source={t.readme.contents} />
          </div>
        </Box>
      )}
    </>
  );
}

function DirIcon() {
  return (
    <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden className="dir">
      <path
        fill="currentColor"
        d="M1.75 1A1.75 1.75 0 0 0 0 2.75v10.5C0 14.216.784 15 1.75 15h12.5A1.75 1.75 0 0 0 16 13.25v-8.5A1.75 1.75 0 0 0 14.25 3H7.5a.25.25 0 0 1-.2-.1l-.9-1.2C6.07 1.26 5.55 1 5 1H1.75Z"
      />
    </svg>
  );
}

function FileIcon() {
  return (
    <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden className="muted">
      <path
        fill="currentColor"
        d="M2 1.75C2 .784 2.784 0 3.75 0h6.586c.464 0 .909.184 1.237.513l2.914 2.914c.329.328.513.773.513 1.237v9.586A1.75 1.75 0 0 1 13.25 16h-9.5A1.75 1.75 0 0 1 2 14.25Zm1.75-.25a.25.25 0 0 0-.25.25v12.5c0 .138.112.25.25.25h9.5a.25.25 0 0 0 .25-.25V6h-2.75A1.75 1.75 0 0 1 9 4.25V1.5Zm6.75.062V4.25c0 .138.112.25.25.25h2.688l-.011-.013-2.914-2.914-.013-.011Z"
      />
    </svg>
  );
}
