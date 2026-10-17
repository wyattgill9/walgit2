import { useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { api, client } from "../api";
import { invalidate, reportError, useData } from "../data";
import { Box } from "../components/Layout";

/** D51/D52, site admins: accounts and orgs (claiming a namespace that has repositories but no
 * account is creating an org with that login). */
export function AdminPage() {
  const me = useData("me", () => api.me(), 60_000);
  const users = useData("admin:users", () => api.users(""), 10_000);
  if (!me.site_admin) return <div className="pad muted">Only site admins see this page.</div>;
  const passwords = me.sign_in.passwords !== "off";
  return (
    <>
      <h1 className="page-title">Admin</h1>
      <Box title={`Accounts (${users.length}${users.length >= 20 ? "+" : ""})`}>
        <ul className="list">
          {users.map((u) => (
            <li key={u.login}>
              <Link to={`/${u.login}`} className="strong">
                {u.login}
              </Link>
              {u.name && <span className="muted small"> {u.name}</span>}
            </li>
          ))}
        </ul>
      </Box>
      <CreateUser passwords={passwords} />
      {passwords && <ResetPassword />}
      <CreateOrg />
    </>
  );
}

/** One small form: `fields` named inputs, `run` with their values; resets on success. */
function MiniForm({ title, fields, submit, run }: { title: string; fields: [string, string, string?][]; submit: string; run: (get: (k: string) => string) => Promise<unknown> }) {
  const empty = Object.fromEntries(fields.map(([k]) => [k, ""]));
  const [v, setV] = useState<Record<string, string>>(empty);
  const [done, setDone] = useState("");
  const onSubmit = async (e: FormEvent) => {
    e.preventDefault();
    setDone("");
    try {
      await run((k) => v[k] ?? "");
      setV(empty);
      setDone("Done.");
    } catch (err) {
      reportError(err, title.toLowerCase());
    }
  };
  return (
    <Box title={title}>
      <form className="form-grid pad" onSubmit={(e) => void onSubmit(e)}>
        {fields.map(([k, label, type]) => (
          <label key={k}>
            {label}
            <input type={type ?? "text"} value={v[k] ?? ""} onChange={(e) => setV({ ...v, [k]: e.target.value })} aria-label={label} />
          </label>
        ))}
        <div className="row gap">
          <button type="submit" className="btn primary">
            {submit}
          </button>
          <span className="muted small">{done}</span>
        </div>
      </form>
    </Box>
  );
}

function CreateUser({ passwords }: { passwords: boolean }) {
  const fields: [string, string, string?][] = [
    ["login", "Username"],
    ["email", "Email (optional)", "email"],
  ];
  if (passwords) fields.push(["password", "Password (optional: none = a robot for tokens)", "password"]);
  return (
    <MiniForm
      title="Create an account"
      fields={fields}
      submit="Create account"
      run={async (get) => {
        await client.createUser({ login: get("login").trim(), email: get("email").trim() || undefined, password: get("password") || undefined });
        invalidate("admin:users");
      }}
    />
  );
}

function ResetPassword() {
  return (
    <MiniForm
      title="Set someone's password"
      fields={[
        ["login", "Username"],
        ["password", "New password", "password"],
      ]}
      submit="Set password"
      run={(get) => client.setPassword(get("login").trim(), get("password"))}
    />
  );
}

function CreateOrg() {
  return (
    <MiniForm
      title="Create an organization (or claim a namespace that has repositories)"
      fields={[
        ["login", "Organization name"],
        ["owners", "Owners (usernames, comma-separated)"],
      ]}
      submit="Create organization"
      run={(get) =>
        client.createOrg({
          login: get("login").trim(),
          owners: get("owners")
            .split(",")
            .map((s) => s.trim())
            .filter(Boolean),
        })
      }
    />
  );
}
