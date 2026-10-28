<script lang="ts">
  import { api } from '../api';
  import type { Address, MessageView } from '../api';
  import { compose } from '../stores/compose.svelte';
  import { mail } from '../stores/mail.svelte';
  import { ui } from '../stores/ui.svelte';
  import { errorMessage, toasts } from '../stores/toasts.svelte';
  import { displayName, longDate, messageDate } from '../utils/format';
  import Avatar from './Avatar.svelte';
  import MailFrame from './MailFrame.svelte';
  import AttachmentTile from './AttachmentTile.svelte';
  import Popover from './Popover.svelte';
  import InviteCard from './InviteCard.svelte';
  import ImageViewer from './ImageViewer.svelte';
  import { printMessage } from '../utils/print';
  import { fileIconKind } from '../utils/format';
  import { Code, Eye, Forward, Image, Mail, More, Printer, Reply, ReplyAll, ShieldCheck, Star, Copy } from '../icons';

  let { message, expanded, ontoggle }: { message: MessageView; expanded: boolean; ontoggle: () => void } = $props();

  let allowRemote = $state(false);
  let stats = $state({ blocked: 0, trackers: 0, designed: false });
  let menu = $state<{ x: number; y: number } | null>(null);
  let showRecipients = $state(false);
  let source = $state<string | null>(null);
  let viewing = $state<number | null>(null);

  const sender = $derived(message.from);
  const fromMe = $derived(!!sender && mail.meEmails.has(sender.email.toLowerCase()));
  const trusted = $derived(!!sender && ui.settings.trustedSenders.includes(sender.email.toLowerCase()));
  const remote = $derived(allowRemote || trusted || ui.settings.loadRemoteImages === 'always');
  const files = $derived(message.attachments.filter((a) => !a.inline));
  const images = $derived(files.filter((a) => fileIconKind(a.mimeType, a.filename) === 'image'));

  function nameOrMe(a: Address) {
    return mail.meEmails.has(a.email.toLowerCase()) ? 'me' : displayName(a);
  }

  const recipientLine = $derived.by(() => {
    const all = [...message.to, ...message.cc];
    if (all.length === 0) return '';
    const names = all.slice(0, 3).map(nameOrMe);
    return `to ${names.join(', ')}${all.length > 3 ? ` +${all.length - 3}` : ''}`;
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
      toasts.show(done ? 'Unsubscribed' : 'Opened unsubscribe page', { kind: 'success' });
    } catch (e) {
      toasts.error(errorMessage(e));
    }
  }
</script>

{#if !expanded}
  <button class="collapsed" onclick={ontoggle}>
    <Avatar person={sender} size={28} />
    <span class="c-name" class:unread={!message.flags.seen}>{fromMe ? 'Me' : displayName(sender)}</span>
    <span class="c-snippet truncate">{message.snippet}</span>
    <time class="c-date">{messageDate(message.date)}</time>
  </button>
{:else}
  <article class="card">
    <header>
      <button class="who" onclick={ontoggle} title="Collapse">
        <Avatar person={sender} size={36} />
      </button>
      <div class="info">
        <div class="row1">
          <span class="name selectable">{fromMe ? 'Me' : displayName(sender)}</span>
          {#if sender && !fromMe && displayName(sender) !== sender.email}
            <span class="addr selectable">{sender.email}</span>
          {/if}
        </div>
        <button class="row2" onclick={() => (showRecipients = !showRecipients)}>{recipientLine}</button>
        {#if showRecipients}
          <div class="recips selectable">
            <div><span>From</span>{sender ? `${displayName(sender)} <${sender.email}>` : ''}</div>
            {#if message.to.length}<div><span>To</span>{message.to.map((a) => `${displayName(a)} <${a.email}>`).join(', ')}</div>{/if}
            {#if message.cc.length}<div><span>Cc</span>{message.cc.map((a) => `${displayName(a)} <${a.email}>`).join(', ')}</div>{/if}
            {#if message.replyTo.length}<div><span>Reply-To</span>{message.replyTo.map((a) => a.email).join(', ')}</div>{/if}
            <div><span>Date</span>{longDate(message.date)}</div>
          </div>
        {/if}
      </div>
      <div class="side">
        {#if message.unsubscribe}
          <button class="unsub" onclick={unsubscribe}>Unsubscribe</button>
        {/if}
        <time title={longDate(message.date)}>{messageDate(message.date)}</time>
        <button class="icon-btn sm" class:starred={message.flags.flagged} title="Star" onclick={() => { message.flags.flagged = !message.flags.flagged; api.setFlags([], [message.id], { flagged: message.flags.flagged }); }}>
          <Star size={15} fill={message.flags.flagged ? 'currentColor' : 'none'} />
        </button>
        <button class="icon-btn sm" title="Reply" onclick={() => compose.reply(message.id, ui.settings.defaultReply)}><Reply size={16} /></button>
        <button class="icon-btn sm" title="More" onclick={openMenu}><More size={16} /></button>
      </div>
    </header>

    {#if message.calendarMethod === 'REQUEST' || message.calendarMethod === 'CANCEL'}
      <InviteCard {message} />
    {/if}

    {#if stats.blocked > 0 && !remote}
      <div class="privacy">
        <Image size={15} />
        <span>Remote images are hidden to protect your privacy.</span>
        <button class="link" onclick={() => (allowRemote = true)}>Load images</button>
        {#if sender}<button class="link" onclick={trustSender}>Always from {sender.email.split('@')[1]}</button>{/if}
      </div>
    {/if}

    <div class="content">
      {#if message.body}
        <MailFrame {message} allowRemote={remote} onstats={(s) => (stats = s)} />
      {:else}
        <div class="loading">
          <span class="skeleton" style="width:72%;height:12px"></span>
          <span class="skeleton" style="width:88%;height:12px"></span>
          <span class="skeleton" style="width:54%;height:12px"></span>
        </div>
      {/if}
    </div>

    {#if files.length}
      <div class="files">
        {#each files as a (a.id)}
          <AttachmentTile att={a} onpreview={(x) => (viewing = images.findIndex((i) => i.id === x.id))} />
        {/each}
      </div>
    {/if}

    {#if stats.trackers > 0}
      <div class="trackers"><ShieldCheck size={13} /> Blocked {stats.trackers} {stats.trackers === 1 ? 'tracker' : 'trackers'}</div>
    {/if}
  </article>
{/if}

{#if viewing != null && viewing >= 0}
  <ImageViewer items={images} start={viewing} onclose={() => (viewing = null)} />
{/if}

{#if menu}
  <Popover x={menu.x} y={menu.y} align="end" onclose={() => (menu = null)} width={230}>
    <button class="menu-item" onclick={() => { menu = null; compose.reply(message.id, 'reply'); }}><Reply size={15} />Reply</button>
    <button class="menu-item" onclick={() => { menu = null; compose.reply(message.id, 'replyAll'); }}><ReplyAll size={15} />Reply all</button>
    <button class="menu-item" onclick={() => { menu = null; compose.reply(message.id, 'forward', false); }}><Forward size={15} />Forward</button>
    <div class="divider"></div>
    <button class="menu-item" onclick={() => { menu = null; mail.markMessageRead(message, !message.flags.seen); }}>
      <Mail size={15} />{message.flags.seen ? 'Mark as unread' : 'Mark as read'}
    </button>
    <button class="menu-item" onclick={() => { menu = null; allowRemote = true; }}><Eye size={15} />Load remote content</button>
    <button class="menu-item" onclick={() => { menu = null; navigator.clipboard.writeText(sender?.email ?? ''); toasts.show('Address copied'); }}><Copy size={15} />Copy sender address</button>
    <button class="menu-item" onclick={() => { menu = null; printMessage(message, remote); }}><Printer size={15} />Print…</button>
    <button class="menu-item" onclick={viewSource}><Code size={15} />View source</button>
  </Popover>
{/if}

{#if source !== null}
  <div class="source-backdrop" role="presentation" onclick={() => (source = null)}>
    <div class="source" role="dialog" aria-label="Message source" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === 'Escape' && (source = null)}>
      <header><strong>Message source</strong><button class="btn sm" onclick={() => { navigator.clipboard.writeText(source ?? ''); toasts.show('Copied'); }}>Copy</button><button class="btn sm" onclick={() => (source = null)}>Close</button></header>
      <pre class="selectable">{source}</pre>
    </div>
  </div>
{/if}

<style>
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
    width: 64px;
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
  @media (max-width: 1100px) {
    .content,
    .files {
      padding-left: 0;
    }
    .trackers {
      margin-left: 0;
    }
  }
</style>
