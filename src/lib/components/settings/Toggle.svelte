<script lang="ts">
  let { checked, onchange, label = '' }: { checked: boolean; onchange: (v: boolean) => void; label?: string } = $props();
</script>

<button class="toggle" class:on={checked} role="switch" aria-checked={checked} aria-label={label} onclick={() => onchange(!checked)}>
  <span class="knob"></span>
</button>

<style>
  .toggle {
    position: relative;
    width: 38px;
    height: 22px;
    border-radius: 11px;
    flex-shrink: 0;
    background: var(--border-strong);
    transition: background var(--dur) var(--ease);
  }
  .toggle.on {
    background: var(--accent);
  }
  .knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 3px rgb(0 0 0 / 0.25);
    transition: transform var(--dur) var(--spring);
  }
  .on .knob {
    transform: translateX(16px);
  }
</style>
