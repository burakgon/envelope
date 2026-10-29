<script lang="ts">
  import { api } from '../api';
  import type { Address, MessageView } from '../api';
  import { compose } from '../stores/compose.svelte';
  import { mail } from '../stores/mail.svelte';
  import { ui } from '../stores/ui.svelte';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { t } from '../i18n';
  import { dateFormat, displayName, longDate, messageDate, previewText, timeOf } from '../utils/format';
  import Avatar from './Avatar.svelte';
  import MailFrame from './MailFrame.svelte';
  import AttachmentTile from './AttachmentTile.svelte';
  import Popover from './Popover.svelte';
  import InviteCard from './InviteCard.svelte';
  import ImageViewer from './ImageViewer.svelte';
  import { printMessage } from '../utils/print';
  import { isContained, isEncrypted, pgp } from '../stores/pgp.svelte';
  import CryptoStatus from './CryptoStatus.svelte';
  import CryptoNotice from './CryptoNotice.svelte';
  import { ai } from '../stores/ai.svelte';
  import { cal } from '../stores/calendar.svelte';
  import type { ExtractedEvent } from '../api';
  import { fileIconKind } from '../utils/format';
  import { CalendarPlus, Code, Eye, FileArrow, Filter, FolderDown, Forward, Image, Mail, More, Pen, Printer, Reply, ReplyAll, ShieldCheck, Star, Copy, Ban, Crown } from '../icons';
  import { vips } from '../stores/vips.svelte';
  import { blockSender, isBlocked, unblock } from '../stores/rules.svelte';
  import { modal } from '../actions/modal';

  let { message, expanded, ontoggle }: { message: MessageView; expanded: boolean; ontoggle: () => void } = $props();
  const uid = $props.id();

  let allowRemote = $state(false);
  let stats = $state({ blocked: 0, trackers: 0, designed: false });
  let menu = $state<{ x: number; y: number } | null>(null);
  let showRecipients = $state(false);
  let source = $state<string | null>(null);
  let viewing = $state<number | null>(null);
  let bodyImages = $state<{ items: { src: string; filename: string }[]; index: number } | null>(null);
  let found = $state<{ x: number; y: number; events: ExtractedEvent[] } | null>(null);

  /** Asks the AI for events in this message and opens the event editor with them. */
  async function addToCalendar() {
    const at = menu ?? { x: innerWidth / 2, y: innerHeight / 3 };
    menu = null;
    const progress = toasts.show(t('message.lookingForEvents'), { kind: 'progress', duration: 0 });
    try {
      const text = await api.ai.run({ kind: 'extractEvents', messageId: message.id }).promise;
      const events: ExtractedEvent[] = JSON.parse(text);
      toasts.dismiss(progress);
      if (!events.length) toasts.show(t('message.noEventsFound'));
      else if (events.length === 1) cal.fromExtracted(events[0]);
      else found = { x: at.x, y: at.y, events };
    } catch (e) {
      toasts.dismiss(progress);
      toasts.error(errorMessage(e));
    }
  }

  function foundWhen(e: ExtractedEvent): string {
    const d = new Date(e.all_day ? `${e.start.slice(0, 10)}T00:00:00` : e.start);
    const day = dateFormat({ weekday: 'short', month: 'short', day: 'numeric' }).format(d);
    return e.all_day ? day : t('time.dateAt', { date: day, time: timeOf(d) });
  }

  const sender = $derived(message.from);
  const fromMe = $derived(!!sender && mail.meEmails.has(sender.email.toLowerCase()));
  const trusted = $derived(!!sender && ui.settings.trustedSenders.includes(sender.email.toLowerCase()));
  const remote = $derived(allowRemote || trusted || ui.settings.loadRemoteImages === 'always');
  // OpenPGP: encrypted messages show what was decrypted (or a copy kept
  // from an earlier session); signed ones show their signature status.
  const opened = $derived(message.crypto ? (pgp.opened[message.id] ?? null) : null);
  $effect(() => {
    if (expanded && message.crypto && !pgp.opened[message.id]) pgp.open(message.id);
  });
  const shown = $derived.by((): MessageView | null => {
    if (opened?.body) {
      const attachments = isContained(message) ? opened.attachments : message.attachments;
      return { ...message, body: opened.body, attachments };
    }
    if (!isEncrypted(message)) return message;
    // A decrypted copy kept from an earlier session.
    if ((message.crypto === 'encrypted' || message.crypto === 'smime-encrypted') && (message.body?.html || message.body?.text)) return message;
    // Inline PGP that cannot be decrypted here reads as it was sent.
    if (message.crypto === 'inline-encrypted' && (opened?.status === 'noKey' || opened?.status === 'error')) return message;
    return null;
  });
  const files = $derived((shown ?? message).attachments.filter((a) => !a.inline));
  const images = $derived(files.filter((a) => fileIconKind(a.mimeType, a.filename) === 'image'));
  /** A draft (stored on the server): it opens in a composer instead of being replied to. */
  const isDraft = $derived(message.flags.draft);
  let saving = $state(false);

  /** Saves every attachment into a folder picked for them, then offers to show them there. */
  async function saveAll() {
    if (saving) return;
    const dir = await api.pickFolder(t('message.saveAllTitle'));
    if (!dir) return;
    saving = true;
    const count = files.length;
    const progress = toasts.show(t('message.savingAll', { count }), { kind: 'progress', duration: 0 });
    try {
      const paths = await api.attachmentsSaveAll(files.map((a) => a.id), dir);
      toasts.dismiss(progress);
      toasts.show(t('message.savedAll', { count: paths.length, folder: dir.split(/[\\/]/).filter(Boolean).pop() ?? dir }), {
        kind: 'success',
        action: { label: t('message.showInFolder'), run: () => api.revealPath(paths[0] ?? dir).catch((e) => toasts.error(errorMessage(e))) },
        duration: 8000,
      });
    } catch (e) {
      toasts.dismiss(progress);
      toasts.error(t('message.saveAllFailed', { error: errorMessage(e) }));
    } finally {
      saving = false;
    }
  }

  function nameOrMe(a: Address) {
    return mail.meEmails.has(a.email.toLowerCase()) ? t('message.recipientMe') : displayName(a);
  }

  const recipientLine = $derived.by(() => {
    const all = [...message.to, ...message.cc];
    if (all.length === 0) return '';
    const names = all.slice(0, 3).map(nameOrMe).join(', ');
    return all.length > 3 ? t('message.recipientsMore', { names, count: all.length - 3 }) : t('message.recipients', { names });
  });

  function trustSender() {
    if (!sender) return;
    ui.set('trustedSenders', [...new Set([...ui.settings.trustedSenders, sender.email.toLowerCase()])]);
    allowRemote = true;
  }

  function openMenu(e: MouseEvent) {
    e.stopPropagation();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    menu = { x: r.right, y: r.bottom + 4 };
  }

  async function viewSource() {
    menu = null;
    try {
      source = await api.messageSource(message.id);
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }

  async function unsubscribe() {
    try {
      const done = await api.unsubscribe(message.id);
      // Subscriptions then shows the sender as left.
      if (done && sender) ui.set('unsubscribed', { ...ui.settings.unsubscribed, [sender.email.toLowerCase()]: Date.now() });
      toasts.show(done ? t('message.unsubscribed') : t('message.unsubscribePageOpened'), { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }
</script>

{#if !expanded}
  <button class="collapsed" onclick={isDraft ? () => compose.editMessage(message) : ontoggle}>
    <Avatar person={sender} size={28} />
    <span class="c-name" class:unread={!message.flags.seen}>{fromMe ? t('message.senderMe') : displayName(sender)}</span>
    {#if !fromMe && vips.has(sender?.email)}<span class="vip" role="img" title={t('vip.badge')} aria-label={t('vip.badge')}><Crown size={12} /></span>{/if}
    {#if isDraft}<span class="draft-chip">{t('threadRow.draft')}</span>{/if}
    <span class="c-snippet truncate">{previewText(message.snippet)}</span>
    <time class="c-date">{messageDate(message.date)}</time>
  </button>
{:else}
  <article class="card" class:draft={isDraft} tabindex="-1" aria-labelledby="{uid}-from {uid}-date">
    <header>
      <button class="who" onclick={ontoggle} title={t('message.collapse')}>
        <Avatar person={sender} size={36} />
      </button>
      <div class="info">
        <div class="row1">
          <span class="name selectable" id="{uid}-from">{fromMe ? t('message.senderMe') : displayName(sender)}</span>
          {#if !fromMe && vips.has(sender?.email)}<span class="vip" role="img" title={t('vip.badge')} aria-label={t('vip.badge')}><Crown size={13} /></span>{/if}
          {#if message.importance}<span class="importance" class:low={message.importance < 0}>{message.importance > 0 ? t('importance.high') : t('importance.low')}</span>{/if}
          {#if isDraft}<span class="draft-chip">{t('threadRow.draft')}</span>{/if}
          {#if sender && !fromMe && displayName(sender) !== sender.email}
            <span class="addr selectable">{sender.email}</span>
          {/if}
        </div>
        <button class="row2" onclick={() => (showRecipients = !showRecipients)}>{recipientLine}</button>
        {#if message.crypto}<CryptoStatus {message} {opened} />{/if}
        {#if showRecipients}
          <div class="recips selectable">
            <div><span>{t('message.headerFrom')}</span>{sender ? `${displayName(sender)} <${sender.email}>` : ''}</div>
            {#if message.to.length}<div><span>{t('message.headerTo')}</span>{message.to.map((a) => `${displayName(a)} <${a.email}>`).join(', ')}</div>{/if}
            {#if message.cc.length}<div><span>{t('message.headerCc')}</span>{message.cc.map((a) => `${displayName(a)} <${a.email}>`).join(', ')}</div>{/if}
            {#if message.replyTo.length}<div><span>{t('message.headerReplyTo')}</span>{message.replyTo.map((a) => a.email).join(', ')}</div>{/if}
            <div><span>{t('message.headerDate')}</span>{longDate(message.date)}</div>
          </div>
        {/if}
      </div>
      <div class="side">
        {#if message.unsubscribe}
          <button class="unsub" onclick={unsubscribe}>{t('message.unsubscribe')}</button>
        {/if}
        <time id="{uid}-date" title={longDate(message.date)}>{messageDate(message.date)}</time>
        <button class="icon-btn sm" class:starred={message.flags.flagged} title={t('message.star')} onclick={() => { message.flags.flagged = !message.flags.flagged; api.setFlags([], [message.id], { flagged: message.flags.flagged }); }}>
          <Star size={15} fill={message.flags.flagged ? 'currentColor' : 'none'} />
        </button>
        {#if isDraft}
          <button class="btn sm primary edit" onclick={() => compose.editMessage(message)}><Pen size={13} />{t('message.editDraft')}</button>
        {:else}
          <button class="icon-btn sm" title={t('reader.reply')} onclick={() => compose.reply(message.id, ui.settings.defaultReply)}><Reply size={16} /></button>
        {/if}
        <button class="icon-btn sm" title={t('common.more')} aria-label={t('common.more')} onclick={openMenu}><More size={16} /></button>
      </div>
    </header>

    {#if message.calendarMethod === 'REQUEST' || message.calendarMethod === 'CANCEL'}
      <InviteCard {message} />
    {/if}

    {#if stats.blocked > 0 && !remote}
      <div class="privacy">
        <Image size={15} />
        <span>{t('message.remoteImagesHidden')}</span>
        <button class="link" onclick={() => (allowRemote = true)}>{t('message.loadImages')}</button>
        {#if sender}<button class="link" onclick={trustSender}>{t('message.alwaysFromSender', { email: sender.email })}</button>{/if}
      </div>
    {/if}

    <div class="content">
      {#if message.crypto && !shown}
        <CryptoNotice {message} {opened} />
      {:else if shown?.body}
        <MailFrame
          message={shown}
          allowRemote={remote}
          onstats={(s) => (stats = s)}
          onimage={(items, index) => (bodyImages = { items, index })}
          onactivate={isDraft ? () => compose.editMessage(message) : undefined}
        />
      {:else}
        <div class="loading">
          <span class="skeleton" style="width:72%;height:12px"></span>
          <span class="skeleton" style="width:88%;height:12px"></span>
          <span class="skeleton" style="width:54%;height:12px"></span>
        </div>
      {/if}
    </div>

    {#if files.length}
      {#if files.length > 1}
        <div class="files-head">
          <span>{t('message.attachmentCount', { count: files.length })}</span>
          <button class="link save-all" onclick={saveAll} disabled={saving}><FolderDown size={14} />{t('message.saveAll')}</button>
        </div>
      {/if}
      <div class="files" class:headed={files.length > 1}>
        {#each files as a (a.id)}
          <AttachmentTile att={a} onpreview={(x) => (viewing = images.findIndex((i) => i.id === x.id))} />
        {/each}
      </div>
    {/if}

    {#if stats.trackers > 0}
      <div class="trackers"><ShieldCheck size={13} /> {t('message.trackersBlocked', { count: stats.trackers })}</div>
    {/if}
  </article>
{/if}

{#if found}
  <Popover x={found.x} y={found.y} align="end" onclose={() => (found = null)} width={300}>
    <div class="menu-label">{t('message.addToCalendar')}</div>
    {#each found.events as e, i (i)}
      <button class="menu-item found" onclick={() => { found = null; cal.fromExtracted(e); }}>
        <CalendarPlus size={15} />
        <span class="found-txt"><b class="truncate">{e.title}</b><span>{foundWhen(e)}{e.location ? ` · ${e.location}` : ''}</span></span>
      </button>
    {/each}
  </Popover>
{/if}

{#if viewing != null && viewing >= 0}
  <ImageViewer items={images} start={viewing} onclose={() => (viewing = null)} />
{/if}
{#if bodyImages}
  <ImageViewer items={bodyImages.items} start={bodyImages.index} onclose={() => (bodyImages = null)} />
{/if}

{#if menu}
  <Popover x={menu.x} y={menu.y} align="end" onclose={() => (menu = null)} width={230}>
    {#if isDraft}
      <button class="menu-item" onclick={() => { menu = null; compose.editMessage(message); }}><Pen size={15} />{t('message.editDraft')}</button>
    {:else}
      <button class="menu-item" onclick={() => { menu = null; compose.reply(message.id, 'reply'); }}><Reply size={15} />{t('reader.reply')}</button>
      <button class="menu-item" onclick={() => { menu = null; compose.reply(message.id, 'replyAll'); }}><ReplyAll size={15} />{t('reader.replyAll')}</button>
      <button class="menu-item" onclick={() => { menu = null; compose.reply(message.id, 'forward', false); }}><Forward size={15} />{t('reader.forward')}</button>
      <button class="menu-item" onclick={() => { menu = null; compose.forwardAsAttachment(message); }}><FileArrow size={15} />{t('message.forwardAsAttachment')}</button>
    {/if}
    <div class="divider"></div>
    <button class="menu-item" onclick={() => { menu = null; mail.markMessageRead(message, !message.flags.seen); }}>
      <Mail size={15} />{message.flags.seen ? t('message.markUnread') : t('message.markRead')}
    </button>
    <button class="menu-item" onclick={() => { menu = null; allowRemote = true; }}><Eye size={15} />{t('message.loadRemoteContent')}</button>
    <button class="menu-item" onclick={() => { menu = null; navigator.clipboard.writeText(sender?.email ?? ''); toasts.show(t('message.addressCopied')); }}><Copy size={15} />{t('message.copySenderAddress')}</button>
    {#if ai.ready}
      <button class="menu-item" onclick={addToCalendar}><CalendarPlus size={15} />{t('message.addToCalendar')}</button>
    {/if}
    {#if sender && !fromMe}
      <button
        class="menu-item"
        onclick={() => {
          menu = null;
          ui.rulesDraft = {
            id: '', name: displayName(sender!), enabled: true, accountId: message.accountId, match: 'all',
            conditions: [{ field: 'from', op: 'contains', value: sender!.email }], actions: [{ kind: 'archive' }], stop: false,
          };
          ui.open('settings', 'rules');
        }}><Filter size={15} />{t('message.createRule')}</button>
      {#if vips.has(sender.email)}
        <button class="menu-item" onclick={() => { menu = null; vips.set(sender!, false); }}><Crown size={15} />{t('vip.remove')}</button>
      {:else}
        <button class="menu-item" onclick={() => { menu = null; vips.set(sender!, true); }}><Crown size={15} />{t('vip.add')}</button>
      {/if}
      {#if isBlocked(sender.email)}
        <button class="menu-item" onclick={() => { menu = null; unblock(sender!.email.toLowerCase()); }}><Ban size={15} />{t('message.unblockSender')}</button>
      {:else}
        <button class="menu-item" onclick={() => { menu = null; blockSender(sender!); }}><Ban size={15} />{t('message.blockSender')}</button>
      {/if}
    {/if}
    <button class="menu-item" onclick={() => { menu = null; printMessage(shown ?? message, remote); }}><Printer size={15} />{t('message.print')}</button>
    <button class="menu-item" onclick={viewSource}><Code size={15} />{t('message.viewSource')}</button>
  </Popover>
{/if}

{#if source !== null}
  <div class="source-backdrop" role="presentation" onclick={() => (source = null)}>
    <div class="source" role="dialog" aria-labelledby="{uid}-source" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === 'Escape' && (source = null)} use:modal>
      <header><strong id="{uid}-source">{t('message.source')}</strong><button class="btn sm" onclick={() => { navigator.clipboard.writeText(source ?? ''); toasts.show(t('common.copied')); }}>{t('common.copy')}</button><button class="btn sm" onclick={() => (source = null)}>{t('common.close')}</button></header>
      <pre class="selectable">{source}</pre>
    </div>
  </div>
{/if}

<style>
  /* Long translations wrap onto a second line instead of spilling out of the fixed-width menu. */
  .menu-item {
    height: auto;
    min-height: 32px;
    padding-top: 6px;
    padding-bottom: 6px;
  }
  .found {
    height: auto;
    padding-top: 6px;
    padding-bottom: 6px;
    align-items: flex-start;
  }
  .found :global(svg) {
    margin-top: 2px;
  }
  .found-txt {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    text-align: left;
  }
  .found-txt span {
    font-size: var(--text-sm);
    color: var(--text-3);
  }
  .collapsed {
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    height: 50px;
    padding: 0 16px;
    border-radius: 12px;
    text-align: left;
    background: var(--bg-elevated);
    box-shadow: 0 0 0 1px var(--border);
    transition: background var(--dur-fast);
  }
  .collapsed:hover {
    background: var(--bg-hover);
  }
  .c-name {
    font-weight: 550;
    flex-shrink: 0;
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .c-name.unread {
    font-weight: 700;
  }
  .c-snippet {
    flex: 1;
    min-width: 0;
    color: var(--text-3);
  }
  .c-date {
    color: var(--text-3);
    font-size: var(--text-sm);
    flex-shrink: 0;
  }
  .card {
    border-radius: 14px;
    background: var(--bg-elevated);
    box-shadow: 0 0 0 1px var(--border), var(--shadow-xs);
    padding: 16px 18px 18px;
    animation: fade-in var(--dur) var(--ease);
  }
  header {
    display: flex;
    gap: 12px;
    align-items: flex-start;
  }
  .who {
    border-radius: 50%;
  }
  .info {
    flex: 1;
    min-width: 0;
  }
  .row1 {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }
  .name {
    font-weight: 650;
    font-size: var(--text-md);
  }
  .importance {
    flex-shrink: 0;
    align-self: center;
    padding: 1px 7px;
    border-radius: 999px;
    font-size: 10.5px;
    font-weight: 650;
    color: var(--red);
    background: var(--red-soft);
  }
  .importance.low {
    color: var(--text-3);
    background: var(--bg-active);
  }
  .vip {
    flex-shrink: 0;
    align-self: center;
    display: inline-grid;
    color: var(--amber-text);
  }
  .addr {
    color: var(--text-3);
    font-size: var(--text-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .row2 {
    color: var(--text-3);
    font-size: var(--text-sm);
    text-align: left;
    margin-top: 1px;
  }
  .row2:hover {
    color: var(--text-2);
  }
  .recips {
    margin-top: 8px;
    padding: 10px 12px;
    border-radius: 10px;
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    color: var(--text-2);
    display: grid;
    gap: 4px;
    word-break: break-all;
  }
  .recips span {
    display: inline-block;
    min-width: 64px;
    padding-right: 8px;
    color: var(--text-3);
  }
  .side {
    display: flex;
    align-items: center;
    gap: 2px;
    color: var(--text-3);
    font-size: var(--text-sm);
    flex-shrink: 0;
  }
  .side time {
    margin-right: 6px;
    white-space: nowrap;
  }
  .unsub {
    font-size: 11.5px;
    font-weight: 600;
    color: var(--text-2);
    height: 24px;
    padding: 0 9px;
    margin-right: 6px;
    border-radius: 7px;
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .unsub:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
  .starred {
    color: var(--star) !important;
  }
  .privacy {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    margin: 14px 0 0;
    padding: 8px 12px;
    border-radius: 10px;
    background: var(--bg-sunken);
    color: var(--text-2);
    font-size: var(--text-sm);
  }
  .privacy span {
    flex: 1;
  }
  .link {
    color: var(--accent-text);
    font-weight: 600;
    font-size: var(--text-sm);
  }
  .content {
    margin-top: 14px;
    padding-left: 48px;
  }
  .loading {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 6px 0;
  }
  .files {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 16px;
    padding-left: 48px;
  }
  .files.headed {
    margin-top: 8px;
  }
  .files-head {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 16px;
    padding-left: 48px;
    font-size: var(--text-sm);
    color: var(--text-3);
  }
  .save-all {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 24px;
    padding: 0 8px;
    margin-left: -8px;
    border-radius: 7px;
  }
  .save-all:hover {
    background: var(--accent-softer);
  }
  .save-all:disabled {
    opacity: 0.55;
  }
  .draft-chip {
    flex-shrink: 0;
    font-size: 11px;
    font-weight: 650;
    letter-spacing: 0.01em;
    color: var(--red);
    padding: 1px 7px;
    border-radius: 999px;
    background: var(--red-soft);
  }
  .card.draft {
    box-shadow: 0 0 0 1px color-mix(in oklab, var(--red) 28%, var(--border)), var(--shadow-xs);
  }
  .edit {
    margin-right: 2px;
  }
  .trackers {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    margin: 12px 0 0 48px;
    font-size: 11.5px;
    color: var(--green);
    font-weight: 550;
  }
  .source-backdrop {
    position: fixed;
    inset: 0;
    z-index: 900;
    background: var(--overlay);
    display: grid;
    place-items: center;
    animation: fade-in var(--dur);
  }
  .source {
    width: min(900px, 90vw);
    height: min(700px, 85vh);
    display: flex;
    flex-direction: column;
    border-radius: 14px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
  }
  .source header {
    align-items: center;
    padding: 10px 14px;
    border-bottom: 1px solid var(--border);
  }
  .source header strong {
    flex: 1;
  }
  .source pre {
    flex: 1;
    margin: 0;
    padding: 14px;
    overflow: auto;
    font-family: var(--font-mono);
    font-size: 12px;
    white-space: pre-wrap;
  }
  @container (max-width: 640px) {
    .content,
    .files,
    .files-head {
      padding-left: 0;
    }
    .trackers {
      margin-left: 0;
    }
  }
</style>
