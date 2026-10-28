<script lang="ts">
  import { ui } from '../stores/ui.svelte';
  import { X } from '../icons';

  const groups = [
    {
      title: 'Navigation',
      keys: [
        ['J / K', 'Next / previous conversation'],
        ['Enter / O', 'Open conversation'],
        ['Esc', 'Close / clear selection'],
        ['/', 'Search'],
        ['Ctrl K', 'Command palette'],
        ['G then I', 'Go to Inbox'],
        ['G then S', 'Go to Starred'],
        ['G then D', 'Go to Drafts'],
        ['G then T', 'Go to Sent'],
        ['G then C', 'Open Calendar'],
        ['1 / 2 / 3', 'Split inbox tabs'],
      ],
    },
    {
      title: 'Actions',
      keys: [
        ['E', 'Archive'],
        ['# / Delete', 'Delete'],
        ['S', 'Star'],
        ['U', 'Mark read / unread'],
        ['H', 'Snooze'],
        ['V', 'Move to folder'],
        ['L', 'Label (Gmail)'],
        ['!', 'Report spam'],
        ['X', 'Select conversation'],
        ['Z', 'Undo'],
        ['Ctrl A', 'Select all'],
      ],
    },
    {
      title: 'Writing',
      keys: [
        ['C', 'New message'],
        ['R', 'Reply'],
        ['A', 'Reply all'],
        ['F', 'Forward'],
        ['Ctrl Enter', 'Send'],
        ['Ctrl K', 'Insert link (in editor)'],
        ['Ctrl B / I / U', 'Bold / italic / underline'],
        ['Esc', 'Close composer (keeps draft)'],
      ],
    },
    {
      title: 'Calendar',
      keys: [
        ['C', 'New event'],
        ['T', 'Jump to today'],
        ['D / W / M / A', 'Day / week / month / agenda'],
        ['J / K', 'Next / previous period'],
        ['G then M', 'Back to Mail'],
        ['Ctrl Enter', 'Save event'],
      ],
    },
  ];
</script>

<div class="backdrop" role="presentation" onclick={() => ui.close()}>
  <div class="sheet" role="dialog" aria-label="Keyboard shortcuts" tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === 'Escape' && ui.close()}>
    <header>
      <h2>Keyboard shortcuts</h2>
      <button class="icon-btn" aria-label="Close" onclick={() => ui.close()}><X size={17} /></button>
    </header>
    <div class="grid">
      {#each groups as g (g.title)}
        <section>
          <h3>{g.title}</h3>
          {#each g.keys as [k, label] (k + label)}
            <div class="row">
              <span>{label}</span>
              <span class="keys">
                {#each k.split(' ') as part, i (i)}
                  {#if part === '/' && k.includes(' / ')}<span class="or">/</span>{:else if part === 'then'}<span class="or">then</span>{:else}<span class="kbd">{part}</span>{/if}
                {/each}
              </span>
            </div>
          {/each}
        </section>
      {/each}
    </div>
  </div>
</div>

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
  .sheet {
    width: min(980px, 94vw);
    max-height: 86vh;
    overflow: auto;
    border-radius: 18px;
    background: var(--bg-elevated);
    box-shadow: var(--shadow-lg);
    padding: 22px 26px 26px;
    animation: pop-in var(--dur) var(--ease-out);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 14px;
  }
  h2 {
    margin: 0;
    font-size: var(--text-xl);
    font-weight: 680;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
    gap: 28px;
  }
  h3 {
    margin: 0 0 8px;
    font-size: 11px;
    font-weight: 650;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-3);
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    height: 32px;
    border-bottom: 1px solid var(--border);
    color: var(--text-2);
  }
  .keys {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .or {
    color: var(--text-3);
    font-size: 11px;
  }
</style>
