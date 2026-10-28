<script lang="ts">
  import { ui } from '../stores/ui.svelte';
  import { t, tParts, type MessageKey } from '../i18n';
  import { X } from '../icons';

  // Combos are a notation, not display text: keys separated by spaces, "Ctrl K" (together),
  // "J / K" (either), "G then I" (one after the other). Key names and "then" are translated when shown.
  const groups = $derived([
    {
      id: 'navigation',
      title: t('shortcuts.groupNavigation'),
      keys: [
        ['J / K', t('shortcuts.nextPreviousConversation')],
        ['Enter / O', t('shortcuts.openConversation')],
        ['Esc', t('shortcuts.closeClearSelection')],
        ['/', t('common.search')],
        ['Ctrl K', t('shortcuts.commandPalette')],
        ['G then I', t('shortcuts.goToInbox')],
        ['G then S', t('shortcuts.goToStarred')],
        ['G then D', t('shortcuts.goToDrafts')],
        ['G then T', t('shortcuts.goToSent')],
        ['G then C', t('shortcuts.openCalendar')],
        ['G then P', t('shortcuts.openContacts')],
        ['1 / 2 / 3', t('shortcuts.splitInboxTabs')],
      ],
    },
    {
      id: 'actions',
      title: t('shortcuts.groupActions'),
      keys: [
        ['E', t('shortcuts.archive')],
        ['# / Delete', t('common.delete')],
        ['S', t('shortcuts.star')],
        ['U', t('shortcuts.markReadUnread')],
        ['H', t('shortcuts.snooze')],
        ['V', t('shortcuts.moveToFolder')],
        ['L', t('shortcuts.labelGmail')],
        ['!', t('shortcuts.reportSpam')],
        ['X', t('shortcuts.selectConversation')],
        ['Z', t('common.undo')],
        ['Ctrl A', t('shortcuts.selectAll')],
      ],
    },
    {
      id: 'writing',
      title: t('shortcuts.groupWriting'),
      keys: [
        ['C', t('shortcuts.newMessage')],
        ['R', t('shortcuts.reply')],
        ['A', t('shortcuts.replyAll')],
        ['F', t('shortcuts.forward')],
        ['Ctrl Enter', t('common.send')],
        ['Ctrl K', t('shortcuts.insertLink')],
        ['Ctrl B / I / U', t('shortcuts.boldItalicUnderline')],
        ['Esc', t('shortcuts.closeComposer')],
      ],
    },
    {
      id: 'calendar',
      title: t('shortcuts.groupCalendar'),
      keys: [
        ['C', t('shortcuts.newEvent')],
        ['T', t('shortcuts.jumpToToday')],
        ['D / W / M / A', t('shortcuts.calendarViews')],
        ['J / K', t('shortcuts.nextPreviousPeriod')],
        ['G then M', t('shortcuts.backToMail')],
        ['Ctrl Enter', t('shortcuts.saveEvent')],
      ],
    },
    {
      id: 'contacts',
      title: t('shortcuts.groupContacts'),
      keys: [
        ['C', t('shortcuts.newContact')],
        ['/', t('shortcuts.searchContacts')],
        ['J / K', t('shortcuts.nextPreviousContact')],
        ['E', t('shortcuts.editContact')],
        ['Ctrl S', t('shortcuts.saveContact')],
      ],
    },
  ]);

  // Named keys are translated ("Strg", "Entrée"); letters and symbols stay as they are.
  const keyNames: Record<string, MessageKey> = { Ctrl: 'keys.ctrl', Enter: 'keys.enter', Esc: 'keys.esc', Delete: 'keys.delete' };
  function keyLabel(key: string): string {
    const name: MessageKey | undefined = keyNames[key];
    return name ? t(name) : key;
  }
</script>

{#snippet combo(keys: string)}
  {#each keys.split(' ') as part, i (i)}
    {#if part === '/' && keys.includes(' / ')}<span class="or">/</span>{:else}<span class="kbd">{keyLabel(part)}</span>{/if}
  {/each}
{/snippet}

<div class="backdrop" role="presentation" onclick={() => ui.close()}>
  <div class="sheet" role="dialog" aria-label={t('shortcuts.title')} tabindex="-1" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === 'Escape' && ui.close()}>
    <header>
      <h2>{t('shortcuts.title')}</h2>
      <button class="icon-btn" aria-label={t('common.close')} onclick={() => ui.close()}><X size={17} /></button>
    </header>
    <div class="grid">
      {#each groups as g (g.id)}
        <section>
          <h3>{g.title}</h3>
          {#each g.keys as [k, label] (k)}
            <div class="row">
              <span>{label}</span>
              <span class="keys">
                {#if k.includes(' then ')}
                  {@const [first, second] = k.split(' then ')}
                  {#each tParts('shortcuts.sequence') as part, i (i)}
                    {#if typeof part !== 'string'}{@render combo(part.slot === 'first' ? first : second)}{:else if part.trim()}<span class="or">{part.trim()}</span>{/if}
                  {/each}
                {:else}
                  {@render combo(k)}
                {/if}
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
  /* A longer translation wraps onto a second line instead of running into the keys. */
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    min-height: 32px;
    padding: 4px 0;
    border-bottom: 1px solid var(--border);
    color: var(--text-2);
  }
  .keys {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
  }
  .or {
    color: var(--text-3);
    font-size: 11px;
  }
</style>
