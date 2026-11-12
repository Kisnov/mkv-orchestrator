import { lazy, Suspense } from "react";
import { Loader2 } from "lucide-react";
import { Navigate, Outlet, Route, Routes } from "react-router-dom";
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

// Keeps the sidebar on screen while a page chunk loads.
function LazyPage() {
  return (
    <Suspense fallback={<div className="grid h-full place-items-center"><Loader2 className="size-6 animate-spin text-accent" /></div>}>
      <Outlet />
    </Suspense>
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
