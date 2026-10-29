<script lang="ts">
  // The senders of newsletters and other list mail, the busiest first: unsubscribe from them, archive
  // what they left in the inbox, delete all of it, or block them.
  import { onMount } from 'svelte';
  import { api } from '../api';
  import type { Subscription, UndoToken } from '../api';
  import { t } from '../i18n';
  import { Archive, Ban, Check, Mails, More, Newspaper, Search, Spinner, Trash } from '../icons';
  import { dialogs } from '../stores/dialogs.svelte';
  import { mail } from '../stores/mail.svelte';
  import { blockSender } from '../stores/rules.svelte';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { ui } from '../stores/ui.svelte';
  import { listDate } from '../utils/format';
  import Avatar from './Avatar.svelte';
  import Popover from './Popover.svelte';
  import { modal } from '../actions/modal';

  let subs = $state<Subscription[] | null>(null);
  let query = $state('');
  /** The sender being worked on. */
  let busy = $state<string | null>(null);
  let menu = $state<{ x: number; y: number; sub: Subscription } | null>(null);

  onMount(load);

  async function load() {
    try {
      // Those already left go last, in the order they had when the dialog opened.
      const list = await api.subscriptions();
      const kept = new Map((subs ?? []).map((s, i) => [s.email, i]));
      subs = subs
        ? list.sort((a, b) => (kept.get(a.email) ?? Infinity) - (kept.get(b.email) ?? Infinity))
        : [...list.filter((s) => !left(s)), ...list.filter(left)];
    } catch (e) {
      toasts.error(errorMessage(e));
      ui.close();
    }
  }

  const left = (s: Subscription) => ui.settings.unsubscribed[s.email] != null;
  const name = (s: Subscription) => s.name ?? s.email;
  /** Address (under a name), conversations, unread ones and the last one's date. */
  const details = (s: Subscription) =>
    [s.name ? s.email : null, t('subscriptions.count', { count: s.conversations }), s.unread ? t('subscriptions.unread', { count: s.unread }) : null, listDate(s.lastDate)]
      .filter(Boolean)
      .join(' · ');

  /** The senders matching the search. */
  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return (subs ?? []).filter((s) => !q || s.email.includes(q) || (s.name ?? '').toLowerCase().includes(q));
  });

  /** Marks a sender unsubscribed from (also when their page was used). */
  function markLeft(s: Subscription) {
    ui.set('unsubscribed', { ...ui.settings.unsubscribed, [s.email]: Date.now() });
  }

  async function unsubscribe(s: Subscription) {
    if (s.unsubscribeMessageId == null || busy) return;
    busy = s.email;
    try {
      // Only a request that unsubscribed counts; a page opened still needs the person to finish there.
      const done = await api.unsubscribe(s.unsubscribeMessageId);
      if (done) markLeft(s);
      toasts.show(done ? t('subscriptions.unsubscribed', { name: name(s) }) : t('message.unsubscribePageOpened'), { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      busy = null;
    }
  }

  /** The conversations only this sender wrote in, as blocking them would move. */
  async function theirs(s: Subscription): Promise<number[]> {
    return (await api.rules.fromSender(s.email, 100_000)).threadIds;
  }

  /** Runs a change to a sender's conversations, then offers to undo it. */
  async function change(s: Subscription, act: (ids: number[]) => Promise<UndoToken[]>, message: (count: number) => string) {
    if (busy) return;
    busy = s.email;
    try {
      const ids = await theirs(s);
      const tokens = await act(ids);
      await load();
      mail.refresh();
      toasts.undoable(message(ids.length), async () => {
        for (const token of tokens) await api.undo(token);
        await load();
        mail.refresh();
      });
    } catch (e) {
      toasts.error(errorMessage(e));
    } finally {
      busy = null;
    }
  }

  function archiveAll(s: Subscription) {
    return change(s, (ids) => api.archiveFromInbox(ids), () => t('subscriptions.archived', { count: s.inInbox, name: name(s) }));
  }

  async function deleteAll(s: Subscription) {
    const ok = await dialogs.confirm({
      title: t('subscriptions.deleteTitle', { name: name(s) }),
      message: t('subscriptions.deleteMessage', { count: s.conversations }),
      confirmLabel: t('common.delete'),
      danger: true,
    });
    if (ok) await change(s, async (ids) => [await api.moveThreads(ids, null, { kind: 'trash' })], (count) => t('subscriptions.deleted', { count }));
  }

  /** Their conversations in the list, as a search. */
  function show(s: Subscription) {
    ui.view = 'mail';
    mail.setSearch(`from:${s.email}`);
    ui.close();
  }

  function openMenu(e: MouseEvent, sub: Subscription) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    menu = { x: r.right - 220, y: r.bottom + 4, sub };
  }

  function fromMenu(act: (s: Subscription) => unknown) {
    const s = menu?.sub;
    menu = null;
    if (s) act(s);
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'Escape' && !menu) {
      e.stopPropagation();
      ui.close();
    }
  }
</script>

<div class="backdrop" role="presentation" onclick={() => ui.close()}>
  <div class="dialog" role="dialog" aria-labelledby="subs-title" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={key} use:modal>
    <header>
      <span class="icon"><Newspaper size={18} /></span>
      <div>
        <h3 id="subs-title">{t('subscriptions.title')}</h3>
        <span class="sub">{t('subscriptions.sub')}</span>
      </div>
    </header>

    {#if subs && subs.length > 6}
      <label class="search">
        <Search size={14} />
        <input bind:value={query} placeholder={t('subscriptions.search')} aria-label={t('subscriptions.search')} spellcheck="false" />
      </label>
    {/if}

    {#if !subs}
      <div class="none"><span class="spin"><Spinner size={18} /></span></div>
    {:else if !subs.length}
      <p class="none">{t('subscriptions.none')}</p>
    {:else if !shown.length}
      <p class="none">{t('subscriptions.noMatch')}</p>
    {:else}
      <div class="list">
        {#each shown as s (s.email)}
          <div class="row" class:left={left(s)}>
            <Avatar person={{ name: s.name, email: s.email }} size={32} />
            <button class="who" title={t('subscriptions.show')} onclick={() => show(s)}>
              <b>{name(s)}</b>
              <span>{details(s)}</span>
            </button>
            {#if busy === s.email}<span class="spin"><Spinner size={14} /></span>{/if}
            {#if left(s)}
              <span class="done"><Check size={13} />{t('message.unsubscribed')}</span>
            {:else if s.unsubscribeMessageId != null}
              <button class="btn sm" disabled={!!busy} aria-label={t('subscriptions.unsubscribeFrom', { name: name(s) })} onclick={() => unsubscribe(s)}>{t('message.unsubscribe')}</button>
            {/if}
            {#if s.inInbox}
              <button class="btn sm" disabled={!!busy} title={t('subscriptions.archiveAll', { count: s.inInbox })} aria-label={t('subscriptions.archiveAll', { count: s.inInbox })} onclick={() => archiveAll(s)}>
                <Archive size={14} />{t('subscriptions.archiveCount', { count: s.inInbox })}
              </button>
            {/if}
            <button class="icon-btn sm" aria-label={t('subscriptions.moreFor', { name: name(s) })} title={t('common.more')} disabled={!!busy} onclick={(e) => openMenu(e, s)}><More size={15} /></button>
          </div>
        {/each}
      </div>
    {/if}

    <footer>
      <button class="btn" onclick={() => ui.close()}>{t('common.close')}</button>
    </footer>
  </div>
</div>

{#if menu}
  <Popover x={menu.x} y={menu.y} width={220} over onclose={() => (menu = null)}>
    <button class="menu-item" onclick={() => fromMenu(show)}><Mails size={15} />{t('subscriptions.show')}</button>
    {#if !left(menu.sub)}
      <button class="menu-item" onclick={() => fromMenu(markLeft)}><Check size={15} />{t('subscriptions.markDone')}</button>
    {/if}
    <button class="menu-item danger" onclick={() => fromMenu(deleteAll)}><Trash size={15} />{t('subscriptions.deleteAll')}</button>
    <button class="menu-item" onclick={() => fromMenu((s) => blockSender({ name: s.name, email: s.email }).then(load))}><Ban size={15} />{t('message.blockSender')}</button>
  </Popover>
{/if}

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
    width: min(620px, calc(100vw - 32px));
    max-height: calc(100vh - 48px);
    display: flex;
    flex-direction: column;
    padding: 20px;
    border-radius: 16px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    animation: pop-in var(--dur) var(--ease-out);
  }
  header {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    margin-bottom: 14px;
  }
  .icon {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    border-radius: 10px;
    color: var(--accent);
    background: color-mix(in oklab, var(--accent) 14%, transparent);
  }
  h3 {
    margin: 0 0 2px;
    font-size: var(--text-lg);
    font-weight: 650;
  }
  .sub {
    color: var(--text-3);
    font-size: var(--text-sm);
    line-height: 1.45;
  }
  .search {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 10px;
    padding: 0 10px;
    height: 34px;
    border-radius: 9px;
    color: var(--text-3);
    background: var(--bg-sunken);
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: 0;
    outline: 0;
    background: transparent;
    color: var(--text);
    font: inherit;
  }
  .list {
    min-height: 0;
    overflow-y: auto;
    margin: 0 -8px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 8px;
    border-radius: 10px;
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .who {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    text-align: left;
  }
  .who b,
  .who span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .who b {
    font-weight: 600;
  }
  .who span {
    color: var(--text-3);
    font-size: var(--text-sm);
  }
  .who:hover b {
    text-decoration: underline;
  }
  .btn :global(svg) {
    margin-right: 4px;
  }
  .done {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-sm);
    color: var(--green);
  }
  .none {
    display: grid;
    place-items: center;
    min-height: 120px;
    margin: 0;
    color: var(--text-3);
    text-align: center;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    margin-top: 16px;
  }
  .spin {
    display: inline-grid;
    color: var(--text-3);
    animation: spin 1s linear infinite;
  }
</style>
