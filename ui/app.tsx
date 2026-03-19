declare const React: any;
declare const ReactDOM: any;
declare const marked: any;
declare const hljs: any;

declare global {
  interface Window {
    __CSRF_TOKEN__: string;
  }
}

type SessionStatus =
  | "starting"
  | "waiting"
  | "running"
  | "cancel_requested"
  | "cancelled"
  | "completed"
  | "timed_out"
  | "error";

type SessionStep = "starting" | "idle" | "review" | "consolidate" | "fix" | "finished";

type Severity = "critical" | "high" | "medium" | "low";

type SeverityCount = {
  level: string;
  count: number;
};

type IterationActivityStatus = "running" | "completed" | "failed" | "cancelled";

type IterationActivity = {
  id: string;
  actor: string;
  status: IterationActivityStatus;
  message: string;
  detail: string | null;
};

type SessionSnapshot = {
  repo_name: string;
  branch: string;
  created_at_unix_secs: number;
  started_at_unix_secs: number;
  timeout_secs: number;
  remaining_secs: number;
  status: SessionStatus;
  current_step: SessionStep;
  current_step_label: string;
  iteration: number;
  active_severity: Severity;
  next_severity: Severity;
  review_agents_total: number;
  review_agents_completed: number;
  review_agents_failed: number;
  total_actionable: number;
  severity_counts: SeverityCount[];
  iteration_activities: IterationActivity[];
  latest_message: string;
  latest_report_filename: string | null;
  latest_round_id: string | null;
  log_filename: string;
  cancel_requested: boolean;
  will_revert_on_cancel: boolean;
  last_error: string | null;
};

type NotesResponse = {
  content: string;
};

type DocKind = "bugfix_log" | "consolidated" | "review";

type StartResponse = {
  started: boolean;
  status: SessionSnapshot;
};

type DocEntry = {
  source: string;
  path: string;
  title: string;
  category: string;
  kind: DocKind;
  round_id: string | null;
  round_label: string | null;
  is_latest: boolean;
  pinned: boolean;
};

type DocsResponse = {
  docs: DocEntry[];
};

type DocResponse = {
  source: string;
  path: string;
  title: string;
  category: string;
  content: string;
};

const { useEffect, useMemo, useRef, useState } = React;

type DocGroup = {
  key: string;
  label: string;
  docs: DocEntry[];
};

const panelShellClass = "glass-panel border-2 border-slate-700/45";
const chipSurfaceClass = "border-2 border-slate-700/45 bg-slate-800/92";
const inputSurfaceClass = "border-2 border-slate-700/50 bg-[#060b12]";
const raisedInsetClass = "border-2 border-slate-700/45 bg-slate-800/78";
const viewerSurfaceClass = "border-2 border-slate-400/55 bg-[#f5f0e4] shadow-[inset_0_1px_0_rgba(255,255,255,0.78)]";
const actionButtonBase = "button-slide border-2 px-4 py-2 text-sm font-medium shadow-[inset_0_1px_0_rgba(255,255,255,0.02)] disabled:cursor-not-allowed disabled:opacity-60";
const neutralActionButtonClass = `${actionButtonBase} button-slide-light border-slate-500/35 bg-slate-200 text-slate-950`;
const accentActionButtonClass = `${actionButtonBase} button-slide-accent border-cyan-200/25 bg-cyan-300 text-slate-950`;
const dangerActionButtonClass = `${actionButtonBase} button-slide-danger border-rose-200/25 bg-rose-300 text-slate-950`;
const docButtonBase = "button-slide button-slide-subtle border-2 px-3 py-2 text-sm font-normal shadow-[inset_0_1px_0_rgba(255,255,255,0.02)]";
const inactiveDocButtonClass = `${docButtonBase} border-slate-600/45 bg-slate-800/96 text-slate-100 hover:text-white`;
const activeDocButtonClass = "border-2 border-cyan-100/30 bg-cyan-200 px-3 py-2 text-sm font-normal text-slate-950 shadow-[inset_0_1px_0_rgba(255,255,255,0.08)]";

function formatDuration(totalSeconds: number): string {
  const seconds = Math.max(0, Math.floor(totalSeconds));
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const remain = seconds % 60;
  return [hours, minutes, remain]
    .map((part, index) => String(part).padStart(index === 0 ? 2 : 2, "0"))
    .join(":");
}

function titleCase(value: string): string {
  return value
    .split("_")
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(" ");
}

async function fetchJson<T>(url: string, options?: RequestInit): Promise<T> {
  const headers: Record<string, string> = {
    "Content-Type": "application/json",
  };
  if (window.__CSRF_TOKEN__) {
    headers["X-CSRF-Token"] = window.__CSRF_TOKEN__;
  }
  const response = await fetch(url, {
    cache: "no-store",
    headers,
    ...options,
  });
  if (!response.ok) {
    const text = await response.text();
    throw new Error(text || `${response.status} ${response.statusText}`);
  }
  return response.json();
}

function sameDoc(left: DocEntry | null, right: DocEntry | null): boolean {
  return !!left && !!right && left.source === right.source && left.path === right.path;
}

function pickDocSelection(docs: DocEntry[], current: DocEntry | null): DocEntry | null {
  if (current) {
    const refreshed = docs.find((doc) => sameDoc(doc, current));
    if (refreshed) {
      return refreshed;
    }
  }

  return (
    docs.find((doc) => doc.kind === "consolidated" && doc.is_latest) ||
    docs.find((doc) => doc.kind === "review" && doc.is_latest) ||
    docs.find((doc) => doc.kind === "bugfix_log") ||
    docs[0] ||
    null
  );
}

function groupDocsBySession(docs: DocEntry[]): { pinned: DocEntry[]; sessions: DocGroup[] } {
  const pinned: DocEntry[] = [];
  const sessions: DocGroup[] = [];
  const byRound = new Map<string, DocGroup>();
  const legacyDocs: DocEntry[] = [];

  docs.forEach((doc) => {
    if (doc.pinned) {
      pinned.push(doc);
      return;
    }

    if (!doc.round_id) {
      legacyDocs.push(doc);
      return;
    }

    let group = byRound.get(doc.round_id);
    if (!group) {
      group = {
        key: doc.round_id,
        label: doc.round_label || doc.round_id,
        docs: [],
      };
      byRound.set(doc.round_id, group);
      sessions.push(group);
    }
    group.docs.push(doc);
  });

  if (legacyDocs.length > 0) {
    sessions.push({
      key: "legacy",
      label: "Legacy / ungrouped",
      docs: legacyDocs,
    });
  }

  return { pinned, sessions };
}

function StatusBadge({ status }: { status: SessionStatus }) {
  const styles: Record<SessionStatus, string> = {
    starting: "bg-cyan-300/16 text-slate-50",
    waiting: "bg-amber-300/16 text-slate-50",
    running: "bg-emerald-300/16 text-slate-50",
    cancel_requested: "bg-amber-300/16 text-slate-50",
    cancelled: "bg-rose-300/18 text-slate-50",
    completed: "bg-sky-300/18 text-slate-50",
    timed_out: "bg-orange-300/16 text-slate-50",
    error: "bg-rose-400/22 text-slate-50",
  };
  return (
    <span className={`inline-flex items-center border-2 border-slate-700/45 px-3 py-1 text-xs font-medium uppercase tracking-[0.22em] ${styles[status]}`}>
      {titleCase(status)}
    </span>
  );
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#039;");
}

function isSafeUrl(url: string): boolean {
  const trimmed = url.trim();
  if (trimmed === "" || trimmed.startsWith("#") || trimmed.startsWith("?")) {
    return true;
  }
  // Relative paths (no scheme) are safe.
  if (!trimmed.includes(":")) {
    return true;
  }
  // Only allow http: and https: schemes.
  try {
    const parsed = new URL(trimmed, "http://localhost");
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

function MarkdownView({ content }: { content: string }) {
  const rendered = useMemo(() => {
    try {
      const renderer = new marked.Renderer();
      renderer.html = function (token: any) {
        return escapeHtml(typeof token === "string" ? token : token.raw || token.text || "");
      };
      renderer.link = function (token: any) {
        const href = typeof token === "string" ? token : token.href || "";
        const text = typeof token === "string" ? token : token.text || href;
        if (!isSafeUrl(href)) {
          return escapeHtml(text);
        }
        return `<a href="${escapeHtml(href)}">${escapeHtml(text)}</a>`;
      };
      renderer.image = function (token: any) {
        const src = typeof token === "string" ? token : token.href || "";
        const alt = typeof token === "string" ? "" : token.text || "";
        if (!isSafeUrl(src)) {
          return escapeHtml(alt || src);
        }
        return `<img src="${escapeHtml(src)}" alt="${escapeHtml(alt)}" />`;
      };
      renderer.code = function (token: any) {
        const code = typeof token === "string" ? token : token.text || "";
        const lang = typeof token === "string" ? "" : token.lang || "";
        let highlighted = "";
        try {
          if (lang && hljs.getLanguage(lang)) {
            highlighted = hljs.highlight(code, { language: lang }).value;
          } else {
            highlighted = hljs.highlightAuto(code).value;
          }
        } catch {
          highlighted = escapeHtml(code);
        }
        const cls = lang ? ` class="hljs language-${escapeHtml(lang)}"` : ' class="hljs"';
        return `<pre><code${cls}>${highlighted}</code></pre>`;
      };
      marked.setOptions({
        gfm: true,
        breaks: true,
      });
      return marked.parse(content || "_No markdown content loaded yet._", { renderer });
    } catch (parseError) {
      console.error("Markdown parse error:", parseError);
      return `<pre>${escapeHtml(content || "")}</pre>`;
    }
  }, [content]);

  return (
    <div
      className="markdown-body prose max-w-none prose-headings:text-slate-950 prose-p:text-slate-800 prose-strong:text-slate-950 prose-a:text-sky-700 prose-li:text-slate-800"
      dangerouslySetInnerHTML={{ __html: rendered }}
    />
  );
}

function StatCard(props: { label: string; value: string; hint?: string }) {
  return (
    <div className={`${panelShellClass} p-5`}>
      <p className="text-xs font-medium uppercase tracking-[0.25em] text-slate-400">{props.label}</p>
      <p className="mt-3 text-3xl font-medium text-white">{props.value}</p>
      {props.hint ? <p className="mt-2 text-sm text-slate-300">{props.hint}</p> : null}
    </div>
  );
}

function iterationActivityAppearance(status: IterationActivityStatus): { row: string; box: string; marker: string; label: string } {
  switch (status) {
    case "completed":
      return {
        row: "border-slate-700/45 bg-emerald-300/12 text-slate-50",
        box: "border-emerald-200/35 bg-emerald-300/18 text-emerald-50",
        marker: "x",
        label: "Done",
      };
    case "failed":
      return {
        row: "border-rose-300/22 bg-rose-400/12 text-rose-50",
        box: "border-rose-200/35 bg-rose-300/18 text-rose-50",
        marker: "!",
        label: "Failed",
      };
    case "cancelled":
      return {
        row: "border-amber-200/18 bg-amber-300/16 text-amber-50",
        box: "border-amber-200/35 bg-amber-300/18 text-amber-50",
        marker: "-",
        label: "Cancelled",
      };
    default:
      return {
        row: "border-slate-700/45 bg-cyan-300/10 text-slate-50",
        box: "border-cyan-200/35 bg-cyan-300/14 text-cyan-50",
        marker: "",
        label: "Running",
      };
  }
}

function StepTimeline({ snapshot }: { snapshot: SessionSnapshot }) {
  const steps: Array<{ key: SessionStep; label: string }> = [
    { key: "starting", label: "Startup" },
    { key: "review", label: "Review" },
    { key: "consolidate", label: "Consolidate" },
    { key: "fix", label: "Fix" },
    { key: "finished", label: "Finish" },
  ];
  const currentIndex = steps.findIndex((step) => step.key === snapshot.current_step);

  return (
    <div className={`${panelShellClass} p-6`}>
      <div className="flex items-center justify-between">
        <div>
          <p className="text-xs font-medium uppercase tracking-[0.25em] text-slate-400">Current phase</p>
          <h2 className="mt-2 text-2xl font-medium text-white">{snapshot.current_step_label}</h2>
        </div>
        <StatusBadge status={snapshot.status} />
      </div>
      <div className="mt-6 grid gap-3 sm:grid-cols-5">
        {steps.map((step, index) => {
          const isActive = snapshot.current_step === step.key;
          const isDone = currentIndex >= index && snapshot.current_step !== "starting";
          return (
            <div
              key={step.key}
                className={`border-2 px-4 py-3 text-sm transition ${
                  isActive
                    ? "border-slate-700/45 bg-cyan-300/24 text-slate-50"
                    : isDone
                      ? "border-slate-700/45 bg-emerald-300/18 text-slate-50"
                      : "border-slate-700/45 bg-[#121927] text-slate-300"
                }`}
            >
              <p className="font-medium">{step.label}</p>
            </div>
          );
        })}
      </div>
      {snapshot.iteration_activities.length > 0 ? (
        <div className="mt-6 border-t-2 border-slate-700/45 pt-5">
          <p className="text-[11px] font-medium uppercase tracking-[0.24em] text-slate-500">Iteration activity</p>
          <div className="mt-3 space-y-2">
            {snapshot.iteration_activities.map((activity) => {
              const appearance = iterationActivityAppearance(activity.status);
              return (
                <div
                  key={activity.id}
                  className={`flex items-start gap-3 border-2 px-3 py-3 text-sm ${appearance.row}`}
                >
                  <div className={`mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center border-2 text-[10px] font-bold ${appearance.box}`}>
                    {appearance.marker}
                  </div>
                  <div className="min-w-0 flex-1">
                    <p className="leading-6">{activity.message}</p>
                    {activity.detail ? <p className="mt-1 text-xs leading-5 opacity-80">Because {activity.detail}</p> : null}
                  </div>
                  <span className="shrink-0 text-[10px] font-medium uppercase tracking-[0.22em] opacity-75">{appearance.label}</span>
                </div>
              );
            })}
          </div>
        </div>
      ) : null}
      <div className="mt-6">
        <p className="text-[11px] font-medium uppercase tracking-[0.24em] text-slate-500">Latest update</p>
        <p className="mt-2 text-sm leading-7 text-slate-300">{snapshot.latest_message}</p>
      </div>
    </div>
  );
}

function App() {
  const [snapshot, setSnapshot] = useState<SessionSnapshot | null>(null);
  const [notes, setNotes] = useState("");
  const [notesDirty, setNotesDirty] = useState(false);
  const [notesSaving, setNotesSaving] = useState(false);
  const [docs, setDocs] = useState<DocEntry[]>([]);
  const [selectedDoc, setSelectedDoc] = useState<DocEntry | null>(null);
  const [docBody, setDocBody] = useState<DocResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [startBusy, setStartBusy] = useState(false);
  const [severitySaving, setSeveritySaving] = useState(false);
  const [cancelBusy, setCancelBusy] = useState(false);
  const [quitSent, setQuitSent] = useState(false);
  const groupedDocs = useMemo(() => groupDocsBySession(docs), [docs]);
  const viewerRef = useRef<any>(null);

  useEffect(() => {
    let cancelled = false;

    async function loadAll() {
      try {
        const [status, notesData, docsData] = await Promise.all([
          fetchJson<SessionSnapshot>("/api/status"),
          fetchJson<NotesResponse>("/api/notes"),
          fetchJson<DocsResponse>("/api/docs"),
        ]);
        if (cancelled) {
          return;
        }
        setSnapshot(status);
        setNotes(notesData.content);
        setDocs(docsData.docs);
        setSelectedDoc((current) => pickDocSelection(docsData.docs, current));
        setError(null);
      } catch (loadError) {
        if (!cancelled) {
          setError(String(loadError));
        }
      }
    }

    loadAll();
    const statusTimer = window.setInterval(async () => {
      try {
        const status = await fetchJson<SessionSnapshot>("/api/status");
        if (!cancelled) {
          setSnapshot(status);
        }
      } catch (statusError) {
        if (!cancelled) {
          setError(String(statusError));
        }
      }
    }, 1000);
    const docTimer = window.setInterval(async () => {
      try {
        const docsData = await fetchJson<DocsResponse>("/api/docs");
        if (!cancelled) {
          setDocs(docsData.docs);
          setSelectedDoc((current) => pickDocSelection(docsData.docs, current));
        }
      } catch {}
    }, 5000);

    return () => {
      cancelled = true;
      window.clearInterval(statusTimer);
      window.clearInterval(docTimer);
    };
  }, []);

  useEffect(() => {
    if (!selectedDoc) {
      return;
    }
    let cancelled = false;
    async function loadDoc() {
      try {
        const query = new URLSearchParams({
          source: selectedDoc.source,
          path: selectedDoc.path,
        });
        const response = await fetchJson<DocResponse>(`/api/doc?${query.toString()}`);
        if (!cancelled) {
          setDocBody(response);
          setError(null);
        }
      } catch (docError) {
        if (!cancelled) {
          setError(String(docError));
        }
      }
    }
    loadDoc();
    return () => {
      cancelled = true;
    };
  }, [selectedDoc]);

  async function saveNotes() {
    setNotesSaving(true);
    try {
      await fetchJson<NotesResponse>("/api/notes", {
        method: "PUT",
        body: JSON.stringify({ content: notes }),
      });
      setNotesDirty(false);
      setError(null);
      if (selectedDoc && selectedDoc.category === "Bugfix log") {
        const query = new URLSearchParams({ source: selectedDoc.source, path: selectedDoc.path });
        const response = await fetchJson<DocResponse>(`/api/doc?${query.toString()}`);
        setDocBody(response);
      }
    } catch (saveError) {
      setError(String(saveError));
    } finally {
      setNotesSaving(false);
    }
  }

  async function changeSeverity(nextSeverity: Severity) {
    setSeveritySaving(true);
    try {
      const status = await fetchJson<SessionSnapshot>("/api/severity", {
        method: "PUT",
        body: JSON.stringify({ severity: nextSeverity }),
      });
      setSnapshot(status);
      setError(null);
    } catch (severityError) {
      setError(String(severityError));
    } finally {
      setSeveritySaving(false);
    }
  }

  async function requestStart() {
    setStartBusy(true);
    try {
      const response = await fetchJson<StartResponse>("/api/start", {
        method: "POST",
        body: JSON.stringify({}),
      });
      setSnapshot(response.status);
      setError(null);
    } catch (startError) {
      setError(String(startError));
    } finally {
      setStartBusy(false);
    }
  }

  async function requestCancel() {
    if (!window.confirm("Cancel the current bugfix session? Current iteration changes will be reverted if a fix is in progress.")) {
      return;
    }
    setCancelBusy(true);
    try {
      const status = await fetchJson<SessionSnapshot>("/api/cancel", {
        method: "POST",
        body: JSON.stringify({}),
      });
      setSnapshot(status);
      setError(null);
    } catch (cancelError) {
      setError(String(cancelError));
    } finally {
      setCancelBusy(false);
    }
  }

  async function requestQuit() {
    setQuitSent(true);
    try {
      await fetchJson<{ ok: boolean }>("/api/quit", {
        method: "POST",
        body: JSON.stringify({}),
      });
    } catch {
      // Server may close before response completes -- that is expected.
    }
  }

  function selectDocument(doc: DocEntry) {
    setSelectedDoc(doc);
    window.requestAnimationFrame(() => {
      viewerRef.current?.scrollIntoView({ behavior: "smooth", block: "start" });
    });
  }

  if (!snapshot) {
    return (
      <main className="mx-auto flex min-h-screen max-w-4xl items-center justify-center px-6 py-16">
        <section className={`${panelShellClass} w-full p-8 text-center`}>
          <p className="text-sm uppercase tracking-[0.28em] text-cyan-300">Connecting</p>
          <h1 className="mt-4 text-4xl font-medium text-white">Opening the live bugfix dashboard...</h1>
          <p className="mt-4 text-sm text-slate-300">Waiting for the local `bod bugfix` session to start reporting state.</p>
          {error ? <pre className="mt-6 overflow-x-auto border-2 border-rose-300/25 bg-rose-400/14 p-4 text-left text-sm text-rose-50">{error}</pre> : null}
        </section>
      </main>
    );
  }

  return (
    <main className="mx-auto max-w-5xl px-4 py-6 sm:px-6 lg:px-8 space-y-6">
      <header className={`${panelShellClass} px-6 py-5 sm:px-8`}>
        <div className="flex flex-wrap items-center gap-x-6 gap-y-3">
          <div className="mr-auto">
            <p className="text-xs font-medium uppercase tracking-[0.32em] text-cyan-300">board of directors</p>
            <h1 className="mt-1 text-2xl font-medium tracking-tight text-white">{snapshot.repo_name} <span className="text-slate-400">/</span> <span className="text-cyan-200">{snapshot.branch}</span></h1>
          </div>
          <StatusBadge status={snapshot.status} />
          <span className={`${chipSurfaceClass} px-3 py-1 text-xs font-medium uppercase tracking-[0.22em] text-slate-100`}>
            {snapshot.status === "waiting" ? "Manual start" : `Iteration ${snapshot.iteration || 1}`}
          </span>
        </div>
      </header>

      {snapshot.status === "waiting" ? (
        <section className={`${panelShellClass} p-6`}>
          <div className="flex flex-wrap items-center gap-4">
            <div className="mr-auto">
              <p className="text-xs font-medium uppercase tracking-[0.28em] text-cyan-300">Ready to start</p>
              <h2 className="mt-2 text-2xl font-medium text-white">Start the bugfix session</h2>
              <p className="mt-2 max-w-2xl text-sm leading-7 text-slate-300">When you are ready, press the start button here or hit Enter in the terminal. This is the action that begins the review and fix loop.</p>
            </div>
            <button
              className={`${accentActionButtonClass} px-6 py-3 text-base`}
              disabled={startBusy}
              onClick={requestStart}
            >
              {startBusy ? "Starting..." : "Start bugfix"}
            </button>
          </div>
        </section>
      ) : null}

      <section className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <StatCard label="Time remaining" value={formatDuration(snapshot.remaining_secs)} hint={`${snapshot.timeout_secs}s timeout`} />
        <StatCard label="Active severity" value={snapshot.active_severity.toUpperCase()} />
        <StatCard label="Reviewers" value={`${snapshot.review_agents_completed}/${snapshot.review_agents_total}`} hint={snapshot.review_agents_failed > 0 ? `${snapshot.review_agents_failed} failed` : "All healthy"} />
        <StatCard label="Actionable issues" value={String(snapshot.total_actionable)} hint={snapshot.severity_counts.length > 0 ? snapshot.severity_counts.map((item) => `${item.count} ${item.level}`).join(", ") : "Waiting for consolidation"} />
      </section>

      <StepTimeline snapshot={snapshot} />

      <section className={`${panelShellClass} p-6`}>
        {(() => {
          const isTerminal = snapshot.status === "completed" || snapshot.status === "cancelled" || snapshot.status === "timed_out" || snapshot.status === "error";
          const isWaiting = snapshot.status === "waiting";
          if (isTerminal) {
            return (
              <div className="flex flex-wrap items-center gap-4">
                <div className="mr-auto">
                  <p className="text-xs font-medium uppercase tracking-[0.25em] text-slate-400">Session finished</p>
                  <p className="mt-1 text-sm text-slate-300">{snapshot.latest_message}</p>
                  {snapshot.latest_round_id ? <p className="mt-1 text-xs uppercase tracking-[0.22em] text-cyan-300">Last round {snapshot.latest_round_id}</p> : null}
                </div>
                <button
                  className={neutralActionButtonClass}
                  disabled={quitSent}
                  onClick={requestQuit}
                >
                  {quitSent ? "Closing..." : "Close session"}
                </button>
              </div>
            );
          }
          return (
            <>
              <div className="flex flex-wrap items-center gap-4">
                <div className="mr-auto">
                  <p className="text-xs font-medium uppercase tracking-[0.25em] text-slate-400">Session controls</p>
                  {isWaiting ? (
                    <p className="mt-1 text-sm text-slate-300">Use the start button above or press Enter in the terminal when you are ready.</p>
                  ) : snapshot.latest_round_id ? (
                    <p className="mt-1 text-xs uppercase tracking-[0.22em] text-cyan-300">Round {snapshot.latest_round_id}</p>
                  ) : null}
                </div>
                <div className="flex items-center gap-3">
                  <label className="text-xs font-medium uppercase tracking-[0.22em] text-slate-400">Next severity</label>
                  <select
                    className={`${inputSurfaceClass} px-3 py-2 text-sm font-normal text-white outline-none ring-0`}
                    value={snapshot.next_severity}
                    onChange={(event) => changeSeverity(event.target.value as Severity)}
                    disabled={severitySaving}
                  >
                    <option value="critical">Critical</option>
                    <option value="high">High</option>
                    <option value="medium">Medium</option>
                    <option value="low">Low</option>
                  </select>
                </div>
                <button
                  className={dangerActionButtonClass}
                  disabled={cancelBusy || snapshot.cancel_requested}
                  onClick={requestCancel}
                >
                  {snapshot.cancel_requested ? "Cancel requested" : cancelBusy ? "Sending cancel..." : "Cancel session"}
                </button>
              </div>
              {snapshot.will_revert_on_cancel ? (
                <p className="mt-4 border-2 border-amber-200/18 bg-amber-300/20 px-4 py-3 text-sm text-amber-50">
                  A rollback snapshot is armed. Cancel will restore the repo to how it looked when this fix step started. Earlier iteration changes and any pre-existing branch changes stay on the branch.
                </p>
              ) : null}
            </>
          );
        })()}
      </section>

      <section className={`${panelShellClass} p-6`}>
        <div className="flex items-center justify-between">
          <div>
            <p className="text-xs font-medium uppercase tracking-[0.25em] text-slate-400">User notes</p>
            <p className="mt-1 text-sm text-slate-400">Persisted in the bugfix log and included in the next consolidation.</p>
          </div>
          <button
            className={accentActionButtonClass}
            disabled={notesSaving || !notesDirty}
            onClick={saveNotes}
          >
            {notesSaving ? "Saving..." : notesDirty ? "Save notes" : "Saved"}
          </button>
        </div>
        <textarea
          className={`mt-4 min-h-[140px] w-full ${inputSurfaceClass} px-4 py-3 text-sm leading-7 text-slate-100 outline-none placeholder:text-slate-500`}
          placeholder="Write anything you notice while the agents are running..."
          value={notes}
          onChange={(event) => {
            setNotes(event.target.value);
            setNotesDirty(true);
          }}
        />
      </section>

      <section className={`${panelShellClass} p-6`}>
        <div className="flex items-center justify-between">
          <div>
            <p className="text-xs font-medium uppercase tracking-[0.25em] text-slate-400">Documents</p>
            <p className="mt-1 text-sm text-slate-400">{docs.length} document(s) for this branch</p>
          </div>
        </div>
        <p className="mt-4 text-[11px] font-medium uppercase tracking-[0.24em] text-slate-500">Available documents</p>
        <div className={`mt-2 ${raisedInsetClass}`}>
          {groupedDocs.pinned.length === 0 && groupedDocs.sessions.length === 0 ? (
            <p className="px-4 py-4 text-sm text-slate-300">Documents will appear here as the session produces them.</p>
          ) : null}
          {groupedDocs.pinned.length > 0 ? (
            <div className="px-4 py-4">
              <p className="mb-3 text-[11px] font-medium uppercase tracking-[0.24em] text-slate-500">Bugfix</p>
              <div className="flex flex-wrap gap-2">
                {groupedDocs.pinned.map((doc) => {
                  const selected = selectedDoc?.source === doc.source && selectedDoc?.path === doc.path;
                  return (
                    <button
                      key={`${doc.source}:${doc.path}`}
                      className={
                       selected
                          ? activeDocButtonClass
                          : inactiveDocButtonClass
                       }
                       onClick={() => selectDocument(doc)}
                     >
                       {doc.title}
                     </button>
                   );
                 })}
              </div>
            </div>
          ) : null}
          {groupedDocs.sessions.map((group, index) => (
            <div
              key={group.key}
              className={`${groupedDocs.pinned.length > 0 || index > 0 ? "border-t-2 border-slate-700/45" : ""} px-4 py-4`}
            >
              <p className="mb-3 text-[11px] font-medium uppercase tracking-[0.24em] text-slate-500">{group.label}</p>
              <div className="flex flex-wrap gap-2">
                {group.docs.map((doc) => {
                  const selected = selectedDoc?.source === doc.source && selectedDoc?.path === doc.path;
                  return (
                    <button
                      key={`${doc.source}:${doc.path}`}
                      className={
                        selected
                          ? activeDocButtonClass
                          : inactiveDocButtonClass
                       }
                       onClick={() => selectDocument(doc)}
                     >
                       {doc.title}
                     </button>
                   );
                 })}
              </div>
            </div>
          ))}
        </div>
        <div ref={viewerRef} className="mt-5 flex items-center justify-between">
          <p className="text-[11px] font-medium uppercase tracking-[0.24em] text-slate-500">Markdown viewer</p>
          <p className="text-xs text-slate-400">{docBody ? "Selected document contents" : "Choose a document to open it here"}</p>
        </div>
        <div className={`mt-2 min-h-[480px] ${viewerSurfaceClass} p-6 overflow-y-auto`} style={{ maxHeight: "75vh" }}>
          {docBody ? (
            <>
              <div className="mb-4 pb-3 border-b-2 border-slate-400/55">
                <h2 className="text-lg font-medium text-slate-950">{docBody.title}</h2>
                <p className="mt-1 text-xs text-slate-600">{docBody.source}:{docBody.path}</p>
              </div>
              <MarkdownView content={docBody.content} />
            </>
          ) : (
            <p className="text-sm text-slate-700">Select a document above to view it here.</p>
          )}
        </div>
      </section>

      {error ? (
        <div className="glass-panel border-2 border-rose-300/25 bg-rose-400/14 p-5 text-sm leading-7 text-rose-50">
          <p className="font-medium uppercase tracking-[0.22em] text-rose-200">Dashboard error</p>
          <p className="mt-2">{error}</p>
        </div>
      ) : null}
    </main>
  );
}

const mountNode = document.getElementById("app");
if (!mountNode) {
  throw new Error("Missing #app mount node");
}

if (ReactDOM.createRoot) {
  ReactDOM.createRoot(mountNode).render(<App />);
} else {
  ReactDOM.render(<App />, mountNode);
}
