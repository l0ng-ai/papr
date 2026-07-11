import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import Icon from "./Icon";

interface Props {
  /** The working (already-loaded, possibly proxied) src of every image in the
   *  article, in reading order. */
  srcs: string[];
  /** Index of the image the user clicked. */
  index: number;
  onClose: () => void;
}

/** Full-screen image viewer (issue #87). Opens from a click on an article-body
 *  image and cycles through every image in the article via the on-screen arrows
 *  or the ← / → keys. Videos are intentionally excluded — sanitize forces
 *  `controls` on every `<video>`, so they already have native fullscreen. */
export default function Lightbox({ srcs, index, onClose }: Props) {
  const { t } = useTranslation();
  const [i, setI] = useState(index);
  const many = srcs.length > 1;

  const prev = useCallback(
    () => setI((v) => (v - 1 + srcs.length) % srcs.length),
    [srcs.length],
  );
  const next = useCallback(
    () => setI((v) => (v + 1) % srcs.length),
    [srcs.length],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      else if (e.key === "ArrowLeft") prev();
      else if (e.key === "ArrowRight") next();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, prev, next]);

  // Clicking the backdrop closes; clicks on the image or the controls don't
  // bubble up to it (see `stop`).
  const stop = (e: React.MouseEvent) => e.stopPropagation();

  return (
    <div className="lightbox" role="dialog" aria-modal="true" onClick={onClose}>
      <button
        className="lightbox-btn lightbox-close"
        aria-label={t("reader.lightboxClose")}
        onClick={onClose}
      >
        <Icon name="x" size={20} />
      </button>

      {many && (
        <button
          className="lightbox-btn lightbox-prev"
          aria-label={t("reader.lightboxPrev")}
          onClick={(e) => {
            stop(e);
            prev();
          }}
        >
          <Icon name="chevron-right" size={26} />
        </button>
      )}

      <img
        className="lightbox-img"
        src={srcs[i]}
        alt=""
        onClick={stop}
        draggable={false}
      />

      {many && (
        <button
          className="lightbox-btn lightbox-next"
          aria-label={t("reader.lightboxNext")}
          onClick={(e) => {
            stop(e);
            next();
          }}
        >
          <Icon name="chevron-right" size={26} />
        </button>
      )}

      {many && (
        <div className="lightbox-counter" onClick={stop}>
          {i + 1} / {srcs.length}
        </div>
      )}
    </div>
  );
}
