<script lang="ts">
  import type { Occurrence } from '../../api';
  import { Video, Repeat } from '../../icons';
  import { t } from '../../i18n';
  import { timeOf } from '../../utils/format';

  let {
    occurrence: o,
    compact = false,
    onpointerdown,
  }: { occurrence: Occurrence; compact?: boolean; onpointerdown?: (e: PointerEvent) => void } = $props();

  const pending = $derived(o.myStatus === 'needsAction' || o.myStatus === 'tentative');
  const declined = $derived(o.myStatus === 'declined' || o.status === 'cancelled');
</script>

<div
  class="evc"
  class:compact
  class:pending
  class:declined
  class:free={o.transparent}
  style:--c={o.color}
  role="button"
  tabindex="0"
  {onpointerdown}
  title="{o.title}{o.location ? ` · ${o.location}` : ''}"
>
  <div class="title">
    {o.title}
    {#if compact}<span class="t">{timeOf(o.start)}</span>{/if}
  </div>
  {#if !compact}
    <div class="meta">
      {t('calendar.event.timeRange', { start: timeOf(o.start), end: timeOf(o.end) })}
      {#if o.conferenceUrl}<Video size={11} />{/if}
      {#if o.isRecurring}<Repeat size={10} />{/if}
    </div>
    {#if o.location}<div class="loc">{o.location}</div>{/if}
  {/if}
</div>

<style>
  .evc {
    height: 100%;
    overflow: hidden;
    padding: 3px 7px 3px 8px;
    border-radius: 7px;
    font-size: 12px;
    line-height: 1.3;
    color: color-mix(in oklab, var(--c) 55%, var(--text));
    background: color-mix(in oklab, var(--c) 17%, var(--bg));
    border-left: 3px solid var(--c);
    cursor: grab;
    transition: background var(--dur-fast), box-shadow var(--dur-fast);
    user-select: none;
  }
  :global([data-theme='dark']) .evc {
    color: color-mix(in oklab, var(--c) 45%, white);
    background: color-mix(in oklab, var(--c) 26%, var(--bg));
  }
  .evc:hover {
    background: color-mix(in oklab, var(--c) 25%, var(--bg));
    box-shadow: 0 2px 8px rgb(0 0 0 / 0.08);
  }
  :global([data-theme='dark']) .evc:hover {
    background: color-mix(in oklab, var(--c) 34%, var(--bg));
  }
  .evc.pending {
    background: repeating-linear-gradient(
      135deg,
      color-mix(in oklab, var(--c) 12%, var(--bg)) 0 6px,
      color-mix(in oklab, var(--c) 20%, var(--bg)) 6px 12px
    );
  }
  .evc.declined {
    opacity: 0.55;
    text-decoration: line-through;
  }
  .evc.free {
    background: var(--bg);
    box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--c) 40%, transparent);
  }
  .title {
    font-weight: 650;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .compact .title {
    font-size: 11.5px;
  }
  .t {
    font-weight: 500;
    opacity: 0.8;
    margin-left: 4px;
  }
  .meta {
    font-size: 11px;
    opacity: 0.85;
    font-variant-numeric: tabular-nums;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta :global(svg) {
    vertical-align: -1px;
    margin-left: 3px;
  }
  .loc {
    font-size: 11px;
    opacity: 0.75;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
