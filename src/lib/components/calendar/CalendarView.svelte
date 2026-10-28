<script lang="ts">
  import WindowControls from '../WindowControls.svelte';
</script>

<section class="cal">
  <header data-tauri-drag-region><span class="grow" data-tauri-drag-region></span><WindowControls /></header>
  <div class="soon">Calendar</div>
</section>

<style>
  .cal { flex: 1; display: flex; flex-direction: column; background: var(--bg); }
  header { display: flex; align-items: center; height: var(--header-h); padding: 0 12px; }
  .grow { flex: 1; align-self: stretch; }
  .soon { flex: 1; display: grid; place-items: center; color: var(--text-3); }
</style>
