<script lang="ts">
  import { onMount } from 'svelte';
  import type { EditScope } from '../../api';
  import { cal } from '../../stores/calendar.svelte';
  import { t, type MessageKey } from '../../i18n';
  import { modal } from '../../actions/modal';

  const ask = cal.scopeAsk!;
  let choice = $state<EditScope>('this');
  let first: HTMLInputElement | undefined = $state();
  onMount(() => first?.focus());

  const options: { value: EditScope; label: MessageKey }[] = [
    { value: 'this', label: 'calendar.scope.thisEvent' },
    { value: 'following', label: 'calendar.scope.thisAndFollowing' },
    { value: 'all', label: 'calendar.scope.allEvents' },
  ];

  function key(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      ask.resolve(null);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      ask.resolve(choice);
    }
  }
</script>

<div class="backdrop" role="presentation" onclick={() => ask.resolve(null)}>
  <div class="dialog" role="dialog" aria-label={ask.title} tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={key} use:modal>
    <h3>{ask.title}</h3>
    <div class="opts" role="radiogroup">
      {#each options as o, i (o.value)}
        {#if i === 0}
          <label><input bind:this={first} type="radio" name="scope" value={o.value} bind:group={choice} />{t(o.label)}</label>
        {:else}
          <label><input type="radio" name="scope" value={o.value} bind:group={choice} />{t(o.label)}</label>
        {/if}
      {/each}
    </div>
    <footer>
      <button class="btn" onclick={() => ask.resolve(null)}>{t('common.cancel')}</button>
      <button class="btn primary" onclick={() => ask.resolve(choice)}>{t('common.ok')}</button>
    </footer>
  </div>
</div>

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
    min-width: 340px;
    max-width: min(460px, 94vw);
    padding: 20px 20px 16px;
    border-radius: 16px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    animation: pop-in var(--dur) var(--ease-out);
  }
  h3 {
    margin: 0 0 14px;
    font-size: var(--text-lg);
    font-weight: 650;
  }
  .opts {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  label {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 34px;
    padding: 6px 8px;
    border-radius: 8px;
    font-weight: 500;
    cursor: pointer;
  }
  label:hover {
    background: var(--bg-hover);
  }
  input {
    flex-shrink: 0;
    accent-color: var(--accent);
    width: 16px;
    height: 16px;
  }
  footer {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }
</style>
