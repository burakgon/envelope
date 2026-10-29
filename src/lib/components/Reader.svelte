<script lang="ts">
  import { tick } from 'svelte';
  import { compose } from '../stores/compose.svelte';
  import { mail } from '../stores/mail.svelte';
  import { ui } from '../stores/ui.svelte';
  import MessageCard from './MessageCard.svelte';
  import Composer from './Composer.svelte';
  import WindowControls from './WindowControls.svelte';
  import SnoozeMenu from './SnoozeMenu.svelte';
  import MoveMenu from './MoveMenu.svelte';
  import AiSummary from './AiSummary.svelte';
  import LinkStatus from './LinkStatus.svelte';
  import { pgp } from '../stores/pgp.svelte';
  import { tasks } from '../stores/tasks.svelte';
  import {
    Archive, BellOff, Clock, Folder, Forward, ListChecks, ListPlus, Mail, MailOpen, Reply, ReplyAll, ShieldAlert, Star, Tag, Trash, Inbox,
    Check, AlarmClock,
    ShieldCheck,
  } from '../icons';
  import { t } from '../i18n';

  let expanded = $state<Record<number, boolean>>({});
  let showAll = $state(false);
  let snoozeAt = $state<{ x: number; y: number } | null>(null);
  /** "Remind me again" of a follow-up reminder. */
  let remindAt = $state<{ x: number; y: number } | null>(null);
  let moveAt = $state<{ x: number; y: number; labels: boolean } | null>(null);
  let scroller: HTMLDivElement | undefined = $state();
  let lastThread = -1;
  const uid = $props.id();
  /** The message that last had the focus in this conversation (n and p go on from it). */
  let lastFocused: number | null = null;

  const d = $derived(mail.detail);
  const thread = $derived(d?.thread);
  /** A draft being edited under the conversation is shown in its composer only. */
  const editing = $derived(new Set(compose.list.filter((c) => c.inline && c.threadId === d?.thread.id).map((c) => c.sourceMessageId)));
  const messages = $derived((d?.messages ?? []).filter((m) => !editing.has(m.id)));
  const account = $derived(thread ? mail.accountById.get(thread.accountId) : undefined);
  const inlineComposer = $derived(thread ? compose.list.find((c) => c.inline && c.threadId === thread.id) : undefined);
  const labels = $derived(
    (thread?.folderIds ?? []).map((id) => mail.folderById.get(id)).filter((f) => f && f.kind === 'label'),
  );
  const inInbox = $derived(!!thread && thread.folderIds.some((id) => mail.folderById.get(id)?.role === 'inbox'));
  const inSpam = $derived(!!thread && mail.isSpam([thread.id]));
  /** An open task made from this conversation. */
  const linkedTask = $derived(thread ? tasks.linked.get(thread.id) : undefined);

  $effect(() => {
    // Expand unread messages and the latest one when a conversation opens.
    if (!d || d.thread.id === lastThread) return;
    lastThread = d.thread.id;
    showAll = d.messages.length <= 4;
    const next: Record<number, boolean> = {};
    d.messages.forEach((m, i) => {
      if (!m.flags.seen || i === d.messages.length - 1) next[m.id] = true;
    });
    expanded = next;
    lastFocused = null;
    scroller?.scrollTo({ top: 0 });
  });

  const hiddenCount = $derived(showAll ? 0 : Math.max(0, messages.length - 3));
  const shown = $derived(
    showAll ? messages : [messages[0], ...messages.slice(messages.length - 2)].filter((m, i, arr) => m && arr.indexOf(m) === i),
  );

  function rect(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    return { x: r.left, y: r.bottom + 4 };
  }

  /** The newest message that is not a draft: replies go to it. */
  const last = $derived([...messages].reverse().find((m) => !m.flags.draft) ?? messages[messages.length - 1]);

  const smooth = () => (matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth');

  /** Scrolls the conversation a page down or up (Space, Page Down…); false when it is at that end already. */
  export function page(dir: 1 | -1): boolean {
    const el = scroller;
    if (!el) return false;
    if (dir > 0 ? el.scrollTop >= el.scrollHeight - el.clientHeight - 2 : el.scrollTop <= 0) return false;
    el.scrollBy({ top: dir * Math.max(40, el.clientHeight - 60), behavior: smooth() });
    return true;
  }

  /** To the top or the bottom of the conversation (Home, End). */
  export function edge(dir: 1 | -1) {
    scroller?.scrollTo({ top: dir > 0 ? scroller.scrollHeight : 0, behavior: smooth() });
  }

  /**
   * The next or previous message (n, p): from the one with the focus (or that had it, while the
   * focus is outside the conversation), else the one at the top of the view. It is unfolded,
   * scrolled to and given the focus, so that a screen reader reads it.
   */
  export async function step(dir: 1 | -1) {
    if (!scroller || !messages.length) return;
    const inside = scroller.contains(document.activeElement);
    const focused = inside ? Number(document.activeElement?.closest<HTMLElement>('[data-message]')?.dataset.message) : lastFocused;
    let at = messages.findIndex((m) => m.id === focused);
    if (at < 0) {
      at = 0;
      const top = scroller.getBoundingClientRect().top + 48;
      for (const el of scroller.querySelectorAll<HTMLElement>('[data-message]')) {
        if (el.getBoundingClientRect().top <= top) at = messages.findIndex((m) => m.id === Number(el.dataset.message));
      }
    }
    const next = messages[at + dir];
    if (!next) return;
    if (!shown.includes(next)) showAll = true;
    expanded[next.id] = true;
    await tick();
    const el = scroller.querySelector<HTMLElement>(`[data-message="${next.id}"]`);
    el?.scrollIntoView({ block: 'start', behavior: smooth() });
    el?.querySelector<HTMLElement>('article, button')?.focus({ preventScroll: true });
  }

  /** Unfolds every message of the conversation (;). */
  export function expandAll() {
    showAll = true;
    expanded = Object.fromEntries(messages.map((m) => [m.id, true]));
  }

  /** Closes the reply being written under the conversation, keeping its draft; false without one. */
  export function closeReply(): boolean {
    if (!inlineComposer) return false;
    compose.close(inlineComposer.key);
    return true;
  }
</script>

<section class="reader">
  <header data-tauri-drag-region>
    {#if thread}
      <div class="actions">
        {#if mail.inFollowUp}
          <button class="btn sm pill" title={t('reader.followUpDoneTitle')} onclick={() => mail.followUpDone([thread.id])}><Check size={14} />{t('reader.followUpDone')}</button>
          <button class="icon-btn" title={t('reader.followUpLater')} aria-label={t('reader.followUpLater')} onclick={(e) => (remindAt = rect(e))}><AlarmClock size={17} /></button>
          <span class="sep"></span>
        {/if}
        {#if inSpam}
          <button class="btn sm pill" title={t('reader.toolbarNotSpam', { key: '!' })} onclick={() => mail.notSpam([thread.id])}><ShieldCheck size={14} />{t('reader.notSpam')}</button>
          <span class="sep"></span>
        {/if}
        {#if inInbox || !mail.isInboxView}
          <button class="icon-btn" title={t('reader.toolbarArchive', { key: 'E' })} onclick={() => mail.archive([thread.id])}><Archive size={17} /></button>
        {:else}
          <button class="icon-btn" title={t('reader.toolbarMoveToInbox')} onclick={() => mail.moveTo({ kind: 'inbox' }, [thread.id])}><Inbox size={17} /></button>
        {/if}
        <button class="icon-btn" title={t('reader.toolbarDelete', { key: '#' })} onclick={() => mail.trash([thread.id])}><Trash size={17} /></button>
        {#if !inSpam}
          <button class="icon-btn" title={t('reader.toolbarSpam', { key: '!' })} onclick={() => mail.moveTo({ kind: 'junk' }, [thread.id])}><ShieldAlert size={17} /></button>
        {/if}
        <span class="sep"></span>
        <button class="icon-btn" title={t('reader.toolbarSnooze', { key: 'H' })} onclick={(e) => (snoozeAt = rect(e))}><Clock size={17} /></button>
        {#if linkedTask}
          <button class="icon-btn active" title={t('tasks.calendar.open')} aria-label={t('tasks.calendar.open')} onclick={() => tasks.reveal(linkedTask.id)}><ListChecks size={17} /></button>
        {:else}
          <button class="icon-btn" title={t('tasks.reader.addToTasks', { key: 'T' })} aria-label={t('tasks.reader.addToTasks', { key: 'T' })} onclick={() => tasks.addFromMail([thread.id])}><ListPlus size={17} /></button>
        {/if}
        <button class="icon-btn" class:active={thread.muted} title={thread.muted ? t('reader.toolbarUnmute', { key: 'M' }) : t('reader.toolbarMute', { key: 'M' })} onclick={() => mail.toggleMute([thread.id])}><BellOff size={17} /></button>
        <button class="icon-btn" title={thread.unreadCount ? t('reader.toolbarMarkRead', { key: 'U' }) : t('reader.toolbarMarkUnread', { key: 'U' })} onclick={() => mail.toggleRead([thread.id])}>
          {#if thread.unreadCount}<MailOpen size={17} />{:else}<Mail size={17} />{/if}
        </button>
        <button class="icon-btn" title={t('reader.toolbarMove', { key: 'V' })} onclick={(e) => (moveAt = { ...rect(e), labels: false })}><Folder size={17} /></button>
        {#if account?.kind === 'gmail' || account?.kind === 'microsoft' || account?.kind === 'exchange'}
          <button class="icon-btn" title={account?.kind === 'gmail' ? t('reader.toolbarLabel', { key: 'L' }) : t('reader.toolbarCategorize', { key: 'L' })} onclick={(e) => (moveAt = { ...rect(e), labels: true })}><Tag size={17} /></button>
        {/if}
      </div>
    {/if}
    <span class="grow" data-tauri-drag-region></span>
    {#if thread}
      <button class="icon-btn" class:star={thread.flagged} title={t('reader.toolbarStar', { key: 'S' })} onclick={() => mail.toggleStar([thread.id])}>
        <Star size={17} fill={thread.flagged ? 'currentColor' : 'none'} />
      </button>
    {/if}
    <WindowControls />
  </header>

  {#if thread}
    <!-- The conversation: where Escape from a reply puts the focus, and what Space and the arrows scroll. -->
    <div
      class="scroll"
      bind:this={scroller}
      tabindex="-1"
      role="region"
      aria-labelledby="{uid}-subject"
      onfocusin={(e) => {
        const id = (e.target as HTMLElement).closest<HTMLElement>('[data-message]')?.dataset.message;
        if (id) lastFocused = Number(id);
      }}
    >
      <div class="inner">
        <div class="subject-block">
          <h2 class="selectable" id="{uid}-subject">{pgp.subjectOf(messages.map((m) => m.id)) ?? (thread.subject || t('common.noSubject'))}</h2>
          <div class="chips">
            {#if mail.multiAccount && account}
              <span class="chip"><span class="dot" style:background={account.color}></span>{account.label ?? account.email}</span>
            {/if}
            {#each labels as l (l!.id)}
              <span class="chip label" style:--c={l!.color ?? 'var(--text-3)'}>{l!.name}</span>
            {/each}
            {#if messages.length > 1}<span class="chip">{t('reader.messageCount', { count: messages.length })}</span>{/if}
          </div>
        </div>

        {#if ui.settings.aiEnabled && messages.length > 0}
          <AiSummary threadId={thread.id} {messages} />
        {/if}

        <div class="messages">
          {#each shown as m, i (m.id)}
            {#if !showAll && i === 1 && hiddenCount > 0}
              <button class="more" onclick={() => (showAll = true)}>
                <span class="line"></span><span class="pill">{t('reader.moreMessages', { count: hiddenCount })}</span><span class="line"></span>
              </button>
            {/if}
            <div class="msg" data-message={m.id}>
              <MessageCard message={m} expanded={!!expanded[m.id]} ontoggle={() => (expanded[m.id] = !expanded[m.id])} />
            </div>
          {/each}
        </div>

        {#if inlineComposer}
          <div class="reply-box">
            <Composer state={inlineComposer} />
          </div>
        {:else if last}
          <div class="quick">
            <button class="qr" onclick={() => compose.reply(last.id, 'reply')}><Reply size={16} />{t('reader.reply')}</button>
            <button class="qr" onclick={() => compose.reply(last.id, 'replyAll')}><ReplyAll size={16} />{t('reader.replyAll')}</button>
            <button class="qr" onclick={() => compose.reply(last.id, 'forward', false)}><Forward size={16} />{t('reader.forward')}</button>
          </div>
        {/if}
      </div>
    </div>
    <LinkStatus />
  {:else if mail.detailLoading}
    <div class="scroll"><div class="inner"><div class="skeleton" style="width:60%;height:24px;margin:20px 0"></div><div class="skeleton" style="height:180px;border-radius:14px"></div></div></div>
  {:else}
    <div class="placeholder" data-tauri-drag-region>
      <svg viewBox="0 0 160 120" aria-hidden="true">
        <rect x="28" y="24" width="104" height="72" rx="14" fill="var(--bg-sunken)" stroke="var(--border-strong)" stroke-width="2" />
        <path d="M40 38 L80 66 L120 38" fill="none" stroke="var(--text-4)" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
      <p class="hint">{t('reader.emptyHint')}</p>
      <div class="keys">
        <span><kbd class="kbd">J</kbd><kbd class="kbd">K</kbd> {t('reader.keyHintNavigate')}</span>
        <span><kbd class="kbd">E</kbd> {t('reader.keyHintArchive')}</span>
        <span><kbd class="kbd">C</kbd> {t('reader.keyHintCompose')}</span>
        <span><kbd class="kbd">{t('keys.ctrl')}</kbd><kbd class="kbd">K</kbd> {t('reader.keyHintCommands')}</span>
      </div>
    </div>
  {/if}
</section>

{#if snoozeAt && thread}
  {@const id = thread.id}
  <SnoozeMenu x={snoozeAt.x} y={snoozeAt.y} onclose={() => (snoozeAt = null)} onpick={(ts) => { snoozeAt = null; mail.snooze(ts, [id]); }} />
{/if}
{#if remindAt && thread}
  {@const id = thread.id}
  <SnoozeMenu x={remindAt.x} y={remindAt.y} title={t('reader.followUpRemindTitle')} onclose={() => (remindAt = null)} onpick={(ts) => { remindAt = null; mail.followUpLater(ts, [id]); }} />
{/if}
{#if moveAt && thread}
  <MoveMenu x={moveAt.x} y={moveAt.y} ids={[thread.id]} labels={moveAt.labels} onclose={() => (moveAt = null)} />
{/if}

<style>
  .reader {
    position: relative;
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg);
  }
  header {
    display: flex;
    align-items: center;
    gap: 2px;
    height: var(--header-h);
    padding: 0 12px;
    flex-shrink: 0;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  /* A labeled action: the one that matters most in this list (Not spam, Done). */
  .pill {
    height: 28px;
    margin-right: 2px;
    color: var(--accent-text);
    background: var(--accent-soft);
    box-shadow: none;
    font-weight: 600;
  }
  .pill:hover {
    background: color-mix(in oklab, var(--accent) 18%, transparent);
  }
  .sep {
    width: 1px;
    height: 18px;
    margin: 0 6px;
    background: var(--border-strong);
  }
  .grow {
    flex: 1;
    align-self: stretch;
  }
  .star {
    color: var(--star);
  }
  .scroll {
    flex: 1;
    overflow-y: auto;
  }
  .scroll:focus-visible {
    outline: none;
  }
  .msg {
    scroll-margin-top: 8px;
  }
  .inner {
    max-width: 920px;
    margin: 0 auto;
    padding: 6px 28px 48px;
  }
  .subject-block {
    padding: 8px 2px 18px;
  }
  h2 {
    margin: 0;
    font-size: var(--text-xl);
    font-weight: 680;
    letter-spacing: -0.02em;
    line-height: 1.3;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 10px;
  }
  .chips:empty {
    display: none;
  }
  .chip .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
  }
  .chip.label {
    color: color-mix(in oklab, var(--c) 55%, var(--text));
    background: color-mix(in oklab, var(--c) 14%, transparent);
  }
  .messages {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .more {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 2px 0;
  }
  .more .line {
    flex: 1;
    height: 1px;
    background: var(--border);
  }
  .more .pill {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--text-2);
    padding: 4px 12px;
    border-radius: 999px;
    box-shadow: inset 0 0 0 1px var(--border-strong);
    background: var(--bg-elevated);
  }
  .more:hover .pill {
    color: var(--text);
    background: var(--bg-hover);
  }
  .reply-box {
    margin-top: 14px;
  }
  .quick {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 16px;
  }
  .qr {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    height: 36px;
    padding: 0 16px;
    border-radius: 10px;
    font-weight: 550;
    color: var(--text-2);
    background: var(--bg-elevated);
    box-shadow: 0 0 0 1px var(--border), var(--shadow-xs);
    transition: background var(--dur-fast), color var(--dur-fast);
  }
  .qr:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
  .placeholder {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    color: var(--text-3);
    padding-bottom: 60px;
  }
  .placeholder svg {
    width: 130px;
    opacity: 0.9;
  }
  .hint {
    margin: 0;
    font-size: var(--text-md);
    font-weight: 500;
  }
  .keys {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 16px;
    margin-top: 10px;
    font-size: var(--text-sm);
  }
  .keys span {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
</style>
