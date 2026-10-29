<script lang="ts">
  // Above the message list: an account that needs a new sign-in, or keeps failing to sync.
  import { health } from '../stores/accountHealth.svelte';
  import { t } from '../i18n';
  import { AlertCircle, KeyRound, WifiOff, X } from '../icons';

  $effect(() => health.forgetRecovered());
</script>

{#each health.alerts as a (a.id)}
  {@const problem = health.problem(a)}
  <div class="alert" class:sign-in={problem === 'signIn'} role="alert">
    <span class="icon">
      {#if problem === 'signIn'}<KeyRound size={15} />{:else if problem === 'unreachable'}<WifiOff size={15} />{:else}<AlertCircle size={15} />{/if}
    </span>
    <span class="text">{health.text(a)}</span>
    <button class="icon-btn sm close" aria-label={t('accountAlert.dismiss')} title={t('accountAlert.dismiss')} onclick={() => health.dismiss(a)}><X size={14} /></button>
    <div class="actions">
      {#if problem === 'signIn'}
        <button class="btn sm primary" onclick={() => health.signIn(a)}>{t('accountAlert.signInButton')}</button>
      {:else}
        <button class="btn sm" onclick={() => health.retry(a)}>{t('accountAlert.retry')}</button>
        <button class="btn sm ghost" onclick={() => health.details(a)}>{t('accountAlert.details')}</button>
      {/if}
    </div>
  </div>
{/each}

<style>
  .alert {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: start;
    gap: 6px 10px;
    margin: 0 12px 8px;
    padding: 10px 6px 10px 12px;
    border-radius: 12px;
    background: var(--amber-soft);
    box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--amber) 28%, transparent);
    animation: slide-up var(--dur) var(--ease-out);
  }
  .alert.sign-in {
    background: var(--accent-soft);
    box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--accent) 26%, transparent);
  }
  .icon {
    display: grid;
    place-items: center;
    height: 20px;
    color: var(--amber-text);
  }
  .sign-in .icon {
    color: var(--accent-text);
  }
  .text {
    min-width: 0;
    padding-top: 1px;
    font-weight: 550;
    line-height: 1.4;
    overflow-wrap: anywhere;
  }
  .close {
    width: 22px;
    height: 22px;
    margin-top: -1px;
  }
  .actions {
    grid-column: 2 / 4;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
</style>
