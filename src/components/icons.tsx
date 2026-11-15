export function Icon({ name, size = 14 }: { name: IconName; size?: number }) {
  const common = {
    width: size,
    height: size,
    viewBox: "0 0 16 16",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 1.4,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
  };
  switch (name) {
    case "search":
      return <svg {...common}><circle cx="6.8" cy="6.8" r="4.3" /><path d="m10 10 3.5 3.5" /></svg>;
    case "server":
      return <svg {...common}><rect x="2" y="2" width="12" height="5" rx="1.5" /><rect x="2" y="9" width="12" height="5" rx="1.5" /><path d="M5 4.5h.01M5 11.5h.01" /></svg>;
    case "inbox":
      return <svg {...common}><path d="m2 9 2-6h8l2 6v4H2zM2 9h4l1 2h2l1-2h4" /></svg>;
    case "grid":
      return (
        <svg {...common}>
          <rect x="2" y="2" width="5" height="5" rx="1.2" />
          <rect x="9" y="2" width="5" height="5" rx="1.2" />
          <rect x="2" y="9" width="5" height="5" rx="1.2" />
          <rect x="9" y="9" width="5" height="5" rx="1.2" />
        </svg>
      );
    case "activity":
      return <svg {...common}><path d="M3 13V9M6.5 13V4M10 13V7M13.5 13V2.5" /></svg>;
    case "queue":
      return (
        <svg {...common}>
          <path d="M3 4h10M3 8h10M3 12h6" />
        </svg>
      );
    case "chevron":
      return (
        <svg {...common}>
          <path d="M6 4l4 4-4 4" />
        </svg>
      );
    case "refresh":
      return (
        <svg {...common}>
          <path d="M13 8a5 5 0 1 1-1.5-3.6M13 2.5V5h-2.5" />
        </svg>
      );
    case "machine":
      return (
        <svg {...common}>
          <rect x="2" y="3" width="12" height="8" rx="1.5" />
          <path d="M6 14h4M8 11v3" />
        </svg>
      );
    case "runner":
      return (
        <svg {...common}>
          <circle cx="8" cy="8" r="5.5" />
          <path d="M7 5.8l2.6 2.2L7 10.2z" fill="currentColor" stroke="none" />
        </svg>
      );
    case "eye-off":
      return (
        <svg {...common}>
          <path d="M2 8s2.2-4 6-4 6 4 6 4-2.2 4-6 4-6-4-6-4z" />
          <path d="M3 13L13 3" />
        </svg>
      );
    case "alert":
      return (
        <svg {...common}>
          <path d="M8 2.5l6 10.5H2z" />
          <path d="M8 7v2.5M8 11.2v.1" />
        </svg>
      );
    case "external":
      return (
        <svg {...common}>
          <path d="M9 3h4v4M13 3L7.5 8.5M11 9.5V13H3V5h3.5" />
        </svg>
      );
    case "settings":
      return (
        <svg {...common}>
          <circle cx="8" cy="8" r="2" />
          <path d="M8 1.8v1.6M8 12.6v1.6M1.8 8h1.6M12.6 8h1.6M3.6 3.6l1.1 1.1M11.3 11.3l1.1 1.1M3.6 12.4l1.1-1.1M11.3 4.7l1.1-1.1" />
        </svg>
      );
    case "check":
      return (
        <svg {...common}>
          <path d="M3.5 8.5l3 3 6-7" />
        </svg>
      );
    case "x":
      return (
        <svg {...common}>
          <path d="M4 4l8 8M12 4l-8 8" />
        </svg>
      );
    case "dash":
      return (
        <svg {...common}>
          <path d="M4.5 8h7" />
        </svg>
      );
    case "plan":
      return (
        <svg {...common}>
          <rect x="3" y="2.5" width="10" height="11" rx="1.5" />
          <path d="M5.5 6h5M5.5 8.5h5M5.5 11h3" />
        </svg>
      );
  }
}

type IconName =
  | "search"
  | "server"
  | "inbox"
  | "grid"
  | "queue"
  | "activity"
  | "chevron"
  | "refresh"
  | "machine"
  | "runner"
  | "eye-off"
  | "alert"
  | "external"
  | "settings"
  | "check"
  | "x"
  | "dash"
  | "plan";
