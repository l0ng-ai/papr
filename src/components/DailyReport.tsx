import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import * as api from "../api";
import { useCal } from "../calStore";
import { t } from "i18next";
import type { AiEvent } from "../types";
import { toast } from "../toast";
import { marked } from "marked";

function todayStr(): string {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

function renderMarkdown(src: string): string {
  if (!src) return "";
  try {
    const r = marked.parse(src);
    return typeof r === "string" ? r : "";
  } catch {
    return "";
  }
}

function fmtDate(dateStr: string): string {
  const [y, m, d] = dateStr.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
  });
}

export default function DailyReport() {
  const today = todayStr();
  const selectedDate = useCal((s) => s.selectedDate);
  const setReportDates = useCal((s) => s.setReportDates);
  const addReportDate = useCal((s) => s.addReportDate);

  const [reportText, setReportText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const bufRef = useRef("");
  const busyRef = useRef(false);
  const genSeqRef = useRef(0);
  const rafRef = useRef(0);

  // ── dates with saved reports → sync to shared store ──
  const datesQ = useQuery({
    queryKey: ["daily-report-dates"],
    queryFn: api.listDailyReportDates,
    staleTime: 30_000,
  });

  useEffect(() => {
    if (datesQ.data) setReportDates(new Set(datesQ.data));
  }, [datesQ.data, setReportDates]);

  // ── streaming helpers ──
  const flush = useCallback(() => {
    rafRef.current = 0;
    setReportText(bufRef.current);
  }, []);

  const scheduleFlush = useCallback(() => {
    if (rafRef.current) return;
    rafRef.current = requestAnimationFrame(flush);
  }, [flush]);

  /** Generate a report for a specific date. */
  const generate = useCallback(
    (date?: string) => {
      if (busyRef.current) return;
      const target = date ?? selectedDate;
      busyRef.current = true;
      setBusy(true);
      setError(false);
      setReportText("");
      bufRef.current = "";

      const seq = ++genSeqRef.current;

      api
        .aiDailyReport((e: AiEvent) => {
          if (e.type === "delta") {
            bufRef.current += e.data;
            scheduleFlush();
          }
        }, target)
        .then(() => {
          if (seq !== genSeqRef.current) return;
          if (rafRef.current) cancelAnimationFrame(rafRef.current);
          rafRef.current = 0;
          setReportText(bufRef.current);
          setBusy(false);
          busyRef.current = false;
          addReportDate(target);
          datesQ.refetch();
        })
        .catch((err: unknown) => {
          if (seq !== genSeqRef.current) return;
          if (rafRef.current) cancelAnimationFrame(rafRef.current);
          rafRef.current = 0;
          if (bufRef.current) setReportText(bufRef.current);
          else setError(true);
          setBusy(false);
          busyRef.current = false;
          const msg = err instanceof Error ? err.message : String(err);
          if (msg !== "noArticles") toast.error(msg);
        });
    },
    [selectedDate, datesQ, addReportDate, scheduleFlush],
  );

  useEffect(() => {
    return () => {
      if (rafRef.current) cancelAnimationFrame(rafRef.current);
    };
  }, []);

  // ── load report from DB when selected date changes ──
  useEffect(() => {
    let cancelled = false;
    setLoaded(false);
    setReportText("");
    setError(false);

    api.getDailyReport(selectedDate).then((text) => {
      if (cancelled) return;
      if (text) setReportText(text);
      setLoaded(true);
    });

    return () => {
      cancelled = true;
    };
  }, [selectedDate]);

  // ── auto-generate today on first mount if needed ──
  const autoGenDone = useRef(false);
  useEffect(() => {
    if (autoGenDone.current) return;
    if (!loaded) return;
    if (!datesQ.isSuccess) return;
    autoGenDone.current = true;
    const reportDates = useCal.getState().reportDates;
    if (!reportText && !reportDates.has(today)) {
      generate(today);
    }
  }, [loaded, datesQ.isSuccess, reportText, today, generate]);

  const html = renderMarkdown(reportText);

  return (
    <div className="daily-report">
      <div className="dr-header">
        <div className="dr-title">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="12" cy="12" r="10"/>
            <polyline points="12 6 12 12 16 14"/>
          </svg>
          {t("dailyReport.title")}
        </div>
        <div className="dr-header-right">
          <span className="dr-date">{fmtDate(selectedDate)}</span>
          <button
            className="dr-refresh"
            onClick={() => generate()}
            disabled={busy}
            title={t("dailyReport.refresh")}
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <polyline points="23 4 23 10 17 10"/>
              <path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/>
            </svg>
          </button>
        </div>
      </div>

      <div className="dr-content">
        <div className="dr-content-body">
          {busy && !reportText && (
            <div className="dr-loading">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="spinning">
                <circle cx="12" cy="12" r="10" strokeDasharray="60" strokeDashoffset="20"/>
              </svg>
              {t("dailyReport.generating")}
            </div>
          )}

          {error && !reportText && (
            <div className="dr-error">
              <span>{t("dailyReport.error")}</span>
              <button className="dr-retry" onClick={() => generate()}>
                {t("dailyReport.refresh")}
              </button>
            </div>
          )}

          {loaded && !reportText && !busy && !error && (
            <div className="dr-empty">
              <span>{t("dailyReport.empty")}</span>
              <button className="dr-retry" onClick={() => generate()}>
                {t("dailyReport.generate")}
              </button>
            </div>
          )}

          {reportText && (
            <div
              className={`dr-prose${busy ? " streaming" : ""}`}
              dangerouslySetInnerHTML={{ __html: html }}
            />
          )}
        </div>
      </div>
    </div>
  );
}
