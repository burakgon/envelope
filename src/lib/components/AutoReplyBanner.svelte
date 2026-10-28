<script lang="ts">
  import type { Account } from '../api';
  import { Away } from '../icons';
  import { autoReplies } from '../stores/autoReplies.svelte';
  import { mail } from '../stores/mail.svelte';
  import AutoReplyDialog from './settings/AutoReplyDialog.svelte';

  // A reminder while replies go out, so they are not left on by accident.
  const away = $derived(mail.accounts.filter((a) => autoReplies.on[a.id]));
  let editing = $state<Account | null>(null);
</script>

{#if away.length}
  <button class="banner" onclick={() => (editing = away[0])} title="Change or turn off automatic replies">
    <Away size={14} />
    <span>
      <b>Automatic replies are on</b>
      <span class="who">{away.length === 1 ? away[0].email : `${away.length} accounts`}</span>
    </span>
  </button>
{/if}

{#if editing}
  <AutoReplyDialog account={editing} onclose={() => (editing = null)} />
{/if}

<style>
  .banner {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 0 10px 8px;
    padding: 9px 12px;
    border-radius: 10px;
    text-align: left;
    font-size: var(--text-sm);
    color: var(--text-2);
    background: color-mix(in oklab, var(--amber) 14%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--amber) 30%, transparent);
  }
  .banner:hover {
    background: color-mix(in oklab, var(--amber) 20%, transparent);
  }
  .banner > :global(svg) {
    flex-shrink: 0;
    color: var(--amber);
  }
  b {
    display: block;
    color: var(--text);
    font-weight: 600;
  }
  .who {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 170px;
  }
</style>
