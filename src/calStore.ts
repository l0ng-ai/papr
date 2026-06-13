import { create } from "zustand";

function todayStr(): string {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

interface CalState {
  selectedDate: string;
  /** Year-month currently shown in the calendar grid. */
  viewYM: { y: number; m: number };
  /** Set of dates that have a saved report (updated by DailyReport). */
  reportDates: Set<string>;
  selectDate: (d: string) => void;
  prevMonth: () => void;
  nextMonth: () => void;
  setReportDates: (dates: Set<string>) => void;
  addReportDate: (d: string) => void;
}

export const useCal = create<CalState>((set) => ({
  selectedDate: todayStr(),
  viewYM: (() => {
    const d = new Date();
    return { y: d.getFullYear(), m: d.getMonth() };
  })(),
  reportDates: new Set<string>(),

  selectDate: (d) => set({ selectedDate: d }),
  prevMonth: () =>
    set((s) => {
      const m = s.viewYM.m - 1;
      return { viewYM: m < 0 ? { y: s.viewYM.y - 1, m: 11 } : { y: s.viewYM.y, m } };
    }),
  nextMonth: () =>
    set((s) => {
      const m = s.viewYM.m + 1;
      return { viewYM: m > 11 ? { y: s.viewYM.y + 1, m: 0 } : { y: s.viewYM.y, m } };
    }),
  setReportDates: (dates) => set({ reportDates: dates }),
  addReportDate: (d) =>
    set((s) => {
      const next = new Set(s.reportDates);
      next.add(d);
      return { reportDates: next };
    }),
}));
