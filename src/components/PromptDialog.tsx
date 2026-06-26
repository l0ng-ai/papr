import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useFocusTrap } from "../hooks/useFocusTrap";
import { NO_AUTOCORRECT } from "../lib/inputProps";

interface Props {
  title: string;
  initialValue?: string;
  placeholder?: string;
  confirmLabel?: string;
  onSubmit: (value: string) => void;
  onClose: () => void;
}

/** A single-field modal prompt — feed / folder rename, new folder. */
export default function PromptDialog({
  title,
  initialValue = "",
  placeholder,
  confirmLabel,
  onSubmit,
  onClose,
}: Props) {
  const { t } = useTranslation();
  const [value, setValue] = useState(initialValue);
  const dialogRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  useFocusTrap(dialogRef);

  // For a rename (the field opens pre-filled), select the text so the user
  // can type a replacement immediately — standard rename UX.
  useEffect(() => {
    if (initialValue) inputRef.current?.select();
  }, [initialValue]);

  // Escape must close the dialog regardless of which control has focus —
  // an input-level handler dies as soon as the user tabs to a button.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  const submit = () => {
    const v = value.trim();
    if (!v) return;
    onSubmit(v);
    onClose();
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal"
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="prompt-dialog-title"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 id="prompt-dialog-title">{title}</h2>
        <input
          className="modal-input"
          ref={inputRef}
          autoFocus
          value={value}
          placeholder={placeholder}
          aria-label={placeholder ?? title}
          {...NO_AUTOCORRECT}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            // Ignore the Enter that confirms an IME candidate (CJK input) —
            // otherwise it submits the dialog with a half-composed name.
            if (e.key === "Enter" && !e.nativeEvent.isComposing) submit();
          }}
          style={{ marginTop: 8 }}
        />
        <div className="modal-actions">
          <button className="s-btn" onClick={onClose}>
            {t("common.cancel")}
          </button>
          <button className="s-btn primary" onClick={submit} disabled={!value.trim()}>
            {confirmLabel ?? t("common.confirm")}
          </button>
        </div>
      </div>
    </div>
  );
}
