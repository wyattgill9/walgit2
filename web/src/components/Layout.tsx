import { Link, NavLink, Outlet, useLocation } from "react-router-dom";
import { useState, type ReactNode } from "react";
import { RouteBoundary, TopProgress, useBusy } from "./Loading";
import { ErrorTray } from "./ErrorTray";
import { InstanceFooter } from "./InstanceFooter";

export function Layout() {
  const busy = useBusy();
  // On a repo page the API tab pre-fills that repo in the examples.
  const m = /^\/([^/_][^/]*)\/([^/]+)/.exec(useLocation().pathname);
  const apiHref = m && m[1] !== "services" ? `/api?repo=${m[1]}/${m[2]}` : "/api";
  return (
    <>
      <header className="topbar">
        <Link to="/" className="brand">
          walgit/
        </Link>
        <nav className="topnav">
          <NavLink to={apiHref} className={({ isActive }) => (isActive ? "topnav-link active" : "topnav-link")}>
            /api
          </NavLink>
          <ThemeToggle />
        </nav>
      </header>
      <TopProgress />
      <main className="container" aria-busy={busy}>
        <RouteBoundary>
          <Outlet />
        </RouteBoundary>
      </main>
      <ErrorTray />
      <InstanceFooter />
    </>
  );
}

/** Dark is the default; the choice is kept per browser (index.html applies it before paint). */
function ThemeToggle() {
  const [theme, setTheme] = useState(() => document.documentElement.dataset.theme ?? "dark");
  const next = theme === "dark" ? "light" : "dark";
  return (
    <button
      type="button"
      className="topnav-link theme-toggle"
      aria-label={`Switch to ${next} theme`}
      onClick={() => {
        document.documentElement.dataset.theme = next;
        try {
          localStorage.setItem("theme", next);
        } catch {
          // storage blocked (private mode): the choice lasts for this page only
        }
        setTheme(next);
      }}
    >
      /{next}
    </button>
  );
}

export function Box({
  title,
  children,
  className = "",
  id,
}: {
  title?: ReactNode;
  children: ReactNode;
  className?: string;
  id?: string;
}) {
  return (
    <div className={`box ${className}`} id={id}>
      {title && <div className="box-header">{title}</div>}
      {children}
    </div>
  );
}
