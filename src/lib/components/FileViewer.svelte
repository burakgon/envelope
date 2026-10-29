<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../api';
  import type { FileEvent, FileMessage, MessageView, OpenedFile } from '../api';
  import { t } from '../i18n';
  import { Calendar, Download, File, Lock, Mail, Reply, Spinner, Users, X } from '../icons';
  import { cal } from '../stores/calendar.svelte';
  import { people, bookName } from '../stores/contacts.svelte';
  import { compose } from '../stores/compose.svelte';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { dateFormat, displayName, fileSize, longDate, timeOf } from '../utils/format';
  import MailFrame from './MailFrame.svelte';

  /** A saved message or calendar file opened with Envelope. */
  let { path, onclose }: { path: string; onclose: () => void } = $props();

  let file = $state<OpenedFile | null>(null);
  let failure = $state<string | null>(null);
  let allowRemote = $state(false);
  let blocked = $state(0);
  let chosen = $state<string[]>([]);
  let calendarId = $state<number | null>(null);
  let picked = $state<number[]>([]);
  let bookId = $state<number | null>(null);
  let adding = $state(false);

  const name = $derived(path.split('/').pop() ?? path);
  const writable = $derived(cal.calendars.filter((c) => !c.readOnly));

  onMount(async () => {
    try {
      const opened = await api.files.open(path);
      file = opened;
      if (opened.kind === 'contacts') {
        picked = opened.contacts.filter((c) => !c.exists).map((c) => c.index);
        if (!people.books.length) await people.loadBooks();
        bookId = people.writable[0]?.id ?? null;
      }
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
      toasts.show(t('attachment.saved', { file: filename }), { kind: 'success' });
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
      return one ? dateFormat(day).format(first) : t('calendar.popover.dateRange', { start: dateFormat(day).format(first), end: dateFormat(day).format(last) });
    }
    const start = new Date(e.start);
    const end = new Date(e.end);
    const sameDay = start.toDateString() === end.toDateString();
    return t('fileViewer.timeRange', {
      date: dateFormat(day).format(start),
      start: timeOf(start),
      end: sameDay ? timeOf(end) : dateFormat({ ...day, hour: 'numeric', minute: '2-digit' }).format(end),
    });
  }

  function toggle(uid: string, on: boolean) {
    chosen = on ? [...chosen, uid] : chosen.filter((u) => u !== uid);
  }

  async function addEvents() {
    if (calendarId == null || !chosen.length || adding) return;
    adding = true;
    try {
      const result = await api.files.importEvents(path, calendarId, chosen);
      toasts.show(
        result.skipped
          ? t('fileViewer.addedEventsSomeSkipped', { count: result.added, skipped: result.skipped })
          : t('fileViewer.addedEvents', { count: result.added }),
        { kind: 'success' },
      );
      cal.load();
      onclose();
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      adding = false;
    }
  }

  async function addContacts() {
    if (bookId == null || !picked.length || adding) return;
    adding = true;
    try {
      const result = await api.files.importContacts(path, bookId, picked);
      toasts.show(
        result.skipped
          ? t('fileViewer.addedContactsSomeSkipped', { count: result.added, skipped: result.skipped })
          : t('fileViewer.addedContacts', { count: result.added }),
        { kind: 'success' },
      );
      people.load();
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
        {#if file?.kind === 'calendar'}<Calendar size={16} />{:else if file?.kind === 'contacts'}<Users size={16} />{:else}<Mail size={16} />{/if}
      </span>
      <span class="name" title={path}>{name}</span>
      <button class="icon-btn" aria-label={t('common.close')} onclick={onclose}><X size={16} /></button>
    </header>

    {#if failure}
      <p class="failure">{t('fileViewer.openFailed', { error: failure })}</p>
    {:else if !file}
      <div class="loading"><span class="spin"><Spinner size={18} /></span></div>
    {:else if file.kind === 'message'}
      {@const m = file}
      <div class="message">
        <div class="head">
          <h2>
            {m.subject || t('common.noSubject')}
            {#if m.encrypted}<span class="locked" title={t('fileViewer.encryptedTitle')}><Lock size={12} />{t('crypto.status.encrypted')}</span>{/if}
          </h2>
          <div class="meta">
            <div>
              <b>{displayName(m.from)}</b>
              {#if m.from}<span class="addr">&lt;{m.from.email}&gt;</span>{/if}
            </div>
            {#if m.to.length}<div class="line">{t('fileViewer.toLine', { names: m.to.map(displayName).join(', ') })}</div>{/if}
            {#if m.cc.length}<div class="line">{t('fileViewer.ccLine', { names: m.cc.map(displayName).join(', ') })}</div>{/if}
            {#if m.date}<div class="line">{longDate(m.date)}</div>{/if}
          </div>
          <button class="btn sm" disabled={!m.from && !m.replyTo.length} onclick={() => reply(m)}><Reply size={14} />{t('reader.reply')}</button>
        </div>
        {#if blocked && !allowRemote}
          <div class="remote">
            {t('fileViewer.picturesHidden')}
            <button class="link" onclick={() => (allowRemote = true)}>{t('fileViewer.showPictures')}</button>
          </div>
        {/if}
        <div class="body">
          <MailFrame message={asView(m)} {allowRemote} onstats={(s) => (blocked = s.blocked)} />
        </div>
        {#if m.attachments.length}
          <div class="parts">
            {#each m.attachments as a (a.partId)}
              <div class="part">
                <button class="open" onclick={() => openPart(a.partId)} title={t('common.open')}>
                  <File size={15} />
                  <span class="fname">{a.filename}</span>
                  <span class="size">{fileSize(a.size)}</span>
                </button>
                <button class="icon-btn sm" aria-label={t('common.save')} title={t('common.save')} onclick={() => savePart(a.partId, a.filename)}><Download size={14} /></button>
              </div>
            {/each}
          </div>
        {/if}
      </div>
    {:else if file.kind === 'contacts'}
      {@const list = file.contacts}
      <div class="calendar">
        <h2>{list.length === 1 ? list[0].name : t('fileViewer.contactCount', { count: list.length })}</h2>
        <ul>
          {#each list as p (p.index)}
            <li class:exists={p.exists}>
              <label>
                <input
                  type="checkbox"
                  disabled={p.exists}
                  checked={p.exists || picked.includes(p.index)}
                  onchange={(ev) => {
                    const on = (ev.currentTarget as HTMLInputElement).checked;
                    picked = on ? [...picked, p.index] : picked.filter((i) => i !== p.index);
                  }}
                />
                <span class="ev">
                  <b>{p.name}</b>
                  {#if p.email || p.organization}<span>{[p.email, p.organization].filter(Boolean).join(' · ')}</span>{/if}
                  {#if p.exists}<span class="note">{t('fileViewer.alreadyInContacts')}</span>{/if}
                </span>
              </label>
            </li>
          {/each}
        </ul>
        <footer>
          {#if list.every((p) => p.exists)}
            <span class="all-there">{t('fileViewer.allAlreadyInContacts')}</span>
            <span class="grow"></span>
            <button class="btn primary" onclick={onclose}>{t('common.close')}</button>
          {:else}
            {#if people.writable.length > 1}
              <select class="input sel" bind:value={bookId} aria-label={t('contacts.addressBook')}>
                {#each people.writable as b (b.id)}
                  <option value={b.id}>{bookName(b)}</option>
                {/each}
              </select>
            {/if}
            <span class="grow"></span>
            <button class="btn" onclick={onclose}>{t('common.cancel')}</button>
            <button class="btn primary" disabled={!picked.length || bookId == null || adding} onclick={addContacts}>
              {#if adding}<span class="spin"><Spinner size={14} /></span>{/if}
              {t('fileViewer.addContacts', { count: picked.length })}
            </button>
          {/if}
        </footer>
      </div>
    {:else}
      {@const c = file}
      <div class="calendar">
        <h2>{c.name ?? (c.events.length === 1 ? c.events[0].summary || t('fileViewer.untitledEvent') : t('fileViewer.eventCount', { count: c.events.length }))}</h2>
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
                  <b>{e.summary || t('fileViewer.noTitle')}</b>
                  <span>{e.recurring ? t('fileViewer.whenRepeats', { when: when(e) }) : when(e)}</span>
                  {#if e.location}<span>{e.location}</span>{/if}
                  {#if e.exists}<span class="note">{t('fileViewer.alreadyOnCalendar')}</span>{/if}
                </span>
              </label>
            </li>
          {/each}
        </ul>
        <footer>
          {#if c.events.every((e) => e.exists)}
            <span class="all-there">{t('fileViewer.allAlreadyOnCalendar')}</span>
            <span class="grow"></span>
            <button class="btn primary" onclick={onclose}>{t('common.close')}</button>
          {:else}
            {#if writable.length}
              <select class="input sel" bind:value={calendarId} aria-label={t('fileViewer.targetCalendar')}>
                {#each writable as w (w.id)}
                  <option value={w.id}>{w.name}</option>
                {/each}
              </select>
            {/if}
            <span class="grow"></span>
            <button class="btn" onclick={onclose}>{t('common.cancel')}</button>
            <button class="btn primary" disabled={!chosen.length || calendarId == null || adding} onclick={addEvents}>
              {#if adding}<span class="spin"><Spinner size={14} /></span>{/if}
              {t('fileViewer.addEvents', { count: chosen.length })}
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
  .locked {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin-left: 8px;
    padding: 2px 8px;
    border-radius: 999px;
    vertical-align: middle;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--green);
    background: color-mix(in oklab, var(--green) 14%, transparent);
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
    white-space: nowrap;
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
    min-width: 0;
    max-width: 220px;
  }
  .spin {
    display: inline-grid;
    animation: spin 1s linear infinite;
  }
</style>
