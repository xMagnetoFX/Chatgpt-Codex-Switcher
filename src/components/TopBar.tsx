import type { ReactNode } from "react";

interface TopBarProps {
  title: string;
  heading: string;
  description: string;
  actions?: ReactNode;
  compact?: boolean;
}

export function TopBar({ title, heading, description, actions, compact = false }: TopBarProps) {
  return (
    <header className="app-topbar">
      <div
        className={`app-topbar-inner ${
          actions ? "xl:flex-row xl:items-end xl:justify-between xl:gap-5" : ""
        } flex flex-col gap-3`}
      >
        <div>
          <div className="kicker">{title}</div>
          <h2
            style={{
              margin: 0,
              fontSize: compact ? 22 : 26,
              fontWeight: 600,
              letterSpacing: "-0.01em",
              color: "var(--text-strong)",
            }}
          >
            {heading}
          </h2>
          <p
            style={{
              margin: "6px 0 0",
              fontSize: 13.5,
              color: "var(--text-muted)",
              lineHeight: 1.55,
            }}
          >
            {description}
          </p>
        </div>
        {actions && <div className="flex flex-wrap items-center gap-2">{actions}</div>}
      </div>
    </header>
  );
}
