import React, { useEffect, useState } from 'react';

/** Mirrors `runner::Step` in crates/agent. */
export type AgentStep =
  | { kind: 'thinking'; text: string; done: boolean }
  | { kind: 'text'; text: string }
  | { kind: 'tool'; id?: string; name: string; label: string; detail?: string; state: 'running' | 'done' | 'failed'; started_at?: number; finished_at?: number };

type ToolStep = Extract<AgentStep, { kind: 'tool' }>;

/* ---------- Markdown: a small, safe subset rendered as React elements ---------- */

type ModelLinkProps = {
  resolveModel?: (path: string) => { entry: string; builder: string } | undefined;
  onOpenModel?: (entry: string, builder: string) => void;
};

function inline(text: string, key: string, link?: ModelLinkProps): React.ReactNode[] {
  // `code`, **bold**, *em* / _em_, [label](url) (label only; links never navigate the app).
  const pattern = /(`[^`\n]+`|\*\*[^*\n]+\*\*|\*[^*\s][^*\n]*\*|_[^_\s][^_\n]*_|\[[^\]\n]+\]\([^)\s]+\))/g;
  const out: React.ReactNode[] = [];
  let last = 0, match: RegExpExecArray | null, index = 0;
  while ((match = pattern.exec(text))) {
    if (match.index > last) out.push(text.slice(last, match.index));
    const token = match[0], k = `${key}-${index++}`;
    if (token.startsWith('`')) {
      const code = token.slice(1, -1);
      const target = link?.onOpenModel && link.resolveModel ? link.resolveModel(code) : undefined;
      if (target && link?.onOpenModel) {
        out.push(<button type="button" key={k} className="md-model-link" title={`Open ${target.entry}`} onClick={() => link.onOpenModel!(target.entry, target.builder)}><code>{code}</code></button>);
      } else {
        out.push(<code key={k}>{code}</code>);
      }
    }
    else if (token.startsWith('**')) out.push(<strong key={k}>{inline(token.slice(2, -2), k, link)}</strong>);
    else if (token.startsWith('[')) out.push(<span key={k} className="md-link">{token.slice(1, token.indexOf(']'))}</span>);
    else out.push(<em key={k}>{inline(token.slice(1, -1), k, link)}</em>);
    last = match.index + token.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

export function Markdown({ text, resolveModel, onOpenModel }: { text: string } & ModelLinkProps) {
  const link = resolveModel && onOpenModel ? { resolveModel, onOpenModel } : undefined;
  const lines = text.replace(/\r\n?/g, '\n').split('\n');
  const blocks: React.ReactNode[] = [];
  let i = 0, key = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) { i++; continue; }
    const fence = line.match(/^\s*```(\w*)/);
    if (fence) {
      const body: string[] = [];
      for (i++; i < lines.length && !/^\s*```/.test(lines[i]); i++) body.push(lines[i]);
      i++;
      blocks.push(<pre key={key++} className="md-code"><code>{body.join('\n')}</code></pre>);
      continue;
    }
    const heading = line.match(/^(#{1,4})\s+(.*)$/);
    if (heading) {
      blocks.push(<p key={key++} className={`md-heading md-h${heading[1].length}`}>{inline(heading[2], `h${key}`, link)}</p>);
      i++; continue;
    }
    if (/^\s*([-*•]|\d+[.)])\s+/.test(line)) {
      const ordered = /^\s*\d+[.)]\s+/.test(line), items: string[] = [];
      while (i < lines.length && /^\s*([-*•]|\d+[.)])\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*([-*•]|\d+[.)])\s+/, ''));
        i++;
        // Indented continuation lines belong to the item.
        while (i < lines.length && /^\s{2,}\S/.test(lines[i]) && !/^\s*([-*•]|\d+[.)])\s+/.test(lines[i])) items[items.length - 1] += ' ' + lines[i++].trim();
      }
      const children = items.map((item, n) => <li key={n}>{inline(item, `l${key}-${n}`, link)}</li>);
      blocks.push(ordered ? <ol key={key++}>{children}</ol> : <ul key={key++}>{children}</ul>);
      continue;
    }
    const paragraph: string[] = [];
    while (i < lines.length && lines[i].trim() && !/^\s*```/.test(lines[i]) && !/^#{1,4}\s/.test(lines[i]) && !/^\s*([-*•]|\d+[.)])\s+/.test(lines[i])) paragraph.push(lines[i++]);
    blocks.push(<p key={key++}>{paragraph.flatMap((part, n) => n ? [<br key={`br${n}`} />, ...inline(part, `p${key}-${n}`, link)] : inline(part, `p${key}-${n}`, link))}</p>);
  }
  return <div className="markdown">{blocks}</div>;
}

/* ---------- Steps ---------- */

const tail = (text: string, size: number) => {
  const clean = text.replace(/\s+/g, ' ').trim();
  return clean.length > size ? `…${clean.slice(clean.length - size)}` : clean;
};

/** How long a call ran: 420ms, 3.4s, 1m 12s. */
function duration(ms: number) {
  const safe = Math.max(0, ms);
  if (safe < 1000) return `${Math.round(safe)}ms`;
  const seconds = safe / 1000;
  if (seconds < 60) return `${seconds < 10 ? seconds.toFixed(1) : Math.round(seconds)}s`;
  return `${Math.floor(seconds / 60)}m ${Math.round(seconds % 60)}s`;
}

/** Ticks while a tool is running so its elapsed time stays current. */
function useNow(active: boolean) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(timer);
  }, [active]);
  return now;
}

function ToolRow({ step, now }: { step: ToolStep; now: number }) {
  const started = step.started_at ?? 0;
  const finished = step.finished_at ?? (step.state === 'running' ? now : started);
  const took = started ? duration(finished - started) : '';
  return <li className={`step-tool is-${step.state}`} title={step.name}>
    <span className="step-icon" aria-hidden="true">{step.state === 'done' ? '✓' : step.state === 'failed' ? '!' : ''}</span>
    <span className="step-label">{step.label}{step.state === 'running' ? '…' : ''}</span>
    {took && <span className="step-time">{took}</span>}
    {step.detail && <span className="step-detail">{step.detail}</span>}
    <span className="visually-hidden">{step.state === 'running' ? ' (in progress)' : step.state === 'failed' ? ' (needs attention)' : ' (done)'}</span>
  </li>;
}

function Thinking({ step, live }: { step: Extract<AgentStep, { kind: 'thinking' }>; live: boolean }) {
  if (live && !step.done) return <div className="step-thinking is-active" role="status">
    <span className="thinking-label">Thinking</span>
    {step.text.trim() && <p className="thinking-hint">{tail(step.text, 180)}</p>}
  </div>;
  if (!step.text.trim()) return null;
  return <details className="step-thinking"><summary>Thought</summary><p>{step.text.trim()}</p></details>;
}

/** Thinking, narration and tool calls in the order they happened. */
export function StepList({ steps, live, resolveModel, onOpenModel }: { steps: AgentStep[]; live: boolean } & ModelLinkProps) {
  const now = useNow(live && steps.some(step => step.kind === 'tool' && step.state === 'running'));
  const out: React.ReactNode[] = [];
  for (let i = 0; i < steps.length; i++) {
    const step = steps[i];
    if (step.kind === 'tool') {
      const group: ToolStep[] = [];
      while (i < steps.length && steps[i].kind === 'tool') group.push(steps[i++] as ToolStep);
      i--;
      out.push(<ul key={`tools${i}`} className="step-tools">{group.map((tool, n) => <ToolRow key={tool.id ?? `${i}-${n}`} step={tool} now={now} />)}</ul>);
    } else if (step.kind === 'thinking') out.push(<Thinking key={`think${i}`} step={step} live={live} />);
    else if (step.text.trim()) out.push(<Markdown key={`text${i}`} text={step.text} resolveModel={resolveModel} onOpenModel={onOpenModel} />);
  }
  return <div className="agent-steps">{out}</div>;
}

/** A finished reply: the answer up front, with what the agent did tucked away. */
export function FinishedReply({ text, steps, resolveModel, onOpenModel }: { text: string; steps?: AgentStep[] } & ModelLinkProps) {
  const all = steps ?? [];
  const tools = all.filter((step): step is ToolStep => step.kind === 'tool');
  const failed = tools.filter(step => step.state === 'failed').length;
  // The agent's last text segment is the answer. Everything before it keeps its
  // place in the timeline, so narration between tool calls stays in order.
  let answerAt = -1;
  if (tools.length) answerAt = all.reduce((found, step, i) => (step.kind === 'text' && !!step.text.trim() ? i : found), -1);
  const answerStep = answerAt >= 0 ? all[answerAt] : undefined;
  const timeline = answerAt < 0 ? all.filter(step => step.kind !== 'text') : [...all.slice(0, answerAt), ...all.slice(answerAt + 1)];
  const answer = answerStep?.kind === 'text' ? answerStep.text : text;
  return <>
    {!!timeline.length && <details className="step-summary">
      <summary>{tools.length ? `${tools.length} ${tools.length === 1 ? 'step' : 'steps'}` : 'Thought process'}{failed ? ` · ${failed} needed another try` : ''}</summary>
      <StepList steps={timeline} live={false} resolveModel={resolveModel} onOpenModel={onOpenModel} />
    </details>}
    <Markdown text={answer} resolveModel={resolveModel} onOpenModel={onOpenModel} />
  </>;
}

function useElapsed(since?: number) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => { const timer = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(timer); }, []);
  if (!since) return '';
  const seconds = Math.max(0, Math.round((now - since) / 1000));
  return seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

/** The in-progress reply: live steps plus a one-line hint of what is happening now. */
export function LiveReply({ steps, activity, startedAt, stopping, resolveModel, onOpenModel }: { steps: AgentStep[]; activity: string; startedAt?: number; stopping?: boolean } & ModelLinkProps) {
  const elapsed = useElapsed(startedAt);
  const last = steps[steps.length - 1];
  // The open thinking step already says "Thinking"; avoid repeating it below.
  const hint = stopping ? 'Stopping' : last?.kind === 'thinking' && !last.done ? '' : activity || 'Working';
  return <>
    <StepList steps={steps} live resolveModel={resolveModel} onOpenModel={onOpenModel} />
    {hint && <p className="agent-status" role="status"><i aria-hidden="true" />{hint.replace(/…$/, '')}…{elapsed && <span>{elapsed}</span>}</p>}
  </>;
}
