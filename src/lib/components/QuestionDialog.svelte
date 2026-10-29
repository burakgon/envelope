<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { dialogs } from '../stores/dialogs.svelte';
  import { t } from '../i18n';
  import { composing } from '../utils/format';
  import { HelpCircle, KeyRound, Link, Pen, Warning } from '../icons';

  // One question; the dialog is made anew for the next one.
  const ask = dialogs.current!;
  const uid = $props.id();
  const danger = ask.kind === 'confirm' && !!ask.danger;
  const Icon = ask.kind === 'confirm' ? (danger ? Warning : HelpCircle) : ask.type === 'password' ? KeyRound : ask.type === 'url' ? Link : Pen;

  let value = $state(ask.kind === 'prompt' ? (ask.value ?? '') : '');
  let box: HTMLDivElement | undefined = $state();
  let field: HTMLInputElement | undefined = $state();
  let yes: HTMLButtonElement | undefined = $state();
  let no: HTMLButtonElement | undefined = $state();
  /** Where the focus was, to go back to. */
  const before = document.activeElement instanceof HTMLElement ? document.activeElement : null;

  onMount(() => {
    if (field) {
      field.focus();
      field.select();
    } else (danger ? no : yes)?.focus();
  });

  // Once the dialog is gone, and unless what the answer did put the focus somewhere.
  onDestroy(() =>
    queueMicrotask(() => {
      const now = document.activeElement;
      if (before?.isConnected && (!now || now === document.body)) before.focus();
    }),
  );

  function key(e: KeyboardEvent) {
    // The app's shortcuts wait until the question is answered.
    e.stopPropagation();
    if (composing(e)) return;
    if (e.key === 'Escape') {
      e.preventDefault();
      dialogs.answer(false);
    } else if (e.key === 'Enter' && !(e.target as HTMLElement).closest('button')) {
      e.preventDefault();
      dialogs.answer(true, value);
    } else if (e.key === 'Tab' && box) {
      // The focus goes round within the dialog.
      e.preventDefault();
      const stops = [...box.querySelectorAll<HTMLElement>('input, button:not(:disabled)')];
      const i = stops.indexOf(document.activeElement as HTMLElement);
      stops[e.shiftKey ? (i <= 0 ? stops.length - 1 : i - 1) : (i + 1) % stops.length]?.focus();
    }
  }
</script>

<div class="backdrop" role="presentation" onpointerdown={(e) => e.target === e.currentTarget && dialogs.answer(false)}>
  <div
    bind:this={box}
    class="dialog"
    role={ask.kind === 'confirm' ? 'alertdialog' : 'dialog'}
    aria-modal="true"
    aria-labelledby="{uid}-title"
    aria-describedby={ask.message ? `${uid}-message` : undefined}
    tabindex="-1"
    onkeydown={key}
  >
    <div class="head">
      <span class="icon" class:danger><Icon size={18} /></span>
      <h3 id="{uid}-title">{ask.title}</h3>
    </div>
    {#if ask.message}<p class="message" id="{uid}-message">{ask.message}</p>{/if}
    {#if ask.kind === 'confirm' && ask.detail}<code class="detail selectable">{ask.detail}</code>{/if}
    {#if ask.kind === 'prompt'}
      <input
        bind:this={field}
        class="input"
        type={ask.type ?? 'text'}
        {value}
        aria-labelledby="{uid}-title"
        autocomplete="off"
        autocapitalize={ask.type === 'text' ? undefined : 'off'}
        spellcheck={ask.type === 'text' ? undefined : false}
        oninput={(e) => (value = e.currentTarget.value)}
      />
    {/if}
    <footer>
      <button bind:this={no} class="btn" onclick={() => dialogs.answer(false)}>{t('common.cancel')}</button>
      <button bind:this={yes} class="btn {danger ? 'danger' : 'primary'}" onclick={() => dialogs.answer(true, value)}>{ask.confirmLabel ?? t('common.ok')}</button>
    </footer>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1800;
    background: var(--overlay);
    display: grid;
    place-items: center;
    animation: fade-in var(--dur-fast);
  }
  .dialog {
    display: grid;
    gap: 12px;
    width: min(400px, calc(100vw - 32px));
    padding: 20px;
    border-radius: 16px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    animation: pop-in var(--dur) var(--ease-out);
  }
  .dialog:focus-visible {
    outline: none;
  }
  .head {
    display: flex;
    gap: 12px;
    align-items: center;
  }
  .icon {
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    border-radius: 10px;
    background: var(--accent-soft);
    color: var(--accent-text);
    flex-shrink: 0;
  }
  .icon.danger {
    background: var(--red-soft);
    color: var(--red);
  }
  h3 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 650;
    line-height: 1.3;
    overflow-wrap: anywhere;
  }
  .message {
    margin: 0;
    color: var(--text-2);
    line-height: 1.5;
    white-space: pre-line;
  }
  .detail {
    padding: 8px 10px;
    border-radius: 8px;
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--text-2);
    background: var(--bg-sunken);
    word-spacing: 0.2em;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }
</style>
