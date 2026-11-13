import { useOverlayAccessibility } from "./useOverlayAccessibility";
import { useEffect, useRef, useState } from "react";
import { ChevronLeft, ChevronRight, Activity, Captions, Database, FileCog, FolderOpen, ListVideo, Logs, Settings } from "lucide-react";
import { NavLink, Outlet, useLocation } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { getStatus } from "../api";
import mkvoIcon from "../assets/mkvo-icon-purple.png";
import { useMediaLibrary } from "../state/MediaLibraryContext";
import { useOperationJob } from "../state/OperationJobContext";
import { SignOutButton } from "./SignOutButton";

const navItems = [
  { to: "/dashboard", label: "Dashboard", icon: Activity },
  { to: "/rename", label: "Rename Files", icon: ListVideo },
  { to: "/tracks", label: "Tracks", icon: FileCog },
  { to: "/subtitles", label: "Subtitles", icon: Captions },
  { to: "/library", label: "Library", icon: Database },
  { to: "/logs", label: "Logs", icon: Logs },
  { to: "/settings", label: "Settings", icon: Settings }
];

// The mobile breakpoint of index.css, the only layout that uses the table labels.
const mobileLayoutQuery = "(max-width: 767px), (max-width: 1023px) and (pointer: coarse)";

// The mobile layout turns tables into blocks, which drops their semantics and their header row.
function labelTable(table: HTMLTableElement) {
  const headers = [...(table.tHead?.rows[0]?.cells ?? [])].map((cell) => cell.textContent?.trim() ?? "");
  table.classList.add("mobile-data-table");
  table.setAttribute("role", "table");
  for (const body of table.tBodies) for (const row of body.rows) {
    [...row.cells].forEach((cell, index) => { if (cell.colSpan === 1) cell.dataset.label = headers[index] ?? ""; });
  }
}

export function Layout() {
  useOverlayAccessibility();
  const [sidebarTooltip, setSidebarTooltip] = useState<{ label: string; top: number } | null>(null);
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false);
  const location = useLocation();
  useEffect(() => { setMobileMenuOpen(false); }, [location.pathname]);
  const [collapsed, setCollapsed] = useState(() => localStorage.getItem("mkvo.sidebar.collapsed") === null ? window.innerWidth < 1100 : localStorage.getItem("mkvo.sidebar.collapsed") === "true");
  // Narrowing the window folds the sidebar for room; widening it restores the saved choice.
  useEffect(() => {
    if (!window.matchMedia) return;
    const narrow = window.matchMedia("(max-width: 1099px)");
    const sync = () => setCollapsed(narrow.matches || localStorage.getItem("mkvo.sidebar.collapsed") === "true");
    narrow.addEventListener("change", sync);
    return () => narrow.removeEventListener("change", sync);
  }, []);
  const pageContentRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const content = pageContentRef.current;
    if (!content || !window.matchMedia) return;
    const media = window.matchMedia(mobileLayoutQuery);
    // Relabels only the tables a mutation touched.
    const observer = new MutationObserver((records) => {
      const tables = new Set<HTMLTableElement>();
      for (const record of records) {
        const table = (record.target as Element).closest("table");
        if (table) tables.add(table);
        else for (const node of record.addedNodes) {
          if (node instanceof HTMLTableElement) tables.add(node);
          else if (node instanceof Element) node.querySelectorAll("table").forEach((added) => tables.add(added));
        }
      }
      tables.forEach(labelTable);
    });
    const sync = () => {
      observer.disconnect();
      if (!media.matches) return;
      content.querySelectorAll("table").forEach(labelTable);
      observer.observe(content, { childList: true, subtree: true });
    };
    sync();
    media.addEventListener("change", sync);
    return () => { observer.disconnect(); media.removeEventListener("change", sync); };
  }, []);
  const isSettingsPage = useLocation().pathname === "/settings";
  const status = useQuery({ queryKey: ["status"], queryFn: getStatus });
  const missingTools = status.data?.tools.filter((tool) => !tool.available).length ?? 0;
  const { selectionError } = useMediaLibrary();
  const operation = useOperationJob();
  const toggleSidebar = () => setCollapsed((value) => {
    localStorage.setItem("mkvo.sidebar.collapsed", String(!value));
    return !value;
  });

  return (
    <div className="app-shell h-screen overflow-hidden bg-window text-text">
      <div className={`app-shell-grid grid h-screen ${collapsed ? "grid-cols-[4.5rem_minmax(0,1fr)]" : "grid-cols-[14.75rem_minmax(0,1fr)]"}`}>
        <aside className="desktop-navigation relative flex h-screen min-h-0 flex-col border-r border-border bg-sidebar px-3 py-5">
          <button type="button" aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"} data-tooltip={collapsed ? "Expand sidebar" : "Collapse sidebar"} aria-expanded={!collapsed} onClick={toggleSidebar} className="sidebar-collapse-button">
            <span>{collapsed ? <ChevronRight size={12} /> : <ChevronLeft size={12} />}</span>
          </button>
          <div className="mb-8 flex items-center gap-3 px-1">
            <div className="flex h-9 w-9 items-center justify-center">
              <span
                className="h-9 w-9"
                style={{
                  backgroundColor: "var(--color-app-title)",
                  maskImage: `url(${mkvoIcon})`,
                  maskPosition: "center",
                  maskRepeat: "no-repeat",
                  maskSize: "contain",
                  WebkitMaskImage: `url(${mkvoIcon})`,
                  WebkitMaskPosition: "center",
                  WebkitMaskRepeat: "no-repeat",
                  WebkitMaskSize: "contain",
                  filter: "drop-shadow(0 0 0.875rem color-mix(in srgb, var(--color-app-title) 32%, transparent))"
                }}
                aria-hidden="true"
              />
            </div>
            <div className={collapsed ? "hidden" : ""}>
              <div className="text-base font-bold text-app-title">MKV Orchestrator</div>
              <div className="mt-0.5 text-xs text-subtle">Media operations console</div>
            </div>
          </div>

          <nav className="min-h-0 flex-1 overflow-y-auto space-y-1.5">
            {navItems.map((item) => {
              const Icon = item.icon;
              return (
                <NavLink
                  key={item.to}
                  aria-label={item.label}
                  aria-describedby={collapsed && sidebarTooltip?.label === item.label ? "collapsed-sidebar-tooltip" : undefined}
                  to={item.to}
                  onMouseEnter={(event) => collapsed && setSidebarTooltip({ label: item.label, top: event.currentTarget.getBoundingClientRect().top + event.currentTarget.getBoundingClientRect().height / 2 })}
                  onMouseLeave={() => setSidebarTooltip(null)}
                  onFocus={(event) => collapsed && setSidebarTooltip({ label: item.label, top: event.currentTarget.getBoundingClientRect().top + event.currentTarget.getBoundingClientRect().height / 2 })}
                  onBlur={() => setSidebarTooltip(null)}
                  className={({ isActive }) =>
                    [
                      "flex h-9 items-center gap-3 rounded-md px-3 text-sm font-medium transition",
                      isActive
                        ? "bg-selected text-text shadow-[inset_3px_0_0_var(--color-accent)]"
                        : "text-muted hover:bg-input-hover hover:text-text"
                    ].join(" ")
                  }
                >
                  <Icon size={16} />
                  <span className={collapsed ? "sr-only" : ""}>{item.label}</span>
                </NavLink>
              );
            })}
          </nav>

          <div className={`mt-auto ${collapsed ? "hidden" : ""}`}>
          <SignOutButton className="mb-3 hidden md:block" />
          <div className="min-w-0 overflow-hidden rounded-lg border border-border bg-panel p-3">
            <div className="text-[0.6875rem] font-semibold uppercase tracking-wide text-subtle">Status</div>
            <div className={["mt-2 min-w-0 break-words text-sm font-medium [overflow-wrap:anywhere]", operation.isRunning ? "text-accent" : "text-success"].join(" ")} title={operation.statusText ?? undefined}>
              {operation.statusText ?? (status.isLoading ? "checking tools" : missingTools === 0 ? "ready" : `${missingTools} tool issue(s)`)}
            </div>
            <div className="mt-3 flex items-center gap-2 truncate text-xs text-muted">
              <FolderOpen size={14} />
              <span className="truncate">{status.data?.mediaRoot ?? "/media"}</span>
            </div>
          </div>
          </div>
        </aside>
        {collapsed && sidebarTooltip ? (
          <div id="collapsed-sidebar-tooltip" role="tooltip" className="collapsed-sidebar-tooltip" style={{ top: sidebarTooltip.top }}>
            {sidebarTooltip.label}
          </div>
        ) : null}

        <div className="mobile-navigation">
          <div className="flex min-h-14 items-center justify-between gap-2 px-4">
            <span className="font-bold text-app-title">MKV Orchestrator</span>
            <button type="button" aria-expanded={mobileMenuOpen} aria-controls="mobile-navigation-menu" onClick={() => setMobileMenuOpen((open) => !open)} className="rounded-md border border-border px-3">{mobileMenuOpen ? "Close Menu" : "Menu"}</button>
          </div>
          {mobileMenuOpen ? <nav onKeyDown={(event) => { if (event.key === "Escape") { setMobileMenuOpen(false); document.querySelector<HTMLButtonElement>('[aria-controls="mobile-navigation-menu"]')?.focus(); } }} id="mobile-navigation-menu" aria-label="Mobile navigation" className="grid grid-cols-2 gap-2 border-t border-border p-4">
            {navItems.map((item) => <NavLink key={item.to} to={item.to} className={({ isActive }) => `flex items-center gap-2 rounded-md px-2 py-2 text-sm ${isActive ? "bg-selected text-accent" : "text-muted"}`}><item.icon size={16} /><span>{item.label}</span></NavLink>)}
            <SignOutButton className="col-span-2" />
          </nav> : null}
          <div role="status" className="break-words border-t border-border px-4 py-2 text-xs text-muted">{operation.statusText ?? (missingTools ? `${missingTools} tool issue(s)` : "Ready")}</div>
        </div>
        <main className={`app-main flex min-h-0 min-w-0 flex-col overflow-hidden ${isSettingsPage ? "px-4 py-4 sm:px-6 lg:px-8" : "px-4 py-4 lg:px-8 lg:py-8"}`}>
          <header className={`desktop-signout shrink-0 ${collapsed ? "" : "md:hidden"}`}><SignOutButton className="mb-3" /></header>
          {selectionError ? (
            <div role="alert" className="mb-3 shrink-0 rounded-md border border-warning bg-panel px-4 py-2 text-sm text-warning">
              Selection sync failed: {selectionError}
            </div>
          ) : null}
          <div ref={pageContentRef} className="page-content min-h-0 min-w-0 flex-1 overflow-auto">
            <Outlet />
          </div>
        </main>
      </div>
    </div>
  );
}
