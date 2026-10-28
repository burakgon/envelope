<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../../api';
  import type { Contact, ThreadSummary } from '../../api';
  import { fieldLabel, people } from '../../stores/contacts.svelte';
  import { compose } from '../../stores/compose.svelte';
  import { mail } from '../../stores/mail.svelte';
  import { ui } from '../../stores/ui.svelte';
  import { dateFormat, listDate } from '../../utils/format';
  import { t } from '../../i18n';
  import Avatar from '../Avatar.svelte';
  import { Cake, Globe, Mail, MapPin, Pen, Phone, StickyNote, Trash } from '../../icons';

  let { contact: c }: { contact: Contact } = $props();

  let threads = $state<ThreadSummary[]>([]);
  let confirmDelete = $state(false);
  const book = $derived(people.books.find((b) => b.id === c.bookId));
  const primary = $derived(c.emails[0]?.value ?? null);

  onMount(async () => {
    const address = c.emails[0]?.value;
    if (!address) return;
    try {
      const page = await api.threads({ mailbox: { kind: 'everything' }, search: address, offset: 0, limit: 8 });
      threads = page.items;
    } catch {
      threads = [];
    }
  });

  function write(email: string) {
    compose.new({ to: [{ name: c.name, email }] });
  }

  function openThread(th: ThreadSummary) {
    ui.view = 'mail';
    mail.setSearch(primary ?? '');
    mail.select(th.id);
  }

  function birthday(b: string): string {
    const yearless = b.startsWith('--');
    const d = new Date(yearless ? `2000-${b.slice(2)}T00:00:00` : `${b}T00:00:00`);
    if (Number.isNaN(d.getTime())) return b;
    const text = dateFormat(yearless ? { month: 'long', day: 'numeric' } : { year: 'numeric', month: 'long', day: 'numeric' }).format(d);
    if (yearless) return text;
    const now = new Date();
    let age = now.getFullYear() - d.getFullYear();
    if (now.getMonth() < d.getMonth() || (now.getMonth() === d.getMonth() && now.getDate() < d.getDate())) age--;
    return t('contacts.birthdayWithAge', { date: text, age });
  }
</script>

<div class="detail">
  <div class="hero">
    {#if c.photo}
      <img class="photo" src={c.photo} alt="" />
    {:else}
      <Avatar person={{ name: c.name, email: primary ?? c.name }} size={84} />
    {/if}
    <div class="who">
      <h2 class="selectable">{c.name}</h2>
      {#if c.title || c.organization}
        <p class="selectable">{[c.title, c.organization].filter(Boolean).join(' · ')}</p>
      {/if}
      <div class="actions">
        {#if primary}<button class="btn primary sm" onclick={() => write(primary)}><Mail size={15} />{t('contacts.sendEmail')}</button>{/if}
        {#if c.phones[0]}<button class="btn sm" onclick={() => api.openUrl(`tel:${c.phones[0].value.replace(/\s/g, '')}`)}><Phone size={15} />{t('contacts.call')}</button>{/if}
        {#if !book?.readOnly}
          <button class="btn sm" onclick={() => people.edit()}><Pen size={14} />{t('common.edit')}</button>
          {#if confirmDelete}
            <button class="btn danger sm" onclick={() => people.remove()}>{t('contacts.deleteContact')}</button>
            <button class="btn sm" onclick={() => (confirmDelete = false)}>{t('common.cancel')}</button>
          {:else}
            <button class="icon-btn sm" title={t('common.delete')} onclick={() => (confirmDelete = true)}><Trash size={15} /></button>
          {/if}
        {/if}
      </div>
    </div>
  </div>

  <div class="cards">
    <div class="card">
      {#each c.emails as e (e.value)}
        <div class="field">
          <Mail size={16} />
          <div class="val">
            <button class="link" onclick={() => write(e.value)}>{e.value}</button>
            {#if e.label}<span class="lbl">{fieldLabel(e.label)}</span>{/if}
          </div>
        </div>
      {/each}
      {#each c.phones as p (p.value)}
        <div class="field">
          <Phone size={16} />
          <div class="val">
            <span class="selectable">{p.value}</span>
            {#if p.label}<span class="lbl">{fieldLabel(p.label)}</span>{/if}
          </div>
        </div>
      {/each}
      {#each c.addresses as a (a.value)}
        <div class="field">
          <MapPin size={16} />
          <div class="val">
            <button class="link addr" onclick={() => api.openUrl(`https://www.openstreetmap.org/search?query=${encodeURIComponent(a.value.replace(/\n/g, ', '))}`)}>{a.value}</button>
            {#if a.label}<span class="lbl">{fieldLabel(a.label)}</span>{/if}
          </div>
        </div>
      {/each}
      {#if c.birthday}
        <div class="field"><Cake size={16} /><div class="val"><span>{birthday(c.birthday)}</span><span class="lbl">{t('contacts.birthday')}</span></div></div>
      {/if}
      {#each c.urls as u (u)}
        <div class="field"><Globe size={16} /><div class="val"><button class="link" onclick={() => api.openUrl(u)}>{u.replace(/^https?:\/\//, '')}</button></div></div>
      {/each}
      {#if c.notes}
        <div class="field"><StickyNote size={16} /><div class="val"><span class="notes selectable">{c.notes}</span></div></div>
      {/if}
      {#if !c.emails.length && !c.phones.length && !c.addresses.length && !c.birthday && !c.urls.length && !c.notes}
        <div class="nothing">{t('contacts.noDetails')}</div>
      {/if}
    </div>

    {#if threads.length}
      <div class="card">
        <div class="head">{t('contacts.recentConversations')}</div>
        {#each threads as th (th.id)}
          <button class="thread" onclick={() => openThread(th)}>
            <span class="subject truncate" class:unread={th.unreadCount > 0}>{th.subject || t('common.noSubject')}</span>
            <span class="snippet truncate">{th.snippet}</span>
            <span class="date">{listDate(th.lastDate)}</span>
          </button>
        {/each}
      </div>
    {/if}

    {#if book}<div class="book">{t('contacts.inBook', { book: book.name })}</div>{/if}
  </div>
</div>

<style>
  .detail {
    flex: 1;
    overflow-y: auto;
    padding: 12px 40px 40px;
  }
  .hero {
    display: flex;
    gap: 22px;
    align-items: center;
    max-width: 760px;
    margin: 0 auto 22px;
  }
  .photo {
    width: 84px;
    height: 84px;
    border-radius: 50%;
    object-fit: cover;
  }
  .who {
    flex: 1;
    min-width: 0;
  }
  h2 {
    margin: 0;
    font-size: 24px;
    font-weight: 700;
    letter-spacing: -0.015em;
  }
  .who p {
    margin: 2px 0 0;
    color: var(--text-2);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 12px;
  }
  .actions .btn {
    gap: 6px;
  }
  .cards {
    max-width: 760px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .card {
    padding: 6px 4px;
    border-radius: 14px;
    background: var(--bg-elevated);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .field {
    display: flex;
    gap: 14px;
    padding: 10px 16px;
    color: var(--text-3);
  }
  .field + .field {
    border-top: 1px solid var(--border);
  }
  .field :global(svg) {
    flex-shrink: 0;
    margin-top: 2px;
  }
  .val {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
    color: var(--text);
  }
  .lbl {
    font-size: var(--text-sm);
    color: var(--text-3);
  }
  .link {
    align-self: flex-start;
    color: var(--accent-text);
    text-align: left;
  }
  .link:hover {
    text-decoration: underline;
  }
  .addr,
  .notes {
    white-space: pre-line;
  }
  .nothing {
    padding: 14px 16px;
    color: var(--text-3);
  }
  .head {
    padding: 10px 16px 6px;
    font-size: 11.5px;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-3);
  }
  .thread {
    display: grid;
    grid-template-columns: minmax(0, 2fr) minmax(0, 3fr) auto;
    gap: 14px;
    align-items: center;
    width: 100%;
    height: 38px;
    padding: 0 16px;
    border-radius: 9px;
    text-align: left;
  }
  .thread:hover {
    background: var(--bg-hover);
  }
  .subject.unread {
    font-weight: 650;
  }
  .snippet {
    color: var(--text-3);
    font-size: var(--text-sm);
  }
  .date {
    color: var(--text-3);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }
  .book {
    text-align: center;
    font-size: var(--text-sm);
    color: var(--text-4);
  }
</style>
