import { useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { useArticleActions } from "../hooks/articleActions";
import { reportError } from "../toast";
import Icon from "./Icon";

export default function WebDavSyncButton({ className, onOpenSettings, onToast }: {
  className: string; onOpenSettings: () => void; onToast: (message: string) => void;
}) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const actions = useArticleActions();
  const status = useQuery({ queryKey: ["webdav-status"], queryFn: api.webdavStatus, refetchInterval: 15_000 });
  const transfer = useQuery({ queryKey: ["webdav-progress"], queryFn: api.webdavProgress, enabled: !!status.data?.connected, refetchInterval: (q) => q.state.data?.running ? 1000 : 5000 });
  const [busy, setBusy] = useState(false);
  const clicked = useRef(false);
  const sync = async () => {
    if (status.isError) { reportError(status.error); return; }
    if (!status.data?.connected) { onOpenSettings(); return; }
    if (clicked.current) return;
    clicked.current = true; setBusy(true);
    try { await api.webdavSync(); onToast(t("settings.webdav.done")); }
    catch (e) { reportError(e); }
    finally {
      clicked.current = false; setBusy(false);
      actions.refreshAfterFetch();
      await Promise.all(["webdav-status", "webdav-progress", "webdav-conflicts"].map((key) => qc.invalidateQueries({ queryKey: [key] })));
    }
  };
  const running = busy || !!transfer.data?.running;
  return <button className={className} title={t("sidebar.webdavSync")} aria-label={t(running ? "settings.sync.syncing" : "sidebar.webdavSync")} disabled={running || status.isPending} onClick={() => { void sync(); }}>
    <Icon name="cloud-sync" size={16} />
    <span>{t(running ? "sidebar.syncing" : "sidebar.sync")}</span>
  </button>;
}
