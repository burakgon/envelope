<script lang="ts">
  import type { ThreadSummary } from '../api';
  import { mail, roleLabel } from '../stores/mail.svelte';
  import { ui } from '../stores/ui.svelte';
  import { listDate, participantSummary, displayName, previewText } from '../utils/format';
  import Avatar from './Avatar.svelte';
  import { Archive, Calendar, Check, Clock, CloudOff, File, MailOpen, Mail, Paperclip, Star, Trash } from '../icons';
  import { t } from '../i18n';

  let {
    thread,
    selected,
    checked,
    onopen,
    onwindow,
    oncontext,
    onsnooze,
  }: {
    thread: ThreadSummary;
    selected: boolean;
    checked: boolean;
    onopen: (e: MouseEvent) => void;
    /** A double click: the conversation in a window of its own. */
    onwindow: () => void;
    oncontext: (e: MouseEvent) => void;
    onsnooze: (e: MouseEvent) => void;
  } = $props();

  const unread = $derived(thread.unreadCount > 0);
  const lastSender = $derived(thread.lastSender ?? thread.participants[thread.participants.length - 1]);
  const names = $derived(participantSummary(thread.participants, mail.meEmails, thread.messageCount));
  const account = $derived(mail.accountById.get(thread.accountId));
  const labels = $derived(
    thread.folderIds
      .map((id) => mail.folderById.get(id))
      .filter((f) => f && f.kind === 'label')
      .slice(0, 3),
  );
  // Search results say where they are: the inbox first, then another folder, Gmail's All Mail last.
  const place = $derived.by(() => {
    if (!mail.search) return null;
    const folders = thread.folderIds.map((id) => mail.folderById.get(id)).filter((f) => !!f && f.kind !== 'label');
    const f = folders.find((x) => x!.role === 'inbox') ?? folders.find((x) => x!.role !== 'all') ?? folders[0];
    return f ? (f.role ? roleLabel(f.role) : f.name) : null;
  });
  const compact = $derived(ui.settings.density === 'compact');
  /** A draft only on this computer: it can be opened or discarded, nothing else. */
  const local = $derived(thread.localDraftId != null);
  /** The row's buttons are reached with Tab only on the selected row (the list itself keeps the focus). */
  const tab = $derived(selected ? 0 : -1);

  function dragStart(e: DragEvent) {
    const ids = checked && mail.checked.size > 0 ? [...mail.checked] : [thread.id];
    e.dataTransfer?.setData('application/x-envelope-threads', JSON.stringify(ids));
    e.dataTransfer?.setData('text/plain', thread.subject);
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  }

  function act(e: MouseEvent, fn: () => void) {
    e.stopPropagation();
    fn();
  }
</script>

<div
  class="row"
  class:selected
  class:checked
  class:unread
  class:compact
  id="thread-{thread.id}"
  role="option"
  aria-selected={selected}
  aria-checked={mail.checked.size ? checked : undefined}
  tabindex="-1"
  draggable={!local}
  ondragstart={dragStart}
  onclick={onopen}
  ondblclick={(e) => {
    if (!e.ctrlKey && !e.metaKey && !e.shiftKey && !local) onwindow();
  }}
  oncontextmenu={oncontext}
  onkeydown={() => {}}
>
  {#if mail.multiAccount && account}
    <span class="acct" style:background={account.color} title={account.label ?? account.email}></span>
  {/if}
  <div class="lead">
    {#if unread}<span class="dot"></span>{/if}
    {#if local && ui.settings.showAvatars && !compact}
      <span class="local-av"><Avatar person={lastSender} size={34} /></span>
    {:else if local}
      <span class="local-box"></span>
    {:else if ui.settings.showAvatars && !compact}
      <button class="avatar-btn" class:on={checked} tabindex={tab} aria-label={t('threadRow.select')} onclick={(e) => act(e, () => mail.toggleCheck(thread.id))}>
        <span class="av"><Avatar person={lastSender} size={34} /></span>
        <span class="tick"><Check size={16} strokeWidth={3} /></span>
      </button>
    {:else}
      <button class="checkbox" class:on={checked} tabindex={tab} aria-label={t('threadRow.select')} onclick={(e) => act(e, () => mail.toggleCheck(thread.id))}>
        {#if checked}<Check size={12} strokeWidth={3} />{/if}
      </button>
    {/if}
  </div>

  <div class="body">
    <div class="line top">
      <span class="from truncate" title={lastSender ? `${displayName(lastSender)} <${lastSender.email}>` : ''}>{names}</span>
      {#if thread.messageCount > 1}<span class="n">{thread.messageCount}</span>{/if}
      {#if thread.draftCount > 0}<span class="draft">{t('threadRow.draft')}</span>{/if}
      {#if local}<span class="here" title={t('threadRow.onlyHere')} aria-label={t('threadRow.onlyHere')}><CloudOff size={13} /></span>{/if}
      <span class="spacer"></span>
      <span class="meta">
        {#if thread.isInvite}<Calendar size={13} />{/if}
        {#if thread.hasAttachments}<Paperclip size={13} />{/if}
        {#if thread.snoozedUntil}<Clock size={13} />{/if}
        <time class="tabular">{listDate(thread.snoozedUntil ?? thread.lastDate)}</time>
      </span>
    </div>
    {#if compact}
      <div class="line">
        <span class="subject truncate">{thread.subject || t('common.noSubject')}</span>
        <span class="snippet truncate">{previewText(thread.snippet)}</span>
      </div>
    {:else}
      <div class="line">
        <span class="subject truncate">{thread.subject || t('common.noSubject')}</span>
        {#if place}<span class="place">{place}</span>{/if}
        {#each labels as l (l!.id)}
          <span class="label" style:--c={l!.color ?? 'var(--text-3)'}>{l!.name}</span>
        {/each}
      </div>
      <div class="line snippet-line">
        <span class="snippet truncate">{previewText(thread.snippet) || ' '}</span>
        {#if thread.flagged}<span class="star"><Star size={13} fill="currentColor" /></span>{/if}
      </div>
    {/if}
  </div>

  <div class="actions" role="toolbar">
    {#if local}
      <button class="icon-btn sm" tabindex={tab} title={t('composer.discardDraft')} aria-label={t('composer.discardDraft')} onclick={(e) => act(e, () => mail.trash([thread.id]))}><Trash size={15} /></button>
    {:else}
    <button class="icon-btn sm" tabindex={tab} title={t('threadRow.archive', { key: 'E' })} onclick={(e) => act(e, () => mail.archive([thread.id]))}><Archive size={15} /></button>
    <button class="icon-btn sm" tabindex={tab} title={t('threadRow.delete', { key: '#' })} onclick={(e) => act(e, () => mail.trash([thread.id]))}><Trash size={15} /></button>
    <button class="icon-btn sm" tabindex={tab} title={unread ? t('threadRow.markAsRead', { key: 'U' }) : t('threadRow.markAsUnread', { key: 'U' })} onclick={(e) => act(e, () => mail.toggleRead([thread.id]))}>
      {#if unread}<MailOpen size={15} />{:else}<Mail size={15} />{/if}
    </button>
    <button class="icon-btn sm" tabindex={tab} title={t('threadRow.snooze', { key: 'H' })} onclick={(e) => act(e, () => onsnooze(e))}><Clock size={15} /></button>
    <button class="icon-btn sm star-btn" tabindex={tab} class:on={thread.flagged} title={t('threadRow.star', { key: 'S' })} onclick={(e) => act(e, () => mail.toggleStar([thread.id]))}>
      <Star size={15} fill={thread.flagged ? 'currentColor' : 'none'} />
    </button>
    {/if}
  </div>
</div>

<style>
  .row {
    position: relative;
    display: flex;
    align-items: stretch;
    gap: 12px;
    height: var(--row-h);
    padding: 0 14px 0 10px;
    border-radius: 10px;
    margin: 0 6px;
    transition: background var(--dur-fast) var(--ease);
    contain: layout style;
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row.selected {
    background: var(--bg-selected);
  }
  .row.checked {
    background: var(--accent-softer);
  }
  .row.checked.selected {
    background: var(--bg-selected);
  }
  .acct {
    position: absolute;
    left: 2px;
    top: 14px;
    bottom: 14px;
    width: 3px;
    border-radius: 3px;
    opacity: 0.85;
  }
  .lead {
    position: relative;
    display: flex;
    align-items: flex-start;
    padding-top: 12px;
    padding-left: 6px;
  }
  .compact .lead {
    align-items: center;
    padding-top: 0;
  }
  .dot {
    position: absolute;
    left: -3px;
    top: 26px;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
    box-shadow: 0 0 0 2px oklch(0.6 0.2 var(--accent-h) / 0.18);
  }
  .compact .dot {
    top: calc(50% - 3.5px);
  }
  .avatar-btn {
    position: relative;
    width: 34px;
    height: 34px;
    border-radius: 50%;
  }
  .av,
  .tick {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    border-radius: 50%;
    transition: opacity var(--dur-fast), transform var(--dur) var(--spring);
  }
  .tick {
    opacity: 0;
    transform: scale(0.6);
    background: var(--accent);
    color: #fff;
  }
  .avatar-btn.on .tick,
  .avatar-btn:hover .tick {
    opacity: 1;
    transform: scale(1);
  }
  .avatar-btn:hover:not(.on) .tick {
    background: var(--bg-elevated);
    color: var(--text-3);
    box-shadow: inset 0 0 0 1.5px var(--border-strong);
  }
  .avatar-btn.on .av,
  .avatar-btn:hover .av {
    opacity: 0;
  }
  .checkbox {
    width: 16px;
    height: 16px;
    border-radius: 5px;
    margin-top: 3px;
    display: grid;
    place-items: center;
    box-shadow: inset 0 0 0 1.5px var(--border-strong);
    color: #fff;
  }
  .compact .checkbox {
    margin-top: 0;
  }
  .checkbox.on {
    background: var(--accent);
    box-shadow: none;
  }
  .body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 1px;
  }
  .line {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .from {
    font-size: var(--text-md);
    font-weight: 500;
    color: var(--text);
    min-width: 0;
  }
  .unread .from {
    font-weight: 650;
  }
  .n {
    font-size: 11px;
    font-weight: 600;
    color: var(--text-3);
    padding: 0 5px;
    border-radius: 6px;
    background: var(--bg-active);
  }
  .draft {
    flex-shrink: 0;
    font-size: 11px;
    font-weight: 600;
    color: var(--red);
    white-space: nowrap;
  }
  .here {
    display: inline-grid;
    place-items: center;
    flex-shrink: 0;
    color: var(--text-3);
  }
  .local-av {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
  }
  .local-box {
    width: 16px;
  }
  .spacer {
    flex: 1;
  }
  .meta {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-3);
    font-size: var(--text-sm);
    flex-shrink: 0;
  }
  .unread .meta time {
    color: var(--accent-text);
    font-weight: 600;
  }
  .subject {
    color: var(--text);
    font-weight: 450;
    min-width: 0;
  }
  .unread .subject {
    font-weight: 600;
  }
  .compact .subject {
    flex: 0 1 auto;
    max-width: 55%;
  }
  .snippet {
    color: var(--text-3);
    min-width: 0;
    flex: 1;
  }
  .compact .line .snippet::before {
    content: '— ';
  }
  .place {
    flex-shrink: 0;
    font-size: 10.5px;
    font-weight: 600;
    padding: 1px 7px;
    border-radius: 999px;
    color: var(--text-2);
    background: var(--bg-active);
  }
  .label {
    flex-shrink: 0;
    font-size: 10.5px;
    font-weight: 600;
    padding: 1px 7px;
    border-radius: 999px;
    color: color-mix(in oklab, var(--c) 55%, var(--text));
    background: color-mix(in oklab, var(--c) 14%, transparent);
  }
  .star {
    color: var(--star);
    display: grid;
  }
  .actions {
    position: absolute;
    right: 10px;
    top: 50%;
    translate: 0 -50%;
    display: flex;
    gap: 1px;
    padding: 3px;
    border-radius: 10px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-md);
    opacity: 0;
    pointer-events: none;
    transform: translateX(6px);
    transition: opacity var(--dur-fast), transform var(--dur) var(--ease-out);
  }
  /* With the pointer over the row, or the keyboard on one of its buttons. */
  .row:hover .actions,
  .actions:focus-within {
    opacity: 1;
    pointer-events: auto;
    transform: none;
  }
  .avatar-btn:focus-visible .tick {
    opacity: 1;
    transform: scale(1);
  }
  .avatar-btn:focus-visible .av {
    opacity: 0;
  }
  .avatar-btn:focus-visible:not(.on) .tick {
    background: var(--bg-elevated);
    color: var(--text-3);
    box-shadow: inset 0 0 0 1.5px var(--border-strong);
  }
  .star-btn.on {
    color: var(--star);
  }
</style>
