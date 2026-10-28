<script lang="ts">
  import { onMount } from 'svelte';
  import { formatFingerprint, pgp } from '../stores/pgp.svelte';
  import { KeyRound } from '../icons';

  let value = $state('');
  let busy = $state(false);
  let field: HTMLInputElement | undefined = $state();
  onMount(() => field?.focus());

  const ask = $derived(pgp.ask);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (!value || busy) return;
    busy = true;
    await pgp.submit(value);
    busy = false;
    value = '';
    field?.focus();
  }
</script>

{#if ask}
  <div class="backdrop" role="presentation" onclick={() => pgp.cancel()}>
    <div
      class="dialog"
      role="dialog"
      aria-label="Unlock your key"
      tabindex="-1"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => {
        if (e.key === 'Escape') {
          e.stopPropagation();
          pgp.cancel();
        }
      }}
    >
      <form onsubmit={submit}>
        <div class="head">
          <span class="icon"><KeyRound size={18} /></span>
          <div>
            <h3>Unlock your key</h3>
            <p class="who">{ask.key.userIds[0] ?? ask.key.emails[0] ?? ''}</p>
          </div>
        </div>
        <code class="fp selectable">{formatFingerprint(ask.key.fingerprint)}</code>
        <input bind:this={field} class="input" type="password" autocomplete="off" placeholder="Passphrase" bind:value />
        {#if ask.error}<p class="error">{ask.error}</p>{/if}
        <p class="hint">Envelope remembers it until you quit.</p>
        <footer>
          <button type="button" class="btn" onclick={() => pgp.cancel()}>Cancel</button>
          <button type="submit" class="btn primary" disabled={!value || busy}>Unlock</button>
        </footer>
      </form>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 1500;
    background: var(--overlay);
    display: grid;
    place-items: center;
    animation: fade-in var(--dur-fast);
  }
  .dialog {
    width: 380px;
    padding: 20px;
    border-radius: 16px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    animation: pop-in var(--dur) var(--ease-out);
  }
  form {
    display: grid;
    gap: 10px;
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
  h3 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 650;
  }
  .who {
    margin: 2px 0 0;
    color: var(--text-2);
    font-size: var(--text-sm);
    word-break: break-all;
  }
  .fp {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--text-3);
  }
  .error {
    margin: 0;
    color: var(--red);
    font-size: var(--text-sm);
  }
  .hint {
    margin: 0;
    color: var(--text-3);
    font-size: var(--text-sm);
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }
</style>
