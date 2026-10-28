// Simulated AI for the browser demo: streams plausible answers from the sample data.

import type * as T from './types';
import * as data from './mockdata';

function stream(text: string, onEvent?: (e: T.AiEvent) => void): T.AiRun {
  let cancelled = false;
  const promise = new Promise<string>((resolve, reject) => {
    const parts = text.match(/\S+\s*/g) ?? [text];
    let i = 0;
    const tick = () => {
      if (cancelled) return reject({ message: 'Cancelled', kind: 'cancelled' });
      if (i >= parts.length) {
        onEvent?.({ type: 'done', text });
        return resolve(text);
      }
      onEvent?.({ type: 'delta', text: parts.slice(i, i + 2).join('') });
      i += 2;
      setTimeout(tick, 28);
    };
    setTimeout(tick, 350);
  });
  return { promise, cancel: () => (cancelled = true) };
}

function summary(threadId: number): string {
  const t = data.threads.find((x) => x.id === threadId);
  const ms = data.messages.filter((m) => m.threadId === threadId);
  if (!t) return 'Nothing to summarize.';
  if (/roadmap/i.test(t.subject)) {
    return '**TL;DR** — The Q4 roadmap is nearly final; the review is tomorrow at 10:00.\n\n- **Headline:** offline search, as Alex suggested.\n- **Moved to Q1:** calendar sharing, until the sync rewrite lands (Daniel).\n- Priya updated the doc with everyone’s notes.\n\n**Action items**\n- Alex: review the updated doc before **tomorrow 10:00**.';
  }
  if (/offsite/i.test(t.subject)) {
    return '**TL;DR** — The team offsite is in **Cappadocia, 21–23 November**; flights are booked.\n\n- Everyone except Priya preferred Cappadocia over the Black Forest.\n- Olivia attached the itinerary.\n\n**Action items**\n- Alex: book the hot-air balloon ride (you volunteered).';
  }
  if (/onboarding/i.test(t.subject)) {
    return '**TL;DR** — Maya shared the first pass of the new onboarding and wants feedback **before Friday**.\n\n- Account setup is now a single screen with provider auto-detection.\n- Open questions: when to show the AI step, and whether dark mode should follow the system.\n\n**Action items**\n- Alex: send notes to Maya by Friday.';
  }
  const last = ms[ms.length - 1];
  return `**TL;DR** — ${last?.from?.name ?? 'The sender'} wrote about “${t.subject}”.\n\n- ${t.snippet.slice(0, 140)}${t.snippet.length > 140 ? '…' : ''}`;
}

export function mockAiRun(task: T.AiTask, onEvent?: (e: T.AiEvent) => void): T.AiRun {
  switch (task.kind) {
    case 'summarize':
      return stream(summary(task.threadId), onEvent);
    case 'quickReplies': {
      const replies = [
        { label: 'Sounds good', text: 'Sounds good — thanks for the update!' },
        { label: 'I’ll take a look', text: 'Thanks! I’ll take a look today and get back to you with notes.' },
        { label: 'Can we talk?', text: 'Thanks for sending this. Do you have 15 minutes tomorrow to go through it together?' },
      ];
      return stream(JSON.stringify(replies), (e) => e.type === 'done' && onEvent?.(e));
    }
    case 'draftReply':
      return stream(`Hi,\n\nThanks for the note — ${task.instruction.toLowerCase().includes('decline') ? 'unfortunately I won’t be able to make it this time, but please keep me posted' : 'that works for me, and I’ll follow up with details shortly'}.\n\nBest,\nAlex`, onEvent);
    case 'compose':
      return stream(JSON.stringify({ subject: 'Quick update', body: `Hi,\n\n${task.instruction}\n\nLet me know what you think.\n\nBest,\nAlex` }), (e) => e.type === 'done' && onEvent?.(e));
    case 'rewrite': {
      const t = task.text.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ').trim();
      const styled = task.style === 'shorter' ? t.split('. ').slice(0, 2).join('. ') : task.style === 'formal' ? `Dear colleague,\n\n${t}\n\nKind regards,\nAlex` : t;
      return stream(styled || 'Nothing to rewrite.', onEvent);
    }
    case 'translate':
      return stream(`[${task.language}] ${task.text.replace(/<[^>]+>/g, ' ').trim()}`, onEvent);
    case 'searchQuery':
      return stream(task.question.toLowerCase().includes('attachment') ? 'has:attachment' : task.question, (e) => e.type === 'done' && onEvent?.(e));
    case 'extractEvents':
      return stream('[]', (e) => e.type === 'done' && onEvent?.(e));
    case 'ask': {
      const q = task.question.toLowerCase();
      const words = q.split(/\W+/).filter((w) => w.length > 3);
      const score = (t: T.ThreadSummary) => words.filter((w) => (t.subject + ' ' + t.snippet).toLowerCase().includes(w)).length;
      const hits = data.threads.filter((t) => score(t) > 0).sort((a, b) => score(b) - score(a)).slice(0, 4);
      const sources: T.AiSource[] = hits.map((t, i) => {
        const m = data.messages.filter((x) => x.threadId === t.id).pop()!;
        return { index: i + 1, messageId: m.id, threadId: t.id, subject: t.subject, from: m.from?.name ?? m.from?.email ?? '', date: m.date };
      });
      onEvent?.({ type: 'sources', sources });
      let answer: string;
      if (/flight|berlin|trip/.test(q)) answer = 'Your flight to Berlin is **Thursday, 14 November**, departing Istanbul (IST) at **09:40** and arriving at **11:55** — seat **12A**. The return is on Sunday, 17 November at 18:20 [1]. The booking reference is **SKY7Q2** [1].';
      else if (/offsite|cappadocia/.test(q)) answer = 'The offsite is in **Cappadocia** from **21–23 November**; flights are booked and Olivia shared the itinerary [1].';
      else if (hits.length) answer = `I found ${hits.length} related conversation${hits.length > 1 ? 's' : ''}. The most relevant is “${hits[0].subject}” [1].`;
      else answer = 'I couldn’t find anything about that in your mail.';
      return stream(answer, onEvent);
    }
  }
}

export const mockPresets: T.AiPreset[] = [
  { id: 'openrouter', name: 'OpenRouter', kind: 'OpenAI-compatible', baseUrl: 'https://openrouter.ai/api/v1', requiresKey: true, keyUrl: 'https://openrouter.ai/keys', defaultModel: 'openrouter/auto', local: false },
  { id: 'openai', name: 'OpenAI', kind: 'OpenAI-compatible', baseUrl: 'https://api.openai.com/v1', requiresKey: true, keyUrl: 'https://platform.openai.com/api-keys', defaultModel: null, local: false },
  { id: 'anthropic', name: 'Anthropic', kind: 'Anthropic', baseUrl: 'https://api.anthropic.com/v1', requiresKey: true, keyUrl: 'https://console.anthropic.com/settings/keys', defaultModel: null, local: false },
  { id: 'gemini', name: 'Google Gemini', kind: 'Google Gemini', baseUrl: 'https://generativelanguage.googleapis.com/v1beta', requiresKey: true, keyUrl: 'https://aistudio.google.com/apikey', defaultModel: null, local: false },
  { id: 'ollama', name: 'Ollama', kind: 'OpenAI-compatible', baseUrl: 'http://localhost:11434/v1', requiresKey: false, keyUrl: null, defaultModel: null, local: true },
  { id: 'lmstudio', name: 'LM Studio', kind: 'OpenAI-compatible', baseUrl: 'http://localhost:1234/v1', requiresKey: false, keyUrl: null, defaultModel: null, local: true },
];
