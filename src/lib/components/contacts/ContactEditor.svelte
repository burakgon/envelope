<script lang="ts">
  import { onMount } from 'svelte';
  import { fieldLabel, people } from '../../stores/contacts.svelte';
  import type { LabeledValue } from '../../api';
  import { t } from '../../i18n';
  import Avatar from '../Avatar.svelte';
  import { Camera, Plus, X } from '../../icons';

  const c = people.editing!;
  // Exchange keeps three e-mail addresses and three postal addresses (work, home, other).
  const most = $derived(people.books.find((b) => b.id === c.bookId)?.kind === 'ews' ? 3 : Infinity);
  let first: HTMLInputElement | undefined = $state();
  let file: HTMLInputElement | undefined = $state();
  onMount(() => first?.focus());

  // Stored label values; the options show them translated.
  const emailLabels = ['work', 'home', 'other'];
  const phoneLabels = ['mobile', 'work', 'home', 'other'];
  const addressLabels = ['home', 'work', 'other'];
  const name = $derived([c.givenName, c.familyName].filter((x) => x?.trim()).join(' ') || c.name || t('contacts.newContact'));

  function add(list: LabeledValue[], label: string) {
    list.push({ value: '', label });
  }

  function remove(list: LabeledValue[], i: number) {
    list.splice(i, 1);
  }

  /** Loads a picture as a small square JPEG data URL. */
  function pickPhoto(e: Event) {
    const f = (e.currentTarget as HTMLInputElement).files?.[0];
    if (!f) return;
    const img = new Image();
    img.onload = () => {
      const size = 256;
      const canvas = document.createElement('canvas');
      canvas.width = canvas.height = size;
      const ctx = canvas.getContext('2d')!;
      const s = Math.min(img.width, img.height);
      ctx.drawImage(img, (img.width - s) / 2, (img.height - s) / 2, s, s, 0, 0, size, size);
      c.photo = canvas.toDataURL('image/jpeg', 0.86);
      URL.revokeObjectURL(img.src);
    };
    img.src = URL.createObjectURL(f);
  }

  function key(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      people.closeEditor();
    } else if ((e.ctrlKey || e.metaKey) && (e.key === 'Enter' || e.key === 's')) {
      e.preventDefault();
      people.save();
    }
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="editor" onkeydown={key}>
  <div class="top">
    <button class="photo-btn" title={t('contacts.changePhoto')} onclick={() => file?.click()}>
      {#if c.photo}
        <img src={c.photo} alt="" />
      {:else}
        <Avatar person={{ name, email: c.emails[0]?.value || name }} size={84} />
      {/if}
      <span class="cam"><Camera size={15} /></span>
    </button>
    <input bind:this={file} type="file" accept="image/*" hidden onchange={pickPhoto} />
    <div class="names">
      <div class="two">
        <input bind:this={first} class="input" placeholder={t('contacts.firstName')} bind:value={c.givenName} />
        <input class="input" placeholder={t('contacts.lastName')} bind:value={c.familyName} />
      </div>
      <div class="two">
        <input class="input" placeholder={t('contacts.company')} bind:value={c.organization} />
        <input class="input" placeholder={t('contacts.jobTitle')} bind:value={c.title} />
      </div>
      {#if c.photo}<button class="text-btn" onclick={() => (c.photo = null)}>{t('contacts.removePhoto')}</button>{/if}
    </div>
  </div>

  <div class="group">
    <div class="label">{t('contacts.email')}</div>
    {#each c.emails as e, i (i)}
      <div class="line">
        <select class="input sel" bind:value={e.label}>{#each emailLabels as l (l)}<option value={l}>{fieldLabel(l)}</option>{/each}</select>
        <input class="input" type="email" placeholder={t('contacts.emailPlaceholder')} bind:value={e.value} />
        <button class="icon-btn sm" aria-label={t('common.remove')} onclick={() => remove(c.emails, i)}><X size={14} /></button>
      </div>
    {/each}
    {#if c.emails.length < most}
      <button class="add" onclick={() => add(c.emails, c.emails.length ? 'home' : 'work')}><Plus size={14} />{t('contacts.addEmail')}</button>
    {/if}
  </div>

  <div class="group">
    <div class="label">{t('contacts.phone')}</div>
    {#each c.phones as p, i (i)}
      <div class="line">
        <select class="input sel" bind:value={p.label}>{#each phoneLabels as l (l)}<option value={l}>{fieldLabel(l)}</option>{/each}</select>
        <input class="input" type="tel" placeholder={t('contacts.phonePlaceholder')} bind:value={p.value} />
        <button class="icon-btn sm" aria-label={t('common.remove')} onclick={() => remove(c.phones, i)}><X size={14} /></button>
      </div>
    {/each}
    <button class="add" onclick={() => add(c.phones, 'mobile')}><Plus size={14} />{t('contacts.addPhone')}</button>
  </div>

  <div class="group">
    <div class="label">{t('contacts.address')}</div>
    {#each c.addresses as a, i (i)}
      <div class="line top-align">
        <select class="input sel" bind:value={a.label}>{#each addressLabels as l (l)}<option value={l}>{fieldLabel(l)}</option>{/each}</select>
        <textarea class="input area" rows="3" placeholder={t('contacts.addressPlaceholder')} bind:value={a.value}></textarea>
        <button class="icon-btn sm" aria-label={t('common.remove')} onclick={() => remove(c.addresses, i)}><X size={14} /></button>
      </div>
    {/each}
    {#if c.addresses.length < most}
      <button class="add" onclick={() => add(c.addresses, 'home')}><Plus size={14} />{t('contacts.addAddress')}</button>
    {/if}
  </div>

  <div class="group two-col">
    <div>
      <label class="label" for="ct-bday">{t('contacts.birthday')}</label>
      <input id="ct-bday" class="input" placeholder={t('contacts.birthdayPlaceholder')} bind:value={c.birthday} />
    </div>
    <div>
      <label class="label" for="ct-web">{t('contacts.website')}</label>
      <input
        id="ct-web"
        class="input"
        placeholder="https://"
        value={c.urls[0] ?? ''}
        oninput={(e) => (c.urls = [(e.currentTarget as HTMLInputElement).value, ...c.urls.slice(1)])}
      />
    </div>
  </div>

  <div class="group">
    <label class="label" for="ct-notes">{t('contacts.notes')}</label>
    <textarea id="ct-notes" class="input area" rows="4" bind:value={c.notes}></textarea>
  </div>

  {#if c.id == null && people.writable.length > 1}
    <div class="group">
      <label class="label" for="ct-book">{t('contacts.addressBook')}</label>
      <select id="ct-book" class="input" bind:value={c.bookId}>
        {#each people.writable as b (b.id)}<option value={b.id}>{b.name}</option>{/each}
      </select>
    </div>
  {/if}

  <footer>
    <button class="btn" onclick={() => people.closeEditor()}>{t('common.cancel')}</button>
    <button class="btn primary" onclick={() => people.save()}>{c.id == null ? t('contacts.addContact') : t('common.save')}</button>
  </footer>
</div>

<style>
  .editor {
    flex: 1;
    overflow-y: auto;
    padding: 12px 40px 40px;
  }
  .editor > * {
    max-width: 640px;
    margin-left: auto;
    margin-right: auto;
  }
  .top {
    display: flex;
    gap: 22px;
    align-items: flex-start;
    margin-bottom: 22px;
  }
  .photo-btn {
    position: relative;
    width: 84px;
    height: 84px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .photo-btn img {
    width: 84px;
    height: 84px;
    border-radius: 50%;
    object-fit: cover;
  }
  .cam {
    position: absolute;
    right: -2px;
    bottom: -2px;
    width: 30px;
    height: 30px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    color: var(--text-2);
    background: var(--bg-elevated);
    box-shadow: var(--shadow-sm), inset 0 0 0 1px var(--border);
  }
  .names {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }
  .text-btn {
    align-self: flex-start;
    font-size: var(--text-sm);
    color: var(--text-3);
  }
  .text-btn:hover {
    color: var(--red);
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 18px;
  }
  .two-col {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  .label {
    margin-bottom: 0;
  }
  .line {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .line.top-align {
    align-items: flex-start;
  }
  /* As wide as the longest translated type, but no narrower than before. */
  .sel {
    width: auto;
    min-width: 118px;
    max-width: 45%;
    flex-shrink: 0;
  }
  .area {
    height: auto;
    padding: 8px 11px;
    resize: vertical;
  }
  .add {
    align-self: flex-start;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 8px;
    border-radius: 7px;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--accent-text);
  }
  .add:hover {
    background: var(--accent-softer);
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding-top: 8px;
  }
</style>
