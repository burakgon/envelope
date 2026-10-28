<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../../api';
  import type { Account, AutoReply, AutoReplyAudience, AutoReplyInfo } from '../../api';
  import { Away, Spinner } from '../../icons';
  import { errorMessage, toasts } from '../../stores/toasts.svelte';
  import { autoReplies } from '../../stores/autoReplies.svelte';
  import DatePicker from '../calendar/DatePicker.svelte';
  import Toggle from './Toggle.svelte';

  let { account, onclose }: { account: Account; onclose: () => void } = $props();

  let info = $state<AutoReplyInfo | null>(null);
  let reply = $state<AutoReply | null>(null);
  let scheduled = $state(false);
  let start = $state('');
  let end = $state('');
  let failure = $state<string | null>(null);
  let saving = $state(false);

  /** `YYYY-MM-DD` of today plus `days`, in local time. */
  function day(days: number): string {
    const d = new Date();
    d.setDate(d.getDate() + days);
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
  }

  onMount(async () => {
    try {
      const loaded = await api.autoReply.get(account.id);
      info = loaded;
      reply = structuredClone(loaded.reply);
      scheduled = !!(loaded.reply.start && loaded.reply.end);
      start = loaded.reply.start ?? day(0);
      end = loaded.reply.end ?? day(7);
      autoReplies.known(account.id, loaded.reply.enabled);
    } catch (e) {
      failure = errorMessage(e);
    }
  });

  // Exchange and Microsoft 365 always answer colleagues; their choice is about outsiders.
  const organizational = $derived(account.kind === 'microsoft' || account.kind === 'exchange');
  const audienceLabels = $derived<Record<AutoReplyAudience, string>>({
    everyone: 'Everyone',
    contacts: organizational ? 'My organization and my contacts' : 'Only my contacts',
    organization: 'Only my organization',
  });

  function onStart() {
    if (end < start) end = start;
  }

  async function save() {
    if (!reply || saving) return;
    saving = true;
    try {
      const next: AutoReply = { ...$state.snapshot(reply), start: scheduled ? start : null, end: scheduled ? end : null };
      await api.autoReply.set(account.id, next);
      autoReplies.known(account.id, next.enabled);
      toasts.show(next.enabled ? 'Automatic replies are on' : 'Automatic replies are off', { kind: 'success' });
      onclose();
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      saving = false;
    }
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      onclose();
    }
  }
</script>

<div class="backdrop" role="presentation" onclick={onclose}>
  <div class="dialog" role="dialog" aria-label="Automatic replies" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={key}>
    <header>
      <span class="icon"><Away size={18} /></span>
      <div>
        <h3>Automatic replies</h3>
        <span class="sub">{account.email}</span>
      </div>
    </header>

    {#if failure}
      <p class="failure">{failure}</p>
      <footer><button class="btn" onclick={onclose}>Close</button></footer>
    {:else if !reply || !info}
      <div class="loading"><span class="spin"><Spinner size={18} /></span></div>
    {:else}
      <div class="row">
        <div class="txt"><b>Send automatic replies</b><span>Answer incoming mail while you are away.</span></div>
        <Toggle checked={reply.enabled} onchange={(v) => (reply!.enabled = v)} />
      </div>

      <div class="body" class:off={!reply.enabled}>
        <label class="check">
          <input type="checkbox" bind:checked={scheduled} />
          Only during these days
        </label>
        {#if scheduled}
          <div class="dates">
            <DatePicker bind:value={start} onchange={onStart} />
            <span class="dash">–</span>
            <DatePicker bind:value={end} onchange={() => { if (end < start) start = end; }} />
          </div>
        {/if}

        {#if info.subject}
          <label class="label" for="ar-subject">Subject</label>
          <input id="ar-subject" class="input" placeholder="Out of office" bind:value={reply.subject} />
        {/if}

        <label class="label" for="ar-message">Message</label>
        <textarea
          id="ar-message"
          class="input message"
          rows="6"
          placeholder="I'm away until Monday and will reply when I'm back."
          bind:value={reply.message}
        ></textarea>

        {#if info.audiences.length > 1}
          <label class="label" for="ar-audience">Reply to</label>
          <select id="ar-audience" class="input" bind:value={reply.audience}>
            {#each info.audiences as a (a)}
              <option value={a}>{audienceLabels[a]}</option>
            {/each}
          </select>
        {/if}
      </div>

      <footer>
        <button class="btn" onclick={onclose}>Cancel</button>
        <button class="btn primary" disabled={saving || (reply.enabled && !reply.message.trim())} onclick={save}>
          {#if saving}<span class="spin"><Spinner size={14} /></span>{/if}Save
        </button>
      </footer>
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1400;
    background: var(--overlay);
    display: grid;
    place-items: center;
    animation: fade-in var(--dur-fast);
  }
  .dialog {
    width: min(480px, calc(100vw - 32px));
    max-height: calc(100vh - 48px);
    overflow: auto;
    padding: 20px;
    border-radius: 16px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    animation: pop-in var(--dur) var(--ease-out);
  }
  header {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 16px;
  }
  .icon {
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    border-radius: 10px;
    color: var(--accent);
    background: color-mix(in oklab, var(--accent) 14%, transparent);
  }
  h3 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 650;
  }
  .sub {
    color: var(--text-3);
    font-size: var(--text-sm);
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 12px 14px;
    border-radius: 12px;
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .txt {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .txt span {
    color: var(--text-3);
    font-size: var(--text-sm);
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 14px;
    transition: opacity var(--dur-fast);
  }
  .body.off {
    opacity: 0.55;
  }
  .label {
    margin-top: 8px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 500;
    cursor: pointer;
  }
  .check input {
    accent-color: var(--accent);
    width: 16px;
    height: 16px;
  }
  .dates {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .dash {
    color: var(--text-3);
  }
  .message {
    height: auto;
    min-height: 120px;
    padding: 10px 12px;
    line-height: 1.5;
    resize: vertical;
  }
  .loading {
    display: grid;
    place-items: center;
    height: 120px;
    color: var(--text-3);
  }
  .failure {
    margin: 0;
    padding: 12px 14px;
    border-radius: 12px;
    color: var(--text-2);
    background: var(--bg-hover);
    line-height: 1.5;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 18px;
  }
  .spin {
    display: inline-grid;
    animation: spin 1s linear infinite;
  }
</style>
