<script lang="ts">
  import { onMount } from 'svelte';
  import type { AiRun, DraftInput } from '../api';
  import { ai } from '../stores/ai.svelte';
  import { mail } from '../stores/mail.svelte';
  import { ui } from '../stores/ui.svelte';
  import { errorMessage } from '../stores/toasts.svelte';
  import Popover from './Popover.svelte';
  import { ArrowRight, Languages, Refresh, Sparkles, Wand, X } from '../icons';

  let {
    x,
    y,
    draft,
    currentText,
    onclose,
    onapply,
    onsubject,
  }: {
    x: number;
    y: number;
    draft: DraftInput;
    currentText: string;
    onclose: () => void;
    onapply: (text: string, replace: boolean) => void;
    onsubject: (s: string) => void;
  } = $props();

  let instruction = $state('');
  let result = $state('');
  let busy = $state(false);
  let error = $state('');
  let replace = $state(false);
  let showLangs = $state(false);
  let input: HTMLInputElement | undefined = $state();
  let run: AiRun | null = null;
  let lastAction: (() => void) | null = null;

  const plain = $derived(currentText.replace(/<div class="envelope-signature">[\s\S]*$/, '').replace(/<[^>]+>/g, ' ').replace(/&nbsp;/g, ' ').replace(/\s+/g, ' ').trim());
  const hasText = $derived(plain.length > 0);
  const isReply = $derived(!!draft.replyToMessageId && draft.mode !== 'forward');
  const threadId = $derived(isReply ? (mail.detail?.messages.find((m) => m.id === draft.replyToMessageId)?.threadId ?? mail.detail?.thread.id ?? null) : null);

  onMount(() => input?.focus());

  const suggestions = $derived(
    isReply
      ? ['Say yes and thank them', 'Politely decline', 'Ask for more details', 'Propose another time']
      : ['Follow up on our last conversation', 'Introduce myself briefly', 'Ask for a quick call this week'],
  );

  async function start(fn: () => AiRun, isReplace: boolean, onDone?: (t: string) => string) {
    run?.cancel();
    result = '';
    error = '';
    busy = true;
    replace = isReplace;
    run = fn();
    try {
      const final = await run.promise;
      result = onDone ? onDone(final) : final;
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  function write(text = instruction) {
    if (!text.trim()) return;
    lastAction = () => write(text);
    if (isReply && threadId != null) {
      start(() => ai.run({ kind: 'draftReply', threadId, instruction: text }, (e) => e.type === 'delta' && (result += e.text)), false);
    } else {
      const recipients = draft.to.map((a) => (a.name ? `${a.name} <${a.email}>` : a.email));
      start(() => ai.run({ kind: 'compose', instruction: text, recipients }), false, (out) => {
        try {
          const parsed = JSON.parse(out) as { subject: string; body: string };
          if (parsed.subject) onsubject(parsed.subject);
          return parsed.body;
        } catch {
          return out;
        }
      });
    }
  }

  function rewrite(style: string) {
    lastAction = () => rewrite(style);
    start(() => ai.run({ kind: 'rewrite', text: plain, style }, (e) => e.type === 'delta' && (result += e.text)), true);
  }

  function translate(language: string) {
    showLangs = false;
    lastAction = () => translate(language);
    start(() => ai.run({ kind: 'translate', text: plain, language }, (e) => e.type === 'delta' && (result += e.text)), true);
  }

  const styles = [
    { id: 'improve', label: 'Improve' },
    { id: 'shorter', label: 'Shorter' },
    { id: 'longer', label: 'Longer' },
    { id: 'formal', label: 'More formal' },
    { id: 'friendly', label: 'Friendlier' },
    { id: 'fixGrammar', label: 'Fix grammar' },
  ];
  const languages = ['English', 'Türkçe', 'Deutsch', 'Español', 'Français', 'Italiano', 'Português', '日本語', '中文', 'Русский', 'العربية'];
</script>

<Popover x={x} y={y - 8} {onclose} width={400}>
  {#if !ai.ready}
    <div class="setup">
      <Sparkles size={20} />
      <div>
        <b>Write faster with AI</b>
        <p>Connect OpenRouter, OpenAI, Anthropic, Gemini or a local model like Ollama.</p>
        <button class="btn sm primary" onclick={() => { onclose(); ui.open('settings', 'ai'); }}>Set up AI</button>
      </div>
    </div>
  {:else}
    <form class="ask" onsubmit={(e) => { e.preventDefault(); write(); }}>
      <Sparkles size={16} />
      <input bind:this={input} bind:value={instruction} placeholder={isReply ? 'Describe your reply…' : 'What should I write?'} />
      <button class="go" type="submit" disabled={!instruction.trim() || busy} aria-label="Write"><ArrowRight size={15} /></button>
    </form>

    {#if !result && !busy && !error}
      <div class="chips">
        {#each suggestions as s (s)}
          <button class="chip-btn" onclick={() => write(s)}>{s}</button>
        {/each}
      </div>
      {#if hasText}
        <div class="menu-label">Rewrite your text</div>
        <div class="chips">
          {#each styles as s (s.id)}
            <button class="chip-btn" onclick={() => rewrite(s.id)}><Wand size={12} />{s.label}</button>
          {/each}
          <button class="chip-btn" onclick={() => (showLangs = !showLangs)}><Languages size={12} />Translate</button>
        </div>
        {#if showLangs}
          <div class="chips">
            {#each languages as l (l)}
              <button class="chip-btn" onclick={() => translate(l)}>{l}</button>
            {/each}
          </div>
        {/if}
      {/if}
    {/if}

    {#if busy || result || error}
      <div class="result" class:error={!!error}>
        {#if error}
          {error}
        {:else if result}
          <div class="text selectable">{result}</div>
        {:else}
          <div class="thinking"><span></span><span></span><span></span></div>
        {/if}
      </div>
      <div class="actions">
        {#if busy}
          <button class="btn sm" onclick={() => run?.cancel()}><X size={13} />Stop</button>
        {:else}
          <button class="btn sm ghost" onclick={() => lastAction?.()}><Refresh size={13} />Try again</button>
          <span class="grow"></span>
          <button class="btn sm" onclick={() => { result = ''; error = ''; }}>Discard</button>
          {#if result}
            <button class="btn sm primary" onclick={() => onapply(result, replace)}>{replace ? 'Replace' : 'Insert'}</button>
          {/if}
        {/if}
      </div>
    {/if}
  {/if}
</Popover>

<style>
  .setup {
    display: flex;
    gap: 12px;
    padding: 10px;
    color: var(--accent-text);
  }
  .setup b {
    color: var(--text);
  }
  .setup p {
    margin: 4px 0 10px;
    color: var(--text-2);
    line-height: 1.45;
  }
  .ask {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 4px 8px 10px;
    color: var(--accent-text);
  }
  .ask input {
    flex: 1;
    height: 30px;
    border: none;
    background: none;
    font-size: var(--text-md);
    color: var(--text);
  }
  .go {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 8px;
    background: var(--accent);
    color: var(--accent-fg);
  }
  .go:disabled {
    opacity: 0.4;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    padding: 2px 6px 8px;
  }
  .chip-btn {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 26px;
    padding: 0 10px;
    border-radius: 999px;
    font-size: 12px;
    font-weight: 550;
    color: var(--text-2);
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .chip-btn:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
  .result {
    margin: 2px 6px 8px;
    padding: 10px 12px;
    border-radius: 10px;
    background: var(--bg-sunken);
    max-height: 260px;
    overflow: auto;
    line-height: 1.55;
  }
  .result.error {
    background: var(--red-soft);
    color: var(--red);
  }
  .text {
    white-space: pre-wrap;
  }
  .actions {
    display: flex;
    gap: 6px;
    padding: 0 6px 6px;
  }
  .grow {
    flex: 1;
  }
  .thinking {
    display: flex;
    gap: 5px;
  }
  .thinking span {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
    animation: pulse 1s infinite ease-in-out;
  }
  .thinking span:nth-child(2) {
    animation-delay: 0.15s;
  }
  .thinking span:nth-child(3) {
    animation-delay: 0.3s;
  }
  @keyframes pulse {
    50% {
      opacity: 0.3;
      transform: scale(0.7);
    }
  }
</style>
