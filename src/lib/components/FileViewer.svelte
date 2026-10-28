<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../api';
  import type { FileEvent, FileMessage, MessageView, OpenedFile } from '../api';
  import { Calendar, Download, File, Mail, Reply, Spinner, X } from '../icons';
  import { cal } from '../stores/calendar.svelte';
  import { compose } from '../stores/compose.svelte';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { displayName, fileSize, longDate } from '../utils/format';
  import MailFrame from './MailFrame.svelte';

  /** A saved message or calendar file opened with Envelope. */
  let { path, onclose }: { path: string; onclose: () => void } = $props();

  let file = $state<OpenedFile | null>(null);
  let failure = $state<string | null>(null);
  let allowRemote = $state(false);
  let blocked = $state(0);
  let chosen = $state<string[]>([]);
  let calendarId = $state<number | null>(null);
  let adding = $state(false);

  const name = $derived(path.split('/').pop() ?? path);
  const writable = $derived(cal.calendars.filter((c) => !c.readOnly));

  onMount(async () => {
    try {
      const opened = await api.files.open(path);
      file = opened;
      if (opened.kind === 'calendar') {
        chosen = opened.events.filter((e) => !e.exists).map((e) => e.uid);
        if (!cal.calendars.length) await cal.loadCalendars();
        calendarId = cal.defaultCalendar()?.id ?? writable[0]?.id ?? null;
      }
    } catch (e) {
      failure = errorMessage(e);
    }
  });

  /** The message in the shape the mail frame renders. */
  function asView(m: FileMessage): MessageView {
    return {
      id: -1,
      threadId: -1,
      accountId: -1,
      folderIds: [],
      messageId: null,
      inReplyTo: null,
      references: [],
      subject: m.subject,
      from: m.from,
      to: m.to,
      cc: m.cc,
      bcc: [],
      replyTo: m.replyTo,
      date: m.date,
      flags: { seen: true, flagged: false, answered: false, draft: false, forwarded: false },
      snippet: '',
      size: 0,
      hasAttachments: m.attachments.length > 0,
      attachments: [],
      body: { html: m.html, text: m.text },
      unsubscribe: null,
      calendarMethod: null,
      isAuto: false,
    };
  }

  function reply(m: FileMessage) {
    const to = m.replyTo[0] ?? m.from;
    if (!to) return;
    const subject = /^re:/i.test(m.subject) ? m.subject : `Re: ${m.subject}`;
    compose.fromMailto(`mailto:${encodeURIComponent(to.email)}?subject=${encodeURIComponent(subject)}`);
    onclose();
  }

  async function openPart(partId: string) {
    try {
      await api.files.openPart(path, partId);
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async function savePart(partId: string, filename: string) {
    const target = await api.pickSavePath(filename);
    if (!target) return;
    try {
      await api.files.savePart(path, partId, target);
      toasts.show(`Saved ${filename}`, { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  function when(e: FileEvent): string {
    const day: Intl.DateTimeFormatOptions = { weekday: 'short', month: 'short', day: 'numeric', year: 'numeric' };
    if (e.allDay) {
      const last = new Date(e.end - 1);
      const first = new Date(e.start);
      const one = first.toDateString() === last.toDateString();
      return one ? first.toLocaleDateString(undefined, day) : `${first.toLocaleDateString(undefined, day)} – ${last.toLocaleDateString(undefined, day)}`;
    }
    const start = new Date(e.start);
    const time: Intl.DateTimeFormatOptions = { hour: 'numeric', minute: '2-digit' };
    const end = new Date(e.end);
    const sameDay = start.toDateString() === end.toDateString();
    return `${start.toLocaleDateString(undefined, day)}, ${start.toLocaleTimeString(undefined, time)} – ${
      sameDay ? end.toLocaleTimeString(undefined, time) : end.toLocaleString(undefined, { ...day, ...time })
    }`;
  }

  function toggle(uid: string, on: boolean) {
    chosen = on ? [...chosen, uid] : chosen.filter((u) => u !== uid);
  }

  async function addEvents() {
    if (calendarId == null || !chosen.length || adding) return;
    adding = true;
    try {
      const result = await api.files.importEvents(path, calendarId, chosen);
      const added = result.added === 1 ? '1 event' : `${result.added} events`;
      toasts.show(result.skipped ? `Added ${added}; ${result.skipped} already on your calendar` : `Added ${added}`, { kind: 'success' });
      cal.load();
      onclose();
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      adding = false;
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
  <div
    class="window"
    class:wide={file?.kind === 'message'}
    role="dialog"
    aria-label={name}
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    onkeydown={key}
  >
    <header>
      <span class="kind">
        {#if file?.kind === 'calendar'}<Calendar size={16} />{:else}<Mail size={16} />{/if}
      </span>
      <span class="name" title={path}>{name}</span>
      <button class="icon-btn" aria-label="Close" onclick={onclose}><X size={16} /></button>
    </header>

    {#if failure}
      <p class="failure">{failure}</p>
    {:else if !file}
      <div class="loading"><span class="spin"><Spinner size={18} /></span></div>
    {:else if file.kind === 'message'}
      {@const m = file}
      <div class="message">
        <div class="head">
          <h2>{m.subject || '(no subject)'}</h2>
          <div class="meta">
            <div>
              <b>{displayName(m.from)}</b>
              {#if m.from}<span class="addr">&lt;{m.from.email}&gt;</span>{/if}
            </div>
            {#if m.to.length}<div class="line">To {m.to.map(displayName).join(', ')}</div>{/if}
            {#if m.cc.length}<div class="line">Cc {m.cc.map(displayName).join(', ')}</div>{/if}
            {#if m.date}<div class="line">{longDate(m.date)}</div>{/if}
          </div>
          <button class="btn sm" disabled={!m.from && !m.replyTo.length} onclick={() => reply(m)}><Reply size={14} />Reply</button>
        </div>
        {#if blocked && !allowRemote}
          <div class="remote">
            Pictures from the internet are hidden.
            <button class="link" onclick={() => (allowRemote = true)}>Show them</button>
          </div>
        {/if}
        <div class="body">
          <MailFrame message={asView(m)} {allowRemote} onstats={(s) => (blocked = s.blocked)} />
        </div>
        {#if m.attachments.length}
          <div class="parts">
            {#each m.attachments as a (a.partId)}
              <div class="part">
                <button class="open" onclick={() => openPart(a.partId)} title="Open">
                  <File size={15} />
                  <span class="fname">{a.filename}</span>
                  <span class="size">{fileSize(a.size)}</span>
                </button>
                <button class="icon-btn sm" aria-label="Save" title="Save" onclick={() => savePart(a.partId, a.filename)}><Download size={14} /></button>
              </div>
            {/each}
          </div>
        {/if}
      </div>
    {:else}
      {@const c = file}
      <div class="calendar">
        <h2>{c.name ?? (c.events.length === 1 ? c.events[0].summary || 'Event' : `${c.events.length} events`)}</h2>
        <ul>
          {#each c.events as e (e.uid)}
            <li class:exists={e.exists}>
              <label>
                <input
                  type="checkbox"
                  disabled={e.exists}
                  checked={e.exists || chosen.includes(e.uid)}
                  onchange={(ev) => toggle(e.uid, (ev.currentTarget as HTMLInputElement).checked)}
                />
                <span class="ev">
                  <b>{e.summary || '(no title)'}</b>
                  <span>{when(e)}{e.recurring ? ' · repeats' : ''}</span>
                  {#if e.location}<span>{e.location}</span>{/if}
                  {#if e.exists}<span class="note">Already on your calendar</span>{/if}
                </span>
              </label>
            </li>
          {/each}
        </ul>
        <footer>
          {#if c.events.every((e) => e.exists)}
            <span class="all-there">Everything here is already on your calendar.</span>
            <span class="grow"></span>
            <button class="btn primary" onclick={onclose}>Close</button>
          {:else}
            {#if writable.length}
              <select class="input sel" bind:value={calendarId} aria-label="Calendar">
                {#each writable as w (w.id)}
                  <option value={w.id}>{w.name}</option>
                {/each}
              </select>
            {/if}
            <span class="grow"></span>
            <button class="btn" onclick={onclose}>Cancel</button>
            <button class="btn primary" disabled={!chosen.length || calendarId == null || adding} onclick={addEvents}>
              {#if adding}<span class="spin"><Spinner size={14} /></span>{/if}
              {chosen.length === 1 ? 'Add 1 event' : `Add ${chosen.length} events`}
            </button>
          {/if}
        </footer>
      </div>
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1350;
    background: var(--overlay);
    display: grid;
    place-items: center;
    animation: fade-in var(--dur-fast);
  }
  .window {
    display: flex;
    flex-direction: column;
    width: min(560px, calc(100vw - 32px));
    max-height: calc(100vh - 48px);
    border-radius: 16px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
    animation: pop-in var(--dur) var(--ease-out);
  }
  .window.wide {
    width: min(860px, calc(100vw - 32px));
    height: calc(100vh - 48px);
  }
  header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 10px 10px 16px;
    border-bottom: 1px solid var(--border);
  }
  .kind {
    display: grid;
    color: var(--accent);
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-2);
    font-size: var(--text-sm);
  }
  .loading {
    display: grid;
    place-items: center;
    height: 160px;
    color: var(--text-3);
  }
  .failure {
    margin: 20px;
    color: var(--text-2);
    line-height: 1.5;
  }
  .message {
    display: flex;
    flex-direction: column;
    min-height: 0;
    flex: 1;
  }
  .head {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 6px 16px;
    padding: 18px 20px 12px;
  }
  h2 {
    grid-column: 1 / -1;
    margin: 0;
    font-size: var(--text-xl);
    font-weight: 650;
    line-height: 1.3;
  }
  .meta {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    font-size: var(--text-sm);
  }
  .addr,
  .line {
    color: var(--text-3);
  }
  .head .btn {
    align-self: start;
  }
  .remote {
    margin: 0 20px 8px;
    padding: 8px 12px;
    border-radius: 10px;
    font-size: var(--text-sm);
    color: var(--text-2);
    background: var(--bg-hover);
  }
  .link {
    color: var(--accent-text);
    font-weight: 600;
  }
  .body {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 0 20px 16px;
  }
  .parts {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    padding: 12px 20px 16px;
    border-top: 1px solid var(--border);
  }
  .part {
    display: flex;
    align-items: center;
    gap: 2px;
    padding-right: 4px;
    border-radius: 10px;
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .open {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 34px;
    padding: 0 6px 0 10px;
    max-width: 260px;
    color: var(--text-2);
  }
  .fname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text);
    font-weight: 500;
  }
  .size {
    color: var(--text-3);
    font-size: var(--text-xs);
  }
  .calendar {
    display: flex;
    flex-direction: column;
    min-height: 0;
    padding: 16px 20px;
  }
  .calendar h2 {
    margin-bottom: 12px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  li label {
    display: flex;
    gap: 12px;
    padding: 10px 12px;
    border-radius: 12px;
    box-shadow: inset 0 0 0 1px var(--border);
    cursor: pointer;
  }
  li.exists label {
    cursor: default;
    opacity: 0.6;
  }
  li input {
    accent-color: var(--accent);
    width: 16px;
    height: 16px;
    margin-top: 2px;
    flex-shrink: 0;
  }
  .ev {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--text-2);
  }
  .ev b {
    color: var(--text);
    font-size: var(--text-base);
    font-weight: 600;
  }
  .note {
    color: var(--text-3);
    font-style: italic;
  }
  footer {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 16px;
  }
  .grow {
    flex: 1;
  }
  .all-there {
    color: var(--text-2);
    font-size: var(--text-sm);
  }
  .sel {
    max-width: 220px;
  }
  .spin {
    display: inline-grid;
    animation: spin 1s linear infinite;
  }
</style>
