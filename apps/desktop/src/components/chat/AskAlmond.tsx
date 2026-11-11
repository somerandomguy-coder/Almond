import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import type { ChatEntry } from "../../gen/ChatEntry";
import type { ChatScope } from "../../gen/ChatScope";
import type { ChatSource } from "../../gen/ChatSource";
import type { ChatThread } from "../../gen/ChatThread";
import { useApi, useNotification } from "../../api/context";
import { errorMessage } from "../../api/errors";
import { ulid } from "../../lib/ids";
import { Button, Icon, Spinner } from "../ui";
import { ChatMarkdown, SourceList } from "./ChatAnswer";

export interface AskAlmondProps {
  scope: ChatScope;
  /** Input placeholder, e.g. "Ask about this meeting…". */
  placeholder: string;
  /**
   * Open a source. Return true when handled (e.g. the open meeting jumping within its own
   * transcript); otherwise the meeting opens at the cited segment.
   */
  onOpenSource?: (s: ChatSource) => boolean;
}

type Pending = { threadId: string; question: string; text: string };

function isOllamaDown(e: unknown) {
  return /ollama/i.test(errorMessage(e));
}

/**
 * "Ask Almond": a chat bar floating at the bottom of the screen. Asking opens the conversation
 * above the bar (streaming answer with source chips); Close folds it back into the bar. The
 * conversation resets when the scope changes (another page).
 */
export function AskAlmond({ scope, placeholder, onOpenSource }: AskAlmondProps) {
  const { t } = useTranslation();
  const api = useApi();
  const nav = useNavigate();
  const [threadId, setThreadId] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatEntry[]>([]);
  const [pending, setPending] = useState<Pending | null>(null);
  const pendingRef = useRef<Pending | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [input, setInput] = useState("");
  const [open, setOpen] = useState(false);
  const [showHistory, setShowHistory] = useState(false);
  const [threads, setThreads] = useState<ChatThread[]>([]);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const endRef = useRef<HTMLDivElement>(null);
  const scopeKey = JSON.stringify(scope);

  // A new page (scope) starts a fresh, folded chat.
  useEffect(() => {
    setThreadId(null);
    setMessages([]);
    setError(null);
    setOpen(false);
    setShowHistory(false);
  }, [scopeKey]);

  const loadThreads = useCallback(() => {
    api
      .call("chat.threads", { scope: JSON.parse(scopeKey) as ChatScope, limit: 8 })
      .then(setThreads)
      .catch(() => setThreads([]));
  }, [api, scopeKey]);

  useNotification("chat.delta", (d) => {
    const p = pendingRef.current;
    if (!p || p.threadId !== d.thread_id) return;
    const next = { ...p, text: p.text + d.text };
    pendingRef.current = next;
    setPending(next);
  });

  useEffect(() => {
    endRef.current?.scrollIntoView?.({ block: "nearest" });
  }, [messages, pending?.text, open]);

  const openSource = useCallback(
    (s: ChatSource) => {
      if (onOpenSource?.(s)) return;
      if (s.meeting_id) nav(`/meeting/${s.meeting_id}${s.span ? `?seg=${s.span.segment}` : ""}`);
    },
    [nav, onOpenSource],
  );

  const ask = async (q: string) => {
    const question = q.trim();
    if (!question || pendingRef.current) return;
    const tid = threadId ?? ulid();
    const p = { threadId: tid, question, text: "" };
    pendingRef.current = p;
    setPending(p);
    setError(null);
    setInput("");
    setOpen(true);
    setShowHistory(false);
    try {
      const r = await api.call("chat.ask", { thread_id: tid, scope, message: question });
      const user: ChatEntry = { id: ulid(), role: "user", text: question, sources: [], model: null, at: Date.now() };
      setThreadId(r.thread_id);
      setMessages((m) => [...m, user, r.entry]);
    } catch (e) {
      setError(e);
      setInput(question);
    } finally {
      pendingRef.current = null;
      setPending(null);
    }
  };

  const submit = (e: FormEvent) => {
    e.preventDefault();
    void ask(input);
  };

  const stop = () => {
    if (pending) void api.call("chat.cancel", { thread_id: pending.threadId }).catch(() => {});
  };

  const newChat = () => {
    if (pending) return;
    setThreadId(null);
    setMessages([]);
    setError(null);
    setShowHistory(false);
    inputRef.current?.focus();
  };

  const toggleHistory = () => {
    if (!showHistory) loadThreads();
    setShowHistory((v) => !v);
    setOpen(true);
  };

  const openThread = async (id: string) => {
    if (pending) return;
    try {
      const th = await api.call("chat.get", { thread_id: id });
      setThreadId(th.id);
      setMessages(th.messages);
      setError(null);
      setShowHistory(false);
    } catch (e) {
      setError(e);
    }
  };

  const removeThread = async (id: string) => {
    await api.call("chat.delete", { thread_id: id }).catch(() => {});
    if (id === threadId) newChat();
    loadThreads();
  };

  const hasConversation = messages.length > 0 || pending != null;
  const panelOpen = open;

  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-16 z-30 flex justify-center px-4 md:bottom-4 md:left-60" data-testid="ask-almond">
      <div className="pointer-events-auto flex w-full max-w-2xl flex-col gap-2">
        {panelOpen && (
          <section
            aria-label={t("chat.title")}
            className="flex max-h-[60vh] flex-col overflow-hidden rounded-2xl border border-line bg-surface shadow-lg"
          >
            <header className="flex items-center gap-1 border-b border-line px-3 py-2">
              <Icon.sparkle width={14} height={14} className="text-accent" />
              <h2 className="text-sm font-semibold">{t("chat.title")}</h2>
              <Button size="sm" variant="ghost" className="ml-auto" onClick={toggleHistory} aria-pressed={showHistory}>
                {t("chat.recent")}
              </Button>
              {hasConversation && (
                <Button size="sm" variant="ghost" onClick={newChat} disabled={!!pending}>
                  <Icon.plus width={14} height={14} />
                  {t("chat.newChat")}
                </Button>
              )}
              <Button size="sm" variant="ghost" onClick={() => setOpen(false)} aria-label={t("chat.close")}>
                <Icon.close width={14} height={14} />
              </Button>
            </header>

            <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3 text-sm">
              {showHistory ? (
                threads.length === 0 ? (
                  <p className="text-muted">{t("chat.noHistory")}</p>
                ) : (
                  <ul className="flex flex-col">
                    {threads.map((th) => (
                      <li key={th.id} className="group flex items-center gap-2">
                        <button type="button" onClick={() => void openThread(th.id)} className="min-w-0 flex-1 truncate rounded-lg px-2 py-1.5 text-left hover:bg-sunken">
                          {th.title || t("chat.title")}
                        </button>
                        <button
                          type="button"
                          onClick={() => void removeThread(th.id)}
                          aria-label={`${t("chat.deleteThread")}: ${th.title}`}
                          className="rounded-md p-1 text-faint opacity-0 group-hover:opacity-100 hover:text-bad focus:opacity-100"
                        >
                          <Icon.trash width={14} height={14} />
                        </button>
                      </li>
                    ))}
                  </ul>
                )
              ) : (
                <ol className="flex flex-col gap-4" aria-live="polite">
                  {!hasConversation && <li className="text-muted">{t("chat.empty")}</li>}
                  {messages.map((m) =>
                    m.role === "user" ? (
                      <li key={m.id} className="self-end rounded-2xl rounded-br-md bg-sunken px-3 py-2" data-role="user">
                        {m.text}
                      </li>
                    ) : (
                      <li key={m.id} data-role="assistant">
                        <ChatMarkdown text={m.text} sources={m.sources} onOpen={openSource} />
                        <SourceList sources={m.sources} onOpen={openSource} />
                      </li>
                    ),
                  )}
                  {pending && (
                    <>
                      <li className="self-end rounded-2xl rounded-br-md bg-sunken px-3 py-2" data-role="user">
                        {pending.question}
                      </li>
                      <li data-role="assistant" data-testid="chat-streaming">
                        {pending.text ? (
                          <ChatMarkdown text={pending.text} sources={[]} onOpen={openSource} />
                        ) : (
                          <span className="inline-flex items-center gap-2 text-muted">
                            <Spinner />
                            {t("chat.thinking")}
                          </span>
                        )}
                      </li>
                    </>
                  )}
                  <div ref={endRef} />
                </ol>
              )}
              {error != null && (
                <div role="alert" className="mt-3 rounded-xl bg-bad-soft p-3 text-bad">
                  {isOllamaDown(error) ? (
                    <>
                      <p className="font-medium">{t("chat.ollamaDown")}</p>
                      <p className="mt-1 text-xs opacity-80">{errorMessage(error)}</p>
                    </>
                  ) : (
                    t("app.error", { message: errorMessage(error) })
                  )}
                </div>
              )}
            </div>
          </section>
        )}

        <form
          onSubmit={submit}
          className="flex items-end gap-2 rounded-2xl border border-line bg-surface px-3 py-2 shadow-lg focus-within:border-accent"
        >
          <Icon.sparkle width={16} height={16} className="mb-1.5 shrink-0 text-accent" />
          <textarea
            ref={inputRef}
            rows={1}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onFocus={() => hasConversation && setOpen(true)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                void ask(input);
              } else if (e.key === "Escape") {
                setOpen(false);
              }
            }}
            placeholder={placeholder}
            aria-label={placeholder}
            className="max-h-32 min-h-6 flex-1 resize-none bg-transparent py-1 text-sm outline-none placeholder:text-faint"
          />
          {pending ? (
            <Button size="sm" variant="secondary" onClick={stop}>
              <Icon.stop width={12} height={12} />
              {t("chat.stop")}
            </Button>
          ) : (
            input.trim() && (
              <Button size="sm" variant="primary" type="submit">
                {t("chat.ask")}
              </Button>
            )
          )}
        </form>
      </div>
    </div>
  );
}
