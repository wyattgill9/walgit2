import { Link, NavLink, Outlet, useLocation } from "react-router-dom";
import type { ReactNode } from "react";
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
          walgit<span className="c">_</span>
        </Link>
        <nav className="topnav">
          <NavLink to={apiHref} className={({ isActive }) => (isActive ? "topnav-link active" : "topnav-link")}>
            /api
          </NavLink>
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
