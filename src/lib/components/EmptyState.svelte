<script lang="ts">
  import { mail } from '../stores/mail.svelte';
  import { t, type MessageKey, type Params } from '../i18n';

  type Kind = 'zero' | 'search' | 'snoozed' | 'starred' | 'empty';

  const kind = $derived.by((): Kind => {
    if (mail.search) return 'search';
    if (mail.isInboxView) return 'zero';
    if (mail.mailbox.kind === 'snoozed') return 'snoozed';
    if (mail.mailbox.kind === 'starred') return 'starred';
    return 'empty';
  });

  // Search operators and shortcut keys stay as they are in every language.
  const copy: Record<Kind, { title: MessageKey; body: MessageKey; params?: Params }> = {
    zero: { title: 'emptyState.inboxZeroTitle', body: 'emptyState.inboxZeroBody' },
    search: { title: 'emptyState.searchTitle', body: 'emptyState.searchBody', params: { operators: 'from:, has:attachment, after:2026-01-01' } },
    snoozed: { title: 'emptyState.snoozedTitle', body: 'emptyState.snoozedBody', params: { key: 'H' } },
    starred: { title: 'emptyState.starredTitle', body: 'emptyState.starredBody', params: { key: 'S' } },
    empty: { title: 'emptyState.folderEmptyTitle', body: 'emptyState.folderEmptyBody' },
  };
</script>

<div class="empty">
  {#if kind === 'zero'}
    <svg class="art" viewBox="0 0 200 140" aria-hidden="true">
      <defs>
        <linearGradient id="es-sky" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stop-color="oklch(0.75 0.12 var(--accent-h))" stop-opacity=".35" />
          <stop offset="1" stop-color="oklch(0.75 0.12 var(--accent-h))" stop-opacity="0" />
        </linearGradient>
      </defs>
      <circle cx="100" cy="70" r="62" fill="url(#es-sky)" />
      <circle cx="132" cy="44" r="13" fill="oklch(0.85 0.13 85)" opacity=".9" />
      <path d="M38 104 Q70 78 100 96 T162 92 L162 120 L38 120 Z" fill="oklch(0.7 0.13 var(--accent-h))" opacity=".28" />
      <path d="M30 112 Q72 90 108 106 T172 104 L172 124 L30 124 Z" fill="oklch(0.62 0.17 var(--accent-h))" opacity=".38" />
      <g transform="translate(76 52)">
        <rect width="48" height="34" rx="7" fill="var(--bg-elevated)" stroke="oklch(0.6 0.18 var(--accent-h))" stroke-width="2.5" />
        <path d="M6 7 L24 20 L42 7" fill="none" stroke="oklch(0.6 0.18 var(--accent-h))" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" />
      </g>
    </svg>
  {:else}
    <svg class="art small" viewBox="0 0 120 90" aria-hidden="true">
      <rect x="22" y="18" width="76" height="54" rx="10" fill="var(--bg-sunken)" stroke="var(--border-strong)" stroke-width="2" />
      <path d="M30 28 L60 50 L90 28" fill="none" stroke="var(--text-4)" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" />
    </svg>
  {/if}
  <h2>{t(copy[kind].title)}</h2>
  <p>{t(copy[kind].body, copy[kind].params)}</p>
</div>

<style>
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
    height: 100%;
    padding: 40px 32px 80px;
    animation: fade-in var(--dur-slow) var(--ease);
  }
  .art {
    width: 190px;
    margin-bottom: 14px;
  }
  .art.small {
    width: 110px;
  }
  h2 {
    margin: 0 0 6px;
    font-size: var(--text-lg);
    font-weight: 650;
    letter-spacing: -0.01em;
  }
  p {
    margin: 0;
    max-width: 280px;
    color: var(--text-3);
    line-height: 1.5;
  }
</style>
