import { useEffect, useState } from "react";
import { Link, NavLink, useLocation } from "react-router-dom";
import { client, type Me } from "../api";

/** D51/D52: who you are, in the top bar — `/sign in` when anonymous; else `/new` and a menu.
 * Quiet like the tab counts: fetched on navigation, never holds up or breaks the page. */
export function UserMenu() {
  const { pathname, search } = useLocation();
  const [me, setMe] = useState<Me | null | "signed-out">(null);
  useEffect(() => {
    let live = true;
    client.me().then(
      (m) => live && setMe(m),
      () => live && setMe("signed-out"),
    );
    return () => {
      live = false;
    };
  }, [pathname]);
  if (me === null) return null;
  const next = encodeURIComponent(pathname + search);
  if (me === "signed-out" || me.anonymous) {
    if (me !== "signed-out" && !me.sign_in.available) return null;
    return (
      <>
        {me !== "signed-out" && me.sign_in.passwords === "open" && (
          <a href={`/_auth/signup?next=${next}`} className="topnav-link">
            /sign up
          </a>
        )}
        <a href={`/_auth/login?next=${next}`} className="topnav-link">
          /sign in
        </a>
      </>
    );
  }
  const local = me.auth_mode === "none";
  return (
    <>
      <NavLink to="/new" className={({ isActive }) => (isActive ? "topnav-link active" : "topnav-link")}>
        /new
      </NavLink>
      <details className="user-menu">
        <summary className="topnav-link">@{me.login}</summary>
        <div className="user-pop" role="menu">
          <div className="muted small user-pop-head">
            Signed in as <strong>{me.login}</strong>
            {me.site_admin && " · site admin"}
            {local && " · unauthenticated local mode"}
          </div>
          <Link to={`/${me.login}`} role="menuitem">
            Your repositories
          </Link>
          {me.orgs.map((o) => (
            <Link key={o} to={`/${o}`} role="menuitem">
              {o} (org)
            </Link>
          ))}
          <Link to="/new" role="menuitem">
            New repository
          </Link>
          {me.sign_in.tokens && (
            <a href="/_auth/tokens" role="menuitem">
              Access tokens (git)
            </a>
          )}
          {me.sign_in.passwords !== "off" && !local && (
            <a href="/_auth/password" role="menuitem">
              Change password
            </a>
          )}
          {me.site_admin && !local && (
            <Link to="/admin" role="menuitem">
              Admin
            </Link>
          )}
          {!local && (
            <a href="/_auth/logout" role="menuitem">
              Sign out
            </a>
          )}
        </div>
      </details>
    </>
  );
}
