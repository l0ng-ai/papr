// Rounded stroke icon set — redesigned on a consistent 24×24 grid for a
// smoother, more modern, editorial feel. Geometry follows the Lucide
// convention (round caps/joins, generous corner radii, optically balanced).

const STROKE = 1.75;

export type IconName =
  | "inbox" | "circle" | "unread" | "star" | "star-fill" | "bookmark"
  | "bookmark-fill" | "clock" | "tag" | "folder" | "rss" | "search"
  | "plus" | "check" | "check-all" | "sort" | "sparkle" | "sparkle-fill"
  | "open" | "share" | "send" | "more" | "refresh" | "settings" | "chevron-down"
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
      return <svg {...p}><path d="M22 12h-5l-2 3h-6l-2-3H2" /><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z" /></svg>;
    case "circle":
      return <svg {...p}><circle cx="12" cy="12" r="9" /></svg>;
    case "unread":
      return <svg {...p}><circle cx="12" cy="12" r="9" /><circle cx="12" cy="12" r="3.4" fill={color} stroke="none" /></svg>;
    case "star":
      return <svg {...p}><path d="M11.27 3.16a.82.82 0 0 1 1.46 0l2.3 4.66a.82.82 0 0 0 .62.45l5.14.75a.82.82 0 0 1 .45 1.4l-3.72 3.62a.82.82 0 0 0-.24.73l.88 5.12a.82.82 0 0 1-1.19.86l-4.6-2.42a.82.82 0 0 0-.76 0l-4.6 2.42a.82.82 0 0 1-1.19-.86l.88-5.12a.82.82 0 0 0-.24-.73L2.7 10.42a.82.82 0 0 1 .45-1.4l5.14-.75a.82.82 0 0 0 .62-.45z" /></svg>;
    case "star-fill":
      return <svg {...filled}><path d="M11.27 3.16a.82.82 0 0 1 1.46 0l2.3 4.66a.82.82 0 0 0 .62.45l5.14.75a.82.82 0 0 1 .45 1.4l-3.72 3.62a.82.82 0 0 0-.24.73l.88 5.12a.82.82 0 0 1-1.19.86l-4.6-2.42a.82.82 0 0 0-.76 0l-4.6 2.42a.82.82 0 0 1-1.19-.86l.88-5.12a.82.82 0 0 0-.24-.73L2.7 10.42a.82.82 0 0 1 .45-1.4l5.14-.75a.82.82 0 0 0 .62-.45z" /></svg>;
    case "bookmark":
      return <svg {...p}><path d="M18 21l-6-4.2L6 21V6a3 3 0 0 1 3-3h6a3 3 0 0 1 3 3z" /></svg>;
    case "bookmark-fill":
      return <svg {...filled}><path d="M18 21l-6-4.2L6 21V6a3 3 0 0 1 3-3h6a3 3 0 0 1 3 3z" /></svg>;
    case "clock":
      return <svg {...p}><circle cx="12" cy="12" r="9" /><path d="M12 7v5l3.5 2" /></svg>;
    case "tag":
      return <svg {...p}><path d="M12.59 2.59A2 2 0 0 0 11.17 2H5a3 3 0 0 0-3 3v6.17a2 2 0 0 0 .59 1.42l8.3 8.3a2.4 2.4 0 0 0 3.4 0l6.4-6.4a2.4 2.4 0 0 0 0-3.4z" /><circle cx="7.5" cy="7.5" r="1.1" fill={color} stroke="none" /></svg>;
    case "folder":
      return <svg {...p}><path d="M20 20a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2h-7.5a2 2 0 0 1-1.6-.8l-1-1.4A2 2 0 0 0 8.3 4H4a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2z" /></svg>;
    case "rss":
      return <svg {...p}><path d="M4 11a9 9 0 0 1 9 9M4 4a16 16 0 0 1 16 16" /><circle cx="5" cy="19" r="1.4" fill={color} stroke="none" /></svg>;
    case "search":
      return <svg {...p}><circle cx="11" cy="11" r="7.5" /><path d="m20 20-4.3-4.3" /></svg>;
    case "plus":
      return <svg {...p}><path d="M12 5v14M5 12h14" /></svg>;
    case "check":
      return <svg {...p}><path d="M20 6 9 17l-5-5" /></svg>;
    case "check-all":
      return <svg {...p}><path d="M18 6 7 17l-5-5M22 10l-7.5 7.5L13 16" /></svg>;
    case "sort":
      return <svg {...p}><path d="M7 4v16M3.5 7.5 7 4l3.5 3.5M17 20V4M13.5 16.5 17 20l3.5-3.5" /></svg>;
    case "sparkle":
      return <svg {...p}><path d="M11.4 2.6a.65.65 0 0 1 1.2 0l1.55 4.13a2 2 0 0 0 1.17 1.17l4.13 1.55a.65.65 0 0 1 0 1.2l-4.13 1.55a2 2 0 0 0-1.17 1.17l-1.55 4.13a.65.65 0 0 1-1.2 0l-1.55-4.13a2 2 0 0 0-1.17-1.17L4.55 11.6a.65.65 0 0 1 0-1.2l4.13-1.55a2 2 0 0 0 1.17-1.17z" /><path d="M19 14v3M20.5 15.5h-3M5 4v3M6.5 5.5h-3" /></svg>;
    case "sparkle-fill":
      return <svg {...p}><path d="M11.4 2.6a.65.65 0 0 1 1.2 0l1.55 4.13a2 2 0 0 0 1.17 1.17l4.13 1.55a.65.65 0 0 1 0 1.2l-4.13 1.55a2 2 0 0 0-1.17 1.17l-1.55 4.13a.65.65 0 0 1-1.2 0l-1.55-4.13a2 2 0 0 0-1.17-1.17L4.55 11.6a.65.65 0 0 1 0-1.2l4.13-1.55a2 2 0 0 0 1.17-1.17z" fill={color} stroke="none" /><path d="M19 14v3M20.5 15.5h-3M5 4v3M6.5 5.5h-3" /></svg>;
    case "open":
      return <svg {...p}><path d="M15 3h6v6M21 3l-9 9M18 13v5a3 3 0 0 1-3 3H6a3 3 0 0 1-3-3V9a3 3 0 0 1 3-3h5" /></svg>;
    case "share":
      return <svg {...p}><path d="M12 15V3M8.5 6.5 12 3l3.5 3.5M5 13v5a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-5" /></svg>;
    case "send":
      return <svg {...p}><path d="M21.5 2.5 10.5 13.5M21.5 2.5l-7 19-4-8.5-8.5-4z" /></svg>;
    case "more":
      return <svg {...p}><circle cx="5" cy="12" r="1.3" fill={color} stroke="none" /><circle cx="12" cy="12" r="1.3" fill={color} stroke="none" /><circle cx="19" cy="12" r="1.3" fill={color} stroke="none" /></svg>;
    case "refresh":
      return <svg {...p}><path d="M3 12a9 9 0 0 1 15.3-6.4L21 8M21 3v5h-5M21 12a9 9 0 0 1-15.3 6.4L3 16M3 21v-5h5" /></svg>;
    case "settings":
      return <svg {...p}><circle cx="12" cy="12" r="3" /><path d="M12.2 2h-.4a2 2 0 0 0-2 2v.2a2 2 0 0 1-1 1.7l-.4.3a2 2 0 0 1-2 0l-.2-.1a2 2 0 0 0-2.7.7l-.2.4a2 2 0 0 0 .7 2.7l.2.1a2 2 0 0 1 1 1.7v.5a2 2 0 0 1-1 1.7l-.2.1a2 2 0 0 0-.7 2.7l.2.4a2 2 0 0 0 2.7.7l.2-.1a2 2 0 0 1 2 0l.4.3a2 2 0 0 1 1 1.7V20a2 2 0 0 0 2 2h.4a2 2 0 0 0 2-2v-.2a2 2 0 0 1 1-1.7l.4-.3a2 2 0 0 1 2 0l.2.1a2 2 0 0 0 2.7-.7l.2-.4a2 2 0 0 0-.7-2.7l-.2-.1a2 2 0 0 1-1-1.7v-.5a2 2 0 0 1 1-1.7l.2-.1a2 2 0 0 0 .7-2.7l-.2-.4a2 2 0 0 0-2.7-.7l-.2.1a2 2 0 0 1-2 0l-.4-.3a2 2 0 0 1-1-1.7V4a2 2 0 0 0-2-2z" /></svg>;
    case "chevron-down":
      return <svg {...p}><path d="m6 9 6 6 6-6" /></svg>;
    case "chevron-right":
      return <svg {...p}><path d="m9 18 6-6-6-6" /></svg>;
    case "globe":
      return <svg {...p}><circle cx="12" cy="12" r="9" /><path d="M3 12h18M12 3a13 13 0 0 1 0 18M12 3a13 13 0 0 0 0 18" /></svg>;
    case "focus":
      return <svg {...p}><path d="M4 8V6a2 2 0 0 1 2-2h2M16 4h2a2 2 0 0 1 2 2v2M20 16v2a2 2 0 0 1-2 2h-2M8 20H6a2 2 0 0 1-2-2v-2" /></svg>;
    case "arrow-down":
      return <svg {...p}><path d="M12 5v14M19 12l-7 7-7-7" /></svg>;
    case "arrow-up":
      return <svg {...p}><path d="M12 19V5M5 12l7-7 7 7" /></svg>;
    case "eye":
      return <svg {...p}><path d="M2.5 12a10 10 0 0 1 19 0 10 10 0 0 1-19 0z" /><circle cx="12" cy="12" r="3" /></svg>;
    case "eye-off":
      return <svg {...p}><path d="M10.7 5.1A10 10 0 0 1 21.5 12a10.2 10.2 0 0 1-1.6 2.6M6.5 7.4A10 10 0 0 0 2.5 12a10 10 0 0 0 13 5.4M9.9 9.9a3 3 0 0 0 4.2 4.2" /><path d="m3 3 18 18" /></svg>;
    case "trash":
      return <svg {...p}><path d="M3 6h18M19 6v13a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M10 11v5M14 11v5" /></svg>;
    case "mute":
      return <svg {...p}><path d="M11 5 6 9H3a1 1 0 0 0-1 1v4a1 1 0 0 0 1 1h3l5 4z" /><path d="m16 9 5 6M21 9l-5 6" /></svg>;
    case "pin":
      return <svg {...p}><path d="M12 17v5M9 10.8a2 2 0 0 1-1.1 1.8l-1.8.9A2 2 0 0 0 5 15.2v.8a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.8a2 2 0 0 0-1.1-1.7l-1.8-.9a2 2 0 0 1-1.1-1.8V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z" /></svg>;
    case "x":
      return <svg {...p}><path d="M18 6 6 18M6 6l12 12" /></svg>;
    case "command":
      return <svg {...p}><path d="M15 6v12a3 3 0 1 0 3-3H6a3 3 0 1 0 3 3V6a3 3 0 1 0-3 3h12a3 3 0 1 0-3-3" /></svg>;
    case "copy":
      return <svg {...p}><rect x="8" y="8" width="13" height="13" rx="2.5" /><path d="M4 16a2 2 0 0 1-2-2V5a3 3 0 0 1 3-3h9a2 2 0 0 1 2 2" /></svg>;
    case "list":
      return <svg {...p}><path d="M8 6h13M8 12h13M8 18h13" /><circle cx="3.5" cy="6" r="1.1" fill={color} stroke="none" /><circle cx="3.5" cy="12" r="1.1" fill={color} stroke="none" /><circle cx="3.5" cy="18" r="1.1" fill={color} stroke="none" /></svg>;
    case "grid":
      return <svg {...p}><rect x="3" y="3" width="7.5" height="7.5" rx="2" /><rect x="13.5" y="3" width="7.5" height="7.5" rx="2" /><rect x="3" y="13.5" width="7.5" height="7.5" rx="2" /><rect x="13.5" y="13.5" width="7.5" height="7.5" rx="2" /></svg>;
    case "text":
      return <svg {...p}><path d="M4 7V4h16v3M12 4v16M9 20h6" /></svg>;
    case "alert":
      return <svg {...p}><path d="M10.3 3.9 2.4 18a2 2 0 0 0 1.7 3h15.8a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z" /><path d="M12 9v4" /><circle cx="12" cy="17" r="0.5" fill={color} stroke="none" /></svg>;
    case "play":
      return <svg {...filled}><path d="M8 5.14a1 1 0 0 1 1.52-.85l10.5 6.86a1 1 0 0 1 0 1.7L9.52 19.7A1 1 0 0 1 8 18.86z" /></svg>;
    case "pause":
      return <svg {...filled}><rect x="6" y="5" width="4.2" height="14" rx="1.6" /><rect x="13.8" y="5" width="4.2" height="14" rx="1.6" /></svg>;
    case "skip-back":
      return <svg {...p}><path d="M11 5a7 7 0 1 1-6.32 4" /><path d="M5 3.5 4.5 9 10 8.5" /></svg>;
    case "skip-fwd":
      return <svg {...p}><path d="M13 5a7 7 0 1 0 6.32 4" /><path d="M19 3.5 19.5 9 14 8.5" /></svg>;
    case "headphones":
      return <svg {...p}><path d="M4 14v-2a8 8 0 0 1 16 0v2" /><rect x="2.5" y="13" width="4.5" height="7" rx="2.2" /><rect x="17" y="13" width="4.5" height="7" rx="2.2" /></svg>;
    case "papr":
      return <svg {...p}><path d="M6 3h7l5 5v12a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z" /><path d="M13 3v4a1 1 0 0 0 1 1h4" /><path d="M8 13.5a3.5 3.5 0 0 1 3.5 3.5M8 11a6 6 0 0 1 6 6" /><circle cx="8" cy="17" r="1.1" fill={color} stroke="none" /></svg>;
    default:
      return null;
  }
}
