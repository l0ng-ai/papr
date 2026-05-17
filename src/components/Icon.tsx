// Minimal stroke icon set — ported 1:1 from the design prototype (icons.jsx).

const STROKE = 1.6;

export type IconName =
  | "inbox" | "circle" | "unread" | "star" | "star-fill" | "bookmark"
  | "bookmark-fill" | "clock" | "tag" | "folder" | "rss" | "search"
  | "plus" | "check" | "check-all" | "sort" | "sparkle" | "sparkle-fill"
  | "open" | "share" | "more" | "refresh" | "settings" | "chevron-down"
  | "chevron-right" | "globe" | "focus" | "arrow-down" | "arrow-up"
  | "eye" | "eye-off" | "trash" | "mute" | "pin" | "x" | "command"
  | "copy" | "list" | "grid" | "text" | "alert" | "papr"
  | "play" | "pause" | "skip-back" | "skip-fwd" | "headphones";

interface Props {
  name: IconName;
  size?: number;
  color?: string;
  className?: string;
}

export default function Icon({
  name,
  size = 16,
  color = "currentColor",
  className,
}: Props) {
  // Icons are decorative — every place that renders one also supplies a
  // text label or an aria-label, so hide the SVG from assistive tech.
  const p = {
    width: size,
    height: size,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: color,
    strokeWidth: STROKE,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
    className,
    "aria-hidden": true,
  };
  const filled = {
    width: size,
    height: size,
    viewBox: "0 0 24 24",
    fill: color,
    className,
    "aria-hidden": true,
  };

  switch (name) {
    case "inbox":
      return <svg {...p}><path d="M3 12l3-7h12l3 7M3 12v6a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-6M3 12h5l2 3h4l2-3h5" /></svg>;
    case "circle":
      return <svg {...p}><circle cx="12" cy="12" r="8" /></svg>;
    case "unread":
      return <svg {...p}><circle cx="12" cy="12" r="8" /><circle cx="12" cy="12" r="3.2" fill={color} stroke="none" /></svg>;
    case "star":
      return <svg {...p}><path d="M12 3.5l2.7 5.4 6 .9-4.3 4.2 1 6-5.4-2.8L6.6 20l1-6L3.3 9.8l6-.9z" /></svg>;
    case "star-fill":
      return <svg {...filled}><path d="M12 3.5l2.7 5.4 6 .9-4.3 4.2 1 6-5.4-2.8L6.6 20l1-6L3.3 9.8l6-.9z" /></svg>;
    case "bookmark":
      return <svg {...p}><path d="M6 4h12v17l-6-3.5L6 21z" /></svg>;
    case "bookmark-fill":
      return <svg {...filled}><path d="M6 4h12v17l-6-3.5L6 21z" /></svg>;
    case "clock":
      return <svg {...p}><circle cx="12" cy="12" r="8" /><path d="M12 7.5V12l3 2" /></svg>;
    case "tag":
      return <svg {...p}><path d="M3 12V4h8l10 10-8 8-10-10z" /><circle cx="7.5" cy="7.5" r="1.2" fill={color} stroke="none" /></svg>;
    case "folder":
      return <svg {...p}><path d="M3 6a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" /></svg>;
    case "rss":
      return <svg {...p}><path d="M4 11a9 9 0 0 1 9 9M4 4a16 16 0 0 1 16 16" /><circle cx="5" cy="19" r="1.5" fill={color} stroke="none" /></svg>;
    case "search":
      return <svg {...p}><circle cx="11" cy="11" r="7" /><path d="M16 16l4 4" /></svg>;
    case "plus":
      return <svg {...p}><path d="M12 5v14M5 12h14" /></svg>;
    case "check":
      return <svg {...p}><path d="M4 12l5 5L20 6" /></svg>;
    case "check-all":
      return <svg {...p}><path d="M3 12l4 4 8-8M11 16l5 5L24 11" /></svg>;
    case "sort":
      return <svg {...p}><path d="M7 4v16M4 7l3-3 3 3M17 20V4M14 17l3 3 3-3" /></svg>;
    case "sparkle":
      return <svg {...p}><path d="M12 4l1.7 4.3L18 10l-4.3 1.7L12 16l-1.7-4.3L6 10l4.3-1.7zM19 4.5l.7 1.8L21.5 7l-1.8.7L19 9.5l-.7-1.8L16.5 7l1.8-.7zM6 16l.7 1.8L8.5 18.5l-1.8.7L6 21l-.7-1.8L3.5 18.5l1.8-.7z" /></svg>;
    case "sparkle-fill":
      return <svg {...filled}><path d="M12 4l1.7 4.3L18 10l-4.3 1.7L12 16l-1.7-4.3L6 10l4.3-1.7zM19 4.5l.7 1.8L21.5 7l-1.8.7L19 9.5l-.7-1.8L16.5 7l1.8-.7zM6 16l.7 1.8L8.5 18.5l-1.8.7L6 21l-.7-1.8L3.5 18.5l1.8-.7z" /></svg>;
    case "open":
      return <svg {...p}><path d="M14 4h6v6M20 4l-8 8M19 13v5a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h5" /></svg>;
    case "share":
      return <svg {...p}><path d="M12 4v12M12 4l-4 4M12 4l4 4M5 14v4a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-4" /></svg>;
    case "more":
      return <svg {...p}><circle cx="5" cy="12" r="1.2" fill={color} stroke="none" /><circle cx="12" cy="12" r="1.2" fill={color} stroke="none" /><circle cx="19" cy="12" r="1.2" fill={color} stroke="none" /></svg>;
    case "refresh":
      return <svg {...p}><path d="M20 8a8 8 0 0 0-14 0M20 4v4h-4M4 16a8 8 0 0 0 14 0M4 20v-4h4" /></svg>;
    case "settings":
      return <svg {...p}><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" /></svg>;
    case "chevron-down":
      return <svg {...p}><path d="M6 9l6 6 6-6" /></svg>;
    case "chevron-right":
      return <svg {...p}><path d="M9 6l6 6-6 6" /></svg>;
    case "globe":
      return <svg {...p}><circle cx="12" cy="12" r="9" /><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18" /></svg>;
    case "focus":
      return <svg {...p}><path d="M4 9V5h4M20 9V5h-4M4 15v4h4M20 15v4h-4" /></svg>;
    case "arrow-down":
      return <svg {...p}><path d="M12 5v14M5 12l7 7 7-7" /></svg>;
    case "arrow-up":
      return <svg {...p}><path d="M12 19V5M5 12l7-7 7 7" /></svg>;
    case "eye":
      return <svg {...p}><path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z" /><circle cx="12" cy="12" r="3" /></svg>;
    case "eye-off":
      return <svg {...p}><path d="M3 3l18 18M10.6 6.2A10.5 10.5 0 0 1 22 12s-1.4 2.6-4 4.5M6.4 7.4C3.6 9.2 2 12 2 12s3.5 7 10 7c2 0 3.7-.6 5.2-1.6M9.9 9.9a3 3 0 0 0 4.2 4.2" /></svg>;
    case "trash":
      return <svg {...p}><path d="M4 7h16M9 7V4h6v3M6 7l1 13a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-13" /></svg>;
    case "mute":
      return <svg {...p}><path d="M11 5L6 9H3v6h3l5 4zM16 9l4 6M20 9l-4 6" /></svg>;
    case "pin":
      return <svg {...p}><path d="M12 2l3 6 5 1-4 4 1 6-5-3-5 3 1-6-4-4 5-1z" /></svg>;
    case "x":
      return <svg {...p}><path d="M6 6l12 12M18 6L6 18" /></svg>;
    case "command":
      return <svg {...p}><path d="M18 6a3 3 0 1 0-3 3h3v6h-3a3 3 0 1 0 3 3v-3H9v3a3 3 0 1 0 3-3V9H9V6a3 3 0 1 0-3 3" /></svg>;
    case "copy":
      return <svg {...p}><rect x="9" y="9" width="11" height="11" rx="2" /><path d="M5 15V5a2 2 0 0 1 2-2h10" /></svg>;
    case "list":
      return <svg {...p}><path d="M4 6h16M4 12h16M4 18h16" /></svg>;
    case "grid":
      return <svg {...p}><rect x="3" y="3" width="8" height="8" rx="1.5" /><rect x="13" y="3" width="8" height="8" rx="1.5" /><rect x="3" y="13" width="8" height="8" rx="1.5" /><rect x="13" y="13" width="8" height="8" rx="1.5" /></svg>;
    case "text":
      return <svg {...p}><path d="M5 6h14M5 6V4.5M19 6V4.5M12 6v14M9 20h6" /></svg>;
    case "alert":
      return <svg {...p}><path d="M12 4L2.5 20.5h19zM12 10v5" /><circle cx="12" cy="17.6" r="0.4" fill={color} stroke="none" /></svg>;
    case "play":
      return <svg {...filled}><path d="M7 4.5v15l13-7.5z" /></svg>;
    case "pause":
      return <svg {...filled}><rect x="6" y="4.5" width="4.2" height="15" rx="1" /><rect x="13.8" y="4.5" width="4.2" height="15" rx="1" /></svg>;
    case "skip-back":
      return <svg {...p}><path d="M11 6a8 8 0 1 1-3 6" /><path d="M8 4v6h6" /></svg>;
    case "skip-fwd":
      return <svg {...p}><path d="M13 6a8 8 0 1 0 3 6" /><path d="M16 4v6h-6" /></svg>;
    case "headphones":
      return <svg {...p}><path d="M4 14v-2a8 8 0 0 1 16 0v2" /><rect x="2.5" y="13" width="4.5" height="7" rx="2" /><rect x="17" y="13" width="4.5" height="7" rx="2" /></svg>;
    case "papr":
      return <svg {...p}><path d="M5 3h8.5L19 8.5V21H5Z" /><path d="M13.5 3v5.5H19" /><path d="M8 13.5a3.5 3.5 0 0 1 3.5 3.5M8 11a6 6 0 0 1 6 6" /><circle cx="8" cy="17" r="1.15" fill={color} stroke="none" /></svg>;
    default:
      return null;
  }
}
