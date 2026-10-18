import { useState } from "react";
import { api } from "../api";
import { useData } from "../data";
import { Box } from "../components/Layout";

/** `/settings`: your account, how you sign in, and how walgit looks. */
export function UserSettingsPage() {
  const me = useData("me", () => api.me(), 60_000);
  const local = me.auth_mode === "none";
  const passwords = me.sign_in.passwords !== "off" && !local;
  return (
    <>
      <h1 className="page-title">Settings</h1>
      {!me.anonymous && (
        <Box title="Account">
          <ul className="list">
            <li>
              <span className="muted">Username</span> <strong>{me.login}</strong>
              {me.site_admin && <span className="muted small"> · site admin</span>}
            </li>
            {me.name && (
              <li>
                <span className="muted">Name</span> {me.name}
              </li>
            )}
            {me.email && (
              <li>
                <span className="muted">Email</span> {me.email}
              </li>
            )}
            {local && <li className="muted small">Unauthenticated local mode: everyone on this machine is {me.login}.</li>}
          </ul>
        </Box>
      )}
      {(passwords || me.sign_in.tokens) && (
        <Box title="Sign-in and git">
          <ul className="list">
            {passwords && (
              <li>
                <a href="/_auth/password">Change password</a>
              </li>
            )}
            {me.sign_in.tokens && (
              <li>
                <a href="/_auth/tokens">Access tokens</a> <span className="muted small">for git and scripts</span>
              </li>
            )}
          </ul>
        </Box>
      )}
      <Box title="Appearance">
        <div className="pad row gap">
          <span className="muted">Theme</span>
          <ThemePicker />
          <span className="muted small">Kept in this browser.</span>
        </div>
      </Box>
    </>
  );
}

/** Dark is the default; `index.html` (and the `/_auth/*` pages) apply the saved choice before paint. */
function ThemePicker() {
  const [theme, setTheme] = useState(() => document.documentElement.dataset.theme ?? "dark");
  const pick = (t: "dark" | "light") => {
    document.documentElement.dataset.theme = t;
    try {
      localStorage.setItem("theme", t);
    } catch {
      // storage blocked (private mode): the choice lasts for this page only
    }
    setTheme(t);
  };
  return (
    <span className="seg">
      {(["dark", "light"] as const).map((t) => (
        <button key={t} type="button" className={theme === t ? "active" : ""} aria-pressed={theme === t} onClick={() => pick(t)}>
          {t}
        </button>
      ))}
    </span>
  );
}
