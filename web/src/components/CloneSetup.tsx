import { api, type SetupRecipes } from "../api";
import { useData } from "../data";
import { CodeSample } from "./CopyButton";

/**
 * Clone/setup recipes for this host, rendered by the server at
 * `/services/setup.json` (crates/walgit-server/src/setup.rs — the same strings
 * go into /services/public/install.sh, the overview JSON and git's auth error text).
 * The UI is a client of those recipes, never a fork: where tokens come from and
 * what the installer does are the server's to define.
 *
 * Only `/services/public/*` (the installer) is reachable without a credential;
 * everything else needs a signed-in browser or a bearer token.
 */
export function useRecipes(repo?: string): SetupRecipes {
  return useData(`setup:${repo ?? ""}`, () => api.setupRecipes(repo), Infinity);
}

/** Shared by the Clone dropdown (compact) and the WAL page. `repo` = owner/name. */
export function CloneSetup({ repo, compact = false }: { repo: string; compact?: boolean }) {
  const r = useRecipes(repo);
  const url = `${r.base_url}/${repo}.git`;
  return (
    <div className="clone-setup">
      {r.trust && (
        <>
          <div className="small strong">Once per computer: trust this host's certificate</div>
          <CodeSample code={r.trust} />
        </>
      )}
      <div className="small strong">Clone</div>
      <CodeSample code={r.plain_clone} />
      {r.token_url && (
        <div className="small muted">
          When git asks: Username = your username, Password = an access token (
          <a href={r.token_url} target="_blank" rel="noreferrer">
            create one
          </a>
          ). Git keeps it in its credential store; a rejected token is forgotten and git asks again.
        </div>
      )}
      <div className="small strong">CI — the token from the environment</div>
      <CodeSample code={r.manual_clone} />
      {!compact && (
        <>
          <p className="small muted">
            <strong>Blobless clone</strong> (full history, blobs on demand): keep <code>--sparse</code>: a full checkout
            would ask for every blob of HEAD's tree at once (minutes of server time); add areas with{" "}
            <code>git sparse-checkout add</code>.
          </p>
          <CodeSample code={r.blobless_clone} />
          <p className="small muted">
            <strong>CI / shallow clones</strong>: use <code>--depth=1 --single-branch</code>
            when the job only needs the current branch tip.
          </p>
        </>
      )}
      <div className="small muted">Clone URL</div>
      <CodeSample code={url} />
    </div>
  );
}
