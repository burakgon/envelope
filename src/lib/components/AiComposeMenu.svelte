<script lang="ts">
  import type { DraftInput } from '../api';
  import Popover from './Popover.svelte';

  let {
    x,
    y,
    draft,
    currentText,
    onclose,
    onapply,
    onsubject,
  }: {
    x: number;
    y: number;
    draft: DraftInput;
    currentText: string;
    onclose: () => void;
    onapply: (text: string, replace: boolean) => void;
    onsubject: (s: string) => void;
  } = $props();
  void draft;
  void currentText;
  void onapply;
  void onsubject;
</script>

<Popover {x} {y} {onclose} width={320}>
  <div class="menu-label">AI assistant</div>
  <p style="padding: 4px 10px 10px; margin: 0; color: var(--text-3)">Set up an AI provider in Settings → AI.</p>
</Popover>
