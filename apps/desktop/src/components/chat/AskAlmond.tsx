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
import { Button, cx, Icon, Spinner } from "../ui";
import { ChatMarkdown, SourceList } from "./ChatAnswer";

export interface AskAlmondProps {
  scope: ChatScope;
  title: string;
  placeholder: string;
  /** Suggested questions (translated). One ending in "…" fills the input instead of asking. */
  suggestions: string[];
  /**
   * Open a source. Return true when handled (e.g. the meeting view jumping within its own
   * transcript); otherwise the meeting opens at the cited segment.
   */
  onOpenSource?: (s: ChatSource) => boolean;
  className?: string;
}

type Pending = { threadId: string; question: string; text: string };

function isOllamaDown(e: unknown) {
  return /ollama/i.test(errorMessage(e));
}

/** "Ask Almond": question box, streaming answer with source chips, recent threads. */
export function AskAlmond({ scope, title, placeholder, suggestions, onOpenSource, className }: AskAlmondProps) {
  const { t } = useTranslation();
  const api = useApi();
  const nav = useNavigate();
  const [threadId, setThreadId] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatEntry[]>([]);
  const [pending, setPending] = useState<Pending | null>(null);
  const pendingRef = useRef<Pending | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [input, setInput] = useState("");
  const [threads, setThreads] = useState<ChatThread[]>([]);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const endRef = useRef<HTMLDivElement>(null);
  const scopeKey = JSON.stringify(scope);

  const loadThreads = useCallback(() => {
    api
      .call("chat.threads", { scope: JSON.parse(scopeKey) as ChatScope, limit: 5 })
      .then(setThreads)
      .catch(() => setThreads([]));
  }, [api, scopeKey]);
  useEffect(loadThreads, [loadThreads]);

  useNotification("chat.delta", (d) => {
    const p = pendingRef.current;
    if (!p || p.threadId !== d.thread_id) return;
    const next = { ...p, text: p.text + d.text };
    pendingRef.current = next;
    setPending(next);
  });

  useEffect(() => {
    endRef.current?.scrollIntoView?.({ block: "nearest" });
  }, [messages, pending?.text]);

  const open = useCallback(
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
    try {
      const r = await api.call("chat.ask", { thread_id: tid, scope, message: question });
      const user: ChatEntry = { id: ulid(), role: "user", text: question, sources: [], model: null, at: Date.now() };
      setThreadId(r.thread_id);
      setMessages((m) => [...m, user, r.entry]);
      loadThreads();
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

  const openThread = async (id: string) => {
    if (pending) return;
    try {
      const th = await api.call("chat.get", { thread_id: id });
      setThreadId(th.id);
      setMessages(th.messages);
      setError(null);
    } catch (e) {
      setError(e);
    }
  };

  const newChat = () => {
    if (pending) return;
    setThreadId(null);
    setMessages([]);
    setError(null);
    inputRef.current?.focus();
  };

  const removeThread = async (id: string) => {
    await api.call("chat.delete", { thread_id: id }).catch(() => {});
    if (id === threadId) newChat();
    loadThreads();
  };

  const suggest = (s: string) => {
    if (s.endsWith("…")) {
      setInput(s.slice(0, -1) + " ");
      inputRef.current?.focus();
    } else void ask(s);
  };

  const empty = messages.length === 0 && !pending;

  return (
    <section aria-label={title} className={cx("flex flex-col gap-3 rounded-2xl border border-line bg-surface p-4", className)} data-testid="ask-almond">
      <div className="flex items-center gap-2">
        <Icon.sparkle width={16} height={16} className="text-accent" />
        <h2 className="text-sm font-semibold">{title}</h2>
        {!empty && (
          <Button size="sm" variant="ghost" className="ml-auto" onClick={newChat} disabled={!!pending}>
            <Icon.plus width={14} height={14} />
            {t("chat.newChat")}
          </Button>
        )}
      </div>

      {!empty && (
        <ol className="flex max-h-[28rem] flex-col gap-4 overflow-y-auto pr-1 text-sm" aria-live="polite">
          {messages.map((m) =>
            m.role === "user" ? (
              <li key={m.id} className="self-end rounded-2xl rounded-br-md bg-sunken px-3 py-2" data-role="user">
                {m.text}
              </li>
            ) : (
              <li key={m.id} data-role="assistant">
                <ChatMarkdown text={m.text} sources={m.sources} onOpen={open} />
                <SourceList sources={m.sources} onOpen={open} />
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
                  <ChatMarkdown text={pending.text} sources={[]} onOpen={open} />
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
        <div role="alert" className="rounded-xl bg-bad-soft p-3 text-sm text-bad">
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

      <form onSubmit={submit} className="flex items-end gap-2 rounded-xl border border-line bg-bg px-3 py-2 focus-within:border-accent">
        <textarea
          ref={inputRef}
          rows={1}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              void ask(input);
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
          <Button size="sm" variant="primary" type="submit" disabled={!input.trim()}>
            {t("chat.ask")}
          </Button>
        )}
      </form>

      {empty && (
        <>
          <p className="text-xs text-muted">{t("chat.empty")}</p>
          <div className="flex flex-wrap gap-2">
            {suggestions.map((s) => (
              <button key={s} type="button" onClick={() => suggest(s)} className="rounded-full border border-line px-3 py-1 text-xs text-muted hover:border-accent hover:text-accent">
                {s}
              </button>
            ))}
          </div>
        </>
      )}

      {empty && threads.length > 0 && (
        <div>
          <p className="mb-1 text-xs font-semibold tracking-wider text-muted uppercase">{t("chat.recent")}</p>
          <ul className="flex flex-col">
            {threads.map((th) => (
              <li key={th.id} className="group flex items-center gap-2">
                <button type="button" onClick={() => void openThread(th.id)} className="min-w-0 flex-1 truncate rounded-lg px-2 py-1.5 text-left text-sm hover:bg-sunken">
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
        </div>
      )}
    </section>
  );
}
