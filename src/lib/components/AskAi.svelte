<script lang="ts">
  import { ui } from '../stores/ui.svelte';
</script>

<div style="position:fixed;inset:0;z-index:1500;background:var(--overlay)" role="presentation" onclick={() => ui.close()}></div>
