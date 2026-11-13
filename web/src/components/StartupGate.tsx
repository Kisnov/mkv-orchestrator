import { useEffect, useState, type ReactNode } from "react";
import { Loader2 } from "lucide-react";
import { getStatus } from "../api";
import { Logo } from "./AuthLogo";

/** Desktop counterpart of AuthGate: holds the app until migration and recovery have run, and shows why they failed. */
export default function StartupGate({ children }: { children: ReactNode }) {
  const [started, setStarted] = useState(false);
  const [error, setError] = useState("");
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    let cancelled = false;
    void getStatus().then(() => {
      if (!cancelled) setStarted(true);
    }).catch((cause) => {
      if (!cancelled) setError(cause instanceof Error ? cause.message : "MKVO could not start.");
    });
    return () => { cancelled = true; };
  }, [attempt]);

  if (started) return children;
  if (error) {
    return (
      <main className="grid h-full place-items-center bg-window px-4">
        <div className="w-full max-w-sm rounded-xl border border-border bg-card p-6 shadow-xl">
          <Logo />
          <h1 className="mt-8 font-semibold text-text">MKVO is not responding</h1>
          <p role="alert" className="mt-2 text-sm text-red-400">{error}</p>
          <button type="button" onClick={() => { setError(""); setAttempt((current) => current + 1); }} className="mt-5 w-full rounded-lg bg-accent px-4 py-2.5 font-medium text-black">
            Retry
          </button>
        </div>
      </main>
    );
  }
  return <div className="grid h-full place-items-center"><Loader2 className="size-6 animate-spin text-accent" /></div>;
}
