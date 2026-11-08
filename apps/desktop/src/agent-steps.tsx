import React, { useEffect, useState } from 'react';

/** Mirrors `runner::Step` in crates/agent. */
export type AgentStep =
  | { kind: 'thinking'; text: string; done: boolean }
  | { kind: 'text'; text: string }
  | { kind: 'tool'; id?: string; name: string; label: string; detail?: string; state: 'running' | 'done' | 'failed' };

type ToolStep = Extract<AgentStep, { kind: 'tool' }>;

/* ---------- Markdown: a small, safe subset rendered as React elements ---------- */

function inline(text: string, key: string): React.ReactNode[] {
  // `code`, **bold**, *em* / _em_, [label](url) (label only; links never navigate the app).
  const pattern = /(`[^`\n]+`|\*\*[^*\n]+\*\*|\*[^*\s][^*\n]*\*|_[^_\s][^_\n]*_|\[[^\]\n]+\]\([^)\s]+\))/g;
  const out: React.ReactNode[] = [];
  let last = 0, match: RegExpExecArray | null, index = 0;
  while ((match = pattern.exec(text))) {
    if (match.index > last) out.push(text.slice(last, match.index));
    const token = match[0], k = `${key}-${index++}`;
    if (token.startsWith('`')) out.push(<code key={k}>{token.slice(1, -1)}</code>);
    else if (token.startsWith('**')) out.push(<strong key={k}>{inline(token.slice(2, -2), k)}</strong>);
    else if (token.startsWith('[')) out.push(<span key={k} className="md-link">{token.slice(1, token.indexOf(']'))}</span>);
    else out.push(<em key={k}>{inline(token.slice(1, -1), k)}</em>);
    last = match.index + token.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

export function Markdown({ text }: { text: string }) {
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
      blocks.push(<p key={key++} className={`md-heading md-h${heading[1].length}`}>{inline(heading[2], `h${key}`)}</p>);
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
      const children = items.map((item, n) => <li key={n}>{inline(item, `l${key}-${n}`)}</li>);
      blocks.push(ordered ? <ol key={key++}>{children}</ol> : <ul key={key++}>{children}</ul>);
      continue;
    }
    const paragraph: string[] = [];
    while (i < lines.length && lines[i].trim() && !/^\s*```/.test(lines[i]) && !/^#{1,4}\s/.test(lines[i]) && !/^\s*([-*•]|\d+[.)])\s+/.test(lines[i])) paragraph.push(lines[i++]);
    blocks.push(<p key={key++}>{paragraph.flatMap((part, n) => n ? [<br key={`br${n}`} />, ...inline(part, `p${key}-${n}`)] : inline(part, `p${key}-${n}`))}</p>);
  }
  return <div className="markdown">{blocks}</div>;
}

/* ---------- Steps ---------- */

const tail = (text: string, size: number) => {
  const clean = text.replace(/\s+/g, ' ').trim();
  return clean.length > size ? `…${clean.slice(clean.length - size)}` : clean;
};

function ToolRow({ step }: { step: ToolStep }) {
  return <li className={`step-tool is-${step.state}`} title={step.name}>
    <span className="step-icon" aria-hidden="true">{step.state === 'done' ? '✓' : step.state === 'failed' ? '!' : ''}</span>
    <span className="step-label">{step.label}{step.state === 'running' ? '…' : ''}</span>
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
export function StepList({ steps, live }: { steps: AgentStep[]; live: boolean }) {
  const out: React.ReactNode[] = [];
  for (let i = 0; i < steps.length; i++) {
    const step = steps[i];
    if (step.kind === 'tool') {
      const group: ToolStep[] = [];
      while (i < steps.length && steps[i].kind === 'tool') group.push(steps[i++] as ToolStep);
      i--;
      out.push(<ul key={`tools${i}`} className="step-tools">{group.map((tool, n) => <ToolRow key={tool.id ?? `${i}-${n}`} step={tool} />)}</ul>);
    } else if (step.kind === 'thinking') out.push(<Thinking key={`think${i}`} step={step} live={live} />);
    else if (step.text.trim()) out.push(<Markdown key={`text${i}`} text={step.text} />);
  }
  return <div className="agent-steps">{out}</div>;
}

/** A finished reply: the text up front, with what the agent did tucked away. */
export function FinishedReply({ text, steps }: { text: string; steps?: AgentStep[] }) {
  const work = (steps ?? []).filter(step => step.kind !== 'text');
  const tools = work.filter((step): step is ToolStep => step.kind === 'tool');
  const failed = tools.filter(step => step.state === 'failed').length;
  return <>
    {!!work.length && <details className="step-summary">
      <summary>{tools.length ? `${tools.length} ${tools.length === 1 ? 'step' : 'steps'}` : 'Thought process'}{failed ? ` · ${failed} needed another try` : ''}</summary>
      <StepList steps={work} live={false} />
    </details>}
    <Markdown text={text} />
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
export function LiveReply({ steps, activity, startedAt, stopping }: { steps: AgentStep[]; activity: string; startedAt?: number; stopping?: boolean }) {
  const elapsed = useElapsed(startedAt);
  const last = steps[steps.length - 1];
  // The open thinking step already says "Thinking"; avoid repeating it below.
  const hint = stopping ? 'Stopping' : last?.kind === 'thinking' && !last.done ? '' : activity || 'Working';
  return <>
    <StepList steps={steps} live />
    {hint && <p className="agent-status" role="status"><i aria-hidden="true" />{hint.replace(/…$/, '')}…{elapsed && <span>{elapsed}</span>}</p>}
  </>;
}
