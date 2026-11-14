import { NavLink, Outlet } from "react-router-dom";

const views = [
  { to: "/dashboard", label: "Overview", end: true },
  { to: "/dashboard/scan", label: "Scan", end: false }
];

/** Dashboard's two views, the overview and the scan it used to be, under one menu entry like the Tracks tabs. */
export function DashboardTabs() {
  return (
    <div className="flex h-full min-h-0 flex-col">
      <nav aria-label="Dashboard views" className="mb-5 flex shrink-0 gap-x-5 shadow-[inset_0_-1px_0_var(--color-border)]">
        {views.map((view) => (
          <NavLink
            key={view.to}
            to={view.to}
            end={view.end}
            className={({ isActive }) => `shrink-0 whitespace-nowrap border-b-2 pb-2 text-sm font-semibold transition ${isActive ? "border-accent text-text" : "border-transparent text-muted hover:text-text"}`}
          >
            {view.label}
          </NavLink>
        ))}
      </nav>
      <div className="min-h-0 flex-1 overflow-auto">
        <Outlet />
      </div>
    </div>
  );
}
