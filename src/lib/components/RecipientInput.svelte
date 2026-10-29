<script lang="ts">
  import { api } from '../api';
  import type { Address, ContactSuggestion, GroupSuggestion } from '../api';
  import { displayName } from '../utils/format';
  import { t } from '../i18n';
  import Avatar from './Avatar.svelte';
  import { UsersRound, X } from '../icons';

  /** A suggestion: a person's address, or a contact group (all its people at once). */
  type Suggestion = { person: ContactSuggestion; group?: undefined } | { group: GroupSuggestion; person?: undefined };

  let {
    label,
    value = $bindable(),
    onchange,
    autofocus = false,
    children,
  }: {
    label: string;
    value: Address[];
    onchange?: () => void;
    autofocus?: boolean;
    children?: import('svelte').Snippet;
  } = $props();

  // The label is translated text, so it doesn't make a good element id.
  const uid = $props.id();

  let text = $state('');
  let input: HTMLInputElement | undefined = $state();
  let suggestions = $state<Suggestion[]>([]);
  let hi = $state(0);
  let focused = $state(false);
  let selectedChip = $state<number | null>(null);
  let timer: ReturnType<typeof setTimeout> | null = null;

  const EMAIL = /^[^\s@<>()]+@[^\s@<>()]+\.[^\s@<>()]+$/;

  $effect(() => {
    if (autofocus) input?.focus();
  });

  /** Parses "Name <a@b.c>, d@e.f" into addresses. */
  function parse(raw: string): Address[] {
    const out: Address[] = [];
    const re = /\s*(?:"?([^"<,;]*?)"?\s*<([^>]+)>|([^\s,;<>]+@[^\s,;<>]+))\s*[,;]?/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(raw))) {
      const email = (m[2] ?? m[3] ?? '').trim();
      if (!email) continue;
      const name = (m[1] ?? '').trim();
      out.push({ email, name: name || null });
    }
    return out;
  }

  function add(list: Address[]) {
    const existing = new Set(value.map((a) => a.email.toLowerCase()));
    // Each address once, also within the list (a group's people can share one).
    const fresh: Address[] = [];
    for (const a of list) {
      if (existing.has(a.email.toLowerCase())) continue;
      existing.add(a.email.toLowerCase());
      fresh.push(a);
    }
    if (fresh.length) {
      value = [...value, ...fresh];
      onchange?.();
    }
    text = '';
    suggestions = [];
  }

  function remove(i: number) {
    value = value.filter((_, j) => j !== i);
    selectedChip = null;
    onchange?.();
    input?.focus();
  }

  function commitText(): boolean {
    const parsed = parse(text);
    if (parsed.length) {
      add(parsed);
      return true;
    }
    return false;
  }

  /** Takes in an address still being typed (Send was pressed before it became a chip). */
  export function flush() {
    if (text.trim()) commitText();
  }

  function onInput() {
    selectedChip = null;
    if (/[,;]$/.test(text) && commitText()) return;
    if (timer) clearTimeout(timer);
    const q = text.trim();
    if (!q) {
      suggestions = [];
      return;
    }
    timer = setTimeout(async () => {
      const [people, groups] = await Promise.all([api.suggestContacts(q), api.suggestGroups(q).catch(() => [])]);
      const taken = new Set(value.map((a) => a.email.toLowerCase()));
      // Groups first (at most two), unless everyone in them is there already.
      const lists: Suggestion[] = groups
        .filter((g) => g.addresses.some((a) => !taken.has(a.email.toLowerCase())))
        .slice(0, 2)
        .map((group) => ({ group }));
      const persons: Suggestion[] = people.filter((s) => !taken.has(s.email.toLowerCase())).map((person) => ({ person }));
      suggestions = [...lists, ...persons].slice(0, 6);
      hi = 0;
    }, 70);
  }

  /** Adds a person's address, or every address of a group. */
  function pick(s: Suggestion) {
    add(s.group ? s.group.addresses : [{ email: s.person.email, name: s.person.name }]);
    input?.focus();
  }

  function onKey(e: KeyboardEvent) {
    if (suggestions.length && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
      e.preventDefault();
      hi = (hi + (e.key === 'ArrowDown' ? 1 : -1) + suggestions.length) % suggestions.length;
      return;
    }
    if ((e.key === 'Enter' || e.key === 'Tab') && suggestions.length && text.trim()) {
      e.preventDefault();
      pick(suggestions[hi]);
      return;
    }
    if ((e.key === 'Enter' || e.key === 'Tab' || e.key === ' ') && text.trim() && EMAIL.test(text.trim())) {
      if (e.key !== 'Tab') e.preventDefault();
      commitText();
      return;
    }
    if (e.key === 'Backspace' && !text && value.length) {
      e.preventDefault();
      if (selectedChip === null) selectedChip = value.length - 1;
      else remove(selectedChip);
      return;
    }
    if (e.key === 'Escape' && suggestions.length) {
      e.stopPropagation();
      suggestions = [];
    }
  }

  function onPaste(e: ClipboardEvent) {
    const pasted = e.clipboardData?.getData('text') ?? '';
    const parsed = parse(pasted);
    if (parsed.length > 1 || (parsed.length === 1 && pasted.includes('<'))) {
      e.preventDefault();
      add(parsed);
    }
  }

  function onBlur() {
    focused = false;
    setTimeout(() => {
      if (!focused && text.trim()) commitText();
      suggestions = [];
    }, 150);
  }
</script>

<div class="field" class:focused>
  <label class="lbl" for="rcpt-{uid}">{label}</label>
  <div class="chips" role="presentation" onclick={() => input?.focus()}>
    {#each value as a, i (a.email + i)}
      <span class="chip-addr" class:invalid={!EMAIL.test(a.email)} class:sel={selectedChip === i} title={a.email}>
        {displayName(a)}
        <button aria-label={t('recipients.remove', { email: a.email })} onclick={(e) => { e.stopPropagation(); remove(i); }}><X size={11} strokeWidth={2.5} /></button>
      </span>
    {/each}
    <input
      id="rcpt-{uid}"
      bind:this={input}
      bind:value={text}
      oninput={onInput}
      onkeydown={onKey}
      onpaste={onPaste}
      onfocus={() => (focused = true)}
      onblur={onBlur}
      autocomplete="off"
      spellcheck="false"
    />
  </div>
  {@render children?.()}
  {#if suggestions.length && focused}
    <div class="suggest popover" role="listbox">
      {#each suggestions as s, i (s.group ? `group ${s.group.id}` : s.person.email)}
        <button class="opt" class:hi={i === hi} role="option" aria-selected={i === hi} onmouseenter={() => (hi = i)} onmousedown={(e) => { e.preventDefault(); pick(s); }}>
          {#if s.group}
            <span class="group"><UsersRound size={15} /></span>
            <span class="who">
              <span class="n">{s.group.name}<span class="count">{` · ${t('groups.peopleCount', { count: s.group.count })}`}</span></span>
            </span>
          {:else}
            <Avatar person={{ email: s.person.email, name: s.person.name }} size={26} />
            <span class="who">
              <span class="n">{s.person.name ?? s.person.email}</span>
              {#if s.person.name}<span class="e">{s.person.email}</span>{/if}
            </span>
          {/if}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .field {
    position: relative;
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 38px;
    padding: 3px 0;
    border-bottom: 1px solid var(--border);
  }
  .lbl {
    color: var(--text-3);
    min-width: 54px;
    flex-shrink: 0;
    font-size: var(--text-base);
  }
  .chips {
    flex: 1;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px;
    min-width: 0;
    cursor: text;
  }
  .chips input {
    flex: 1;
    min-width: 120px;
    height: 28px;
    border: none;
    background: none;
  }
  .chip-addr {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 24px;
    padding: 0 4px 0 9px;
    border-radius: 7px;
    font-weight: 500;
    font-size: var(--text-sm);
    background: var(--accent-soft);
    color: var(--accent-text);
  }
  .chip-addr.sel {
    background: var(--accent-solid);
    color: var(--accent-fg);
  }
  .chip-addr.invalid {
    background: var(--red-soft);
    color: var(--red);
  }
  .chip-addr button {
    display: grid;
    place-items: center;
    width: 16px;
    height: 16px;
    border-radius: 5px;
    opacity: 0.7;
  }
  .chip-addr button:hover {
    opacity: 1;
    background: rgb(0 0 0 / 0.08);
  }
  .suggest {
    position: absolute;
    left: 56px;
    top: calc(100% + 4px);
    width: min(360px, 100%);
    z-index: 50;
  }
  .opt {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 6px 8px;
    border-radius: 8px;
    text-align: left;
  }
  .opt.hi {
    background: var(--bg-hover);
  }
  .who {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .n {
    font-weight: 550;
  }
  .e {
    font-size: var(--text-sm);
    color: var(--text-3);
  }
  /* A contact group: its icon where a person has a face, and how many people it holds. */
  .group {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    flex-shrink: 0;
    border-radius: 50%;
    color: var(--accent-text);
    background: var(--accent-soft);
  }
  .count {
    font-weight: 400;
    color: var(--text-3);
  }
</style>
