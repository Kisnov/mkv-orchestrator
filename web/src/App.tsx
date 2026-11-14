import { Component, lazy, Suspense, type ReactNode } from "react";
import { Loader2 } from "lucide-react";
import { Navigate, Outlet, Route, Routes, useLocation } from "react-router-dom";
import AuthGate from "./components/AuthGate";
import StartupGate from "./components/StartupGate";
import { getBackendTransport } from "./api";
import { DashboardTabs } from "./components/DashboardTabs";
import { Layout } from "./components/Layout";
import { MediaLibraryProvider, MediaLibrarySynchronizer } from "./state/MediaLibraryContext";
import { PropEditTemplateWarmer } from "./state/propEditTemplate";
import { OperationJobProvider } from "./state/OperationJobContext";

const DashboardPage = lazy(() => import("./pages/DashboardPage").then((page) => ({ default: page.DashboardPage })));
const ScanPage = lazy(() => import("./pages/ScanPage").then((page) => ({ default: page.ScanPage })));
const LibraryPage = lazy(() => import("./pages/LibraryPage").then((page) => ({ default: page.LibraryPage })));
const LogsPage = lazy(() => import("./pages/LogsPage").then((page) => ({ default: page.LogsPage })));
const MuxRemuxPage = lazy(() => import("./pages/MuxRemuxPage").then((page) => ({ default: page.MuxRemuxPage })));
const RenamePage = lazy(() => import("./pages/RenamePage").then((page) => ({ default: page.RenamePage })));
const SettingsPage = lazy(() => import("./pages/SettingsPage").then((page) => ({ default: page.SettingsPage })));
const TracksPage = lazy(() => import("./pages/TracksPage").then((page) => ({ default: page.TracksPage })));

export default function App() {
  if (getBackendTransport() === "tauri") return <StartupGate><ProtectedApp /></StartupGate>;
  return <AuthGate><ProtectedApp /></AuthGate>;
}

// A tab left open across an update asks for page chunks that no longer exist.
class PageErrorBoundary extends Component<{ path: string; children: ReactNode }, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidUpdate(previous: { path: string }) {
    if (this.state.failed && previous.path !== this.props.path) this.setState({ failed: false });
  }

  render() {
    if (!this.state.failed) return this.props.children;
    return (
      <div className="grid h-full place-items-center px-4">
        <div role="alert" className="w-full max-w-sm rounded-xl border border-border bg-card p-6 shadow-xl">
          <h1 className="font-semibold text-text">This page could not be loaded</h1>
          <p className="mt-2 text-sm text-muted">MKVO may have been updated since this tab was opened.</p>
          <button type="button" onClick={() => window.location.reload()} className="mt-5 w-full rounded-lg bg-accent px-4 py-2.5 font-medium text-black">
            Reload
          </button>
        </div>
      </div>
    );
  }
}

// Keeps the sidebar on screen while a page chunk loads, or fails to.
function LazyPage() {
  const { pathname } = useLocation();
  return (
    <PageErrorBoundary path={pathname}>
      <Suspense fallback={<div className="grid h-full place-items-center"><Loader2 className="size-6 animate-spin text-accent" /></div>}>
        <Outlet />
      </Suspense>
    </PageErrorBoundary>
  );
}

function ProtectedApp() {
  return (
    <MediaLibraryProvider>
      <OperationJobProvider>
      {/* Reads the track layout as soon as a scan lands, so Edit Tracks
          opens with it rather than starting the read on arrival. */}
      <MediaLibrarySynchronizer />
      <PropEditTemplateWarmer />
      <Routes>
        <Route element={<Layout />}>
          <Route element={<LazyPage />}>
            <Route index element={<Navigate to="/dashboard" replace />} />
            <Route element={<DashboardTabs />}>
              <Route path="/dashboard" element={<DashboardPage />} />
              <Route path="/dashboard/scan" element={<ScanPage />} />
            </Route>
            <Route path="/rename" element={<RenamePage />} />
            <Route path="/tracks" element={<Navigate to="/tracks/properties" replace />} />
            <Route path="/tracks/:op" element={<TracksPage />} />
            <Route path="/subtitles" element={<MuxRemuxPage workflow="subtitles" />} />
            {/* Earlier addresses, kept for bookmarks. */}
            <Route path="/scan" element={<Navigate to="/dashboard/scan" replace />} />
            <Route path="/edit-tracks" element={<Navigate to="/tracks/properties" replace />} />
            <Route path="/track-properties" element={<Navigate to="/tracks/properties" replace />} />
            <Route path="/reorder-tracks" element={<Navigate to="/tracks/reorder" replace />} />
            <Route path="/subtitle-format" element={<Navigate to="/tracks/subtitle-format" replace />} />
            <Route path="/remove-tracks" element={<Navigate to="/tracks/remove" replace />} />
            <Route path="/mux-remux" element={<Navigate to="/tracks/remove" replace />} />
            <Route path="/convert-remux" element={<Navigate to="/tracks/convert" replace />} />
            <Route path="/library" element={<LibraryPage />} />
            <Route path="/settings" element={<SettingsPage />} />
            <Route path="/logs" element={<LogsPage />} />
          </Route>
        </Route>
      </Routes>
      </OperationJobProvider>
    </MediaLibraryProvider>
  );
}
