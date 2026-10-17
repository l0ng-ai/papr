import { useState } from "react";
import { useTranslation } from "react-i18next";
import { openUrl } from "@tauri-apps/plugin-opener";
import { openYoutubePlayer } from "../api";
import { isMac, isMobile } from "../lib/platform";
import { reportError } from "../toast";
import Icon from "./Icon";

export default function YoutubePlayer({ videoId, title }: { videoId: string; title: string }) {
  const { t } = useTranslation();
  const [opening, setOpening] = useState(false);
  const native = isMac || (isMobile && /iPhone|iPad|iPod|MacIntel/.test(navigator.userAgent + navigator.platform));
  const watch = `https://www.youtube.com/watch?v=${videoId}`;
  return <div className="youtube-player">
    {native ? <button className="youtube-play" disabled={opening} aria-label={t("reader.playYoutube")}
      onClick={async () => { setOpening(true); try { await openYoutubePlayer(videoId); } catch (e) { reportError(e); } finally { setOpening(false); } }}>
      <img src={`https://i.ytimg.com/vi/${videoId}/hqdefault.jpg`} alt={title} referrerPolicy="no-referrer" />
      <span><Icon name="play" size={30} />{t("reader.playYoutube")}</span>
    </button> : <iframe style={{ width: "100%", aspectRatio: "16 / 9", minHeight: 200 }}
      src={`https://www.youtube-nocookie.com/embed/${videoId}?playsinline=1&origin=${encodeURIComponent(window.location.origin)}`}
      title={title} referrerPolicy="strict-origin-when-cross-origin" allow="autoplay; encrypted-media; picture-in-picture; fullscreen" allowFullScreen />}
    <button className="youtube-open" onClick={() => openUrl(watch).catch(reportError)}>{t("reader.openYoutube")}</button>
  </div>;
}
