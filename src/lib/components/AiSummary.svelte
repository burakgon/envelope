<script lang="ts">
  import type { AiRun, MessageView } from '../api';
  import { ai } from '../stores/ai.svelte';
  import { compose } from '../stores/compose.svelte';
  import { ui } from '../stores/ui.svelte';
  import { errorMessage } from '../stores/toasts.svelte';
  import Markdown from './Markdown.svelte';
  import { MessageText, Refresh, Sparkles, X } from '../icons';

  let { threadId, messages }: { threadId: number; messages: MessageView[] } = $props();

  type State = 'idle' | 'running' | 'done' | 'error';
  let phase = $state<State>('idle');
  let text = $state('');
  let error = $state('');
  let replies = $state<{ label: string; text: string }[]>([]);
  let repliesBusy = $state(false);
  let run: AiRun | null = null;
  let current = -1;

  $effect(() => {
    // Reset (or restore from cache) when the conversation changes.
    if (threadId === current) return;
    current = threadId;
    run?.cancel();
    replies = [];
    const cached = ai.summaries.get(threadId);
    if (cached) {
      text = cached;
      phase = 'done';
    } else {
      text = '';
      phase = 'idle';
    }
  });

  async function summarize() {
    const id = threadId;
    phase = 'running';
    text = '';
    error = '';
    run = ai.run({ kind: 'summarize', threadId: id }, (e) => {
      if (id !== threadId) return;
      if (e.type === 'delta') text += e.text;
    });
    try {
      const final = await run.promise;
      if (id !== threadId) return;
      text = final;
      ai.summaries.set(id, final);
      phase = 'done';
    } catch (e) {
      if (id !== threadId) return;
      error = errorMessage(e);
      phase = error === 'Cancelled' ? 'idle' : 'error';
    }
  }

  async function suggestReplies() {
    const id = threadId;
    repliesBusy = true;
    try {
      const out = await ai.run({ kind: 'quickReplies', threadId: id }).promise;
      if (id === threadId) replies = JSON.parse(out);
    } catch (e) {
      error = errorMessage(e);
    } finally {
      repliesBusy = false;
    }
  }

  async function useReply(r: { text: string }) {
    const last = messages[messages.length - 1];
    if (!last) return;
    await compose.reply(last.id, 'reply');
    const c = compose.list.find((x) => x.inline && x.threadId === threadId);
    if (c) {
      const body = r.text.split(/\n{2,}/).map((p) => `<p>${p.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/\n/g, '<br>')}</p>`).join('');
      c.data.html = body + c.data.html;
      c.focus++;
    }
  }
</script>

{#if !ai.ready}
  {#if ui.settings.aiEnabled && !ai.config?.enabled}
    <!-- AI not configured: stay out of the way. -->
  {/if}
{:else if phase === 'idle'}
  <div class="bar">
    <button class="pill" onclick={summarize}><Sparkles size={14} />Summarize</button>
    <button class="pill" onclick={suggestReplies} disabled={repliesBusy}><MessageText size={14} />{repliesBusy ? 'Thinking…' : 'Suggest replies'}</button>
    {#each replies as r (r.label)}
      <button class="reply" onclick={() => useReply(r)}>{r.label}</button>
    {/each}
  </div>
{:else}
  <div class="card" class:error={phase === 'error'}>
    <div class="head">
      <span class="title"><Sparkles size={14} />Summary</span>
      <span class="grow"></span>
      {#if phase === 'running'}
        <button class="icon-btn sm" title="Stop" onclick={() => run?.cancel()}><X size={14} /></button>
      {:else}
        <button class="icon-btn sm" title="Regenerate" onclick={summarize}><Refresh size={14} /></button>
        <button class="icon-btn sm" title="Hide" onclick={() => (phase = 'idle')}><X size={14} /></button>
      {/if}
    </div>
    {#if phase === 'error'}
      <p class="err">{error}</p>
    {:else if text}
      <Markdown {text} streaming={phase === 'running'} />
    {:else}
      <div class="thinking"><span></span><span></span><span></span></div>
    {/if}
    {#if phase === 'done'}
      <div class="replies">
        {#if replies.length === 0}
          <button class="pill" onclick={suggestReplies} disabled={repliesBusy}><MessageText size={14} />{repliesBusy ? 'Thinking…' : 'Suggest replies'}</button>
        {/if}
        {#each replies as r (r.label)}
          <button class="reply" title={r.text} onclick={() => useReply(r)}>{r.label}</button>
        {/each}
      </div>
    {/if}
  </div>
{/if}

<style>
  .bar {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin: -6px 0 16px;
  }
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 11px;
    border-radius: 999px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--accent-text);
    background: var(--accent-softer);
    box-shadow: inset 0 0 0 1px var(--accent-soft);
    transition: background var(--dur-fast);
  }
  .pill:hover {
    background: var(--accent-soft);
  }
  .pill:disabled {
    opacity: 0.7;
  }
  .reply {
    height: 28px;
    padding: 0 12px;
    border-radius: 999px;
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--text);
    background: var(--bg-elevated);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    animation: pop-in var(--dur) var(--ease-out);
  }
  .reply:hover {
    background: var(--bg-hover);
  }
  .card {
    position: relative;
    margin: -4px 0 16px;
    padding: 12px 16px 14px;
    border-radius: 14px;
    background: linear-gradient(135deg, var(--accent-softer), transparent 60%), var(--bg-elevated);
    box-shadow: inset 0 0 0 1px var(--accent-soft), var(--shadow-xs);
    animation: slide-up var(--dur) var(--ease-out);
  }
  .card.error {
    background: var(--red-soft);
    box-shadow: none;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 2px;
    margin-bottom: 6px;
  }
  .title {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11.5px;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--accent-text);
  }
  .grow {
    flex: 1;
  }
  .err {
    margin: 0;
    color: var(--red);
  }
  .replies {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 12px;
  }
  .replies:empty {
    display: none;
  }
  .thinking {
    display: flex;
    gap: 5px;
    padding: 8px 0;
  }
  .thinking span {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
    animation: bounce 1s infinite ease-in-out;
  }
  .thinking span:nth-child(2) {
    animation-delay: 0.15s;
  }
  .thinking span:nth-child(3) {
    animation-delay: 0.3s;
  }
  @keyframes bounce {
    0%,
    80%,
    100% {
      transform: scale(0.6);
      opacity: 0.4;
    }
    40% {
      transform: scale(1);
      opacity: 1;
    }
  }
</style>
