<script lang="ts">
  import type { Address } from '../api';
  import { avatarHue, initials } from '../utils/format';

  let { person, size = 32, ring = null }: { person: Address | null | undefined; size?: number; ring?: string | null } = $props();

  const key = $derived((person?.email ?? '?').toLowerCase());
  const hue = $derived(avatarHue(key));
  const letters = $derived(initials(person));
</script>

<span
  class="avatar"
  style:width="{size}px"
  style:height="{size}px"
  style:font-size="{Math.round(size * 0.4)}px"
  style:--h={hue}
  style:box-shadow={ring ? `0 0 0 2px var(--bg), 0 0 0 3.5px ${ring}` : null}
  aria-hidden="true"
>
  {letters}
</span>

<style>
  .avatar {
    display: inline-grid;
    place-items: center;
    flex-shrink: 0;
    border-radius: 50%;
    font-weight: 600;
    letter-spacing: 0.01em;
    color: oklch(0.42 0.12 var(--h));
    background: linear-gradient(145deg, oklch(0.93 0.06 var(--h)), oklch(0.87 0.09 var(--h)));
    user-select: none;
  }
  :global([data-theme='dark']) .avatar {
    color: oklch(0.9 0.08 var(--h));
    background: linear-gradient(145deg, oklch(0.42 0.1 var(--h)), oklch(0.34 0.09 var(--h)));
  }
</style>
