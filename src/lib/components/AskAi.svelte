<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import type { AiRun, AiSource } from '../api';
  import { ai } from '../stores/ai.svelte';
  import { mail } from '../stores/mail.svelte';
  import { ui } from '../stores/ui.svelte';
  import { errorMessage } from '../stores/toasts.svelte';
  import { listDate } from '../utils/format';
  import Markdown from './Markdown.svelte';
  import { ArrowRight, Mail, Sparkles, X } from '../icons';
  import { t } from '../i18n';
  import { modal } from '../actions/modal';

  let question = $state(ui.askAi);
  let asked = $state('');
  let answer = $state('');
  let sources = $state<AiSource[]>([]);
  let busy = $state(false);
  let error = $state('');
  let input: HTMLInputElement | undefined = $state();
  let run: AiRun | null = null;

  const examples = $derived([t('askAi.exampleFlight'), t('askAi.exampleDecision'), t('askAi.exampleMissed'), t('askAi.exampleInvoices')]);

  onMount(() => {
    input?.focus();
    if (question.trim()) ask();
  });
  onDestroy(() => run?.cancel());

  async function ask(q = question) {
    if (!q.trim()) return;
    if (!ai.ready) {
      error = t('askAi.notSetUp');
      return;
    }
    run?.cancel();
    question = q;
    asked = q.trim();
    answer = '';
    sources = [];
    error = '';
    busy = true;
    run = ai.run({ kind: 'ask', question: asked }, (e) => {
      if (e.type === 'delta') answer += e.text;
      else if (e.type === 'sources') sources = e.sources;
    });
    try {
      answer = await run.promise;
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  function open(s: AiSource) {
    ui.close();
    ui.view = 'mail';
    mail.showConversation(s.threadId);
  }
</script>

<div class="backdrop" role="presentation" onclick={() => ui.close()}>
  <div class="panel" role="dialog" aria-label={t('askAi.title')} tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === 'Escape' && ui.close()} use:modal>
    <form class="q" onsubmit={(e) => { e.preventDefault(); ask(); }}>
      <span class="spark"><Sparkles size={18} /></span>
      <input bind:this={input} bind:value={question} placeholder={t('askAi.placeholder')} aria-label={t('askAi.title')} />
      <button class="go" type="submit" disabled={!question.trim() || busy} aria-label={t('askAi.ask')}><ArrowRight size={16} /></button>
      <button class="icon-btn" type="button" aria-label={t('common.close')} onclick={() => ui.close()}><X size={17} /></button>
    </form>

    <div class="body">
      {#if !asked}
        <p class="intro">{t('askAi.intro')}</p>
        <div class="examples">
          {#each examples as ex (ex)}
            <button onclick={() => ask(ex)}>{ex}</button>
          {/each}
        </div>
      {:else}
        <div class="asked">{asked}</div>
        {#if error}
          <p class="error">{error}</p>
        {:else if answer}
          <Markdown text={answer} streaming={busy} oncite={(n) => { const s = sources.find((x) => x.index === n); if (s) open(s); }} />
        {:else}
          <div class="thinking"><span></span><span></span><span></span> {t('askAi.searching')}</div>
        {/if}
        {#if sources.length}
          <div class="sources">
            <div class="label">{t('askAi.sources')}</div>
            {#each sources as s (s.index)}
              <button class="src" onclick={() => open(s)}>
                <span class="n">{s.index}</span>
                <Mail size={14} />
                <span class="subj truncate">{s.subject || t('common.noSubject')}</span>
                <span class="from truncate">{s.from}</span>
                <span class="date">{listDate(s.date)}</span>
              </button>
            {/each}
          </div>
        {/if}
      {/if}
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1500;
    background: var(--overlay);
    display: flex;
    justify-content: center;
    padding-top: 10vh;
    animation: fade-in var(--dur-fast);
  }
  .panel {
    align-self: flex-start;
    width: min(720px, 92vw);
    max-height: 78vh;
    display: flex;
    flex-direction: column;
    border-radius: 18px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
    animation: pop-in var(--dur) var(--ease-out);
  }
  .q {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 12px 0 16px;
    height: 60px;
    border-bottom: 1px solid var(--border);
  }
  .spark {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 10px;
    color: #fff;
    background: linear-gradient(135deg, oklch(0.62 0.22 var(--accent-h)), oklch(0.62 0.22 calc(var(--accent-h) + 50)));
  }
  .q input {
    flex: 1;
    border: none;
    background: none;
    font-size: var(--text-lg);
    height: 100%;
  }
  .go {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 9px;
    background: var(--accent-solid);
    color: var(--accent-fg);
  }
  .go:disabled {
    opacity: 0.4;
  }
  .body {
    overflow-y: auto;
    padding: 18px 22px 22px;
  }
  .intro {
    margin: 0 0 14px;
    color: var(--text-2);
    line-height: 1.55;
  }
  .examples {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .examples button {
    min-height: 32px;
    padding: 6px 14px;
    border-radius: 999px;
    color: var(--text-2);
    font-weight: 500;
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .examples button:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
  .asked {
    font-size: var(--text-lg);
    font-weight: 650;
    margin-bottom: 12px;
    letter-spacing: -0.01em;
  }
  .error {
    color: var(--red);
  }
  .thinking {
    display: flex;
    align-items: center;
    gap: 5px;
    color: var(--text-3);
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
    margin-right: 6px;
  }
  @keyframes pulse {
    50% {
      opacity: 0.3;
      transform: scale(0.7);
    }
  }
  .sources {
    margin-top: 18px;
    display: grid;
    gap: 4px;
  }
  .label {
    font-size: 11px;
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-3);
    margin-bottom: 4px;
  }
  .src {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 36px;
    padding: 0 10px;
    border-radius: 9px;
    color: var(--text-2);
    background: var(--bg-sunken);
    text-align: left;
  }
  .src:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .n {
    display: grid;
    place-items: center;
    min-width: 18px;
    height: 18px;
    border-radius: 6px;
    font-size: 11px;
    font-weight: 700;
    color: var(--accent-text);
    background: var(--accent-soft);
  }
  .subj {
    flex: 1;
    min-width: 0;
    font-weight: 550;
    color: var(--text);
  }
  .from {
    max-width: 160px;
    color: var(--text-3);
  }
  .date {
    color: var(--text-3);
    font-size: var(--text-sm);
    flex-shrink: 0;
  }
</style>
