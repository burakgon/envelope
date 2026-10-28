<script lang="ts">
  import { ymd } from '../../stores/calendar.svelte';
  import { dateFormat } from '../../utils/format';
  import Popover from '../Popover.svelte';
  import MiniMonth from './MiniMonth.svelte';

  let { value = $bindable(), onchange }: { value: string; onchange?: () => void } = $props();

  let open = $state<{ x: number; y: number } | null>(null);
  const day = $derived(new Date(`${value}T00:00:00`));
  const label = $derived(
    Number.isNaN(day.getTime())
      ? value
      : dateFormat({
          weekday: 'short',
          month: 'short',
          day: 'numeric',
          year: day.getFullYear() === new Date().getFullYear() ? undefined : 'numeric',
        }).format(day),
  );

  function pick(ms: number) {
    value = ymd(new Date(ms));
    open = null;
    onchange?.();
  }
</script>

<button
  class="field"
  class:open={!!open}
  onclick={(e) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    open = open ? null : { x: r.left, y: r.bottom + 4 };
  }}>{label}</button>

{#if open}
  <Popover x={open.x} y={open.y} onclose={() => (open = null)} width={250}>
    <MiniMonth value={day.getTime()} onpick={pick} />
  </Popover>
{/if}

<style>
  .field {
    height: 34px;
    padding: 0 12px;
    border-radius: var(--r);
    font-weight: 500;
    background: var(--bg-input);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    white-space: nowrap;
    transition: box-shadow var(--dur-fast);
  }
  .field:hover {
    box-shadow: inset 0 0 0 1px var(--text-4);
  }
  .field.open {
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
</style>
