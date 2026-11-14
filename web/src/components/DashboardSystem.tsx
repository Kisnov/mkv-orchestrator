import type { ReactNode } from "react";
import { AlertTriangle, Check, Loader2, Minus, RefreshCw } from "lucide-react";
import { Link } from "react-router-dom";

import type { ToolStatus, WebMediaServer } from "../api";
import { shortToolVersion, type Health, type WatchState } from "../lib/systemHealth";
import { DashboardCard } from "./DashboardCard";
import { rowAction } from "./dashboardStyles";

/** One media server and what the last connection check said. */
export type ServerCheck = {
  server: WebMediaServer;
  health: Health;
  text: string;
  isChecking: boolean;
  check: () => void;
};

function HealthIcon({ health }: { health: Health }) {
  if (health === "ok") return <Check size={14} className="text-success" aria-hidden="true" />;
  if (health === "problem") return <AlertTriangle size={14} className="text-warning" aria-hidden="true" />;
  if (health === "checking") return <Loader2 size={14} className="animate-spin text-accent" aria-hidden="true" />;
  return <Minus size={14} className="text-muted" aria-hidden="true" />;
}

function Row({ health, name, children, action, className = "" }: { health: Health; name: ReactNode; children: ReactNode; action?: ReactNode; className?: string }) {
  return (
    <li className={`grid grid-cols-[0.875rem_minmax(0,1fr)_auto] items-start gap-x-2 text-sm ${className}`}>
      <span className="mt-1"><HealthIcon health={health} /></span>
      <div className="min-w-0">
        <div className="font-medium text-text">{name}</div>
        <div className={["break-words text-xs [overflow-wrap:anywhere]", "text-text"].join(" ")}>{children}</div>
      </div>
      {action}
    </li>
  );
}

function Group({ title, link, children, columns = false }: { title: string; link: { to: string; label: string }; children: ReactNode; columns?: boolean }) {
  return (
    <div className="px-4 py-3">
      <div className="flex items-baseline justify-between gap-3">
        <h3 className="text-xs font-semibold text-muted">{title}</h3>
        <Link to={link.to} className={rowAction}>{link.label}</Link>
      </div>
      <ul className={columns ? "mt-2 grid grid-cols-2 gap-x-4 gap-y-1.5" : "mt-2 space-y-2"}>{children}</ul>
    </div>
  );
}

/** Tool versions, the folder watcher and the media servers, each with the place to fix it. */
export function DashboardSystem({ tools, toolsLoading, watch, servers }: {
  tools: ToolStatus[];
  toolsLoading: boolean;
  watch: WatchState;
  servers: ServerCheck[];
}) {
  return (
    <DashboardCard title="System">
      <div className="divide-y divide-border">
        <Group title="Tools" link={{ to: "/settings?tab=general", label: "Tool settings" }} columns>
          {toolsLoading ? <Row className="col-span-2" health="checking" name="Checking tools">Reading tool versions.</Row> : null}
          {!toolsLoading && tools.length === 0 ? <Row className="col-span-2" health="problem" name="No tools reported">MKVO could not list its tools.</Row> : null}
          {[...tools.filter((tool) => !tool.available), ...tools.filter((tool) => tool.available)].map((tool) => tool.available ? (
            <li key={tool.name} className="flex min-w-0 items-center gap-2 text-sm" title={tool.version}>
              <HealthIcon health="ok" />
              <span className="font-medium text-text">{tool.name}</span>
              <span className="min-w-0 truncate text-xs tabular-nums text-muted"><span className="sr-only">available, version </span>{shortToolVersion(tool.version)}</span>
            </li>
          ) : (
            <Row key={tool.name} className="col-span-2" health="problem" name={tool.name}>{tool.error || "Not found"}</Row>
          ))}
        </Group>
        <Group title="Watch folders" link={{ to: "/settings?tab=library", label: "Watch settings" }}>
          <Row health={watch.health} name={watch.name}>{watch.text}</Row>
        </Group>
        <Group title="Media servers" link={{ to: "/settings?tab=library", label: "Server settings" }}>
          {servers.length === 0 ? <Row health="off" name="None configured">Add Jellyfin, Emby or Plex to browse your library with posters.</Row> : null}
          {servers.map(({ server, health, text, isChecking, check }) => (
            <Row
              key={server.id}
              health={isChecking ? "checking" : health}
              name={<>{server.name} <span className="text-xs font-normal text-muted">{server.type}</span></>}
              action={server.hasApiKey ? (
                <button type="button" onClick={check} disabled={isChecking} aria-label={`Check ${server.name} again`} title="Check again" className="inline-flex h-7 w-7 items-center justify-center rounded-md text-muted transition hover:bg-button-hover hover:text-text disabled:text-disabled pointer-coarse:h-11 pointer-coarse:w-11">
                  <RefreshCw size={14} aria-hidden="true" />
                </button>
              ) : undefined}
            >
              {isChecking ? "Checking the connection." : text}
            </Row>
          ))}
        </Group>
      </div>
    </DashboardCard>
  );
}
