import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { api, client, type Visibility } from "../api";
import { reportError, useData } from "../data";
import { Box } from "../components/Layout";

/** D51: create a repository without an API call or a first push — under your own login or an
 * org you own (a site admin may name any namespace). */
export function NewRepoPage() {
  const me = useData("me", () => api.me(), 60_000);
  const navigate = useNavigate();
  const owners = [me.login, ...me.orgs];
  const [owner, setOwner] = useState(owners[0] ?? "");
  const [name, setName] = useState("");
  const [visibility, setVisibility] = useState<Visibility>("public");
  const [busy, setBusy] = useState(false);
  if (me.anonymous) return <div className="pad muted">Sign in to create a repository.</div>;
  if (!me.can_create && !me.site_admin) return <div className="pad muted">Your account may not create repositories on this host.</div>;
  const valid = /^[A-Za-z0-9_.-]+$/.test(name) && !name.startsWith(".") && owner.length > 0;
  return (
    <>
      <h1 className="page-title">New repository</h1>
      <Box>
        <form
          className="form-grid pad"
          onSubmit={async (e) => {
            e.preventDefault();
            setBusy(true);
            try {
              await client.repo(owner, name).create(undefined, visibility);
              navigate(`/${owner}/${name}`);
            } catch (err) {
              reportError(err, "create repository");
              setBusy(false);
            }
          }}
        >
          <label>
            Owner
            {me.site_admin ? (
              <input list="new-owners" value={owner} onChange={(e) => setOwner(e.target.value.trim())} aria-label="Owner" />
            ) : (
              <select value={owner} onChange={(e) => setOwner(e.target.value)} aria-label="Owner">
                {owners.map((o) => (
                  <option key={o} value={o}>
                    {o}
                  </option>
                ))}
              </select>
            )}
            <datalist id="new-owners">
              {owners.map((o) => (
                <option key={o} value={o} />
              ))}
            </datalist>
          </label>
          <label>
            Repository name
            <input value={name} onChange={(e) => setName(e.target.value.trim())} placeholder="my-project" autoFocus required aria-label="Repository name" />
          </label>
          <fieldset className="row gap">
            <legend className="small">Visibility</legend>
            {(
              [
                ["public", "Public — anyone who can read this host"],
                ["internal", "Internal — any signed-in account"],
                ["private", "Private — you and the people you add"],
              ] as const
            ).map(([v, label]) => (
              <label key={v} className="row gap small">
                <input type="radio" name="visibility" checked={visibility === v} onChange={() => setVisibility(v)} />
                {label}
              </label>
            ))}
          </fieldset>
          <div>
            <button type="submit" className="btn primary" disabled={!valid || busy}>
              Create repository
            </button>
          </div>
          <div className="muted small">
            Then push: <code>git push {location.origin}/{owner || "owner"}/{name || "name"}.git main</code>
          </div>
        </form>
      </Box>
    </>
  );
}
