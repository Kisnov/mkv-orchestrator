import { Component, lazy, Suspense, type ReactNode } from "react";
import { Loader2 } from "lucide-react";
import { Navigate, Outlet, Route, Routes, useLocation } from "react-router-dom";
import AuthGate from "./components/AuthGate";
import { getBackendTransport } from "./api";
import { Layout } from "./components/Layout";
import { MediaLibraryProvider, MediaLibrarySynchronizer } from "./state/MediaLibraryContext";
import { PropEditTemplateWarmer } from "./state/propEditTemplate";
import { OperationJobProvider } from "./state/OperationJobContext";

const DashboardPage = lazy(() => import("./pages/DashboardPage").then((page) => ({ default: page.DashboardPage })));
const LibraryPage = lazy(() => import("./pages/LibraryPage").then((page) => ({ default: page.LibraryPage })));
const LogsPage = lazy(() => import("./pages/LogsPage").then((page) => ({ default: page.LogsPage })));
const MuxRemuxPage = lazy(() => import("./pages/MuxRemuxPage").then((page) => ({ default: page.MuxRemuxPage })));
const RenamePage = lazy(() => import("./pages/RenamePage").then((page) => ({ default: page.RenamePage })));
const ReorderTracksPage = lazy(() => import("./pages/ReorderTracksPage").then((page) => ({ default: page.ReorderTracksPage })));
const SubtitleFormatPage = lazy(() => import("./pages/SubtitleFormatPage").then((page) => ({ default: page.SubtitleFormatPage })));
const SettingsPage = lazy(() => import("./pages/SettingsPage").then((page) => ({ default: page.SettingsPage })));
const TrackPropertiesPage = lazy(() => import("./pages/TrackPropertiesPage").then((page) => ({ default: page.TrackPropertiesPage })));

export default function App() {
  if (getBackendTransport() === "tauri") return <ProtectedApp />;
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
            <Route path="/dashboard" element={<DashboardPage />} />
            <Route path="/rename" element={<RenamePage />} />
            <Route path="/remove-tracks" element={<MuxRemuxPage workflow="remove" />} />
            <Route path="/reorder-tracks" element={<ReorderTracksPage />} />
            <Route path="/subtitle-format" element={<SubtitleFormatPage />} />
            <Route path="/subtitles" element={<MuxRemuxPage workflow="subtitles" />} />
            <Route path="/convert-remux" element={<MuxRemuxPage workflow="convert" />} />
            <Route path="/edit-tracks" element={<TrackPropertiesPage />} />
            <Route path="/mux-remux" element={<Navigate to="/remove-tracks" replace />} />
            <Route path="/track-properties" element={<Navigate to="/edit-tracks" replace />} />
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
