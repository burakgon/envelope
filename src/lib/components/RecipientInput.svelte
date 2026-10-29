<script lang="ts">
  import { api } from '../api';
  import type { Address, ContactSuggestion } from '../api';
  import { displayName } from '../utils/format';
  import { t } from '../i18n';
  import Avatar from './Avatar.svelte';
  import { X } from '../icons';

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
  let suggestions = $state<ContactSuggestion[]>([]);
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
    const fresh = list.filter((a) => !existing.has(a.email.toLowerCase()));
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
      const res = await api.suggestContacts(q);
      const taken = new Set(value.map((a) => a.email.toLowerCase()));
      suggestions = res.filter((s) => !taken.has(s.email.toLowerCase())).slice(0, 6);
      hi = 0;
    }, 70);
  }

  function pick(s: ContactSuggestion) {
    add([{ email: s.email, name: s.name }]);
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
      {#each suggestions as s, i (s.email)}
        <button class="opt" class:hi={i === hi} role="option" aria-selected={i === hi} onmouseenter={() => (hi = i)} onmousedown={(e) => { e.preventDefault(); pick(s); }}>
          <Avatar person={{ email: s.email, name: s.name }} size={26} />
          <span class="who">
            <span class="n">{s.name ?? s.email}</span>
            {#if s.name}<span class="e">{s.email}</span>{/if}
          </span>
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
</style>
